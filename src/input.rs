//! Controller polling thread (XInput), shared state, and performing actions.

use std::sync::atomic::{AtomicBool, AtomicU32, AtomicU64, AtomicU8, AtomicUsize, Ordering::Relaxed};
use std::sync::Mutex;
use std::time::{Duration, Instant};

use windows_sys::Win32::System::LibraryLoader::{GetProcAddress, LoadLibraryW};
use windows_sys::Win32::UI::WindowsAndMessaging::PostMessageW;

use crate::buttons::{self, LT_BIT, RT_BIT, SHARE_BIT};
use crate::engine::{Act, Engine};
use crate::keys::{self, Phase};
use crate::util::{now_ms, w};

// ---- state shared between the poller thread and the UI thread ----
pub static PRESSED: AtomicU32 = AtomicU32::new(0); // buttons held right now (incl. Share)
pub static CONNECTED: AtomicBool = AtomicBool::new(false);
pub static ENABLED: AtomicBool = AtomicBool::new(true);
pub static PAUSED: AtomicBool = AtomicBool::new(false); // editor open: don't fire shortcuts
pub static STOP: AtomicBool = AtomicBool::new(false);
pub static THRESHOLD: AtomicU8 = AtomicU8::new(100);
pub static MAIN_HWND: AtomicUsize = AtomicUsize::new(0);
pub static ENGINE: Mutex<Option<Engine>> = Mutex::new(None);

// Share arrives as keystrokes, seen on the UI thread (raw input / hook).
pub static SHARE_DOWN: AtomicBool = AtomicBool::new(false);
pub static SHARE_LAST_DOWN: AtomicU64 = AtomicU64::new(0);
pub static SHARE_LAST_EVENT: AtomicU64 = AtomicU64::new(0);
const SHARE_LATCH_MS: u64 = 150; // keep a quick tap visible to the poller
const SHARE_STUCK_MS: u64 = 3000; // forget a key-down whose key-up was missed

pub const WM_APP_STATUS: u32 = 0x8000 + 10;

/// Share on Xbox-driver controllers (Xbox One/Series, PDP...), reported by GameInput.
pub static GI_SHARE: AtomicBool = AtomicBool::new(false);

pub fn share_held(now: u64) -> bool {
    GI_SHARE.load(Relaxed)
        || (SHARE_DOWN.load(Relaxed) && now.saturating_sub(SHARE_LAST_EVENT.load(Relaxed)) < SHARE_STUCK_MS)
        || now.saturating_sub(SHARE_LAST_DOWN.load(Relaxed)) < SHARE_LATCH_MS
}

/// XInput has no Share button. Windows' built-in GameInput (v0, GameInput.dll) does report it,
/// through its system-button callback, for controllers on the Xbox driver. Runs on GameInput's
/// own thread; if GameInput isn't available this simply does nothing.
pub fn spawn_gameinput_share() {
    type Callback = unsafe extern "system" fn(u64, *mut core::ffi::c_void, *mut core::ffi::c_void, u64, i32, i32);
    type Register = unsafe extern "system" fn(
        *mut core::ffi::c_void, *mut core::ffi::c_void, i32, *mut core::ffi::c_void, Callback, *mut u64,
    ) -> i32;
    type Create = unsafe extern "system" fn(*mut *mut core::ffi::c_void) -> i32;
    const SYSTEM_BUTTON_SHARE: i32 = 2;

    unsafe extern "system" fn on_system_buttons(
        _token: u64, _ctx: *mut core::ffi::c_void, _device: *mut core::ffi::c_void, _ts: u64, current: i32, _previous: i32,
    ) {
        let now = now_ms();
        let down = current & SYSTEM_BUTTON_SHARE != 0;
        if std::env::var_os("REBINDR_DEBUG").is_some() {
            crate::util::log(&format!("GameInput system buttons: {current} (share {})", if down { "down" } else { "up" }));
        }
        if down && !GI_SHARE.swap(true, Relaxed) {
            SHARE_LAST_DOWN.store(now, Relaxed); // latch: a very quick tap is still seen by the poller
        } else if !down {
            GI_SHARE.store(false, Relaxed);
        }
    }

    std::thread::spawn(|| unsafe {
        let lib = LoadLibraryW(w("GameInput.dll").as_ptr());
        if lib.is_null() {
            return;
        }
        let Some(create) = GetProcAddress(lib, b"GameInputCreate\0".as_ptr()) else { return };
        let create: Create = std::mem::transmute(create);
        let mut gi: *mut core::ffi::c_void = std::ptr::null_mut();
        if create(&mut gi) != 0 || gi.is_null() {
            crate::util::log("GameInput unavailable: Xbox-controller Share won't be detected");
            return;
        }
        // IGameInput (v0) vtable slot 10 = RegisterSystemButtonCallback
        let vtbl = *(gi as *const *const usize);
        // slot 21 = SetFocusPolicy: 0 = default (keep background input flowing to us)
        let set_focus: unsafe extern "system" fn(*mut core::ffi::c_void, i32) = std::mem::transmute(*vtbl.add(21));
        set_focus(gi, 0);
        let register: Register = std::mem::transmute(*vtbl.add(10));
        let mut token = 0u64;
        let hr = register(gi, std::ptr::null_mut(), SYSTEM_BUTTON_SHARE, std::ptr::null_mut(), on_system_buttons, &mut token);
        if hr != 0 {
            crate::util::log(&format!("GameInput Share callback failed: {hr:#x}"));
            return;
        }
        // keep GameInput alive for the life of the app; callbacks arrive on its own thread
        while !STOP.load(Relaxed) {
            std::thread::sleep(Duration::from_millis(500));
        }
    });
}

pub fn perform(act: &Act, phase: Phase) {
    if std::env::var_os("REBINDR_DEBUG").is_some() {
        crate::util::log(&format!("fire: {act:?} {phase:?}"));
    }
    match act {
        Act::Keys(vks) => keys::send(vks, phase),
        Act::Open(target) if phase != Phase::Up => {
            let t = target.clone();
            std::thread::spawn(move || open_target(&t));
        }
        _ => {}
    }
}

fn open_target(target: &str) {
    use windows_sys::Win32::System::Com::{CoInitializeEx, COINIT_APARTMENTTHREADED, COINIT_DISABLE_OLE1DDE};
    use windows_sys::Win32::System::Environment::ExpandEnvironmentStringsW;
    use windows_sys::Win32::UI::Shell::ShellExecuteW;
    use windows_sys::Win32::UI::WindowsAndMessaging::SW_SHOWNORMAL;
    unsafe {
        CoInitializeEx(std::ptr::null(), (COINIT_APARTMENTTHREADED | COINIT_DISABLE_OLE1DDE) as u32);
        let src = w(target.trim().trim_matches('"'));
        let mut buf = vec![0u16; 2048];
        let n = ExpandEnvironmentStringsW(src.as_ptr(), buf.as_mut_ptr(), buf.len() as u32);
        let file = if n > 0 && (n as usize) <= buf.len() { buf } else { src };
        let r = ShellExecuteW(std::ptr::null_mut(), w("open").as_ptr(), file.as_ptr(), std::ptr::null(), std::ptr::null(), SW_SHOWNORMAL);
        if (r as isize) <= 32 {
            crate::util::log(&format!("Could not open {target:?} (code {})", r as isize));
        }
    }
}

#[repr(C)]
#[derive(Default, Clone, Copy)]
struct XState {
    packet: u32,
    buttons: u16,
    lt: u8,
    rt: u8,
    lx: i16,
    ly: i16,
    rx: i16,
    ry: i16,
    _pad: u32, // XInputGetStateEx writes 4 extra bytes
}

type GetState = unsafe extern "system" fn(u32, *mut XState) -> u32;

fn load_xinput() -> Option<GetState> {
    for dll in ["xinput1_4.dll", "xinput1_3.dll", "xinput9_1_0.dll"] {
        unsafe {
            let h = LoadLibraryW(w(dll).as_ptr());
            if h.is_null() {
                continue;
            }
            // Ordinal 100 = XInputGetStateEx, which also reports the Home button.
            let f = GetProcAddress(h, 100usize as *const u8).or_else(|| GetProcAddress(h, b"XInputGetState\0".as_ptr()));
            if let Some(f) = f {
                return Some(std::mem::transmute::<unsafe extern "system" fn() -> isize, GetState>(f));
            }
        }
    }
    None
}

fn notify_ui() {
    let h = MAIN_HWND.load(Relaxed);
    if h != 0 {
        unsafe { PostMessageW(h as _, WM_APP_STATUS, 0, 0) };
    }
}

/// Polls the controller and drives the engine. Adaptive rate: 125 Hz while buttons are in use
/// (accurate combo / long-press timing), ~33 Hz when idle, 4 Hz with no controller.
pub fn spawn_poller() {
    std::thread::spawn(|| {
        let Some(get) = load_xinput() else {
            crate::util::log("XInput is not available");
            return;
        };
        let read = |slot: u32| -> Option<XState> {
            let mut st = XState::default();
            (unsafe { get(slot, &mut st) } == 0).then_some(st)
        };
        let mut slot: Option<u32> = None;
        let mut next_scan = Instant::now();
        let (mut was_active, mut last_pressed, mut last_change) = (false, 0u32, Instant::now());
        let mut perform_cb = |a: &Act, p: Phase| perform(a, p);
        while !STOP.load(Relaxed) {
            let now = Instant::now();
            if slot.is_none() && now >= next_scan {
                next_scan = now + Duration::from_secs(1); // probing empty slots is slow
                slot = (0..4).find(|&s| read(s).is_some());
            }
            let mut pressed = 0u32;
            let mut connected = false;
            if let Some(s) = slot {
                match read(s) {
                    Some(st) => {
                        connected = true;
                        let thr = THRESHOLD.load(Relaxed);
                        pressed = buttons::from_xinput(st.buttons);
                        if st.lt >= thr {
                            pressed |= LT_BIT;
                        }
                        if st.rt >= thr {
                            pressed |= RT_BIT;
                        }
                    }
                    None => slot = None,
                }
            }
            if share_held(now_ms()) {
                pressed |= SHARE_BIT;
            }
            if std::env::var_os("REBINDR_DEBUG").is_some() && (pressed ^ PRESSED.load(Relaxed)) & SHARE_BIT != 0 {
                crate::util::log(&format!("poller: Share {}", if pressed & SHARE_BIT != 0 { "held" } else { "released" }));
            }
            PRESSED.store(pressed, Relaxed);
            if CONNECTED.swap(connected, Relaxed) != connected {
                notify_ui();
            }
            let active = ENABLED.load(Relaxed) && !PAUSED.load(Relaxed);
            if let Ok(mut guard) = ENGINE.lock() {
                if let Some(e) = guard.as_mut() {
                    if active {
                        e.update(pressed, now, &mut perform_cb);
                    } else if was_active {
                        e.reset(&mut perform_cb);
                    }
                }
            }
            was_active = active;
            if pressed != last_pressed {
                last_pressed = pressed;
                last_change = now;
            }
            let ms = if slot.is_none() && pressed == 0 {
                250
            } else if pressed != 0 || now.duration_since(last_change) < Duration::from_millis(1500) {
                8
            } else {
                30
            };
            std::thread::sleep(Duration::from_millis(ms));
        }
        if let Ok(mut guard) = ENGINE.lock() {
            if let Some(e) = guard.as_mut() {
                e.reset(&mut perform_cb);
            }
        }
    });
}

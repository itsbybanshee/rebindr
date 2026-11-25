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

pub fn share_held(now: u64) -> bool {
    (SHARE_DOWN.load(Relaxed) && now.saturating_sub(SHARE_LAST_EVENT.load(Relaxed)) < SHARE_STUCK_MS)
        || now.saturating_sub(SHARE_LAST_DOWN.load(Relaxed)) < SHARE_LATCH_MS
}

pub fn perform(act: &Act, phase: Phase) {
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

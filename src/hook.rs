//! Keyboard-side input.
//!
//! * Raw Input (UI thread): the GameSir dongle's Share button arrives as keystrokes from its
//!   own HID keyboard (VID 3537). Keystrokes from every other keyboard are ignored.
//! * A low-level keyboard hook on its own dedicated thread, installed only while needed:
//!   - while a shortcut uses Share, it swallows the Share burst (Right Win + PrtScn, plus the
//!     Win + Alt + PrtScn echo other software injects) so Game Bar never sees it;
//!   - while the editor records a keyboard shortcut, it captures (and blocks) that chord.
//!   The thread does nothing but answer the hook, so UI work can never delay it (Windows
//!   silently drops hooks that answer too slowly).

use std::cell::RefCell;
use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, Ordering::Relaxed};
use std::sync::Mutex;

use windows_sys::Win32::Foundation::{HWND, LPARAM, LRESULT, WPARAM};
use windows_sys::Win32::System::LibraryLoader::GetModuleHandleW;
use windows_sys::Win32::System::Threading::GetCurrentThreadId;
use windows_sys::Win32::UI::Input::{
    GetRawInputData, GetRawInputDeviceInfoW, RegisterRawInputDevices, HRAWINPUT, RAWINPUT, RAWINPUTDEVICE,
    RAWINPUTHEADER, RIDEV_INPUTSINK, RIDI_DEVICENAME, RID_INPUT, RIM_TYPEKEYBOARD,
};
use windows_sys::Win32::UI::WindowsAndMessaging::{
    CallNextHookEx, DispatchMessageW, GetMessageW, KillTimer, PeekMessageW, PostMessageW, PostThreadMessageW,
    SetTimer, SetWindowsHookExW, UnhookWindowsHookEx, KBDLLHOOKSTRUCT, MSG, PM_NOREMOVE, WH_KEYBOARD_LL, WM_QUIT,
    WM_TIMER,
};

use crate::input::{SHARE_DOWN, SHARE_LAST_DOWN, SHARE_LAST_EVENT};
use crate::keys::{self, OUR_TAG};
use crate::util::{log, now_ms};

pub const WM_APP_KEYREC: u32 = 0x8000 + 21;
const WM_HK_CHECK: u32 = 0x8000 + 30; // hook thread: install/remove the hook as needed
const WM_HK_REPLAY: u32 = 0x8000 + 31; // hook thread: re-send held-back keys
const WM_HK_SHARE_TIMER: u32 = 0x8000 + 32; // hook thread: arm the Share burst timer

const SHARE_START_VK: u32 = 0x5C; // Right Win starts the burst...
const SHARE_TRIGGER_VK: u32 = 0x2C; // ...PrintScreen within it = Share
const SHARE_SEQ_VKS: [u32; 3] = [0x5C, 0x2C, 0xFF];
const ECHO_VKS: [u32; 6] = [0x5B, 0x5C, 0x12, 0xA4, 0xA5, 0x2C];
const SHARE_WINDOW_MS: u64 = 80;
const ECHO_GRACE_MS: u64 = 250;
const STUCK_MS: u64 = 3000;
const LLKHF_INJECTED: u32 = 0x10;
const LLKHF_UP: u32 = 0x80;

struct Seq {
    t0: u64,
    buf: Vec<(u32, u32, u32)>,
    confirmed: bool,
    down: Vec<u32>,
}

struct KeyRec {
    until: u64,
    keys: Vec<String>,
    down: Vec<String>, // by key name, so the hook and raw input can both feed it
    result: Option<Vec<String>>,
    notify: usize, // editor HWND
}

/// Hook health: the hook sees every key before raw input does, so if raw input keeps
/// seeing hardware keys while the hook's call count stands still, Windows dropped the hook.
static HOOK_CALLS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);
const WM_HK_HEAL: u32 = 0x8000 + 33;
/// Reinstall the hook so it is first in line: Windows calls the most recently installed
/// low-level hook first, and other tools (e.g. PowerToys Keyboard Manager) can otherwise
/// grab a hotkey combo like Ctrl+Shift+E before rebindr sees its last key.
const WM_HK_FRONT: u32 = 0x8000 + 34;

/// Shared between the UI thread (settings) and the hook thread (key events).
struct State {
    suppress: bool,
    editor_open: bool,
    seq: Option<Seq>,
    echo_until: u64,
    replay: Vec<(u32, u32, u32)>,
    krec: Option<KeyRec>,
}

static S: Mutex<State> = Mutex::new(State {
    suppress: false,
    editor_open: false,
    seq: None,
    echo_until: 0,
    replay: Vec::new(),
    krec: None,
});
static HOOK_THREAD: AtomicU32 = AtomicU32::new(0);

fn st() -> std::sync::MutexGuard<'static, State> {
    S.lock().unwrap_or_else(|e| e.into_inner())
}

fn post_hook(msg: u32) {
    let t = HOOK_THREAD.load(Relaxed);
    if t != 0 {
        unsafe { PostThreadMessageW(t, msg, 0, 0) };
    }
}

// ------------------------------------------------------------------ setup

pub fn init(ui_hwnd: HWND) {
    let rid = RAWINPUTDEVICE { usUsagePage: 1, usUsage: 6, dwFlags: RIDEV_INPUTSINK, hwndTarget: ui_hwnd };
    if unsafe { RegisterRawInputDevices(&rid, 1, std::mem::size_of::<RAWINPUTDEVICE>() as u32) } == 0 {
        log("RegisterRawInputDevices failed");
    }
    let (tx, rx) = std::sync::mpsc::channel();
    std::thread::spawn(move || hook_thread(tx));
    let _ = rx.recv(); // thread id published and message queue ready
    post_hook(WM_HK_CHECK);
}

pub fn shutdown() {
    post_hook(WM_QUIT);
}

fn hook_thread(ready: std::sync::mpsc::Sender<()>) {
    unsafe {
        let mut msg: MSG = std::mem::zeroed();
        PeekMessageW(&mut msg, std::ptr::null_mut(), 0, 0, PM_NOREMOVE); // create the queue
        HOOK_THREAD.store(GetCurrentThreadId(), Relaxed);
        let _ = ready.send(());
        let mut hook: windows_sys::Win32::UI::WindowsAndMessaging::HHOOK = std::ptr::null_mut();
        let mut check_timer = 0usize;
        let mut share_timer = 0usize;
        while GetMessageW(&mut msg, std::ptr::null_mut(), 0, 0) > 0 {
            match msg.message {
                WM_HK_CHECK => {}
                WM_HK_HEAL | WM_HK_FRONT => {
                    if std::env::var_os("REBINDR_DEBUG").is_some() {
                        log(&format!("hook thread: {} (installed={})", if msg.message == WM_HK_HEAL { "heal" } else { "front" }, !hook.is_null()));
                    }
                    if !hook.is_null() {
                        if msg.message == WM_HK_HEAL {
                            log("Keyboard hook was dropped by Windows \u{2014} reinstalling it");
                        }
                        UnhookWindowsHookEx(hook);
                        hook = std::ptr::null_mut();
                        if check_timer != 0 {
                            KillTimer(std::ptr::null_mut(), check_timer);
                            check_timer = 0;
                        }
                    }
                }
                WM_HK_REPLAY => {
                    let events = std::mem::take(&mut st().replay);
                    keys::replay(&events);
                    continue;
                }
                WM_HK_SHARE_TIMER => {
                    if share_timer != 0 {
                        KillTimer(std::ptr::null_mut(), share_timer);
                    }
                    share_timer = SetTimer(std::ptr::null_mut(), 0, SHARE_WINDOW_MS as u32, None);
                    continue;
                }
                WM_TIMER if msg.wParam == share_timer && share_timer != 0 => {
                    KillTimer(std::ptr::null_mut(), share_timer);
                    share_timer = 0;
                    on_share_timeout(); // the burst never completed, so it wasn't Share
                }
                WM_TIMER if msg.wParam == check_timer => {
                    if std::env::var_os("REBINDR_DEBUG").is_some() {
                        log(&format!("hook calls so far: {}", HOOK_CALLS.load(Relaxed)));
                    }
                }
                _ => {
                    DispatchMessageW(&msg);
                    continue;
                }
            }
            // (re)decide whether the hook is needed
            let need = needed(&st());
            if need && hook.is_null() {
                hook = SetWindowsHookExW(WH_KEYBOARD_LL, Some(ll_hook), GetModuleHandleW(std::ptr::null()), 0);
                if hook.is_null() {
                    log("Keyboard hook failed");
                } else {
                    check_timer = SetTimer(std::ptr::null_mut(), 0, 2000, None); // re-check to remove when idle
                }
            } else if !need && !hook.is_null() {
                UnhookWindowsHookEx(hook);
                hook = std::ptr::null_mut();
                KillTimer(std::ptr::null_mut(), check_timer);
                check_timer = 0;
            }
        }
        if !hook.is_null() {
            UnhookWindowsHookEx(hook);
        }
    }
}

fn needed(s: &State) -> bool {
    s.suppress || s.editor_open || s.krec.is_some() || s.seq.is_some() || now_ms() < s.echo_until
}

pub fn set_suppress(on: bool) {
    st().suppress = on;
    post_hook(WM_HK_CHECK);
}

pub fn set_editor_open(open: bool) {
    st().editor_open = open;
    if open {
        post_hook(WM_HK_FRONT);
    }
    post_hook(WM_HK_CHECK);
}

// ------------------------------------------------------------------ raw input (UI thread)

thread_local! {
    static RAW: RefCell<(Vec<u16>, HashMap<usize, bool>)> = RefCell::new((Vec::new(), HashMap::new()));
    // (hook call count at the previous hardware key, consecutive keys the hook missed, last heal ms)
    static HEAL: RefCell<(u64, u32, u64)> = const { RefCell::new((0, 0, 0)) };
}

/// Every hardware keystroke seen by raw input (UI thread): checks hook health, and records
/// the keyboard shortcut if the hook didn't already catch it.
fn on_raw_key(vk: u16, up: bool, extra: u32, hardware: bool) {
    if extra as usize == OUR_TAG || vk == 0xFF {
        return;
    }
    let calls = HOOK_CALLS.load(Relaxed);
    let hook_wanted = needed(&st());
    let missed = if !hardware {
        0
    } else {
        HEAL.with(|h| {
            let mut h = h.borrow_mut();
            h.1 = if calls == h.0 { h.1 + 1 } else { 0 };
            h.0 = calls;
            h.1
        })
    };
    let now = now_ms();
    if hook_wanted && missed >= 2 && HEAL.with(|h| now.saturating_sub(h.borrow().2) > 1000) {
        HEAL.with(|h| {
            let mut h = h.borrow_mut();
            h.1 = 0;
            h.2 = now;
        });
        post_hook(WM_HK_HEAL);
        post_hook(WM_HK_CHECK);
    }
    let after = {
        let mut s = st();
        if s.krec.is_some() {
            dbg_rec(if hardware { "raw" } else { "raw(inj)" }, &keys::name_of_vk(vk as u32), up);
        }
        record_key(&mut s, keys::name_of_vk(vk as u32), up)
    };
    if let Some((_, After::Notify(h))) = after {
        unsafe { PostMessageW(h as HWND, WM_APP_KEYREC, 0, 0) };
    }
}

fn is_gamesir(devices: &mut HashMap<usize, bool>, dev: usize) -> bool {
    if dev == 0 {
        return false;
    }
    *devices.entry(dev).or_insert_with(|| unsafe {
        let mut n = 0u32;
        GetRawInputDeviceInfoW(dev as _, RIDI_DEVICENAME, std::ptr::null_mut(), &mut n);
        let mut buf = vec![0u16; n as usize + 1];
        GetRawInputDeviceInfoW(dev as _, RIDI_DEVICENAME, buf.as_mut_ptr() as _, &mut n);
        crate::util::from_w(&buf).to_uppercase().contains("VID_3537")
    })
}

pub fn on_raw_input(lparam: LPARAM) {
    unsafe {
        let hsz = std::mem::size_of::<RAWINPUTHEADER>() as u32;
        let mut size = 0u32;
        GetRawInputData(lparam as HRAWINPUT, RID_INPUT, std::ptr::null_mut(), &mut size, hsz);
        if size == 0 || size as usize > 1024 {
            return;
        }
        let mut buf = [0u64; 128]; // aligned scratch
        if GetRawInputData(lparam as HRAWINPUT, RID_INPUT, buf.as_mut_ptr() as _, &mut size, hsz) != size {
            return;
        }
        let ri = &*(buf.as_ptr() as *const RAWINPUT);
        if ri.header.dwType != RIM_TYPEKEYBOARD {
            return;
        }
        let kb = ri.data.keyboard;
        on_raw_key(kb.VKey, kb.Flags & 1 != 0, kb.ExtraInformation, !ri.header.hDevice.is_null());
        RAW.with(|r| {
            let (keys_down, devices) = &mut *r.borrow_mut();
            if !is_gamesir(devices, ri.header.hDevice as usize) {
                return; // not the controller: ignore entirely
            }
            let now = now_ms();
            SHARE_LAST_EVENT.store(now, Relaxed);
            let code = kb.MakeCode | ((kb.Flags & 0x6) << 8);
            if kb.Flags & 1 != 0 {
                keys_down.retain(|&c| c != code);
            } else {
                if keys_down.is_empty() {
                    SHARE_LAST_DOWN.store(now, Relaxed);
                }
                if !keys_down.contains(&code) {
                    keys_down.push(code);
                }
            }
            SHARE_DOWN.store(!keys_down.is_empty(), Relaxed);
        });
    }
}

// ------------------------------------------------------------------ the hook (hook thread)

unsafe extern "system" fn ll_hook(code: i32, wparam: WPARAM, lparam: LPARAM) -> LRESULT {
    HOOK_CALLS.fetch_add(1, Relaxed);
    if code == 0 {
        let k = &*(lparam as *const KBDLLHOOKSTRUCT);
        if handle_key(k.vkCode, k.scanCode, k.flags, k.dwExtraInfo) {
            return 1;
        }
    }
    CallNextHookEx(std::ptr::null_mut(), code, wparam, lparam)
}

enum After {
    Nothing,
    Notify(usize),
    Replay,
    ArmShareTimer,
}

/// Returns true to block the key event.
pub fn handle_key(vk: u32, scan: u32, flags: u32, extra: usize) -> bool {
    if extra == OUR_TAG {
        return false; // our own shortcuts and replays
    }
    let (block, after) = {
        let mut s = st();
        // While recording, every key counts - including keys other software re-sends
        // (e.g. PowerToys Keyboard Manager remaps) - and all of them are blocked.
        if s.krec.is_some() {
            dbg_rec(if flags & LLKHF_INJECTED != 0 { "hook(inj)" } else { "hook" }, &keys::name_of_vk(vk), flags & LLKHF_UP != 0);
        }
        if let Some(r) = record_key(&mut s, keys::name_of_vk(vk), flags & LLKHF_UP != 0) {
            r
        } else if flags & LLKHF_INJECTED != 0 {
            let echo = s.seq.as_ref().is_some_and(|q| q.confirmed) || now_ms() < s.echo_until;
            (echo && ECHO_VKS.contains(&vk), After::Nothing)
        } else if !(s.suppress || s.editor_open) && s.seq.is_none() {
            (false, After::Nothing)
        } else {
            classify(&mut s, vk, scan, flags)
        }
    };
    unsafe {
        match after {
            After::Notify(h) => {
                PostMessageW(h as HWND, WM_APP_KEYREC, 0, 0);
            }
            After::Replay => post_hook(WM_HK_REPLAY),
            After::ArmShareTimer => post_hook(WM_HK_SHARE_TIMER),
            After::Nothing => {}
        }
    }
    block
}

fn dbg_rec(src: &str, name: &Option<String>, up: bool) {
    // REBINDR_DEBUG only, and only modifiers / F13-F24 test keys: never logs real typing
    if std::env::var_os("REBINDR_DEBUG").is_some() {
        if let Some(n) = name {
            if keys::MODIFIERS.contains(&n.as_str()) || (n.starts_with('f') && n.len() == 3) {
                log(&format!("rec {src}: {n} {}", if up { "up" } else { "down" }));
            }
        }
    }
}

fn record_key(s: &mut State, name: Option<String>, up: bool) -> Option<(bool, After)> {
    let r = s.krec.as_mut()?;
    if r.result.is_some() || now_ms() > r.until {
        return None;
    }
    if let Some(name) = name {
        if up {
            r.down.retain(|d| *d != name);
            if r.down.is_empty() && !r.keys.is_empty() {
                r.result = keys::parse_keys(&r.keys.join("+")).ok();
            }
        } else {
            if !r.down.contains(&name) {
                r.down.push(name.clone());
            }
            if !r.keys.contains(&name) {
                r.keys.push(name);
            }
        }
    }
    Some((true, After::Notify(r.notify)))
}

fn classify(s: &mut State, vk: u32, scan: u32, flags: u32) -> (bool, After) {
    let now = now_ms();
    let up = flags & LLKHF_UP != 0;
    let ev = (vk, scan, flags);
    if s.seq.as_ref().is_some_and(|q| now - q.t0 > STUCK_MS) {
        s.seq = None;
    }
    let Some(seq) = s.seq.as_mut() else {
        if !up && vk == SHARE_START_VK {
            s.seq = Some(Seq { t0: now, buf: vec![ev], confirmed: false, down: vec![vk] });
            return (true, After::ArmShareTimer);
        }
        return (false, After::Nothing);
    };
    let track = |down: &mut Vec<u32>| {
        if up {
            down.retain(|&d| d != vk);
        } else if !down.contains(&vk) {
            down.push(vk);
        }
    };
    if seq.confirmed {
        if !SHARE_SEQ_VKS.contains(&vk) {
            return (false, After::Nothing);
        }
        track(&mut seq.down);
        if seq.down.is_empty() {
            SHARE_DOWN.store(false, Relaxed);
            SHARE_LAST_EVENT.store(now, Relaxed);
            s.seq = None;
            s.echo_until = now + ECHO_GRACE_MS;
        }
        return (true, After::Nothing);
    }
    if SHARE_SEQ_VKS.contains(&vk) && now - seq.t0 <= SHARE_WINDOW_MS {
        track(&mut seq.down);
        if vk == SHARE_TRIGGER_VK && !up {
            // It's the Share button: swallow the burst and report the press.
            seq.confirmed = true;
            seq.buf.clear();
            SHARE_DOWN.store(true, Relaxed);
            SHARE_LAST_DOWN.store(now, Relaxed);
            SHARE_LAST_EVENT.store(now, Relaxed);
            return (true, After::Nothing);
        }
        seq.buf.push(ev);
        return (true, After::Nothing);
    }
    // A real key combo (e.g. Win + E): replay everything, in order.
    seq.buf.push(ev);
    (true, flush(s))
}

/// Queues the held-back keys for replay (sent from the hook thread after the hook returns).
fn flush(s: &mut State) -> After {
    match s.seq.take() {
        Some(seq) if !seq.confirmed => {
            s.replay.extend(seq.buf);
            After::Replay
        }
        _ => After::Nothing,
    }
}

fn on_share_timeout() {
    let after = flush(&mut st());
    if let After::Replay = after {
        let events = std::mem::take(&mut st().replay);
        keys::replay(&events);
    }
}

// ------------------------------------------------------------------ keyboard-shortcut recording

pub fn start_key_record(notify: HWND) {
    st().krec = Some(KeyRec { until: now_ms() + 15_000, keys: vec![], down: vec![], result: None, notify: notify as usize });
    post_hook(WM_HK_FRONT); // be first in the hook chain for this recording
    post_hook(WM_HK_CHECK);
}

pub fn stop_key_record() {
    st().krec = None;
    post_hook(WM_HK_CHECK);
}

/// (keys so far, final result, timed out)
pub fn key_record_state() -> (Vec<String>, Option<Vec<String>>, bool) {
    match &st().krec {
        Some(r) => (r.keys.clone(), r.result.clone(), r.result.is_none() && now_ms() > r.until),
        None => (vec![], None, false),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    // Tests share the global state, so they run one at a time (cargo test -- --test-threads=1).
    fn reset(suppress: bool) {
        let mut s = st();
        *s = State { suppress, editor_open: false, seq: None, echo_until: 0, replay: Vec::new(), krec: None };
        SHARE_DOWN.store(false, Relaxed);
        SHARE_LAST_DOWN.store(0, Relaxed);
    }

    const INJ: u32 = 0x10;
    const UP: u32 = 0x80;
    const EXT: u32 = 0x01;

    /// The exact sequence the hook probe captured for one Share press.
    fn share_press() -> Vec<bool> {
        vec![
            handle_key(0x5C, 0x5C, EXT, 0),
            handle_key(0x2C, 0x37, EXT, 0),
            handle_key(0x5B, 0x5B, INJ, 17),
            handle_key(0xA4, 0x38, INJ, 17),
            handle_key(0x2C, 0x54, INJ | EXT, 17),
            handle_key(0x5C, 0x5C, EXT | UP, 0),
            handle_key(0x2C, 0x37, EXT | UP, 0),
            handle_key(0x2C, 0x54, INJ | EXT | UP, 17),
            handle_key(0xA4, 0x38, INJ | UP, 17),
            handle_key(0x5B, 0x5B, INJ | UP, 17),
        ]
    }

    #[test]
    fn share_burst_blocked_and_detected() {
        reset(true);
        assert_eq!(share_press(), vec![true; 10]);
        assert!(crate::input::share_held(now_ms()));
        assert!(st().seq.is_none());
    }

    #[test]
    fn not_suppressing_passes_everything() {
        reset(false);
        assert_eq!(share_press(), vec![false; 10]);
    }

    #[test]
    fn real_win_combo_replayed_in_order() {
        reset(true);
        assert!(handle_key(0x5C, 0x5C, EXT, 0));
        assert!(handle_key(0x45, 0x12, 0, 0));
        assert_eq!(st().replay.iter().map(|e| e.0).collect::<Vec<_>>(), vec![0x5C, 0x45]);
        assert!(!handle_key(0x45, 0x12, UP, 0));
    }

    #[test]
    fn own_and_plain_keys_untouched() {
        reset(true);
        assert!(!handle_key(0x5B, 0x5B, INJ, OUR_TAG));
        assert!(!handle_key(0x41, 0x1E, 0, 0));
        assert!(!handle_key(0x5B, 0x5B, 0, 0));
        assert!(!handle_key(0xA4, 0x38, INJ, 5));
    }

    #[test]
    fn raw_input_records_when_the_hook_misses_keys() {
        reset(false);
        st().krec = Some(KeyRec { until: now_ms() + 5000, keys: vec![], down: vec![], result: None, notify: 0 });
        // raw input reports generic modifiers (0x10 Shift, 0x11 Ctrl), and keys PowerToys re-sends
        for (vk, up, hw) in [(0x11, false, true), (0x10, false, false), (0x52, false, true), (0x52, true, true), (0x10, true, false), (0x11, true, true)] {
            on_raw_key(vk, up, 0, hw);
        }
        assert_eq!(key_record_state().1.unwrap(), ["ctrl", "shift", "r"]);
        st().krec = None;
    }

    #[test]
    fn injected_keys_are_recorded_too() {
        reset(false);
        st().krec = Some(KeyRec { until: now_ms() + 5000, keys: vec![], down: vec![], result: None, notify: 0 });
        assert!(handle_key(0xA2, 0, INJ, 0) && handle_key(0x45, 0, INJ, 0));
        assert!(handle_key(0x45, 0, INJ | UP, 0) && handle_key(0xA2, 0, INJ | UP, 0));
        assert_eq!(key_record_state().1.unwrap(), ["ctrl", "e"]);
        st().krec = None;
    }

    #[test]
    fn records_three_key_chord_and_blocks_it() {
        reset(false);
        st().krec = Some(KeyRec { until: now_ms() + 5000, keys: vec![], down: vec![], result: None, notify: 0 });
        let ev = [(0xA2, 0), (0xA0, 0), (0x52, 0), (0x52, UP), (0xA0, UP), (0xA2, UP)];
        assert!(ev.iter().all(|&(vk, f)| handle_key(vk, 0, f, 0)));
        let (_, result, timeout) = key_record_state();
        assert_eq!(result.unwrap(), ["ctrl", "shift", "r"]);
        assert!(!timeout);
    }
}

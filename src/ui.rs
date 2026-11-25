//! Native, minimal dark UI: the main window (shortcut list, Add, Remove), the Add dialog,
//! and the tray icon. Plain Win32 controls — no web view, no GPU, a few MB of RAM.

#![allow(clippy::missing_safety_doc)]

use std::ptr::{null, null_mut};
use std::sync::atomic::Ordering::Relaxed;

use windows_sys::Win32::Foundation::*;
use windows_sys::Win32::Graphics::Dwm::DwmSetWindowAttribute;
use windows_sys::Win32::Graphics::Gdi::*;
use windows_sys::Win32::System::ApplicationInstallationAndServicing::{ActivateActCtx, CreateActCtxW, ACTCTXW};
use windows_sys::Win32::System::LibraryLoader::{GetModuleFileNameW, GetModuleHandleW, GetProcAddress, LoadLibraryW};
use windows_sys::Win32::System::Registry::*;
use windows_sys::Win32::System::Threading::CreateMutexW;
use windows_sys::Win32::UI::Controls::Dialogs::{GetOpenFileNameW, OFN_FILEMUSTEXIST, OFN_NOCHANGEDIR, OFN_PATHMUSTEXIST, OPENFILENAMEW};
use windows_sys::Win32::UI::Controls::*;
use windows_sys::Win32::UI::HiDpi::{GetDpiForSystem, SetProcessDpiAwarenessContext, DPI_AWARENESS_CONTEXT_SYSTEM_AWARE};
use windows_sys::Win32::UI::Input::KeyboardAndMouse::{EnableWindow, SetFocus, VK_DELETE};
use windows_sys::Win32::UI::Shell::{Shell_NotifyIconW, NIF_ICON, NIF_MESSAGE, NIF_TIP, NIM_ADD, NIM_DELETE, NIM_MODIFY, NOTIFYICONDATAW};
use windows_sys::Win32::UI::WindowsAndMessaging::*;

use crate::config::{self, Action, Config, Mapping};
use crate::engine::{Act, Engine, Rule};
use crate::input::{self, ENABLED, ENGINE, MAIN_HWND, PAUSED, PRESSED, THRESHOLD, WM_APP_STATUS};
use crate::util::{from_w, log, rgb, w, APP_NAME};
use crate::{buttons, hook, icon, keys};

// ---- palette (Windows 11 dark + the rebindr pink) ----
const BG: u32 = rgb(30, 30, 32);
const CARD: u32 = rgb(41, 41, 44);
const CARD_SEL: u32 = rgb(54, 54, 58);
const INPUT: u32 = rgb(44, 44, 47);
const TEXT: u32 = rgb(243, 243, 245);
const MUTED: u32 = rgb(158, 158, 166);
const ACCENT: u32 = rgb(255, 0, 92);
const ACCENT_DOWN: u32 = rgb(214, 0, 78);
const SECOND: u32 = rgb(58, 58, 62);
const SECOND_DOWN: u32 = rgb(72, 72, 78);
const OK: u32 = ACCENT; // success hints use the rebindr pink-red, like the rest of the UI
const ERR: u32 = rgb(255, 120, 120);

// ---- static-control styles (winuser.h) ----
const SS_CENTER: u32 = 0x0001;
const SS_NOTIFY: u32 = 0x0100;
const SS_CENTERIMAGE: u32 = 0x0200;

// ---- ids ----
const ID_LIST: u16 = 100;
const ID_ADD: u16 = 101;
const ID_REMOVE: u16 = 102;
const ID_ENABLED: u16 = 103;
const ID_ENABLED_LBL: u16 = 104;
const ID_STARTUP: u16 = 105;
const ID_STARTUP_LBL: u16 = 106;
const ID_STATUS: u16 = 107;
const ID_EMPTY: u16 = 108;
const ID_IMPORT: u16 = 110;
const ID_EXPORT: u16 = 111;
const T_STATUS: usize = 3;

const IDOK_: u16 = 1;
const IDCANCEL_: u16 = 2;
const ID_E_VAL: u16 = 200;
const ID_E_REC: u16 = 201;
const ID_E_TRIGGER: u16 = 202;
const ID_E_ACTION: u16 = 203;
const ID_E_FIELD: u16 = 204;
const ID_E_FIELDBTN: u16 = 205;
const ID_E_HOLD: u16 = 206;
const ID_E_HOLDLBL: u16 = 207;
const ID_E_HINT: u16 = 208;
const ID_E_FIELDLBL: u16 = 209;

const WM_APP_TRAY: u32 = 0x8000 + 1;
const WM_APP_SHOW: u32 = 0x8000 + 2;
const T_REC: usize = 1;
const T_KREC: usize = 2;
const MAIN_CLASS: &str = "rebindr.main";
const EDITOR_CLASS: &str = "rebindr.editor";

const TRIGGERS: [&str; 2] = ["Press", "Long press (hold \u{00bd} s)"];
const ACTIONS: [&str; 2] = ["Keyboard shortcut", "Open app, file or URL"];
/// The rounded box the Keys / Open input sits in (logical px, editor client coords).
const FIELD_BOX: (i32, i32, i32, i32) = (116, 204, 214, 36);
/// The rounded box for typing / recording the controller button(s).
const BUTTON_BOX: (i32, i32, i32, i32) = (116, 64, 214, 36);
/// Full-width rounded box for the shortcut's name (spans Button box + Record).
const NAME_BOX: (i32, i32, i32, i32) = (116, 18, 364, 36);
const ID_E_NAME: u16 = 210;

struct Ed {
    trigger_idx: usize,
    action_idx: usize,
    field_focus: bool,
    val_focus: bool,
    name_focus: bool,
    name: HWND,
    note: HWND,
    mask: u32,
    rec: bool,
    acc: u32,
    krec: bool,
    dup_ok: bool,
    hint_color: u32,
    val: HWND,
    rec_btn: HWND,
    trigger: HWND,
    action: HWND,
    field_lbl: HWND,
    field: HWND,
    field_btn: HWND,
    hold: HWND,
    hold_lbl: HWND,
    hint: HWND,
}

struct Ui {
    hinst: HINSTANCE,
    main: HWND,
    editor: HWND,
    list: HWND,
    status: HWND,
    empty: HWND,
    chk_enabled: HWND,
    chk_startup: HWND,
    btn_remove: HWND,
    cfg: Config,
    scale: f32,
    font: HFONT,
    font_bold: HFONT,
    font_title: HFONT,
    font_small: HFONT,
    br_bg: HBRUSH,
    br_card: HBRUSH,
    br_input: HBRUSH,
    icon_big: HICON,
    icon_small: HICON,
    logo: HICON,
    taskbar_created: u32,
    hover_x: Option<usize>,
    status_flash: bool,
    ed: Ed,
}

static mut UI: Option<Ui> = None;

/// The UI lives on one thread; window procedures reach it through here.
#[allow(static_mut_refs)]
fn ui() -> &'static mut Ui {
    unsafe { UI.as_mut().expect("ui") }
}

fn s(v: i32) -> i32 {
    (v as f32 * ui().scale).round() as i32
}

fn loword(v: usize) -> u16 {
    (v & 0xFFFF) as u16
}
fn hiword(v: usize) -> u16 {
    ((v >> 16) & 0xFFFF) as u16
}

// ------------------------------------------------------------------ helpers

unsafe fn make_font(pt: f32, weight: i32) -> HFONT {
    let mut lf: LOGFONTW = std::mem::zeroed();
    lf.lfHeight = -((pt * ui().scale * 96.0 / 72.0).round() as i32);
    lf.lfWeight = weight;
    lf.lfQuality = CLEARTYPE_QUALITY as _;
    for (i, c) in "Segoe UI".encode_utf16().enumerate() {
        lf.lfFaceName[i] = c;
    }
    CreateFontIndirectW(&lf)
}

unsafe fn ctl(parent: HWND, class: &str, text: &str, style: u32, x: i32, y: i32, cw: i32, ch: i32, id: u16) -> HWND {
    let h = CreateWindowExW(
        0,
        w(class).as_ptr(),
        w(text).as_ptr(),
        WS_CHILD | WS_VISIBLE | style,
        s(x),
        s(y),
        s(cw),
        s(ch),
        parent,
        id as usize as HMENU,
        ui().hinst,
        null(),
    );
    SendMessageW(h, WM_SETFONT, ui().font as usize, 1);
    h
}

unsafe fn set_text(h: HWND, t: &str) {
    SetWindowTextW(h, w(t).as_ptr());
}

unsafe fn get_text(h: HWND) -> String {
    let n = GetWindowTextLengthW(h);
    let mut buf = vec![0u16; n as usize + 1];
    GetWindowTextW(h, buf.as_mut_ptr(), buf.len() as i32);
    from_w(&buf)
}

unsafe fn checked(h: HWND) -> bool {
    SendMessageW(h, BM_GETCHECK, 0, 0) as u32 == BST_CHECKED
}

unsafe fn set_check(h: HWND, on: bool) {
    SendMessageW(h, BM_SETCHECK, if on { BST_CHECKED as usize } else { 0 }, 0);
}

// Undocumented-but-stable uxtheme exports that turn on dark scrollbars, menus and controls.
unsafe fn uxtheme(ordinal: usize) -> Option<unsafe extern "system" fn() -> isize> {
    let h = LoadLibraryW(w("uxtheme.dll").as_ptr());
    if h.is_null() {
        return None;
    }
    GetProcAddress(h, ordinal as *const u8)
}

unsafe fn enable_dark_app() {
    if let Some(f) = uxtheme(135) {
        let set_preferred_app_mode: unsafe extern "system" fn(i32) -> i32 = std::mem::transmute(f);
        set_preferred_app_mode(2); // ForceDark
    }
    if let Some(f) = uxtheme(136) {
        let flush_menu_themes: unsafe extern "system" fn() = std::mem::transmute(f);
        flush_menu_themes();
    }
}

unsafe fn dark(h: HWND, theme: &str) {
    if let Some(f) = uxtheme(133) {
        let allow: unsafe extern "system" fn(HWND, BOOL) -> BOOL = std::mem::transmute(f);
        allow(h, 1);
    }
    SetWindowTheme(h, w(theme).as_ptr(), null());
}

unsafe fn dark_titlebar(h: HWND) {
    let on: i32 = 1;
    DwmSetWindowAttribute(h, 20, &on as *const i32 as _, 4); // DWMWA_USE_IMMERSIVE_DARK_MODE
    let caption: u32 = BG;
    DwmSetWindowAttribute(h, 35, &caption as *const u32 as _, 4); // DWMWA_CAPTION_COLOR (Win 11)
}

/// Common Controls v6 (modern themed controls) without an embedded manifest.
unsafe fn enable_visual_styles() {
    let dir = crate::util::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let path = dir.join("rebindr.manifest");
    let manifest = r#"<?xml version="1.0" encoding="UTF-8" standalone="yes"?>
<assembly xmlns="urn:schemas-microsoft-com:asm.v1" manifestVersion="1.0">
  <dependency><dependentAssembly>
    <assemblyIdentity type="win32" name="Microsoft.Windows.Common-Controls" version="6.0.0.0"
      processorArchitecture="*" publicKeyToken="6595b64144ccf1df" language="*"/>
  </dependentAssembly></dependency>
</assembly>"#;
    if std::fs::read_to_string(&path).ok().as_deref() != Some(manifest) {
        let _ = std::fs::write(&path, manifest);
    }
    let src = w(&path.to_string_lossy());
    let mut ctx: ACTCTXW = std::mem::zeroed();
    ctx.cbSize = std::mem::size_of::<ACTCTXW>() as u32;
    ctx.lpSource = src.as_ptr();
    let h = CreateActCtxW(&ctx);
    if h != INVALID_HANDLE_VALUE {
        let mut cookie = 0usize;
        ActivateActCtx(h, &mut cookie);
    } else {
        log("Visual styles unavailable");
    }
    let icc = INITCOMMONCONTROLSEX { dwSize: std::mem::size_of::<INITCOMMONCONTROLSEX>() as u32, dwICC: ICC_STANDARD_CLASSES };
    InitCommonControlsEx(&icc);
}

// ------------------------------------------------------------------ launch at sign-in

const RUN_KEY: &str = "Software\\Microsoft\\Windows\\CurrentVersion\\Run";

unsafe fn run_key(access: u32) -> Option<HKEY> {
    let mut k: HKEY = null_mut();
    (RegOpenKeyExW(HKEY_CURRENT_USER, w(RUN_KEY).as_ptr(), 0, access, &mut k) == ERROR_SUCCESS).then_some(k)
}

unsafe fn get_startup() -> bool {
    let Some(k) = run_key(KEY_QUERY_VALUE) else { return false };
    let ok = RegQueryValueExW(k, w(APP_NAME).as_ptr(), null(), null_mut(), null_mut(), null_mut()) == ERROR_SUCCESS;
    RegCloseKey(k);
    ok
}

unsafe fn set_startup(on: bool) {
    let Some(k) = run_key(KEY_SET_VALUE) else { return };
    RegDeleteValueW(k, w("GameSirShortcuts").as_ptr()); // the earlier Python version
    if on {
        let mut buf = vec![0u16; 1024];
        let n = GetModuleFileNameW(null_mut(), buf.as_mut_ptr(), buf.len() as u32) as usize;
        let cmd = w(&format!("\"{}\" --minimized", String::from_utf16_lossy(&buf[..n])));
        RegSetValueExW(k, w(APP_NAME).as_ptr(), 0, REG_SZ, cmd.as_ptr() as *const u8, (cmd.len() * 2) as u32);
    } else {
        RegDeleteValueW(k, w(APP_NAME).as_ptr());
    }
    RegCloseKey(k);
}

// ------------------------------------------------------------------ engine / config glue

fn apply_mappings() {
    let u = ui();
    let rules: Vec<Rule> = u
        .cfg
        .mappings
        .iter()
        .filter(|m| m.enabled)
        .map(|m| Rule {
            mask: m.mask(),
            act: match &m.action {
                Action::Keys { keys } => Act::Keys(keys.iter().filter_map(|k| keys::vk_of(k)).collect()),
                Action::Open { target } => Act::Open(target.clone()),
            },
            hold: m.is_hold(),
            long: m.is_long(),
        })
        .collect();
    if let Ok(mut g) = ENGINE.lock() {
        if let Some(old) = g.as_mut() {
            old.reset(&mut input::perform);
        }
        *g = Some(Engine::new(rules));
    }
    ENABLED.store(u.cfg.enabled, Relaxed);
    THRESHOLD.store(u.cfg.trigger_threshold, Relaxed);
    // Block Share's own Game Bar screenshot only while a shortcut uses Share.
    let share_used = u.cfg.mappings.iter().any(|m| m.enabled && m.mask() & buttons::SHARE_BIT != 0);
    hook::set_suppress(u.cfg.enabled && share_used);
    unsafe { tray(NIM_MODIFY) };
}

// ------------------------------------------------------------------ tray

unsafe fn tray(op: u32) {
    let u = ui();
    let mut nid: NOTIFYICONDATAW = std::mem::zeroed();
    nid.cbSize = std::mem::size_of::<NOTIFYICONDATAW>() as u32;
    nid.hWnd = u.main;
    nid.uID = 1;
    nid.uFlags = NIF_MESSAGE | NIF_ICON | NIF_TIP;
    nid.uCallbackMessage = WM_APP_TRAY;
    nid.hIcon = u.icon_small;
    let tip = format!("rebindr \u{2014} shortcuts {}", if u.cfg.enabled { "on" } else { "off" });
    for (i, c) in tip.encode_utf16().take(127).enumerate() {
        nid.szTip[i] = c;
    }
    Shell_NotifyIconW(op, &nid);
}

unsafe fn tray_menu() {
    let u = ui();
    let m = CreatePopupMenu();
    AppendMenuW(m, MF_STRING, 1, w("Open rebindr").as_ptr());
    AppendMenuW(m, MF_STRING | if u.cfg.enabled { MF_CHECKED } else { 0 }, 2, w("Shortcuts enabled").as_ptr());
    AppendMenuW(m, MF_SEPARATOR, 0, null());
    AppendMenuW(m, MF_STRING, 3, w("Quit").as_ptr());
    let mut pt = POINT { x: 0, y: 0 };
    GetCursorPos(&mut pt);
    SetForegroundWindow(u.main);
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_RIGHTBUTTON, pt.x, pt.y, 0, u.main, null());
    DestroyMenu(m);
    PostMessageW(u.main, WM_NULL, 0, 0);
    match cmd {
        1 => show_main(),
        2 => set_enabled(!u.cfg.enabled),
        3 => quit(),
        _ => {}
    }
}

// ------------------------------------------------------------------ main window

unsafe fn show_main() {
    let h = ui().main;
    ShowWindow(h, if IsIconic(h) != 0 { SW_RESTORE } else { SW_SHOW });
    SetForegroundWindow(h);
    if !ui().editor.is_null() {
        SetForegroundWindow(ui().editor);
    }
}

unsafe fn quit() {
    if !ui().editor.is_null() {
        close_editor();
    }
    tray(NIM_DELETE);
    input::STOP.store(true, Relaxed);
    hook::shutdown();
    DestroyWindow(ui().main);
}

unsafe fn set_enabled(on: bool) {
    let u = ui();
    u.cfg.enabled = on;
    config::save(&u.cfg);
    set_check(u.chk_enabled, on);
    apply_mappings();
}

/// Shows a short message in the status line (in pink) for a few seconds.
unsafe fn flash(msg: &str) {
    let u = ui();
    u.status_flash = true;
    set_text(u.status, msg);
    SetTimer(u.main, T_STATUS, 5000, None);
}

/// Open / save dialog for shortcut files; returns the chosen path.
unsafe fn json_file_dialog(save: bool) -> Option<String> {
    use windows_sys::Win32::UI::Controls::Dialogs::{GetSaveFileNameW, OFN_OVERWRITEPROMPT};
    let u = ui();
    let mut file = vec![0u16; 1024];
    if save {
        for (i, c) in "rebindr-shortcuts.json".encode_utf16().enumerate() {
            file[i] = c;
        }
    }
    let filter: Vec<u16> = "rebindr shortcuts (*.json)\0*.json\0All files\0*.*\0\0".encode_utf16().collect();
    let ext = w("json");
    let mut ofn: OPENFILENAMEW = std::mem::zeroed();
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = u.main;
    ofn.lpstrFilter = filter.as_ptr();
    ofn.lpstrFile = file.as_mut_ptr();
    ofn.nMaxFile = file.len() as u32;
    ofn.lpstrDefExt = ext.as_ptr();
    ofn.Flags = OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR | if save { OFN_OVERWRITEPROMPT } else { OFN_FILEMUSTEXIST };
    let ok = if save { GetSaveFileNameW(&mut ofn) } else { GetOpenFileNameW(&mut ofn) };
    (ok != 0).then(|| from_w(&file))
}

unsafe fn export_shortcuts() {
    let u = ui();
    if u.cfg.mappings.is_empty() {
        return flash("Nothing to export yet \u{2014} add a shortcut first.");
    }
    let Some(path) = json_file_dialog(true) else { return };
    match std::fs::write(&path, config::export_json(&u.cfg.mappings)) {
        Ok(_) => flash(&format!("Exported to {}", file_name(&path))),
        Err(e) => flash(&format!("Couldn\u{2019}t save the file: {e}")),
    }
}

unsafe fn import_shortcuts() {
    let u = ui();
    let Some(path) = json_file_dialog(false) else { return };
    let text = match std::fs::read_to_string(&path) {
        Ok(t) => t,
        Err(e) => return flash(&format!("Couldn\u{2019}t read the file: {e}")),
    };
    match config::import_json(&text, &mut u.cfg.mappings) {
        Ok(r) => {
            if r.added > 0 {
                config::save(&u.cfg);
                apply_mappings();
                refresh_list();
            }
            flash(&format!("Imported {}", file_name(&path)));
        }
        Err(e) => flash(&e),
    }
}

fn file_name(path: &str) -> &str {
    path.rsplit(['\\', '/']).next().unwrap_or(path)
}

unsafe fn update_status() {
    if ui().status_flash {
        return; // a message is showing; the timer restores the status
    }
    let t = if input::CONNECTED.load(Relaxed) { "Controller connected" } else { "No controller connected" };
    set_text(ui().status, t);
}

unsafe fn refresh_list() {
    let u = ui();
    SendMessageW(u.list, LB_RESETCONTENT, 0, 0);
    for i in 0..u.cfg.mappings.len() {
        SendMessageW(u.list, LB_ADDSTRING, 0, i as isize);
    }
    let empty = u.cfg.mappings.is_empty();
    ShowWindow(u.empty, if empty { SW_SHOW } else { SW_HIDE });
    ShowWindow(u.list, if empty { SW_HIDE } else { SW_SHOW });
    EnableWindow(u.btn_remove, 0);
}

unsafe fn remove_selected() {
    let sel = SendMessageW(ui().list, LB_GETCURSEL, 0, 0);
    if sel >= 0 {
        remove_at(sel as usize);
    }
}

unsafe fn remove_at(idx: usize) {
    let u = ui();
    if idx >= u.cfg.mappings.len() {
        return;
    }
    let sel = SendMessageW(u.list, LB_GETCURSEL, 0, 0);
    u.cfg.mappings.remove(idx);
    config::save(&u.cfg);
    apply_mappings();
    refresh_list();
    let n = u.cfg.mappings.len() as isize;
    if sel >= 0 && n > 0 {
        let keep = if sel as usize > idx { sel - 1 } else { sel };
        SendMessageW(u.list, LB_SETCURSEL, keep.min(n - 1) as usize, 0);
        EnableWindow(u.btn_remove, 1);
    }
}

unsafe fn create_main_controls(h: HWND) {
    let u = ui();
    // stops short of the Import button; long messages end in "…"
    u.status = ctl(h, "STATIC", "", 0x4000 /* SS_ENDELLIPSIS */, 72, 46, 226, 20, ID_STATUS);
    SendMessageW(u.status, WM_SETFONT, u.font_small as usize, 1);
    // header: share your setup
    ctl(h, "BUTTON", "Import", (BS_OWNERDRAW as u32) | WS_TABSTOP, 306, 24, 94, 34, ID_IMPORT);
    ctl(h, "BUTTON", "Export", (BS_OWNERDRAW as u32) | WS_TABSTOP, 406, 24, 94, 34, ID_EXPORT);
    u.chk_enabled = ctl(h, "BUTTON", "", (BS_AUTOCHECKBOX as u32) | WS_TABSTOP, 262, 410, 20, 22, ID_ENABLED);
    ctl(h, "STATIC", "Enabled", SS_NOTIFY | SS_CENTERIMAGE, 286, 409, 64, 24, ID_ENABLED_LBL);
    u.list = ctl(
        h,
        "LISTBOX",
        "",
        (LBS_OWNERDRAWFIXED | LBS_NOTIFY | LBS_NOINTEGRALHEIGHT | LBS_WANTKEYBOARDINPUT) as u32 | WS_VSCROLL | WS_TABSTOP,
        20,
        88,
        480,
        300,
        ID_LIST,
    );
    u.empty = ctl(h, "STATIC", "No shortcuts yet.\nClick Add to map a controller button to a shortcut.", SS_CENTER, 20, 200, 480, 48, ID_EMPTY);
    ctl(h, "BUTTON", "Add", (BS_OWNERDRAW as u32) | WS_TABSTOP, 20, 404, 108, 34, ID_ADD);
    u.btn_remove = ctl(h, "BUTTON", "Remove", (BS_OWNERDRAW as u32) | WS_TABSTOP, 136, 404, 108, 34, ID_REMOVE);
    u.chk_startup = ctl(h, "BUTTON", "", (BS_AUTOCHECKBOX as u32) | WS_TABSTOP, 360, 410, 20, 22, ID_STARTUP);
    ctl(h, "STATIC", "Launch at sign-in", SS_NOTIFY | SS_CENTERIMAGE, 384, 409, 124, 24, ID_STARTUP_LBL);
    for c in [u.chk_enabled, u.chk_startup, u.list] {
        dark(c, "DarkMode_Explorer");
    }
    windows_sys::Win32::UI::Shell::SetWindowSubclass(u.list, Some(list_proc), 1, 0);
    set_check(u.chk_enabled, u.cfg.enabled);
    set_check(u.chk_startup, get_startup());
    update_status();
    refresh_list();
}

unsafe fn paint_main(h: HWND) {
    let u = ui();
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(h, &mut ps);
    // logo centered on the two-line title block (title 16-44, status 46-66 -> center 41)
    DrawIconEx(hdc, s(20), s(41) - s(20), u.logo, s(40), s(40), 0, null_mut(), DI_NORMAL);
    SetBkMode(hdc, TRANSPARENT as _);
    SetTextColor(hdc, TEXT);
    let old = SelectObject(hdc, u.font_title);
    let mut r = RECT { left: s(72), top: s(14), right: s(360), bottom: s(44) };
    let mut t = w("rebindr");
    DrawTextW(hdc, t.as_mut_ptr(), -1, &mut r, DT_SINGLELINE | DT_VCENTER | DT_NOPREFIX);
    SelectObject(hdc, old);
    EndPaint(h, &ps);
}

fn mid_f(r: &RECT) -> f32 {
    (r.top + r.bottom) as f32 / 2.0
}

/// Index of the card whose "x" is under (x, y) in list coordinates, if any.
unsafe fn x_hit(list: HWND, x: i32, y: i32) -> Option<usize> {
    let r = SendMessageW(list, LB_ITEMFROMPOINT, 0, ((y as u32) << 16 | (x as u32 & 0xFFFF)) as isize) as usize;
    if hiword(r) != 0 {
        return None;
    }
    let idx = loword(r) as usize;
    let mut rc: RECT = std::mem::zeroed();
    if SendMessageW(list, LB_GETITEMRECT, idx, &mut rc as *mut RECT as isize) < 0 {
        return None;
    }
    let card_right = rc.right - s(2);
    (x >= card_right - s(46) && y >= rc.top && y < rc.bottom).then_some(idx)
}

/// Listbox subclass: clicking a card's "x" removes it; hovering highlights the "x".
unsafe extern "system" fn list_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM, _id: usize, _data: usize) -> LRESULT {
    use windows_sys::Win32::UI::Input::KeyboardAndMouse::{TrackMouseEvent, TME_LEAVE, TRACKMOUSEEVENT};
    use windows_sys::Win32::UI::Shell::DefSubclassProc;
    let u = ui();
    let (x, y) = ((lp & 0xFFFF) as i16 as i32, ((lp >> 16) & 0xFFFF) as i16 as i32);
    match msg {
        WM_LBUTTONDOWN | WM_LBUTTONDBLCLK => {
            if let Some(idx) = x_hit(h, x, y) {
                u.hover_x = None;
                remove_at(idx);
                return 0;
            }
        }
        WM_MOUSEMOVE => {
            let hit = x_hit(h, x, y);
            if hit != u.hover_x {
                for i in [u.hover_x, hit].into_iter().flatten() {
                    let mut rc: RECT = std::mem::zeroed();
                    SendMessageW(h, LB_GETITEMRECT, i, &mut rc as *mut RECT as isize);
                    InvalidateRect(h, &rc, 0);
                }
                u.hover_x = hit;
                SetCursor(LoadCursorW(null_mut(), if hit.is_some() { IDC_HAND } else { IDC_ARROW }));
                let mut t = TRACKMOUSEEVENT { cbSize: std::mem::size_of::<TRACKMOUSEEVENT>() as u32, dwFlags: TME_LEAVE, hwndTrack: h, dwHoverTime: 0 };
                TrackMouseEvent(&mut t);
            }
        }
        WM_SETCURSOR if u.hover_x.is_some() => {
            SetCursor(LoadCursorW(null_mut(), IDC_HAND));
            return 1;
        }
        0x02A3 /* WM_MOUSELEAVE */ => {
            if let Some(i) = u.hover_x.take() {
                let mut rc: RECT = std::mem::zeroed();
                SendMessageW(h, LB_GETITEMRECT, i, &mut rc as *mut RECT as isize);
                InvalidateRect(h, &rc, 0);
            }
        }
        _ => {}
    }
    DefSubclassProc(h, msg, wp, lp)
}

unsafe fn draw_list_item(d: &DRAWITEMSTRUCT) {
    let u = ui();
    if d.itemID == u32::MAX {
        return;
    }
    let Some(m) = u.cfg.mappings.get(d.itemData) else { return };
    let hdc = d.hDC;
    let rc = d.rcItem;
    FillRect(hdc, &rc, u.br_bg);
    let selected = d.itemState & ODS_SELECTED != 0;
    let card = RECT { left: rc.left, top: rc.top + s(3), right: rc.right - s(2), bottom: rc.bottom - s(3) };
    round_rect(hdc, &card, if selected { CARD_SEL } else { CARD }, s(10));
    if selected {
        let bar = RECT { left: card.left + s(4), top: card.top + s(14), right: card.left + s(7), bottom: card.bottom - s(14) };
        aa::round_rect(hdc, &bar, ACCENT, 1.5 * u.scale, None);
    }
    SetBkMode(hdc, TRANSPARENT as _);
    let pad = s(16);
    let mid = (card.top + card.bottom) / 2;
    // red "x" on the right: click to remove (soft circle behind it on hover)
    let (xc, yc) = ((card.right - s(26)) as f32, mid_f(&card));
    if u.hover_x == Some(d.itemID as usize) {
        let r = s(15);
        let c = RECT { left: xc as i32 - r, top: yc as i32 - r, right: xc as i32 + r, bottom: yc as i32 + r };
        aa::round_rect(hdc, &c, rgb(78, 34, 50), r as f32, None);
    }
    aa::cross(hdc, xc, yc, 10.0 * u.scale, ACCENT, 2.0 * u.scale);
    let text_right = card.right - s(52);
    let mut top = RECT { left: card.left + pad, top: card.top + s(6), right: text_right, bottom: mid };
    let mut bottom = RECT { left: card.left + pad, top: mid, right: text_right, bottom: card.bottom - s(6) };
    // Named: name on top, "buttons · action" below. Unnamed: buttons on top, action below.
    let named = !m.name.trim().is_empty() && m.name != m.action_text();
    let title = if named { m.name.clone() } else { buttons::label_of(m.mask()) };
    let mut detail = if named {
        format!("{}   \u{00b7}   {}", buttons::label_of(m.mask()), m.action_text())
    } else {
        m.action_text()
    };
    if m.is_long() {
        detail += "   \u{00b7}   long press";
    }
    if m.is_hold() {
        detail += "   \u{00b7}   keys held";
    }
    let old = SelectObject(hdc, u.font_bold);
    SetTextColor(hdc, TEXT);
    let mut t1 = w(&title);
    DrawTextW(hdc, t1.as_mut_ptr(), -1, &mut top, DT_SINGLELINE | DT_BOTTOM | DT_END_ELLIPSIS | DT_NOPREFIX);
    SelectObject(hdc, u.font_small);
    SetTextColor(hdc, MUTED);
    let mut t2 = w(&detail);
    DrawTextW(hdc, t2.as_mut_ptr(), -1, &mut bottom, DT_SINGLELINE | DT_TOP | DT_END_ELLIPSIS | DT_NOPREFIX);
    SelectObject(hdc, old);
}

// ------------------------------------------------------------------ anti-aliased drawing (GDI+)

mod aa {
    use windows_sys::Win32::Foundation::RECT;
    use windows_sys::Win32::Graphics::Gdi::HDC;
    use windows_sys::Win32::Graphics::GdiPlus::*;

    pub const RADIUS: f32 = 8.0; // one corner radius everywhere (logical px)

    pub unsafe fn startup() {
        let mut token = 0usize;
        let input = GdiplusStartupInput {
            GdiplusVersion: 1,
            DebugEventCallback: 0 as _,
            SuppressBackgroundThread: 0,
            SuppressExternalCodecs: 0,
        };
        GdiplusStartup(&mut token, &input, std::ptr::null_mut());
    }

    fn argb(c: u32) -> u32 {
        0xFF00_0000 | ((c & 0xFF) << 16) | (c & 0xFF00) | ((c >> 16) & 0xFF)
    }

    unsafe fn graphics(hdc: HDC) -> *mut GpGraphics {
        let mut g = std::ptr::null_mut();
        GdipCreateFromHDC(hdc, &mut g);
        GdipSetSmoothingMode(g, SmoothingModeAntiAlias);
        GdipSetPixelOffsetMode(g, PixelOffsetModeHalf);
        g
    }

    unsafe fn path(x: f32, y: f32, w: f32, h: f32, r: f32) -> *mut GpPath {
        let mut p = std::ptr::null_mut();
        GdipCreatePath(FillModeAlternate, &mut p);
        let d = (r * 2.0).min(w).min(h);
        GdipAddPathArc(p, x, y, d, d, 180.0, 90.0);
        GdipAddPathArc(p, x + w - d, y, d, d, 270.0, 90.0);
        GdipAddPathArc(p, x + w - d, y + h - d, d, d, 0.0, 90.0);
        GdipAddPathArc(p, x, y + h - d, d, d, 90.0, 90.0);
        GdipClosePathFigure(p);
        p
    }

    /// Filled rounded rectangle, optionally with an outline.
    pub unsafe fn round_rect(hdc: HDC, r: &RECT, fill: u32, radius: f32, outline: Option<(u32, f32)>) {
        let g = graphics(hdc);
        let (x, y, w, h) = (r.left as f32, r.top as f32, (r.right - r.left) as f32, (r.bottom - r.top) as f32);
        let p = path(x, y, w, h, radius);
        let mut b = std::ptr::null_mut();
        GdipCreateSolidFill(argb(fill), &mut b);
        GdipFillPath(g, b as *mut GpBrush, p);
        GdipDeleteBrush(b as *mut GpBrush);
        GdipDeletePath(p);
        if let Some((color, width)) = outline {
            let half = width / 2.0;
            let p = path(x + half, y + half, w - width, h - width, (radius - half).max(1.0));
            let mut pen = std::ptr::null_mut();
            GdipCreatePen1(argb(color), width, UnitPixel, &mut pen);
            GdipDrawPath(g, pen, p);
            GdipDeletePen(pen);
            GdipDeletePath(p);
        }
        GdipDeleteGraphics(g);
    }

    /// An "x" centered at (cx, cy).
    pub unsafe fn cross(hdc: HDC, cx: f32, cy: f32, size: f32, color: u32, width: f32) {
        let g = graphics(hdc);
        let mut pen = std::ptr::null_mut();
        GdipCreatePen1(argb(color), width, UnitPixel, &mut pen);
        GdipSetPenStartCap(pen, LineCapRound);
        GdipSetPenEndCap(pen, LineCapRound);
        let h = size / 2.0;
        GdipDrawLine(g, pen, cx - h, cy - h, cx + h, cy + h);
        GdipDrawLine(g, pen, cx - h, cy + h, cx + h, cy - h);
        GdipDeletePen(pen);
        GdipDeleteGraphics(g);
    }

    /// A small "v" chevron centered at (cx, cy).
    pub unsafe fn chevron(hdc: HDC, cx: f32, cy: f32, size: f32, color: u32) {
        let g = graphics(hdc);
        let mut pen = std::ptr::null_mut();
        GdipCreatePen1(argb(color), (size / 4.0).max(1.4), UnitPixel, &mut pen);
        GdipSetPenStartCap(pen, LineCapRound);
        GdipSetPenEndCap(pen, LineCapRound);
        let (hw, hh) = (size / 2.0, size / 4.0);
        GdipDrawLine(g, pen, cx - hw, cy - hh, cx, cy + hh);
        GdipDrawLine(g, pen, cx, cy + hh, cx + hw, cy - hh);
        GdipDeletePen(pen);
        GdipDeleteGraphics(g);
    }
}

unsafe fn round_rect(hdc: HDC, r: &RECT, color: u32, _radius: i32) {
    aa::round_rect(hdc, r, color, aa::RADIUS * ui().scale, None);
}

unsafe fn draw_button(d: &DRAWITEMSTRUCT) {
    let u = ui();
    let id = d.CtlID as u16;
    let hdc = d.hDC;
    FillRect(hdc, &d.rcItem, u.br_bg);
    let focus = d.itemState & ODS_FOCUS != 0 && d.itemState & ODS_NOFOCUSRECT == 0;
    let radius = aa::RADIUS * u.scale;
    if id == ID_E_TRIGGER || id == ID_E_ACTION {
        draw_dropdown(d, focus, radius);
        return;
    }
    let primary = id == ID_ADD || id == IDOK_ || (id == ID_E_REC && u.ed.rec) || (id == ID_E_FIELDBTN && u.ed.krec);
    let pressed = d.itemState & ODS_SELECTED != 0;
    let disabled = d.itemState & ODS_DISABLED != 0;
    let fill = match (primary, pressed) {
        (true, false) => ACCENT,
        (true, true) => ACCENT_DOWN,
        (false, false) => SECOND,
        (false, true) => SECOND_DOWN,
    };
    let outline = focus.then_some((rgb(220, 220, 228), u.scale.max(1.0)));
    aa::round_rect(hdc, &d.rcItem, if disabled { CARD } else { fill }, radius, outline);
    SetBkMode(hdc, TRANSPARENT as _);
    SetTextColor(hdc, if disabled { MUTED } else if primary { rgb(255, 255, 255) } else { TEXT });
    let old = SelectObject(hdc, u.font_bold);
    let mut r = d.rcItem;
    let mut t = w(&get_text(d.hwndItem));
    DrawTextW(hdc, t.as_mut_ptr(), -1, &mut r, DT_SINGLELINE | DT_CENTER | DT_VCENTER | DT_NOPREFIX);
    SelectObject(hdc, old);
}

/// A rounded "select" box: current choice on the left, chevron on the right.
unsafe fn draw_dropdown(d: &DRAWITEMSTRUCT, focus: bool, radius: f32) {
    let u = ui();
    let hdc = d.hDC;
    let open = d.itemState & ODS_SELECTED != 0;
    let outline = if focus || open { Some((ACCENT, u.scale.max(1.0))) } else { Some((rgb(62, 62, 68), u.scale.max(1.0))) };
    aa::round_rect(hdc, &d.rcItem, INPUT, radius, outline);
    SetBkMode(hdc, TRANSPARENT as _);
    SetTextColor(hdc, TEXT);
    let old = SelectObject(hdc, u.font);
    let mut r = RECT { left: d.rcItem.left + s(12), top: d.rcItem.top, right: d.rcItem.right - s(36), bottom: d.rcItem.bottom };
    let mut t = w(&get_text(d.hwndItem));
    DrawTextW(hdc, t.as_mut_ptr(), -1, &mut r, DT_SINGLELINE | DT_VCENTER | DT_END_ELLIPSIS | DT_NOPREFIX);
    SelectObject(hdc, old);
    let cy = (d.rcItem.top + d.rcItem.bottom) as f32 / 2.0;
    aa::chevron(hdc, (d.rcItem.right - s(18)) as f32, cy, 9.0 * u.scale, MUTED);
}

/// Opens the dark choice menu under a dropdown; returns the chosen index.
unsafe fn pick(owner: HWND, btn: HWND, options: &[&str], current: usize) -> Option<usize> {
    let m = CreatePopupMenu();
    for (i, o) in options.iter().enumerate() {
        AppendMenuW(m, MF_STRING | if i == current { MF_CHECKED } else { 0 }, i + 1, w(o).as_ptr());
    }
    let mut r: RECT = std::mem::zeroed();
    GetWindowRect(btn, &mut r);
    let cmd = TrackPopupMenu(m, TPM_RETURNCMD | TPM_LEFTALIGN | TPM_TOPALIGN, r.left, r.bottom + s(4), 0, owner, null());
    DestroyMenu(m);
    (cmd > 0).then(|| cmd as usize - 1)
}

unsafe extern "system" fn main_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let u = ui();
    match msg {
        WM_INPUT => {
            hook::on_raw_input(lp);
            return DefWindowProcW(h, msg, wp, lp);
        }
        WM_APP_STATUS => update_status(),
        WM_TIMER if wp == T_STATUS => {
            KillTimer(h, T_STATUS);
            u.status_flash = false;
            update_status();
        }
        WM_APP_SHOW => show_main(),
        WM_APP_TRAY => match (lp as usize & 0xFFFF) as u32 {
            WM_LBUTTONUP | WM_LBUTTONDBLCLK => show_main(),
            WM_RBUTTONUP | WM_CONTEXTMENU => tray_menu(),
            _ => {}
        },
        WM_PAINT => paint_main(h),
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let hdc = wp as HDC;
            SetBkColor(hdc, BG);
            let c = lp as HWND;
            SetTextColor(hdc, if c == u.status && u.status_flash { ACCENT } else if c == u.status || c == u.empty { MUTED } else { TEXT });
            return u.br_bg as LRESULT;
        }
        WM_CTLCOLORLISTBOX => {
            SetBkColor(wp as HDC, BG);
            SetTextColor(wp as HDC, TEXT);
            return u.br_bg as LRESULT;
        }
        WM_MEASUREITEM => {
            let mi = &mut *(lp as *mut MEASUREITEMSTRUCT);
            if mi.CtlType == ODT_LISTBOX {
                mi.itemHeight = s(64) as u32;
            }
            return 1;
        }
        WM_DRAWITEM => {
            let d = &*(lp as *const DRAWITEMSTRUCT);
            if d.CtlType == ODT_LISTBOX {
                draw_list_item(d);
            } else {
                draw_button(d);
            }
            return 1;
        }
        WM_VKEYTOITEM => {
            if loword(wp) == VK_DELETE {
                remove_selected();
                return -2;
            }
            return -1;
        }
        WM_COMMAND => {
            let (id, code) = (loword(wp), hiword(wp) as u32);
            match (id, code) {
                (ID_ADD, _) => open_editor(),
                (ID_IMPORT, _) => import_shortcuts(),
                (ID_EXPORT, _) => export_shortcuts(),
                (ID_REMOVE, _) => remove_selected(),
                (ID_ENABLED, BN_CLICKED) => set_enabled(checked(u.chk_enabled)),
                (ID_ENABLED_LBL, STN_CLICKED) => set_enabled(!u.cfg.enabled),
                (ID_STARTUP, BN_CLICKED) => set_startup(checked(u.chk_startup)),
                (ID_STARTUP_LBL, STN_CLICKED) => {
                    let on = !checked(u.chk_startup);
                    set_check(u.chk_startup, on);
                    set_startup(on);
                }
                (ID_LIST, LBN_SELCHANGE) => {
                    EnableWindow(u.btn_remove, (SendMessageW(u.list, LB_GETCURSEL, 0, 0) >= 0) as i32);
                }
                _ => {}
            }
        }
        WM_CLOSE => {
            ShowWindow(h, SW_HIDE); // keep running in the tray
            return 0;
        }
        WM_DESTROY => PostQuitMessage(0),
        _ if msg == u.taskbar_created && msg != 0 => tray(NIM_ADD),
        _ => return DefWindowProcW(h, msg, wp, lp),
    }
    0
}

// ------------------------------------------------------------------ Add dialog

unsafe fn open_editor() {
    let u = ui();
    if !u.editor.is_null() {
        SetForegroundWindow(u.editor);
        return;
    }
    let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU;
    let mut r = RECT { left: 0, top: 0, right: s(500), bottom: s(418) };
    AdjustWindowRectEx(&mut r, style, 0, WS_EX_DLGMODALFRAME);
    let mut mr: RECT = std::mem::zeroed();
    GetWindowRect(u.main, &mut mr);
    let (ww, wh) = (r.right - r.left, r.bottom - r.top);
    let x = mr.left + ((mr.right - mr.left) - ww) / 2;
    let y = mr.top + ((mr.bottom - mr.top) - wh) / 3;
    u.ed = Ed::empty();
    let h = CreateWindowExW(
        WS_EX_DLGMODALFRAME,
        w(EDITOR_CLASS).as_ptr(),
        w("Add shortcut").as_ptr(),
        style,
        x,
        y,
        ww,
        wh,
        u.main,
        null_mut(),
        u.hinst,
        null(),
    );
    u.editor = h;
    dark_titlebar(h);
    let e = &mut u.ed;
    let (nx, ny, nw, nh) = NAME_BOX;
    ctl(h, "STATIC", "Name", SS_CENTERIMAGE, 20, ny, 90, nh, 0);
    e.name = ctl(h, "EDIT", "", (ES_AUTOHSCROLL as u32) | WS_TABSTOP, nx + 12, ny + 8, nw - 24, nh - 16, ID_E_NAME);
    SendMessageW(e.name, EM_SETCUEBANNER, 1, w("Optional \u{2014} e.g. Clip that").as_ptr() as isize);
    SendMessageW(e.name, EM_LIMITTEXT, 80, 0);
    let (bx, by, bw, bh) = BUTTON_BOX;
    ctl(h, "STATIC", "Button", SS_CENTERIMAGE, 20, by, 90, bh, 0);
    // typeable too ("View + Share"); Record fills it in
    e.val = ctl(h, "EDIT", "", (ES_AUTOHSCROLL as u32) | WS_TABSTOP, bx + 12, by + 8, bw - 24, bh - 16, ID_E_VAL);
    SendMessageW(e.val, EM_SETCUEBANNER, 1, w("View + Share").as_ptr() as isize);
    e.rec_btn = ctl(h, "BUTTON", "Record", (BS_OWNERDRAW as u32) | WS_TABSTOP, 340, by, 140, bh, ID_E_REC);
    ctl(h, "STATIC", "Trigger", SS_CENTERIMAGE, 20, 112, 90, 36, 0);
    e.trigger = ctl(h, "BUTTON", TRIGGERS[0], (BS_OWNERDRAW as u32) | WS_TABSTOP, 116, 112, 364, 36, ID_E_TRIGGER);
    ctl(h, "STATIC", "Action", SS_CENTERIMAGE, 20, 158, 90, 36, 0);
    e.action = ctl(h, "BUTTON", ACTIONS[0], (BS_OWNERDRAW as u32) | WS_TABSTOP, 116, 158, 364, 36, ID_E_ACTION);
    let (fx, fy, fw, fh) = FIELD_BOX;
    e.field_lbl = ctl(h, "STATIC", "Keys", SS_CENTERIMAGE, 20, fy, 90, fh, ID_E_FIELDLBL);
    // borderless input inside a rounded box painted by the dialog (see WM_PAINT)
    e.field = ctl(h, "EDIT", "", (ES_AUTOHSCROLL as u32) | WS_TABSTOP, fx + 12, fy + 8, fw - 24, fh - 16, ID_E_FIELD);
    e.field_btn = ctl(h, "BUTTON", "Record keys", (BS_OWNERDRAW as u32) | WS_TABSTOP, 340, fy, 140, fh, ID_E_FIELDBTN);
    e.hold = ctl(h, "BUTTON", "", (BS_AUTOCHECKBOX as u32) | WS_TABSTOP, 116, 254, 20, 22, ID_E_HOLD);
    e.hold_lbl = ctl(h, "STATIC", "Keep keys held while the button is held", SS_NOTIFY | SS_CENTERIMAGE, 140, 253, 340, 24, ID_E_HOLDLBL);
    e.note = ctl(h, "STATIC", "", 0, 20, 284, 460, 38, 0);
    SendMessageW(e.note, WM_SETFONT, ui().font_small as usize, 1);
    e.hint = ctl(h, "STATIC", "", 0, 20, 324, 460, 22, ID_E_HINT);
    ctl(h, "BUTTON", "Cancel", (BS_OWNERDRAW as u32) | WS_TABSTOP, 264, 364, 100, 36, IDCANCEL_);
    ctl(h, "BUTTON", "Add", (BS_OWNERDRAW as u32) | WS_TABSTOP, 372, 364, 108, 36, IDOK_);
    dark(e.hold, "DarkMode_Explorer");
    update_action_ui();
    hint("Click Record, then press the controller button or combo.", MUTED);
    EnableWindow(u.main, 0);
    PAUSED.store(true, Relaxed); // don't fire shortcuts while setting one up
    hook::set_editor_open(true);
    ShowWindow(h, SW_SHOW);
    SetFocus(u.ed.name);
}

impl Ed {
    fn empty() -> Ed {
        Ed {
            trigger_idx: 0,
            action_idx: 0,
            field_focus: false,
            val_focus: false,
            name_focus: false,
            name: null_mut(),
            note: null_mut(),
            mask: 0,
            rec: false,
            acc: 0,
            krec: false,
            dup_ok: false,
            hint_color: MUTED,
            val: null_mut(),
            rec_btn: null_mut(),
            trigger: null_mut(),
            action: null_mut(),
            field_lbl: null_mut(),
            field: null_mut(),
            field_btn: null_mut(),
            hold: null_mut(),
            hold_lbl: null_mut(),
            hint: null_mut(),
        }
    }
}

unsafe fn hint(t: &str, color: u32) {
    let e = &mut ui().ed;
    e.hint_color = color;
    set_text(e.hint, t);
    InvalidateRect(e.hint, null(), 1);
}

fn is_keys_action() -> bool {
    ui().ed.action_idx == 0
}

/// Rounded box behind the borderless Keys / Open input; pink outline while it has focus.
unsafe fn paint_editor(h: HWND) {
    let u = ui();
    let mut ps: PAINTSTRUCT = std::mem::zeroed();
    let hdc = BeginPaint(h, &mut ps);
    for ((x, y, bw, bh), focus) in [(FIELD_BOX, u.ed.field_focus), (BUTTON_BOX, u.ed.val_focus), (NAME_BOX, u.ed.name_focus)] {
        let r = RECT { left: s(x), top: s(y), right: s(x + bw), bottom: s(y + bh) };
        let border = if focus { ACCENT } else { rgb(62, 62, 68) };
        aa::round_rect(hdc, &r, INPUT, aa::RADIUS * u.scale, Some((border, u.scale.max(1.0))));
    }
    EndPaint(h, &ps);
}

unsafe fn invalidate_box(b: (i32, i32, i32, i32)) {
    let (x, y, bw, bh) = b;
    let r = RECT { left: s(x), top: s(y), right: s(x + bw), bottom: s(y + bh) };
    InvalidateRect(ui().editor, &r, 1);
}

/// Warns when the chosen buttons also do something built into Windows.
unsafe fn update_note() {
    let m = ui().ed.mask;
    let mut lines = Vec::new();
    if m & buttons::SHARE_BIT != 0 {
        lines.push("\u{24d8}  Share\u{2019}s own Game Bar screenshot is blocked while this shortcut exists.");
    }
    if m & 2 != 0 {
        lines.push("\u{24d8}  Home also opens Game Bar \u{2014} you can turn that off in Game Bar settings.");
    }
    set_text(ui().ed.note, &lines.join("\n"));
}

/// The Button box changed (typed or recorded): parse it.
unsafe fn on_buttons_typed() {
    let e = &mut ui().ed;
    let text = get_text(e.val);
    if text.trim().is_empty() {
        e.mask = 0;
    } else {
        match buttons::parse_buttons(&text) {
            Ok(m) => {
                e.mask = m;
                hint(&format!("Button: {}", buttons::label_of(m)), OK);
            }
            Err(err) => {
                e.mask = 0;
                hint(&err, ERR);
            }
        }
    }
    ui().ed.dup_ok = false;
    update_note();
}

unsafe fn update_action_ui() {
    let e = &mut ui().ed;
    let keys = is_keys_action();
    if e.krec && !keys {
        stop_krec();
    }
    set_text(e.field_lbl, if keys { "Keys" } else { "Open" });
    set_text(e.field_btn, if keys { "Record keys" } else { "Browse\u{2026}" });
    set_text(e.field, "");
    let cue = w(if keys { "Ctrl + Shift + E" } else { "notepad, C:\\Games\\game.exe, https://\u{2026}" });
    SendMessageW(e.field, EM_SETCUEBANNER, 1, cue.as_ptr() as isize);
    let show = if keys { SW_SHOW } else { SW_HIDE };
    ShowWindow(e.hold, show);
    ShowWindow(e.hold_lbl, show);
    InvalidateRect(e.field_btn, null(), 1);
}

unsafe fn toggle_rec() {
    let u = ui();
    let e = &mut u.ed;
    e.rec = !e.rec;
    if e.rec {
        e.acc = 0;
        SetTimer(u.editor, T_REC, 30, None);
        set_text(e.rec_btn, "Listening\u{2026}");
        hint("Press the button(s) on your controller, then let go.", MUTED);
    } else {
        KillTimer(u.editor, T_REC);
        set_text(e.rec_btn, "Record");
    }
    InvalidateRect(e.rec_btn, null(), 1);
}

unsafe fn stop_krec() {
    let u = ui();
    if !u.ed.krec {
        return;
    }
    u.ed.krec = false;
    KillTimer(u.editor, T_KREC);
    hook::stop_key_record();
    set_text(u.ed.field_btn, "Record keys");
    InvalidateRect(u.ed.field_btn, null(), 1);
}

unsafe fn field_button() {
    let u = ui();
    if !is_keys_action() {
        browse();
        return;
    }
    if u.ed.krec {
        stop_krec();
        return;
    }
    u.ed.krec = true;
    hook::start_key_record(u.editor);
    SetTimer(u.editor, T_KREC, 250, None);
    set_text(u.ed.field_btn, "Listening\u{2026}");
    InvalidateRect(u.ed.field_btn, null(), 1);
    hint("Press the whole shortcut, then let go. It won\u{2019}t run while recording.", MUTED);
}

unsafe fn on_keyrec() {
    let (so_far, result, timed_out) = hook::key_record_state();
    if let Some(keys) = result {
        set_text(ui().ed.field, &keys::field_text(&keys));
        stop_krec();
        validate_field();
    } else if timed_out {
        stop_krec();
        hint("Recording timed out \u{2014} try again or type the shortcut.", MUTED);
    } else if !so_far.is_empty() {
        hint(&format!("{} \u{2026} let go to finish", keys::format_keys(&so_far)), TEXT);
    }
}

unsafe fn browse() {
    let u = ui();
    let mut file = vec![0u16; 1024];
    let filter: Vec<u16> = "Apps and files\0*.*\0\0".encode_utf16().collect();
    let mut ofn: OPENFILENAMEW = std::mem::zeroed();
    ofn.lStructSize = std::mem::size_of::<OPENFILENAMEW>() as u32;
    ofn.hwndOwner = u.editor;
    ofn.lpstrFilter = filter.as_ptr();
    ofn.lpstrFile = file.as_mut_ptr();
    ofn.nMaxFile = file.len() as u32;
    ofn.Flags = OFN_FILEMUSTEXIST | OFN_PATHMUSTEXIST | OFN_NOCHANGEDIR;
    if GetOpenFileNameW(&mut ofn) != 0 {
        set_text(u.ed.field, &from_w(&file));
    }
}

unsafe fn validate_field() {
    let text = get_text(ui().ed.field);
    if !is_keys_action() {
        return;
    }
    if text.trim().is_empty() {
        hint("Type a shortcut like Ctrl + C or Win + Shift + S, or click Record keys.", MUTED);
        return;
    }
    match keys::parse_keys(&text) {
        Ok(k) => hint(&format!("Sends {}", keys::format_keys(&k)), OK),
        Err(e) => hint(&e, ERR),
    }
}

unsafe fn save_editor() {
    let u = ui();
    let e = &mut u.ed;
    // Any order is fine: just list what's still missing, and jump to the first gap.
    let text = get_text(e.field);
    let need_button = e.rec || e.mask == 0;
    let need_action = text.trim().is_empty();
    if need_button || need_action {
        let what = match (need_button, need_action, is_keys_action()) {
            (true, true, true) => "a controller button and a keyboard shortcut",
            (true, true, false) => "a controller button and something to open",
            (true, false, _) => "a controller button",
            (false, _, true) => "a keyboard shortcut",
            (false, _, false) => "something to open",
        };
        hint(&format!("Still needed: {what}."), MUTED);
        SetFocus(if need_button { e.val } else { e.field });
        return;
    }
    let long = e.trigger_idx == 1;
    let action = if is_keys_action() {
        match keys::parse_keys(&text) {
            Ok(k) => Action::Keys { keys: k },
            Err(err) => return hint(&err, ERR),
        }
    } else {
        Action::Open { target: text.trim().to_string() }
    };
    let press = if long { "long" } else { "short" };
    if !e.dup_ok && u.cfg.mappings.iter().any(|m| m.mask() == e.mask && m.press == press) {
        e.dup_ok = true;
        return hint("That button already has a shortcut for this trigger. Click Add again to keep both.", ERR);
    }
    let m = Mapping {
        id: String::new(),
        name: get_text(e.name).trim().to_string(),
        buttons: buttons::ids_of(e.mask),
        action,
        mode: if checked(e.hold) { "hold".into() } else { "tap".into() },
        press: press.into(),
        enabled: true,
    };
    match config::clean(m) {
        Ok(m) => {
            u.cfg.mappings.push(m);
            config::save(&u.cfg);
            close_editor();
            apply_mappings();
            refresh_list();
        }
        Err(err) => hint(&err, ERR),
    }
}

unsafe fn close_editor() {
    let u = ui();
    if u.editor.is_null() {
        return;
    }
    KillTimer(u.editor, T_REC);
    stop_krec();
    PAUSED.store(false, Relaxed);
    hook::set_editor_open(false);
    EnableWindow(u.main, 1);
    let h = u.editor;
    u.editor = null_mut();
    DestroyWindow(h);
    SetForegroundWindow(u.main);
}

unsafe extern "system" fn editor_proc(h: HWND, msg: u32, wp: WPARAM, lp: LPARAM) -> LRESULT {
    let u = ui();
    match msg {
        WM_TIMER if wp == T_REC => {
            let p = PRESSED.load(Relaxed);
            if p != 0 {
                u.ed.acc |= p;
            } else if u.ed.acc != 0 {
                u.ed.mask = u.ed.acc;
                u.ed.dup_ok = false;
                toggle_rec();
                set_text(u.ed.val, &buttons::label_of(u.ed.mask));
                hint(&format!("Recorded {}.", buttons::label_of(u.ed.mask)), OK);
            }
        }
        WM_TIMER if wp == T_KREC => on_keyrec(),
        WM_PAINT => paint_editor(h),
        hook::WM_APP_KEYREC => on_keyrec(),
        WM_CTLCOLORSTATIC | WM_CTLCOLORBTN => {
            let hdc = wp as HDC;
            SetBkColor(hdc, BG);
            let c = lp as HWND;
            SetTextColor(hdc, if c == u.ed.hint { u.ed.hint_color } else if c == u.ed.note { rgb(226, 206, 214) } else { TEXT });
            return u.br_bg as LRESULT;
        }
        WM_CTLCOLOREDIT => {
            SetBkColor(wp as HDC, INPUT);
            SetTextColor(wp as HDC, TEXT);
            return u.br_input as LRESULT;
        }
        WM_CTLCOLORLISTBOX => {
            SetBkColor(wp as HDC, CARD);
            SetTextColor(wp as HDC, TEXT);
            return u.br_card as LRESULT;
        }
        WM_DRAWITEM => {
            draw_button(&*(lp as *const DRAWITEMSTRUCT));
            return 1;
        }
        WM_COMMAND => {
            let (id, code) = (loword(wp), hiword(wp) as u32);
            match (id, code) {
                (ID_E_REC, _) => toggle_rec(),
                (ID_E_FIELDBTN, _) => field_button(),
                (ID_E_TRIGGER, BN_CLICKED) => {
                    if let Some(i) = pick(h, u.ed.trigger, &TRIGGERS, u.ed.trigger_idx) {
                        u.ed.trigger_idx = i;
                        u.ed.dup_ok = false;
                        set_text(u.ed.trigger, TRIGGERS[i]);
                        InvalidateRect(u.ed.trigger, null(), 1);
                    }
                }
                (ID_E_ACTION, BN_CLICKED) => {
                    if let Some(i) = pick(h, u.ed.action, &ACTIONS, u.ed.action_idx) {
                        if i != u.ed.action_idx {
                            u.ed.action_idx = i;
                            set_text(u.ed.action, ACTIONS[i]);
                            InvalidateRect(u.ed.action, null(), 1);
                            update_action_ui();
                            validate_field();
                        }
                    }
                }
                (ID_E_FIELD, EN_CHANGE) => {
                    if !u.ed.krec {
                        validate_field();
                    }
                }
                (ID_E_FIELD, EN_SETFOCUS) | (ID_E_FIELD, EN_KILLFOCUS) => {
                    u.ed.field_focus = code == EN_SETFOCUS;
                    invalidate_box(FIELD_BOX);
                    // tidy typed keys into "Ctrl + Shift + E" when leaving the box
                    if code == EN_KILLFOCUS && is_keys_action() && !u.ed.krec {
                        if let Ok(k) = keys::parse_keys(&get_text(u.ed.field)) {
                            let tidy = keys::field_text(&k);
                            if tidy != get_text(u.ed.field) {
                                set_text(u.ed.field, &tidy);
                            }
                        }
                    }
                }
                (ID_E_NAME, EN_SETFOCUS) | (ID_E_NAME, EN_KILLFOCUS) => {
                    u.ed.name_focus = code == EN_SETFOCUS;
                    invalidate_box(NAME_BOX);
                }
                (ID_E_VAL, EN_CHANGE) => on_buttons_typed(),
                (ID_E_VAL, EN_SETFOCUS) | (ID_E_VAL, EN_KILLFOCUS) => {
                    u.ed.val_focus = code == EN_SETFOCUS;
                    invalidate_box(BUTTON_BOX);
                    // tidy typed buttons into "View + Share" when leaving the box
                    if code == EN_KILLFOCUS && u.ed.mask != 0 {
                        let tidy = buttons::label_of(u.ed.mask);
                        if tidy != get_text(u.ed.val) {
                            set_text(u.ed.val, &tidy);
                        }
                    }
                }
                (ID_E_HOLDLBL, STN_CLICKED) => set_check(u.ed.hold, !checked(u.ed.hold)),
                (IDOK_, _) => save_editor(),
                (IDCANCEL_, _) => close_editor(),
                _ => {}
            }
        }
        WM_CLOSE => close_editor(),
        _ => return DefWindowProcW(h, msg, wp, lp),
    }
    0
}

// ------------------------------------------------------------------ entry point

pub fn run() {
    unsafe {
        // One copy at a time: a second launch just brings the window up.
        CreateMutexW(null(), 0, w("Local\\rebindr-single-instance").as_ptr());
        if GetLastError() == ERROR_ALREADY_EXISTS {
            let other = FindWindowW(w(MAIN_CLASS).as_ptr(), null());
            if !other.is_null() {
                PostMessageW(other, WM_APP_SHOW, 0, 0);
            }
            return;
        }
        SetProcessDpiAwarenessContext(DPI_AWARENESS_CONTEXT_SYSTEM_AWARE);
        let hinst = GetModuleHandleW(null());
        let cfg = config::load();
        let minimized = std::env::args().any(|a| a == "--minimized") || cfg.start_minimized;
        UI = Some(Ui {
            hinst,
            main: null_mut(),
            editor: null_mut(),
            list: null_mut(),
            status: null_mut(),
            empty: null_mut(),
            chk_enabled: null_mut(),
            chk_startup: null_mut(),
            btn_remove: null_mut(),
            cfg,
            scale: GetDpiForSystem() as f32 / 96.0,
            font: null_mut(),
            font_bold: null_mut(),
            font_title: null_mut(),
            font_small: null_mut(),
            br_bg: CreateSolidBrush(BG),
            br_card: CreateSolidBrush(CARD),
            br_input: CreateSolidBrush(INPUT),
            icon_big: null_mut(),
            icon_small: null_mut(),
            logo: null_mut(),
            taskbar_created: RegisterWindowMessageW(w("TaskbarCreated").as_ptr()),
            hover_x: None,
            status_flash: false,
            ed: Ed::empty(),
        });
        let u = ui();
        enable_visual_styles();
        enable_dark_app();
        u.font = make_font(10.0, 400);
        u.font_bold = make_font(10.5, 600);
        u.font_small = make_font(9.0, 400);
        u.font_title = make_font(17.0, 600);
        u.icon_big = icon::app_icon(GetSystemMetrics(SM_CXICON));
        u.icon_small = icon::app_icon(GetSystemMetrics(SM_CXSMICON));
        u.logo = icon::logo_mark(s(40));
        aa::startup();

        let cursor = LoadCursorW(null_mut(), IDC_ARROW);
        for (class, proc_) in [
            (MAIN_CLASS, main_proc as unsafe extern "system" fn(HWND, u32, WPARAM, LPARAM) -> LRESULT),
            (EDITOR_CLASS, editor_proc),
        ] {
            let name = w(class);
            let wc = WNDCLASSEXW {
                cbSize: std::mem::size_of::<WNDCLASSEXW>() as u32,
                style: CS_HREDRAW | CS_VREDRAW,
                lpfnWndProc: Some(proc_),
                cbClsExtra: 0,
                cbWndExtra: 0,
                hInstance: hinst,
                hIcon: u.icon_big,
                hCursor: cursor,
                hbrBackground: u.br_bg,
                lpszMenuName: null(),
                lpszClassName: name.as_ptr(),
                hIconSm: u.icon_small,
            };
            RegisterClassExW(&wc);
        }

        let style = WS_OVERLAPPED | WS_CAPTION | WS_SYSMENU | WS_MINIMIZEBOX;
        let mut r = RECT { left: 0, top: 0, right: s(520), bottom: s(456) };
        AdjustWindowRectEx(&mut r, style, 0, 0);
        let (ww, wh) = (r.right - r.left, r.bottom - r.top);
        let x = (GetSystemMetrics(SM_CXSCREEN) - ww) / 2;
        let y = (GetSystemMetrics(SM_CYSCREEN) - wh) / 2;
        let main = CreateWindowExW(0, w(MAIN_CLASS).as_ptr(), w("rebindr").as_ptr(), style, x, y, ww, wh, null_mut(), null_mut(), hinst, null());
        u.main = main;
        MAIN_HWND.store(main as usize, Relaxed);
        dark_titlebar(main);
        create_main_controls(main);
        hook::init(main);
        apply_mappings();
        tray(NIM_ADD);
        input::spawn_poller();
        if !minimized {
            ShowWindow(main, SW_SHOW);
            SetForegroundWindow(main);
        }

        let mut msg: MSG = std::mem::zeroed();
        while GetMessageW(&mut msg, null_mut(), 0, 0) > 0 {
            // While recording a keyboard shortcut, keys are for the recorder only: drop them
            // here so the dialog doesn't type them, beep at them, or open the Alt menu.
            if ui().ed.krec && (WM_KEYFIRST..=WM_KEYLAST).contains(&msg.message) {
                continue;
            }
            let dlg = if !ui().editor.is_null() { ui().editor } else { ui().main };
            if IsDialogMessageW(dlg, &msg) == 0 {
                TranslateMessage(&msg);
                DispatchMessageW(&msg);
            }
        }
    }
}

//! rebindr — maps controller buttons and combos (including the GameSir Nova 2 Lite's
//! Share button) to Windows keyboard shortcuts or to opening an app / file / URL.
//!
//! One small native process: a tray icon, a minimal dark settings window, a controller
//! poller thread, and (only when needed) a low-level keyboard hook.
#![windows_subsystem = "windows"]

mod buttons;
mod config;
mod engine;
mod hook;
mod icon;
mod input;
mod keys;
mod ui;
mod util;

fn main() {
    ui::run();
}

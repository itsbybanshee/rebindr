use std::io::Write;
use std::path::PathBuf;

use windows_sys::Win32::System::SystemInformation::GetTickCount64;

/// NUL-terminated UTF-16 for Win32 calls.
pub fn w(s: &str) -> Vec<u16> {
    s.encode_utf16().chain(std::iter::once(0)).collect()
}

/// UTF-16 buffer (possibly NUL-terminated) to String.
pub fn from_w(buf: &[u16]) -> String {
    let end = buf.iter().position(|&c| c == 0).unwrap_or(buf.len());
    String::from_utf16_lossy(&buf[..end])
}

pub fn now_ms() -> u64 {
    unsafe { GetTickCount64() }
}

pub const APP_NAME: &str = "rebindr";

fn appdata() -> PathBuf {
    std::env::var_os("APPDATA").map(PathBuf::from).unwrap_or_else(|| PathBuf::from("."))
}

pub fn config_dir() -> PathBuf {
    appdata().join("rebindr")
}

/// Settings folder of the earlier Python versions ("GameSir Shortcuts"), for a one-time import.
pub fn legacy_config_dir() -> PathBuf {
    appdata().join("GameSirShortcuts")
}

pub fn log(msg: &str) {
    let dir = config_dir();
    let _ = std::fs::create_dir_all(&dir);
    if let Ok(mut f) = std::fs::OpenOptions::new().create(true).append(true).open(dir.join("log.txt")) {
        let secs = std::time::SystemTime::now()
            .duration_since(std::time::UNIX_EPOCH)
            .map(|d| d.as_secs())
            .unwrap_or(0);
        let _ = writeln!(f, "[{secs}] {msg}");
    }
}

/// COLORREF from r, g, b.
pub const fn rgb(r: u8, g: u8, b: u8) -> u32 {
    r as u32 | (g as u32) << 8 | (b as u32) << 16
}

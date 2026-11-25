//! rebindr's icon and logo, embedded as PNGs (Windows turns PNG data into icons directly).

use windows_sys::Win32::UI::WindowsAndMessaging::{CreateIconFromResourceEx, HICON, LR_DEFAULTCOLOR};

const ICONS: &[(i32, &[u8])] = &[
    (16, include_bytes!("../assets/icon-16.png")),
    (20, include_bytes!("../assets/icon-20.png")),
    (24, include_bytes!("../assets/icon-24.png")),
    (32, include_bytes!("../assets/icon-32.png")),
    (40, include_bytes!("../assets/icon-40.png")),
    (48, include_bytes!("../assets/icon-48.png")),
    (64, include_bytes!("../assets/icon-64.png")),
    (256, include_bytes!("../assets/icon-256.png")),
];

const MARKS: &[(i32, &[u8])] = &[
    (40, include_bytes!("../assets/mark-40.png")),
    (50, include_bytes!("../assets/mark-50.png")),
    (60, include_bytes!("../assets/mark-60.png")),
    (80, include_bytes!("../assets/mark-80.png")),
];

fn make(set: &[(i32, &[u8])], size: i32) -> HICON {
    // exact size when we have it (sharpest); otherwise the next bigger one, scaled down
    let (s, png) = set.iter().find(|(s, _)| *s >= size).unwrap_or(set.last().unwrap());
    unsafe { CreateIconFromResourceEx(png.as_ptr(), png.len() as u32, 1, 0x0003_0000, *s, *s, LR_DEFAULTCOLOR) }
}

/// The pink app icon (window, taskbar, tray).
pub fn app_icon(size: i32) -> HICON {
    make(ICONS, size)
}

/// The white logo mark for the window header.
pub fn logo_mark(size: i32) -> HICON {
    make(MARKS, size)
}

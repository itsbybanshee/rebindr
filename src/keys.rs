//! Keyboard key names, parsing ("ctrl+shift+esc"), formatting, and sending keys.

use windows_sys::Win32::UI::Input::KeyboardAndMouse::{
    MapVirtualKeyW, SendInput, INPUT, INPUT_0, INPUT_KEYBOARD, KEYBDINPUT, KEYEVENTF_EXTENDEDKEY,
    KEYEVENTF_KEYUP, MAPVK_VK_TO_VSC,
};

/// dwExtraInfo on every key this app sends, so its own hook never blocks them.
pub const OUR_TAG: usize = 0x4753_4953;

pub const MODIFIERS: [&str; 4] = ["win", "ctrl", "alt", "shift"]; // display order, as Windows writes them

const NAMED: &[(&str, u16)] = &[
    ("ctrl", 0xA2), ("shift", 0xA0), ("alt", 0xA4), ("win", 0x5B),
    ("enter", 0x0D), ("esc", 0x1B), ("tab", 0x09), ("space", 0x20), ("backspace", 0x08),
    ("delete", 0x2E), ("insert", 0x2D), ("home", 0x24), ("end", 0x23), ("pageup", 0x21), ("pagedown", 0x22),
    ("up", 0x26), ("down", 0x28), ("left", 0x25), ("right", 0x27),
    ("printscreen", 0x2C), ("capslock", 0x14), ("menu", 0x5D), ("pause", 0x13),
    ("numlock", 0x90), ("scrolllock", 0x91),
    ("volumeup", 0xAF), ("volumedown", 0xAE), ("mute", 0xAD),
    ("playpause", 0xB3), ("nexttrack", 0xB0), ("prevtrack", 0xB1), ("stopmedia", 0xB2),
    ("browserback", 0xA6), ("browserforward", 0xA7), ("browserrefresh", 0xA8), ("browserhome", 0xAC),
    (";", 0xBA), ("=", 0xBB), (",", 0xBC), ("-", 0xBD), (".", 0xBE), ("/", 0xBF), ("`", 0xC0),
    ("[", 0xDB), ("\\", 0xDC), ("]", 0xDD), ("'", 0xDE),
];

const ALIASES: &[(&str, &str)] = &[
    ("control", "ctrl"), ("lctrl", "ctrl"), ("rctrl", "ctrl"), ("escape", "esc"), ("return", "enter"),
    ("windows", "win"), ("super", "win"), ("meta", "win"), ("cmd", "win"), ("lwin", "win"),
    ("option", "alt"), ("lalt", "alt"), ("lshift", "shift"),
    ("del", "delete"), ("ins", "insert"), ("pgup", "pageup"), ("pgdn", "pagedown"),
    ("prtsc", "printscreen"), ("printscr", "printscreen"), ("print", "printscreen"),
    ("arrowup", "up"), ("arrowdown", "down"), ("arrowleft", "left"), ("arrowright", "right"),
    ("volup", "volumeup"), ("voldown", "volumedown"), ("play", "playpause"), ("next", "nexttrack"),
    ("prev", "prevtrack"), ("previous", "prevtrack"), ("apps", "menu"),
    ("plus", "="), ("equals", "="), ("minus", "-"), ("comma", ","), ("period", "."), ("slash", "/"),
    ("semicolon", ";"), ("backslash", "\\"), ("quote", "'"), ("grave", "`"),
];

const NICE: &[(&str, &str)] = &[
    ("ctrl", "Ctrl"), ("shift", "Shift"), ("alt", "Alt"), ("win", "Win"), ("esc", "Esc"),
    ("pageup", "PgUp"), ("pagedown", "PgDn"), ("printscreen", "PrtSc"), ("capslock", "Caps"),
    ("volumeup", "Vol +"), ("volumedown", "Vol \u{2212}"), ("mute", "Mute"), ("playpause", "Play/Pause"),
    ("nexttrack", "Next track"), ("prevtrack", "Prev track"), ("stopmedia", "Stop"),
    ("browserback", "Browser back"), ("browserforward", "Browser fwd"),
    ("browserrefresh", "Refresh"), ("browserhome", "Browser home"),
    ("up", "\u{2191}"), ("down", "\u{2193}"), ("left", "\u{2190}"), ("right", "\u{2192}"),
];

fn is_fkey(k: &str) -> Option<u16> {
    let n: u16 = k.strip_prefix('f')?.parse().ok()?;
    (1..=24).contains(&n).then_some(0x6F + n)
}

pub fn vk_of(k: &str) -> Option<u16> {
    let b = k.as_bytes();
    if b.len() == 1 && (b[0].is_ascii_lowercase() || b[0].is_ascii_digit()) {
        return Some(b[0].to_ascii_uppercase() as u16);
    }
    if let Some(v) = is_fkey(k) {
        return Some(v);
    }
    NAMED.iter().find(|(n, _)| *n == k).map(|(_, v)| *v)
}

/// Key name for a virtual-key code seen by the keyboard hook (left/right modifiers merged).
pub fn name_of_vk(vk: u32) -> Option<String> {
    let vk = vk as u16;
    let merged = match vk {
        0x10 | 0xA0 | 0xA1 => return Some("shift".into()),
        0x11 | 0xA2 | 0xA3 => return Some("ctrl".into()),
        0x12 | 0xA4 | 0xA5 => return Some("alt".into()),
        0x5B | 0x5C => return Some("win".into()),
        v => v,
    };
    match merged {
        0x41..=0x5A | 0x30..=0x39 => Some(((merged as u8) as char).to_ascii_lowercase().to_string()),
        0x70..=0x87 => Some(format!("f{}", merged - 0x6F)),
        _ => NAMED.iter().find(|(_, v)| *v == merged).map(|(n, _)| n.to_string()),
    }
}

pub fn parse_keys(text: &str) -> Result<Vec<String>, String> {
    let raw: String = text.trim().to_lowercase().chars().filter(|c| !c.is_whitespace()).collect();
    if raw.is_empty() {
        return Err("Enter a shortcut, e.g. ctrl+shift+esc".into());
    }
    let mut keys: Vec<String> = Vec::new();
    for part in raw.split('+') {
        if part.is_empty() {
            return Err("Empty key \u{2014} write the + key as \u{201c}plus\u{201d}".into());
        }
        let part = ALIASES.iter().find(|(a, _)| *a == part).map(|(_, t)| *t).unwrap_or(part);
        if vk_of(part).is_none() {
            return Err(format!("Unknown key \u{201c}{part}\u{201d}"));
        }
        if !keys.iter().any(|k| k == part) {
            keys.push(part.to_string());
        }
    }
    let mut out: Vec<String> = MODIFIERS.iter().filter(|m| keys.iter().any(|k| k == *m)).map(|m| m.to_string()).collect();
    out.extend(keys.into_iter().filter(|k| !MODIFIERS.contains(&k.as_str())));
    Ok(out)
}

pub fn nice(k: &str) -> String {
    if let Some((_, n)) = NICE.iter().find(|(a, _)| *a == k) {
        return n.to_string();
    }
    if k.chars().count() == 1 || is_fkey(k).is_some() {
        return k.to_uppercase();
    }
    let mut c = k.chars();
    match c.next() {
        Some(f) => f.to_uppercase().collect::<String>() + c.as_str(),
        None => String::new(),
    }
}

/// Properly capitalized text for the editable Keys field ("Ctrl + Shift + F13"). Unlike
/// `nice`, every name here parses back with `parse_keys` (no "+" or symbols in names).
pub fn field_text(keys: &[String]) -> String {
    const FIELD: &[(&str, &str)] = &[
        ("volumeup", "VolumeUp"), ("volumedown", "VolumeDown"), ("playpause", "PlayPause"),
        ("nexttrack", "NextTrack"), ("prevtrack", "PrevTrack"), ("stopmedia", "StopMedia"),
        ("browserback", "BrowserBack"), ("browserforward", "BrowserForward"),
        ("browserrefresh", "BrowserRefresh"), ("browserhome", "BrowserHome"),
        ("pageup", "PageUp"), ("pagedown", "PageDown"), ("printscreen", "PrintScreen"),
        ("capslock", "CapsLock"), ("numlock", "NumLock"), ("scrolllock", "ScrollLock"),
        ("backspace", "Backspace"), ("=", "Plus"),
    ];
    keys.iter()
        .map(|k| match FIELD.iter().find(|(a, _)| *a == k) {
            Some((_, n)) => n.to_string(),
            None if k.chars().count() == 1 || is_fkey(k).is_some() => k.to_uppercase(),
            None => {
                let mut c = k.chars();
                c.next().map(|f| f.to_uppercase().collect::<String>() + c.as_str()).unwrap_or_default()
            }
        })
        .collect::<Vec<_>>()
        .join(" + ")
}

pub fn format_keys(keys: &[String]) -> String {
    keys.iter().map(|k| nice(k)).collect::<Vec<_>>().join(" + ")
}

#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Phase {
    Tap,
    Down,
    Up,
}

fn extended(vk: u16) -> bool {
    matches!(vk, 0x21..=0x28 | 0x2C | 0x2D | 0x2E | 0x5B | 0x5C | 0x5D | 0x90 | 0xA3 | 0xA5 | 0xA6..=0xB7)
}

fn key_input(vk: u16, up: bool) -> INPUT {
    let mut flags = if up { KEYEVENTF_KEYUP } else { 0 };
    if extended(vk) {
        flags |= KEYEVENTF_EXTENDEDKEY;
    }
    INPUT {
        r#type: INPUT_KEYBOARD,
        Anonymous: INPUT_0 {
            ki: KEYBDINPUT {
                wVk: vk,
                wScan: unsafe { MapVirtualKeyW(vk as u32, MAPVK_VK_TO_VSC) } as u16,
                dwFlags: flags,
                time: 0,
                dwExtraInfo: OUR_TAG,
            },
        },
    }
}

pub fn send(vks: &[u16], phase: Phase) {
    let mut ev: Vec<INPUT> = Vec::with_capacity(vks.len() * 2);
    if phase != Phase::Up {
        ev.extend(vks.iter().map(|&v| key_input(v, false)));
    }
    if phase != Phase::Down {
        ev.extend(vks.iter().rev().map(|&v| key_input(v, true)));
    }
    if !ev.is_empty() {
        unsafe { SendInput(ev.len() as u32, ev.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    }
}

/// Re-inject raw hook events (vk, scan, flags) in order, tagged so the hook lets them through.
pub fn replay(events: &[(u32, u32, u32)]) {
    let ev: Vec<INPUT> = events
        .iter()
        .filter(|(vk, _, _)| *vk != 0xFF)
        .map(|&(vk, scan, flags)| INPUT {
            r#type: INPUT_KEYBOARD,
            Anonymous: INPUT_0 {
                ki: KEYBDINPUT {
                    wVk: vk as u16,
                    wScan: (scan & 0xFF) as u16,
                    dwFlags: (if flags & 0x80 != 0 { KEYEVENTF_KEYUP } else { 0 })
                        | (if flags & 0x01 != 0 { KEYEVENTF_EXTENDEDKEY } else { 0 }),
                    time: 0,
                    dwExtraInfo: OUR_TAG,
                },
            },
        })
        .collect();
    if !ev.is_empty() {
        unsafe { SendInput(ev.len() as u32, ev.as_ptr(), std::mem::size_of::<INPUT>() as i32) };
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse() {
        assert_eq!(parse_keys("Shift + Ctrl + Escape").unwrap(), ["ctrl", "shift", "esc"]);
        assert_eq!(parse_keys("shift+s+win").unwrap(), ["win", "shift", "s"]);
        assert_eq!(parse_keys("del+alt+ctrl").unwrap(), ["ctrl", "alt", "delete"]);
        assert_eq!(parse_keys("ctrl+plus").unwrap(), ["ctrl", "="]);
        assert_eq!(parse_keys("VolUp").unwrap(), ["volumeup"]);
        assert!(parse_keys("").is_err());
        assert!(parse_keys("ctrl+").is_err());
        assert!(parse_keys("ctrl+banana").is_err());
    }

    #[test]
    fn format() {
        let k = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        assert_eq!(format_keys(&k(&["ctrl", "shift", "esc"])), "Ctrl + Shift + Esc");
        assert_eq!(format_keys(&k(&["win", "f11", "tab"])), "Win + F11 + Tab");
    }

    #[test]
    fn field_text_is_capitalized_and_round_trips() {
        let k = |v: &[&str]| v.iter().map(|s| s.to_string()).collect::<Vec<_>>();
        for keys in [k(&["ctrl", "shift", "f13"]), k(&["win", "shift", "s"]), k(&["volumeup"]), k(&["ctrl", "="]), k(&["alt", "pagedown"])] {
            let text = field_text(&keys);
            assert_eq!(parse_keys(&text).unwrap(), keys, "{text}");
        }
        assert_eq!(field_text(&k(&["ctrl", "shift", "f13"])), "Ctrl + Shift + F13");
        assert_eq!(field_text(&k(&["volumeup"])), "VolumeUp");
    }

    #[test]
    fn vk_names() {
        assert_eq!(vk_of("r"), Some(0x52));
        assert_eq!(vk_of("f12"), Some(0x7B));
        assert_eq!(name_of_vk(0x52).as_deref(), Some("r"));
        assert_eq!(name_of_vk(0xA3).as_deref(), Some("ctrl"));
        assert_eq!(name_of_vk(0x5C).as_deref(), Some("win"));
        assert_eq!(name_of_vk(0x7B).as_deref(), Some("f12"));
        assert_eq!(name_of_vk(0xAF).as_deref(), Some("volumeup"));
    }

    #[test]
    fn input_size() {
        assert_eq!(std::mem::size_of::<INPUT>(), 40);
    }
}

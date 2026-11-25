//! Controller buttons. Each button is one bit; the table order is also the display order.

pub struct Btn {
    pub id: &'static str,
    pub label: &'static str,
    /// XInput wButtons mask; 0 for buttons that come from elsewhere (triggers, Share).
    pub xmask: u16,
}

pub const BUTTONS: [Btn; 18] = [
    Btn { id: "BACK", label: "View", xmask: 0x0020 },
    Btn { id: "GUIDE", label: "Home", xmask: 0x0400 },
    Btn { id: "SHARE", label: "Share", xmask: 0 },
    Btn { id: "START", label: "Menu", xmask: 0x0010 },
    Btn { id: "LB", label: "LB", xmask: 0x0100 },
    Btn { id: "RB", label: "RB", xmask: 0x0200 },
    Btn { id: "LT", label: "LT", xmask: 0 },
    Btn { id: "RT", label: "RT", xmask: 0 },
    Btn { id: "LS", label: "L-Stick", xmask: 0x0040 },
    Btn { id: "RS", label: "R-Stick", xmask: 0x0080 },
    Btn { id: "UP", label: "D-Up", xmask: 0x0001 },
    Btn { id: "DOWN", label: "D-Down", xmask: 0x0002 },
    Btn { id: "LEFT", label: "D-Left", xmask: 0x0004 },
    Btn { id: "RIGHT", label: "D-Right", xmask: 0x0008 },
    Btn { id: "A", label: "A", xmask: 0x1000 },
    Btn { id: "B", label: "B", xmask: 0x2000 },
    Btn { id: "X", label: "X", xmask: 0x4000 },
    Btn { id: "Y", label: "Y", xmask: 0x8000 },
];

pub const SHARE_BIT: u32 = 1 << 2;
pub const LT_BIT: u32 = 1 << 6;
pub const RT_BIT: u32 = 1 << 7;

pub fn mask_of(ids: &[String]) -> Option<u32> {
    let mut m = 0;
    for id in ids {
        let i = BUTTONS.iter().position(|b| b.id == id)?;
        m |= 1 << i;
    }
    Some(m)
}

pub fn ids_of(mask: u32) -> Vec<String> {
    BUTTONS.iter().enumerate().filter(|(i, _)| mask & (1 << i) != 0).map(|(_, b)| b.id.to_string()).collect()
}

pub fn label_of(mask: u32) -> String {
    BUTTONS
        .iter()
        .enumerate()
        .filter(|(i, _)| mask & (1 << i) != 0)
        .map(|(_, b)| b.label)
        .collect::<Vec<_>>()
        .join(" + ")
}

fn norm(s: &str) -> String {
    s.to_lowercase().chars().filter(|c| !c.is_whitespace() && *c != '-' && *c != '_').collect()
}

/// Other names people use for the same buttons (already normalized).
const ALIASES: &[(&str, &str)] = &[
    ("select", "BACK"), ("share", "SHARE"), ("capture", "SHARE"), ("screenshot", "SHARE"),
    ("xbox", "GUIDE"), ("guide", "GUIDE"), ("menu", "START"), ("options", "START"),
    ("l1", "LB"), ("r1", "RB"), ("l2", "LT"), ("r2", "RT"), ("l3", "LS"), ("r3", "RS"),
    ("leftbumper", "LB"), ("rightbumper", "RB"), ("lefttrigger", "LT"), ("righttrigger", "RT"),
    ("leftstick", "LS"), ("rightstick", "RS"), ("lstick", "LS"), ("rstick", "RS"),
    ("dpadup", "UP"), ("dpaddown", "DOWN"), ("dpadleft", "LEFT"), ("dpadright", "RIGHT"),
];

/// Parses typed buttons like "View + Share" or "back+a".
pub fn parse_buttons(text: &str) -> Result<u32, String> {
    let t = text.trim();
    if t.is_empty() {
        return Err("Enter a controller button, e.g. View + A".into());
    }
    let mut m = 0;
    for part in t.split('+') {
        let p = norm(part);
        if p.is_empty() {
            return Err("Empty button name \u{2014} check the + signs".into());
        }
        let i = BUTTONS
            .iter()
            .position(|b| norm(b.label) == p || norm(b.id) == p)
            .or_else(|| ALIASES.iter().find(|(a, _)| *a == p).and_then(|(_, id)| BUTTONS.iter().position(|b| b.id == *id)))
            .ok_or_else(|| format!("Unknown button \u{201c}{}\u{201d}", part.trim()))?;
        m |= 1 << i;
    }
    Ok(m)
}

/// Buttons held in an XInput wButtons word (triggers handled by the caller).
pub fn from_xinput(wbuttons: u16) -> u32 {
    let mut m = 0;
    for (i, b) in BUTTONS.iter().enumerate() {
        if b.xmask != 0 && wbuttons & b.xmask != 0 {
            m |= 1 << i;
        }
    }
    m
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn roundtrip() {
        let m = mask_of(&["SHARE".into(), "BACK".into()]).unwrap();
        assert_eq!(m, SHARE_BIT | 1);
        assert_eq!(ids_of(m), vec!["BACK", "SHARE"]);
        assert_eq!(label_of(m), "View + Share");
        assert!(mask_of(&["NOPE".into()]).is_none());
        assert_eq!(from_xinput(0x1000 | 0x0020), 1 | (1 << 14));
    }

    #[test]
    fn typed_buttons() {
        assert_eq!(parse_buttons("View + Share").unwrap(), 1 | SHARE_BIT);
        assert_eq!(parse_buttons("back+a").unwrap(), mask_of(&["BACK".into(), "A".into()]).unwrap());
        assert_eq!(parse_buttons(" l-stick + R3 ").unwrap(), mask_of(&["LS".into(), "RS".into()]).unwrap());
        assert_eq!(parse_buttons("D-Up").unwrap(), parse_buttons("dpad up").unwrap());
        assert_eq!(parse_buttons(&label_of(0x3FFFF)).unwrap(), 0x3FFFF); // every label round-trips
        assert!(parse_buttons("").is_err());
        assert!(parse_buttons("View +").is_err());
        assert!(parse_buttons("Banana").is_err());
    }
}

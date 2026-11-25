//! Settings file (%APPDATA%\GameSirShortcuts\config.json), compatible with the earlier versions.

use serde::{Deserialize, Serialize};

use crate::{buttons, keys, util};

#[derive(Serialize, Deserialize, Clone, Debug, PartialEq)]
#[serde(tag = "type", rename_all = "lowercase")]
pub enum Action {
    Keys { keys: Vec<String> },
    Open { target: String },
}

fn t() -> bool {
    true
}
fn tap() -> String {
    "tap".into()
}
fn short() -> String {
    "short".into()
}

#[derive(Serialize, Deserialize, Clone, Debug)]
pub struct Mapping {
    #[serde(default)]
    pub id: String,
    #[serde(default)]
    pub name: String,
    pub buttons: Vec<String>,
    pub action: Action,
    /// "tap" = send once, "hold" = keys stay down while the buttons are held
    #[serde(default = "tap")]
    pub mode: String,
    /// "short" = fire on press, "long" = fire after holding 0.5 s
    #[serde(default = "short")]
    pub press: String,
    #[serde(default = "t")]
    pub enabled: bool,
}

impl Mapping {
    pub fn mask(&self) -> u32 {
        buttons::mask_of(&self.buttons).unwrap_or(0)
    }
    pub fn is_long(&self) -> bool {
        self.press == "long"
    }
    pub fn is_hold(&self) -> bool {
        self.mode == "hold" && matches!(self.action, Action::Keys { .. })
    }
    pub fn action_text(&self) -> String {
        match &self.action {
            Action::Keys { keys } => keys::format_keys(keys),
            Action::Open { target } => {
                let t = target.trim().trim_matches('"');
                let short = t.trim_end_matches(['\\', '/']).rsplit(['\\', '/']).next().unwrap_or(t);
                format!("Open {}", if short.is_empty() { t } else { short })
            }
        }
    }
}

fn two() -> u32 {
    2
}
fn thr() -> u8 {
    100
}

#[derive(Serialize, Deserialize, Debug)]
pub struct Config {
    #[serde(default = "two")]
    pub version: u32,
    #[serde(default = "t")]
    pub enabled: bool,
    #[serde(default = "thr")]
    pub trigger_threshold: u8,
    #[serde(default)]
    pub start_minimized: bool,
    #[serde(default)]
    pub mappings: Vec<Mapping>,
}

impl Default for Config {
    fn default() -> Self {
        Config { version: 2, enabled: true, trigger_threshold: 100, start_minimized: false, mappings: Vec::new() }
    }
}

pub fn new_id() -> String {
    use std::sync::atomic::{AtomicU32, Ordering};
    static N: AtomicU32 = AtomicU32::new(0);
    let t = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_micros()).unwrap_or(0);
    format!("{:x}{:x}", t & 0xFFFF_FFFF, N.fetch_add(1, Ordering::Relaxed))
}

/// Normalizes a mapping; Err when it can't be used.
pub fn clean(mut m: Mapping) -> Result<Mapping, String> {
    let mask = buttons::mask_of(&m.buttons).ok_or("Unknown controller button")?;
    if mask == 0 {
        return Err("Pick at least one controller button".into());
    }
    m.buttons = buttons::ids_of(mask);
    m.action = match m.action {
        Action::Keys { keys } => Action::Keys { keys: keys::parse_keys(&keys.join("+"))? },
        Action::Open { target } => {
            let t = target.trim().to_string();
            if t.is_empty() {
                return Err("Enter what to open".into());
            }
            Action::Open { target: t }
        }
    };
    if m.mode != "hold" || !matches!(m.action, Action::Keys { .. }) {
        m.mode = "tap".into();
    }
    if m.press != "long" {
        m.press = "short".into();
    }
    if m.id.is_empty() {
        m.id = new_id();
    }
    if m.name.trim().is_empty() {
        m.name = m.action_text();
    }
    Ok(m)
}

pub fn path() -> std::path::PathBuf {
    util::config_dir().join("config.json")
}

pub fn load() -> Config {
    // First run of rebindr: import the shortcuts from the earlier "GameSir Shortcuts" version.
    let legacy = util::legacy_config_dir().join("config.json");
    if !path().exists() && legacy.exists() {
        let _ = std::fs::create_dir_all(util::config_dir());
        match std::fs::copy(&legacy, path()) {
            Ok(_) => util::log("Imported shortcuts from GameSir Shortcuts"),
            Err(e) => util::log(&format!("Could not import old shortcuts: {e}")),
        }
    }
    let text = match std::fs::read_to_string(path()) {
        Ok(t) => t,
        Err(_) => return Config::default(),
    };
    let raw: serde_json::Value = match serde_json::from_str(&text) {
        Ok(v) => v,
        Err(e) => {
            util::log(&format!("Could not read config: {e}"));
            return Config::default();
        }
    };
    // Parse settings and shortcuts separately so one bad shortcut can't wipe the rest.
    let mut cfg: Config = serde_json::from_value(serde_json::json!({
        "enabled": raw.get("enabled").cloned().unwrap_or(serde_json::Value::Bool(true)),
        "trigger_threshold": raw.get("trigger_threshold").cloned().unwrap_or(100.into()),
        "start_minimized": raw.get("start_minimized").cloned().unwrap_or(false.into()),
    }))
    .unwrap_or_default();
    if let Some(list) = raw.get("mappings").and_then(|m| m.as_array()) {
        for item in list {
            match serde_json::from_value::<Mapping>(item.clone()).map_err(|e| e.to_string()).and_then(clean) {
                Ok(m) => cfg.mappings.push(m),
                Err(e) => util::log(&format!("Dropped invalid shortcut {item}: {e}")),
            }
        }
    }
    cfg
}

pub fn save(cfg: &Config) {
    let dir = util::config_dir();
    let _ = std::fs::create_dir_all(&dir);
    let tmp = dir.join("config.json.tmp");
    match serde_json::to_string_pretty(cfg) {
        Ok(s) => {
            if std::fs::write(&tmp, s).is_ok() {
                if let Err(e) = std::fs::rename(&tmp, path()) {
                    util::log(&format!("Could not save config: {e}"));
                }
            }
        }
        Err(e) => util::log(&format!("Could not serialize config: {e}")),
    }
}

// ------------------------------------------------------------------ import / export

/// A shareable file: {"app":"rebindr","version":3,"shortcuts":[...]}.
pub fn export_json(maps: &[Mapping]) -> String {
    serde_json::to_string_pretty(&serde_json::json!({ "app": "rebindr", "version": 3, "shortcuts": maps }))
        .unwrap_or_default()
}

pub struct ImportResult {
    pub added: usize,
    pub duplicates: usize,
    pub invalid: usize,
}

/// Adds the shortcuts from an exported file (or a rebindr/GameSir Shortcuts config.json,
/// or a bare list) to `existing`, skipping ones that are already there.
pub fn import_json(text: &str, existing: &mut Vec<Mapping>) -> Result<ImportResult, String> {
    let v: serde_json::Value = serde_json::from_str(text).map_err(|_| "That file isn\u{2019}t valid JSON.".to_string())?;
    let list = v
        .get("shortcuts")
        .or_else(|| v.get("mappings"))
        .and_then(|x| x.as_array())
        .or_else(|| v.as_array())
        .ok_or_else(|| "No rebindr shortcuts found in that file.".to_string())?;
    let mut res = ImportResult { added: 0, duplicates: 0, invalid: 0 };
    for item in list {
        let Ok(mut m) = serde_json::from_value::<Mapping>(item.clone()).map_err(|e| e.to_string()).and_then(clean) else {
            res.invalid += 1;
            continue;
        };
        if existing.iter().any(|e| e.mask() == m.mask() && e.press == m.press && e.action == m.action) {
            res.duplicates += 1;
            continue;
        }
        m.id = new_id(); // never collide with ids already here
        existing.push(m);
        res.added += 1;
    }
    Ok(res)
}

#[cfg(test)]
mod tests {
    #[test]
    fn export_then_import() {
        let mk = |b: &str, k: &[&str], name: &str| {
            clean(Mapping {
                id: String::new(),
                name: name.into(),
                buttons: vec![b.into()],
                action: Action::Keys { keys: k.iter().map(|s| s.to_string()).collect() },
                mode: "tap".into(),
                press: "short".into(),
                enabled: true,
            })
            .unwrap()
        };
        let mine = vec![mk("SHARE", &["ctrl", "shift", "e"], "Clip that"), mk("A", &["f5"], "")];
        let file = export_json(&mine);
        assert!(file.contains("\"app\": \"rebindr\""));
        // importing into an empty list adds both, keeping names
        let mut other = Vec::new();
        let r = import_json(&file, &mut other).unwrap();
        assert_eq!((r.added, r.duplicates, r.invalid), (2, 0, 0));
        assert_eq!(other[0].name, "Clip that");
        // importing again skips duplicates
        let r = import_json(&file, &mut other).unwrap();
        assert_eq!((r.added, r.duplicates), (0, 2));
        // a bare list with one bad entry
        let r = import_json(r#"[{"buttons":["B"],"action":{"type":"keys","keys":["esc"]}},{"buttons":["NOPE"],"action":{"type":"keys","keys":["a"]}}]"#, &mut other).unwrap();
        assert_eq!((r.added, r.invalid), (1, 1));
        assert!(import_json("not json", &mut other).is_err());
        assert!(import_json("{\"x\":1}", &mut other).is_err());
    }

    use super::*;

    #[test]
    fn reads_python_format_and_cleans() {
        let json = r#"{"version":2,"enabled":true,"mappings":[
            {"id":"a","name":"","buttons":["SHARE"],"action":{"type":"keys","keys":["shift","ctrl","e"]},"mode":"tap","press":"short","enabled":true},
            {"id":"b","name":"x","buttons":["SHARE"],"action":{"type":"keys","keys":["ctrl","shift","r"]},"mode":"tap","press":"long"},
            {"buttons":["NOPE"],"action":{"type":"keys","keys":["a"]}},
            {"buttons":["A"],"action":{"type":"weird"}}
        ]}"#;
        let raw: serde_json::Value = serde_json::from_str(json).unwrap();
        let maps: Vec<Mapping> = raw["mappings"]
            .as_array()
            .unwrap()
            .iter()
            .filter_map(|v| serde_json::from_value::<Mapping>(v.clone()).ok())
            .filter_map(|m| clean(m).ok())
            .collect();
        assert_eq!(maps.len(), 2);
        assert_eq!(maps[0].action, Action::Keys { keys: vec!["ctrl".into(), "shift".into(), "e".into()] });
        assert_eq!(maps[0].name, "Ctrl + Shift + E");
        assert!(maps[1].is_long());
    }

    #[test]
    fn open_action_text() {
        let m = clean(Mapping {
            id: String::new(),
            name: String::new(),
            buttons: vec!["A".into()],
            action: Action::Open { target: r"C:\Games\thing.exe".into() },
            mode: "hold".into(),
            press: "short".into(),
            enabled: true,
        })
        .unwrap();
        assert_eq!(m.mode, "tap");
        assert_eq!(m.name, "Open thing.exe");
    }
}

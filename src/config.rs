//! JSON config (the same schema on Linux, Windows and macOS), picture profiles, and the "Start with
//! login" autostart entry.

use crate::hotkey::Hotkey;
use serde::{Deserialize, Serialize};
use std::collections::BTreeMap;
use std::fs;
use std::io;
use std::path::{Path, PathBuf};

pub const APP_ID: &str = "monitor-input-switcher";
pub const APP_NAME: &str = "Monitor Input Switcher";

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct InputEntry {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub value: u32,
    #[serde(default)]
    pub hotkey: Option<String>,
    /// Whether "Next input" cycles through this input (untick inputs with nothing plugged in).
    /// Missing in older configs, which means included.
    #[serde(default = "included")]
    pub include_in_next: bool,
}

fn included() -> bool {
    true
}

impl InputEntry {
    pub fn new(name: impl Into<String>, value: u32, hotkey: Option<String>) -> Self {
        InputEntry {
            name: name.into(),
            value,
            hotkey,
            include_in_next: true,
        }
    }
}

/// The input "Next input" goes to: the first included input after the current one, in list order
/// and wrapping around. Inputs left out of the cycle are skipped, but switching away from one works.
pub fn next_input(inputs: &[InputEntry], current: Option<u32>) -> Result<&InputEntry, String> {
    if inputs.is_empty() {
        return Err("No inputs configured".into());
    }
    let start = current.and_then(|c| inputs.iter().position(|e| crate::ddc::matches(c, e.value)));
    let n = inputs.len();
    let index = match start {
        Some(s) => (1..=n)
            .map(|k| (s + k) % n)
            .find(|&i| inputs[i].include_in_next),
        None => (0..n).find(|&i| inputs[i].include_in_next),
    };
    index
        .map(|i| &inputs[i])
        .ok_or_else(|| "No input is included in \"Next input\" (see Settings)".into())
}

/// A monitor setting a picture profile can include: (config key, VCP code, label, maximum).
/// Profiles apply them in this order: the colour preset first, since changing it can reset the
/// colour gains on some monitors, and brightness after contrast.
pub const PICTURE_CONTROLS: &[(&str, u8, &str, u16)] = &[
    ("ColorPreset", 0x14, "Colour temperature", 0xFF),
    ("RedGain", 0x16, "Red gain", 100),
    ("GreenGain", 0x18, "Green gain", 100),
    ("BlueGain", 0x1A, "Blue gain", 100),
    ("Contrast", 0x12, "Contrast", 100),
    ("Brightness", 0x10, "Brightness", 100),
    ("Volume", 0x62, "Volume", 100),
];

/// MCCS name of a colour preset value (VCP 0x14).
pub fn color_preset_name(value: u16) -> Option<&'static str> {
    Some(match value {
        0x01 => "sRGB",
        0x02 => "Native",
        0x03 => "4000K",
        0x04 => "5000K",
        0x05 => "6500K",
        0x06 => "7500K",
        0x07 => "8200K",
        0x08 => "9300K",
        0x09 => "10000K",
        0x0A => "11500K",
        0x0B => "User 1",
        0x0C => "User 2",
        0x0D => "User 3",
        _ => return None,
    })
}

pub const BRIGHTNESS_UP_HOTKEY: &str = "Ctrl+Alt+Shift+Up";
pub const BRIGHTNESS_DOWN_HOTKEY: &str = "Ctrl+Alt+Shift+Down";
pub const VOLUME_UP_HOTKEY: &str = "Ctrl+Alt+Shift+Right";
pub const VOLUME_DOWN_HOTKEY: &str = "Ctrl+Alt+Shift+Left";
/// The highest brightness or volume set by a step or a number.
pub const LEVEL_MAX: u16 = 100;

/// A monitor setting the shortcuts and `brightness`/`volume` commands change.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Level {
    Brightness,
    Volume,
    /// The audio mute (VCP 0x8D): 1 = muted, 2 = not muted.
    Mute,
}

impl Level {
    pub fn vcp(self) -> u8 {
        match self {
            Level::Brightness => 0x10,
            Level::Volume => 0x62,
            Level::Mute => 0x8D,
        }
    }

    pub fn name(self) -> &'static str {
        match self {
            Level::Brightness => "Brightness",
            Level::Volume => "Volume",
            Level::Mute => "Mute",
        }
    }

    /// What a change to `value` says: "Brightness 60", "Volume 40", "Muted".
    pub fn describe(self, value: u16) -> String {
        match self {
            Level::Mute if value & 0xFF == 1 => "Muted".into(),
            Level::Mute => "Unmuted".into(),
            _ => format!("{} {value}", self.name()),
        }
    }

    /// The value an argument asks for, given the current one.
    /// Brightness/volume: "up"/"down" move by `step`, "+5"/"-5" by that much, "60" sets it, all
    /// within 0-100. Mute: "mute", "unmute" or "toggle".
    pub fn target(self, arg: &str, current: u32, step: u16) -> Result<u16, String> {
        let arg = arg.trim().to_lowercase();
        if self == Level::Mute {
            let muted = current & 0xFF == 1;
            return match arg.as_str() {
                "mute" | "on" => Ok(1),
                "unmute" | "off" => Ok(2),
                "toggle" | "" => Ok(if muted { 2 } else { 1 }),
                _ => Err(format!("{arg:?}: use mute, unmute or toggle")),
            };
        }
        let noun = self.name().to_lowercase();
        let current = i64::from(current.min(u32::from(LEVEL_MAX)));
        let invalid = || {
            format!("{arg:?} is not a {noun}: use up, down, a number (0-100), or +N / -N")
        };
        let target = match arg.as_str() {
            "up" => current + i64::from(step),
            "down" => current - i64::from(step),
            _ if arg.starts_with('+') || arg.starts_with('-') => {
                current + arg.parse::<i64>().map_err(|_| invalid())?
            }
            _ => {
                let v: i64 = arg.parse().map_err(|_| invalid())?;
                if !(0..=i64::from(LEVEL_MAX)).contains(&v) {
                    return Err(format!(
                        "{} {v} is out of range (0 to {LEVEL_MAX})",
                        self.name()
                    ));
                }
                v
            }
        };
        Ok(target.clamp(0, i64::from(LEVEL_MAX)) as u16)
    }
}

/// A named set of monitor settings applied together ("Day", "Evening"...). Only the settings it
/// lists are changed.
#[derive(Clone, Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase")]
pub struct PictureProfile {
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub hotkey: Option<String>,
    /// Config key from `PICTURE_CONTROLS` -> value.
    #[serde(default)]
    pub values: BTreeMap<String, u16>,
}

impl PictureProfile {
    /// (VCP code, value) pairs, in the order they are applied.
    pub fn writes(&self) -> Vec<(u8, u16)> {
        PICTURE_CONTROLS
            .iter()
            .filter_map(|(key, code, _, _)| self.values.get(*key).map(|v| (*code, *v)))
            .collect()
    }

    /// "Brightness 44, Contrast 50, Colour temperature 6500K": the settings people tell profiles
    /// apart by come first.
    pub fn summary(&self) -> String {
        const ORDER: [&str; 7] = [
            "Brightness",
            "Contrast",
            "ColorPreset",
            "RedGain",
            "GreenGain",
            "BlueGain",
            "Volume",
        ];
        let parts: Vec<String> = ORDER
            .iter()
            .filter_map(|k| PICTURE_CONTROLS.iter().find(|(key, _, _, _)| key == k))
            .filter_map(|(key, code, label, _)| {
                let v = *self.values.get(*key)?;
                Some(match color_preset_name(v).filter(|_| *code == 0x14) {
                    Some(name) => format!("{label} {name}"),
                    None => format!("{label} {v}"),
                })
            })
            .collect();
        if parts.is_empty() {
            "(no settings)".into()
        } else {
            parts.join(", ")
        }
    }
}

/// A profile by name (case-insensitive).
pub fn find_profile<'a>(profiles: &'a [PictureProfile], name: &str) -> Option<&'a PictureProfile> {
    let name = name.trim();
    profiles.iter().find(|p| p.name.eq_ignore_ascii_case(name))
}

#[derive(Clone, Debug, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "PascalCase", default)]
pub struct AppConfig {
    pub monitor_name: Option<String>,
    pub monitor_device: Option<String>,
    pub inputs: Vec<InputEntry>,
    pub toggle_hotkey: Option<String>,
    /// Picture profiles, shown in the tray's Picture menu.
    pub profiles: Vec<PictureProfile>,
    pub brightness_up_hotkey: Option<String>,
    pub brightness_down_hotkey: Option<String>,
    /// How much a brightness shortcut changes the brightness (out of 100).
    pub brightness_step: u16,
    pub volume_up_hotkey: Option<String>,
    pub volume_down_hotkey: Option<String>,
    /// How much a volume shortcut changes the monitor's speaker volume (out of 100).
    pub volume_step: u16,
    /// Toggles the monitor's audio mute.
    pub mute_hotkey: Option<String>,
    /// A notification after each switch.
    pub show_notifications: bool,
    /// A notification when the app starts, so it's clear it is running in the tray.
    pub show_startup_notification: bool,
}

/// Monitor-agnostic defaults: no monitor and no inputs yet. The app fills them in from the
/// connected monitor on first run (see autoconfig.rs).
impl Default for AppConfig {
    fn default() -> Self {
        AppConfig {
            monitor_name: None,
            monitor_device: None,
            inputs: Vec::new(),
            toggle_hotkey: Some(crate::autoconfig::TOGGLE_HOTKEY.into()),
            profiles: Vec::new(),
            brightness_up_hotkey: Some(BRIGHTNESS_UP_HOTKEY.into()),
            brightness_down_hotkey: Some(BRIGHTNESS_DOWN_HOTKEY.into()),
            brightness_step: 10,
            volume_up_hotkey: Some(VOLUME_UP_HOTKEY.into()),
            volume_down_hotkey: Some(VOLUME_DOWN_HOTKEY.into()),
            volume_step: 5,
            mute_hotkey: None,
            show_notifications: true,
            show_startup_notification: true,
        }
    }
}

/// This executable. After an upgrade replaced the file, /proc/self/exe reads "<path> (deleted)":
/// the new binary at the same path is what we want then.
pub fn self_exe() -> PathBuf {
    match std::env::current_exe() {
        Ok(p) => {
            let s = p.display().to_string();
            s.strip_suffix(" (deleted)").map(PathBuf::from).unwrap_or(p)
        }
        Err(_) => PathBuf::from(APP_ID),
    }
}

/// `~/.config/monitor-input-switcher/config.json` on Linux,
/// `%APPDATA%\MonitorInputSwitcher\config.json` on Windows,
/// `~/Library/Application Support/MonitorInputSwitcher/config.json` on macOS.
pub fn config_path() -> PathBuf {
    crate::platform::config_dir().join("config.json")
}

pub use crate::platform::autostart;

pub enum Loaded {
    Ok(AppConfig),
    FirstRun(AppConfig),
    /// The file exists but could not be read; defaults are used.
    Error(AppConfig, String),
}

impl AppConfig {
    pub fn load() -> Loaded {
        let path = config_path();
        match fs::read_to_string(&path) {
            Err(e) if e.kind() == io::ErrorKind::NotFound => Loaded::FirstRun(AppConfig::default()),
            Err(e) => Loaded::Error(
                AppConfig::default(),
                format!("Could not read {}: {e}", path.display()),
            ),
            Ok(text) => match serde_json::from_str(&text) {
                Ok(cfg) => Loaded::Ok(cfg),
                Err(e) => Loaded::Error(
                    AppConfig::default(),
                    format!("Could not read {}: {e}. Defaults are used.", path.display()),
                ),
            },
        }
    }

    /// The config, or defaults if it is missing or unreadable.
    pub fn load_or_default() -> AppConfig {
        match Self::load() {
            Loaded::Ok(c) | Loaded::FirstRun(c) | Loaded::Error(c, _) => c,
        }
    }

    fn to_json(&self) -> io::Result<String> {
        let mut text = serde_json::to_string_pretty(self).map_err(io::Error::other)?;
        text.push('\n');
        Ok(text)
    }

    pub fn save(&self) -> io::Result<()> {
        let path = config_path();
        fs::create_dir_all(path.parent().unwrap())?;
        let tmp = path.with_extension("json.tmp");
        fs::write(&tmp, self.to_json()?)?;
        fs::rename(&tmp, &path)
    }

    /// Writes the settings to a file another machine can import (same format as config.json).
    pub fn export(&self, path: &Path) -> Result<(), String> {
        self.to_json()
            .and_then(|text| fs::write(path, text))
            .map_err(|e| format!("Could not write {}: {e}", path.display()))
    }

    /// Reads settings exported by `export` (or a copy of config.json), and checks them.
    pub fn import(path: &Path) -> Result<AppConfig, String> {
        let invalid = |e: String| format!("{} is not a valid settings file: {e}", path.display());
        let read = |e: io::Error| format!("Could not read {}: {e}", path.display());
        // A settings file is a few hundred bytes; don't load a big file picked by mistake.
        if fs::metadata(path).map_err(read)?.len() > 1024 * 1024 {
            return Err(invalid("it is larger than 1 MB".into()));
        }
        let text = fs::read_to_string(path).map_err(read)?;
        Self::from_export(&text).map_err(invalid)
    }

    fn from_export(text: &str) -> Result<AppConfig, String> {
        // Windows PowerShell and older Notepad start UTF-8 files with a byte order mark.
        let text = text.strip_prefix('\u{FEFF}').unwrap_or(text);
        let value: serde_json::Value = serde_json::from_str(text).map_err(|e| e.to_string())?;
        // Every field has a default, so any JSON object would parse: require the fields that
        // carry the settings rather than silently filling them in.
        let inputs = value
            .get("Inputs")
            .and_then(|v| v.as_array())
            .ok_or("no \"Inputs\" list")?;
        for (i, input) in inputs.iter().enumerate() {
            for field in ["Name", "Value"] {
                if input.get(field).is_none() {
                    return Err(format!("input {} has no \"{field}\"", i + 1));
                }
            }
        }
        let mut config: AppConfig = serde_json::from_value(value).map_err(|e| e.to_string())?;
        let blank = |h: &mut Option<String>| {
            if h.as_deref().is_some_and(|t| t.trim().is_empty()) {
                *h = None;
            }
        };
        config.inputs.iter_mut().for_each(|e| blank(&mut e.hotkey));
        config.profiles.iter_mut().for_each(|p| blank(&mut p.hotkey));
        blank(&mut config.brightness_up_hotkey);
        blank(&mut config.brightness_down_hotkey);
        blank(&mut config.volume_up_hotkey);
        blank(&mut config.volume_down_hotkey);
        blank(&mut config.mute_hotkey);
        blank(&mut config.toggle_hotkey);
        config.check()?;
        Ok(config)
    }

    /// Checks what a working config needs: at least one input, each with a name and an input code
    /// in range, and valid shortcuts with no duplicates.
    pub fn check(&self) -> Result<(), String> {
        if self.inputs.is_empty() {
            return Err("Add at least one input.".into());
        }
        let mut shortcuts = Vec::new();
        for (i, e) in self.inputs.iter().enumerate() {
            let name = e.name.trim();
            if name.is_empty() {
                return Err(format!("Input {} has no name.", i + 1));
            }
            if !(1..=0xFFFF).contains(&e.value) {
                return Err(format!(
                    "{name}: input code {} is out of range (1 to 65535).",
                    e.value
                ));
            }
            shortcuts.push((name.to_string(), &e.hotkey));
        }
        shortcuts.push(("Next input".into(), &self.toggle_hotkey));
        shortcuts.push(("Brightness up".into(), &self.brightness_up_hotkey));
        shortcuts.push(("Brightness down".into(), &self.brightness_down_hotkey));
        shortcuts.push(("Volume up".into(), &self.volume_up_hotkey));
        shortcuts.push(("Volume down".into(), &self.volume_down_hotkey));
        shortcuts.push(("Mute".into(), &self.mute_hotkey));
        for (name, step) in [
            ("Brightness", self.brightness_step),
            ("Volume", self.volume_step),
        ] {
            if !(1..=50).contains(&step) {
                return Err(format!("{name} step {step} is out of range (1 to 50)."));
            }
        }
        let mut names: Vec<String> = Vec::new();
        for (i, p) in self.profiles.iter().enumerate() {
            let name = p.name.trim();
            if name.is_empty() {
                return Err(format!("Picture profile {} has no name.", i + 1));
            }
            if names.iter().any(|n| n.eq_ignore_ascii_case(name)) {
                return Err(format!("Two picture profiles are called {name:?}."));
            }
            names.push(name.to_string());
            if p.values.is_empty() {
                return Err(format!("Picture profile {name:?} has no settings to apply."));
            }
            for (key, value) in &p.values {
                let Some((_, _, label, max)) =
                    PICTURE_CONTROLS.iter().find(|(k, _, _, _)| k == key)
                else {
                    return Err(format!("Picture profile {name:?}: unknown setting {key:?}."));
                };
                if value > max {
                    return Err(format!(
                        "Picture profile {name:?}: {label} {value} is out of range (0 to {max})."
                    ));
                }
            }
            shortcuts.push((format!("Picture: {name}"), &p.hotkey));
        }
        let mut seen: Vec<Hotkey> = Vec::new();
        for (owner, text) in shortcuts {
            let Some(text) = text.as_deref().filter(|t| !t.trim().is_empty()) else {
                continue;
            };
            let hotkey =
                Hotkey::parse(text).ok_or_else(|| format!("{owner}: shortcut {text:?} is not valid."))?;
            if seen.contains(&hotkey) {
                return Err(format!("The shortcut {hotkey} is assigned more than once."));
            }
            seen.push(hotkey);
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn inputs() -> Vec<InputEntry> {
        let mut v = vec![
            InputEntry::new("DisplayPort", 15, None),
            InputEntry::new("HDMI-1", 17, None),
            InputEntry::new("HDMI-2", 18, None),
            InputEntry::new("USB-C", 27, None),
        ];
        v[1].include_in_next = false;
        v[2].include_in_next = false;
        v
    }

    #[test]
    fn next_skips_excluded_inputs() {
        let v = inputs();
        assert_eq!(next_input(&v, Some(0x1B0F)).unwrap().name, "USB-C"); // DP -> USB-C, skipping HDMI
        assert_eq!(next_input(&v, Some(27)).unwrap().name, "DisplayPort"); // wraps around
        assert_eq!(next_input(&v, Some(17)).unwrap().name, "USB-C"); // leaving an excluded input works
        assert_eq!(next_input(&v, None).unwrap().name, "DisplayPort"); // unknown current: first included
        let mut none = v.clone();
        none.iter_mut().for_each(|e| e.include_in_next = false);
        assert!(next_input(&none, Some(15)).is_err());
        assert!(next_input(&[], None).is_err());
    }

    #[test]
    fn old_configs_include_every_input() {
        let cfg: AppConfig =
            serde_json::from_str(r#"{"Inputs":[{"Name":"DP","Value":15,"Hotkey":null}]}"#).unwrap();
        assert!(cfg.inputs[0].include_in_next);
        let json = serde_json::to_string(&cfg).unwrap();
        assert!(json.contains(r#""IncludeInNext":true"#));
    }

    #[test]
    fn export_round_trip() {
        let mut cfg = AppConfig::default();
        cfg.monitor_name = Some("AW3926QW".into());
        cfg.inputs = inputs();
        let text = cfg.to_json().unwrap();
        assert_eq!(AppConfig::from_export(&text).unwrap(), cfg);
        assert_eq!(AppConfig::from_export(&format!("\u{FEFF}{text}")).unwrap(), cfg);
    }

    #[test]
    fn import_rejects_bad_files() {
        let bad = [
            "",
            "not json",
            "[1, 2]",
            "{}",
            r#"{"Inputs": "DP"}"#,
            r#"{"Inputs": []}"#,
            r#"{"Inputs": [{}]}"#,
            r#"{"Inputs": [{"Name": "DP"}]}"#,
            r#"{"Inputs": [{"Value": 15}]}"#,
            r#"{"Inputs": [{"Name": " ", "Value": 15}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": "15"}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": -1}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 0}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 70000}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 15, "Hotkey": "Ctrl+Banana"}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 15, "Hotkey": "Ctrl+1"},
                           {"Name": "HDMI", "Value": 17, "Hotkey": "ctrl+1"}]}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 15, "Hotkey": "Ctrl+1"}], "ToggleHotkey": "Ctrl+1"}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 15}], "ShowNotifications": "yes"}"#,
            r#"{"Inputs": [{"Name": "DP", "Value": 15}"#,
        ];
        for text in bad {
            assert!(AppConfig::from_export(text).is_err(), "accepted: {text}");
        }
        // Minimal and lenient: other fields default, unknown fields and blank shortcuts are ignored.
        let ok = AppConfig::from_export(
            r#"{"Inputs": [{"Name": "DP", "Value": 15, "Hotkey": ""}], "Extra": 1}"#,
        )
        .unwrap();
        assert_eq!(ok.inputs[0].name, "DP");
        assert_eq!(ok.inputs[0].hotkey, None);
        assert!(ok.show_notifications);
    }

    fn profile(name: &str, values: &[(&str, u16)], hotkey: Option<&str>) -> PictureProfile {
        PictureProfile {
            name: name.into(),
            hotkey: hotkey.map(String::from),
            values: values.iter().map(|(k, v)| (k.to_string(), *v)).collect(),
        }
    }

    #[test]
    fn profile_writes_follow_control_order() {
        let p = profile("Day", &[("Brightness", 80), ("ColorPreset", 5), ("Contrast", 50)], None);
        // Colour preset first, brightness after contrast, whatever the JSON order.
        assert_eq!(p.writes(), vec![(0x14, 5), (0x12, 50), (0x10, 80)]);
        assert_eq!(
            p.summary(),
            "Brightness 80, Contrast 50, Colour temperature 6500K"
        );
        assert_eq!(profile("Empty", &[], None).summary(), "(no settings)");
    }

    #[test]
    fn profiles_are_checked() {
        let mut cfg = AppConfig {
            inputs: inputs(),
            ..Default::default()
        };
        cfg.profiles = vec![profile("Day", &[("Brightness", 80)], Some("Ctrl+Alt+Shift+D"))];
        assert!(cfg.check().is_ok());

        cfg.profiles = vec![profile("Day", &[("Brightness", 101)], None)];
        assert!(cfg.check().unwrap_err().contains("out of range"));

        cfg.profiles = vec![profile("Day", &[("Sharpness", 5)], None)];
        assert!(cfg.check().unwrap_err().contains("unknown setting"));

        cfg.profiles = vec![profile("Day", &[], None)];
        assert!(cfg.check().unwrap_err().contains("no settings"));

        cfg.profiles = vec![
            profile("Day", &[("Brightness", 80)], None),
            profile("day", &[("Brightness", 30)], None),
        ];
        assert!(cfg.check().unwrap_err().contains("Two picture profiles"));

        // A profile shortcut may not reuse the Next-input shortcut.
        cfg.profiles = vec![profile(
            "Day",
            &[("Brightness", 80)],
            cfg.toggle_hotkey.clone().as_deref(),
        )];
        assert!(cfg.check().unwrap_err().contains("more than once"));
    }

    #[test]
    fn profiles_round_trip_and_old_configs_load() {
        let mut cfg = AppConfig {
            inputs: inputs(),
            ..Default::default()
        };
        cfg.profiles = vec![profile("Evening", &[("Brightness", 30), ("ColorPreset", 5)], None)];
        let text = cfg.to_json().unwrap();
        assert!(text.contains("\"Profiles\""));
        assert!(text.contains("\"Brightness\": 30"));
        assert_eq!(AppConfig::from_export(&text).unwrap(), cfg);
        // A config saved by 1.1.0 has no "Profiles": it loads with none.
        let old = r#"{"Inputs":[{"Name":"HDMI-1","Value":17}]}"#;
        assert!(AppConfig::from_export(old).unwrap().profiles.is_empty());
        assert_eq!(find_profile(&cfg.profiles, " evening ").unwrap().name, "Evening");
    }

    #[test]
    fn brightness_targets() {
        assert_eq!(Level::Brightness.target("up", 44, 10), Ok(54));
        assert_eq!(Level::Brightness.target("DOWN", 44, 10), Ok(34));
        assert_eq!(Level::Brightness.target("up", 95, 10), Ok(100));
        assert_eq!(Level::Brightness.target("down", 4, 10), Ok(0));
        assert_eq!(Level::Brightness.target("+5", 44, 10), Ok(49));
        assert_eq!(Level::Brightness.target("-50", 44, 10), Ok(0));
        assert_eq!(Level::Brightness.target(" 60 ", 44, 10), Ok(60));
        assert!(Level::Brightness.target("120", 44, 10).is_err());
        assert!(Level::Brightness.target("brighter", 44, 10).is_err());
        // A monitor reporting more than 100 is treated as 100.
        assert_eq!(Level::Brightness.target("down", 255, 10), Ok(90));
    }

    #[test]
    fn brightness_settings_are_checked() {
        let mut cfg = AppConfig {
            inputs: inputs(),
            ..Default::default()
        };
        assert!(cfg.check().is_ok());
        cfg.brightness_step = 0;
        assert!(cfg.check().unwrap_err().contains("step"));
        cfg.brightness_step = 10;
        cfg.volume_step = 51;
        assert!(cfg.check().unwrap_err().contains("Volume step"));
        cfg.volume_step = 5;
        cfg.mute_hotkey = cfg.volume_up_hotkey.clone();
        assert!(cfg.check().unwrap_err().contains("more than once"));
        cfg.mute_hotkey = None;
        cfg.brightness_step = 10;
        cfg.brightness_down_hotkey = cfg.brightness_up_hotkey.clone();
        assert!(cfg.check().unwrap_err().contains("more than once"));
        // Configs from 1.2.0 have no brightness fields: they get the default shortcuts.
        let old = r#"{"Inputs":[{"Name":"HDMI-1","Value":17}]}"#;
        let loaded = AppConfig::from_export(old).unwrap();
        assert_eq!(loaded.brightness_up_hotkey.as_deref(), Some(BRIGHTNESS_UP_HOTKEY));
        assert_eq!(loaded.brightness_step, 10);
        assert_eq!(loaded.volume_up_hotkey.as_deref(), Some(VOLUME_UP_HOTKEY));
        assert_eq!(loaded.volume_step, 5);
        assert_eq!(loaded.mute_hotkey, None);
    }

    #[test]
    fn volume_and_mute_targets() {
        assert_eq!(Level::Volume.target("up", 70, 5), Ok(75));
        assert_eq!(Level::Volume.target("-80", 70, 5), Ok(0));
        assert!(Level::Volume.target("200", 70, 5).unwrap_err().contains("Volume 200"));
        // MCCS 0x8D: 1 = muted, 2 = not muted; the high byte (screen blank) is ignored.
        assert_eq!(Level::Mute.target("toggle", 2, 0), Ok(1));
        assert_eq!(Level::Mute.target("toggle", 0x0101, 0), Ok(2));
        assert_eq!(Level::Mute.target("mute", 1, 0), Ok(1));
        assert_eq!(Level::Mute.target("unmute", 1, 0), Ok(2));
        assert!(Level::Mute.target("loud", 1, 0).is_err());
        assert_eq!(Level::Mute.describe(1), "Muted");
        assert_eq!(Level::Volume.describe(40), "Volume 40");
    }
}

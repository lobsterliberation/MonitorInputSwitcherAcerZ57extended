//! Hotkey -> physical key code (W3C names, used by both global-hotkey and the menu's shortcut
//! labels). Like the digit row everywhere else, keys are positional: "Ctrl+[" is the key to the
//! right of P on any layout.

use crate::hotkey::Hotkey;
use global_hotkey::hotkey::{Code, Modifiers};

pub fn code(hk: &Hotkey) -> Option<Code> {
    let key = hk.key.as_str();
    let mut chars = key.chars();
    if let (Some(c), None) = (chars.next(), chars.clone().next()) {
        return match c {
            'A'..='Z' => format!("Key{c}").parse().ok(),
            '0'..='9' => format!("Digit{c}").parse().ok(),
            '`' => Some(Code::Backquote),
            '-' => Some(Code::Minus),
            '=' => Some(Code::Equal),
            '[' => Some(Code::BracketLeft),
            ']' => Some(Code::BracketRight),
            '\\' => Some(Code::Backslash),
            ';' => Some(Code::Semicolon),
            '\'' => Some(Code::Quote),
            ',' => Some(Code::Comma),
            '.' => Some(Code::Period),
            '/' => Some(Code::Slash),
            '§' => Some(Code::IntlBackslash),
            _ => None,
        };
    }
    if let Some(n) = key.strip_prefix('F').filter(|n| n.parse::<u32>().is_ok()) {
        return format!("F{n}").parse().ok();
    }
    Some(match key {
        "Esc" => Code::Escape,
        "Tab" => Code::Tab,
        "Backspace" => Code::Backspace,
        "Return" => Code::Enter,
        "Enter" => Code::NumpadEnter,
        "Ins" => Code::Insert,
        "Del" => Code::Delete,
        "Home" => Code::Home,
        "End" => Code::End,
        "Left" => Code::ArrowLeft,
        "Up" => Code::ArrowUp,
        "Right" => Code::ArrowRight,
        "Down" => Code::ArrowDown,
        "PgUp" => Code::PageUp,
        "PgDown" => Code::PageDown,
        "Space" => Code::Space,
        "Volume Down" => Code::AudioVolumeDown,
        "Volume Mute" => Code::AudioVolumeMute,
        "Volume Up" => Code::AudioVolumeUp,
        _ => return None,
    })
}

pub fn modifiers(hk: &Hotkey) -> Modifiers {
    let mut mods = Modifiers::empty();
    for (flag, on) in [
        (Modifiers::CONTROL, hk.ctrl),
        (Modifiers::ALT, hk.alt),
        (Modifiers::SHIFT, hk.shift),
        (Modifiers::META, hk.meta),
    ] {
        if on {
            mods |= flag;
        }
    }
    mods
}

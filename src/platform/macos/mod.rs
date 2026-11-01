//! macOS backend (Apple Silicon): DDC/CI through IOAVService, a menu bar item and Carbon global
//! hotkeys (tray-icon / global-hotkey), notifications through osascript, a Unix socket for the
//! CLI, and a LaunchAgent for "Open at login".

pub mod ddc;
pub mod ipc;
mod keys;
mod ui;

pub use ui::{Ui, run_daemon};

use std::path::PathBuf;

pub const META_NAME: &str = "Cmd";
pub const START_WITH_LOGIN: &str = "Open at login";
/// Appended to the Settings window's inputs hint.
pub const SHORTCUTS_HINT: &str = "";

fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| PathBuf::from("/"))
}

/// ~/Library/Application Support/MonitorInputSwitcher
pub fn config_dir() -> PathBuf {
    home()
        .join("Library/Application Support")
        .join("MonitorInputSwitcher")
}

/// Nothing to do: a macOS program prints to the terminal it was started from.
pub fn attach_console() {}

/// Not needed: the app releases its hotkeys while the Settings window is open.
pub fn block_shortcuts_for_capture(_blocked: bool) {}

/// The character on the key with this PC set-1 scan code. Shortcuts are physical keys on macOS
/// (Carbon hotkeys take virtual key codes), so the US character names the position.
pub fn layout_char(scancode: u32) -> Option<char> {
    Some(match scancode {
        0x29 => '`',
        0x0C => '-',
        0x0D => '=',
        0x1A => '[',
        0x1B => ']',
        0x2B => '\\',
        0x27 => ';',
        0x28 => '\'',
        0x33 => ',',
        0x34 => '.',
        0x35 => '/',
        _ => return None,
    })
}

/// "Open at login": a per-user LaunchAgent (~/Library/LaunchAgents), no admin rights needed.
pub mod autostart {
    use super::home;
    use std::path::PathBuf;
    use std::{fs, io};

    const LABEL: &str = "io.github.a-elkaim.monitor-input-switcher";

    fn path() -> PathBuf {
        home()
            .join("Library/LaunchAgents")
            .join(format!("{LABEL}.plist"))
    }

    fn xml_escape(s: &str) -> String {
        s.replace('&', "&amp;")
            .replace('<', "&lt;")
            .replace('>', "&gt;")
    }

    fn program() -> String {
        xml_escape(&crate::config::self_exe().display().to_string())
    }

    fn plist() -> String {
        format!(
            r#"<?xml version="1.0" encoding="UTF-8"?>
<!DOCTYPE plist PUBLIC "-//Apple//DTD PLIST 1.0//EN" "http://www.apple.com/DTDs/PropertyList-1.0.dtd">
<plist version="1.0">
<dict>
    <key>Label</key>
    <string>{LABEL}</string>
    <key>ProgramArguments</key>
    <array>
        <string>{}</string>
    </array>
    <key>RunAtLoad</key>
    <true/>
    <key>ProcessType</key>
    <string>Interactive</string>
</dict>
</plist>
"#,
            program()
        )
    }

    pub fn is_enabled() -> bool {
        path().exists()
    }

    /// Takes effect at the next login (launchd reads the folder then); the app is already running.
    pub fn set_enabled(enabled: bool) -> io::Result<()> {
        let path = path();
        if !enabled {
            return match fs::remove_file(&path) {
                Err(e) if e.kind() == io::ErrorKind::NotFound => Ok(()),
                r => r,
            };
        }
        fs::create_dir_all(path.parent().unwrap())?;
        fs::write(&path, plist())
    }

    /// Keeps the program path correct if the app was moved.
    pub fn refresh_path() {
        if let Ok(text) = fs::read_to_string(path())
            && !text.contains(&format!("<string>{}</string>", program()))
        {
            let _ = set_enabled(true);
        }
    }
}

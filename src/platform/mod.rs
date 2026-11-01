//! Everything that differs between Linux and Windows lives behind this module. Each OS module
//! provides the same items:
//!
//! - `Ui`: tray icon + menu, notifications, and global shortcuts
//!   (`start`, `update`, `notify`, `register_shortcuts`, `suspend_shortcuts`, `shutdown`)
//! - `block_shortcuts_for_capture`: lets the Settings window record a combination that is in use
//! - `ddc`: `list_displays`, `open`, `is_device`, and `Bus` (`get_vcp`, `set_vcp`, `capabilities`)
//! - `ipc`: `instance_lock`, `connect`, `listen`, `cleanup` (single instance + CLI transport)
//! - `config_dir`, `autostart`, `META_NAME`, `START_WITH_LOGIN`, `attach_console`

use crate::hotkey::Hotkey;

#[cfg(target_os = "linux")]
mod linux;
#[cfg(target_os = "linux")]
pub use linux::*;

#[cfg(windows)]
mod windows;
#[cfg(windows)]
pub use windows::*;

/// What the tray menu shows.
#[derive(Clone, Debug, Default)]
pub struct TrayModel {
    pub header: String,
    /// (name, shortcut, checked = current input)
    pub inputs: Vec<(String, Option<Hotkey>, bool)>,
    pub toggle: Option<Hotkey>,
    /// (name, shortcut, checked = the profile the app applied last)
    pub profiles: Vec<(String, Option<Hotkey>, bool)>,
    pub autostart: bool,
}

/// Something the user did in the tray menu or with a global shortcut.
#[derive(Debug)]
pub enum UiEvent {
    Switch(usize),
    Next,
    /// Apply a picture profile (index into the config's profiles).
    Profile(usize),
    MenuOpened,
    SetAutostart(bool),
    OpenSettings,
    Quit,
    /// A global shortcut was pressed (action id).
    Hotkey(String),
    /// The user changed a shortcut outside the app (KDE System Settings).
    #[cfg_attr(windows, allow(dead_code))]
    HotkeyChanged(String, Option<Hotkey>),
}

/// A global shortcut the app owns.
#[derive(Clone, Debug)]
pub struct ShortcutAction {
    pub id: String,
    pub friendly: String,
    pub hotkey: Option<Hotkey>,
    /// The action switches the input, which can move the keyboard to the other computer (the
    /// monitor's KVM): Windows then releases the held modifiers first. Other shortcuts (brightness,
    /// picture) leave them alone, so the key can be tapped again while they're held.
    #[cfg_attr(not(windows), allow(dead_code))]
    pub moves_keyboard: bool,
}

pub const ICON_PNG: &[u8] = include_bytes!("../../assets/icon-256.png");

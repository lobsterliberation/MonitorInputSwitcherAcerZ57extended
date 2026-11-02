//! Menu bar item (tray-icon), global hotkeys (global-hotkey: Carbon RegisterEventHotKey, so no
//! Accessibility permission is needed) and notifications (osascript).
//!
//! AppKit objects must be created and used on the main thread, and NSApplication must run there.
//! So `run_daemon` keeps the main thread for NSApplication and runs the app's event loop on another
//! thread; `Ui` talks to the main thread by queueing closures on the main dispatch queue, which the
//! running NSApplication services (also while the menu is open).

use super::keys;
use crate::config::APP_NAME;
use crate::hotkey::Hotkey;
use crate::platform::{ICON_PNG, START_WITH_LOGIN, ShortcutAction, TrayModel, UiEvent};
use dispatch2::DispatchQueue;
use global_hotkey::hotkey::HotKey;
use global_hotkey::{GlobalHotKeyEvent, GlobalHotKeyManager, HotKeyState};
use objc2::MainThreadMarker;
use objc2_app_kit::{NSApplication, NSApplicationActivationPolicy};
use std::cell::RefCell;
use std::process::{Command, Stdio};
use std::sync::mpsc::{self, Sender};
use std::time::Duration;
use tray_icon::menu::accelerator::Accelerator;
use tray_icon::menu::{CheckMenuItem, Menu, MenuEvent, MenuItem, PredefinedMenuItem, Submenu};
use tray_icon::{Icon, MouseButtonState, TrayIcon, TrayIconBuilder, TrayIconEvent};

const ID_NEXT: &str = "next";
const ID_SETTINGS: &str = "settings";
const ID_AUTOSTART: &str = "autostart";
const ID_QUIT: &str = "quit";
const INPUT_PREFIX: &str = "input:";
const PROFILE_PREFIX: &str = "profile:";

/// Keeps the main thread for NSApplication (as a menu bar app with no Dock icon) and runs the
/// tray app on another thread. Exits the process with the app's exit code when it returns.
pub fn run_daemon(run: fn() -> i32) -> i32 {
    let Some(mtm) = MainThreadMarker::new() else {
        return run();
    };
    let app = NSApplication::sharedApplication(mtm);
    app.setActivationPolicy(NSApplicationActivationPolicy::Accessory);
    std::thread::Builder::new()
        .name("app".into())
        .spawn(move || {
            let code = run();
            DispatchQueue::main().exec_async(move || std::process::exit(code));
        })
        .expect("spawn app thread");
    app.run();
    0
}

struct State {
    tray: TrayIcon,
    hotkeys: GlobalHotKeyManager,
    tx: Sender<UiEvent>,
    model: TrayModel,
    actions: Vec<ShortcutAction>,
    /// (registered hotkey, action id)
    registered: Vec<(HotKey, String)>,
    suspended: bool,
}

thread_local! {
    static STATE: RefCell<Option<State>> = const { RefCell::new(None) };
}

/// Runs `f` with the main-thread state, unless it is already borrowed (re-entrant call).
fn with_state<R>(f: impl FnOnce(&mut State) -> R) -> Option<R> {
    STATE.with(|cell| cell.try_borrow_mut().ok()?.as_mut().map(f))
}

/// Runs `f` on the main thread, with the state.
fn on_main(f: impl FnOnce(&mut State) + Send + 'static) {
    DispatchQueue::main().exec_async(move || {
        with_state(f);
    });
}

fn tray_icon() -> Option<Icon> {
    let img = image::load_from_memory(ICON_PNG).ok()?;
    let size = 64;
    let rgba = image::imageops::resize(
        &img.to_rgba8(),
        size,
        size,
        image::imageops::FilterType::Lanczos3,
    );
    Icon::from_rgba(rgba.into_raw(), size, size).ok()
}

/// The shortcut shown next to a menu item.
fn accelerator(hk: &Option<Hotkey>) -> Option<Accelerator> {
    let hk = hk.as_ref()?;
    Some(Accelerator::new(keys::modifiers(hk), keys::code(hk)?))
}

/// The menu strips "&" as a mnemonic marker: "&&" keeps one.
fn label(s: &str) -> String {
    s.replace('&', "&&")
}

fn build_menu(m: &TrayModel) -> Menu {
    let menu = Menu::new();
    let _ = menu.append(&MenuItem::with_id("header", label(&m.header), false, None));
    let _ = menu.append(&PredefinedMenuItem::separator());
    for (i, (name, hk, checked)) in m.inputs.iter().enumerate() {
        let _ = menu.append(&CheckMenuItem::with_id(
            format!("{INPUT_PREFIX}{i}"),
            label(name),
            true,
            *checked,
            accelerator(hk),
        ));
    }
    if m.inputs.len() > 1 {
        let _ = menu.append(&MenuItem::with_id(
            ID_NEXT,
            "Next input",
            true,
            accelerator(&m.toggle),
        ));
    }
    if !m.profiles.is_empty() {
        let picture = Submenu::with_id("picture", "Picture", true);
        for (i, (name, hk, checked)) in m.profiles.iter().enumerate() {
            let _ = picture.append(&CheckMenuItem::with_id(
                format!("{PROFILE_PREFIX}{i}"),
                label(name),
                true,
                *checked,
                accelerator(hk),
            ));
        }
        let _ = menu.append(&PredefinedMenuItem::separator());
        let _ = menu.append(&picture);
    }
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(ID_SETTINGS, "Settings...", true, None));
    let _ = menu.append(&CheckMenuItem::with_id(
        ID_AUTOSTART,
        START_WITH_LOGIN,
        true,
        m.autostart,
        None,
    ));
    let _ = menu.append(&PredefinedMenuItem::separator());
    let _ = menu.append(&MenuItem::with_id(
        ID_QUIT,
        format!("Quit {APP_NAME}"),
        true,
        None,
    ));
    menu
}

fn menu_clicked(id: &str) {
    // AppKit flips a check item's tick on click. The ticks must follow the model (the current
    // input, the real autostart state), so rebuild the menu once this click is handled.
    if id == ID_AUTOSTART || id.starts_with(INPUT_PREFIX) || id.starts_with(PROFILE_PREFIX) {
        on_main(|s| s.rebuild_menu());
    }
    with_state(|s| {
        let ev = match id {
            ID_NEXT => UiEvent::Next,
            ID_SETTINGS => UiEvent::OpenSettings,
            ID_QUIT => UiEvent::Quit,
            ID_AUTOSTART => {
                s.model.autostart = !s.model.autostart;
                UiEvent::SetAutostart(s.model.autostart)
            }
            _ => {
                let index = |prefix: &str| id.strip_prefix(prefix).and_then(|i| i.parse().ok());
                if let Some(i) = index(INPUT_PREFIX) {
                    UiEvent::Switch(i)
                } else if let Some(i) = index(PROFILE_PREFIX) {
                    UiEvent::Profile(i)
                } else {
                    return;
                }
            }
        };
        let _ = s.tx.send(ev);
    });
}

fn hotkey_pressed(event: GlobalHotKeyEvent) {
    if event.state != HotKeyState::Pressed {
        return;
    }
    with_state(|s| {
        if let Some((_, action)) = s.registered.iter().find(|(h, _)| h.id() == event.id) {
            let _ = s.tx.send(UiEvent::Hotkey(action.clone()));
        }
    });
}

impl State {
    /// Must run on the main thread.
    fn create(tx: Sender<UiEvent>) -> Result<State, String> {
        let model = TrayModel::default();
        let mut builder = TrayIconBuilder::new()
            .with_tooltip(APP_NAME)
            .with_menu(Box::new(build_menu(&model)))
            .with_menu_on_left_click(true);
        if let Some(icon) = tray_icon() {
            builder = builder.with_icon(icon);
        } else {
            builder = builder.with_title("MIS");
        }
        let tray = builder
            .build()
            .map_err(|e| format!("Could not create the menu bar item: {e}"))?;
        let hotkeys = GlobalHotKeyManager::new()
            .map_err(|e| format!("Global shortcuts are unavailable: {e}"))?;

        MenuEvent::set_event_handler(Some(|e: MenuEvent| menu_clicked(&e.id.0)));
        GlobalHotKeyEvent::set_event_handler(Some(hotkey_pressed));
        let opened = tx.clone();
        TrayIconEvent::set_event_handler(Some(move |e: TrayIconEvent| {
            if let TrayIconEvent::Click {
                button_state: MouseButtonState::Down,
                ..
            } = e
            {
                let _ = opened.send(UiEvent::MenuOpened);
            }
        }));

        Ok(State {
            tray,
            hotkeys,
            tx,
            model,
            actions: Vec::new(),
            registered: Vec::new(),
            suspended: false,
        })
    }

    fn rebuild_menu(&mut self) {
        self.tray.set_menu(Some(Box::new(build_menu(&self.model))));
    }

    fn set_model(&mut self, model: TrayModel) {
        if model != self.model {
            self.model = model;
            self.rebuild_menu();
        }
    }

    fn unregister_all(&mut self) {
        for (hotkey, _) in self.registered.drain(..) {
            let _ = self.hotkeys.unregister(hotkey);
        }
    }

    /// Registers every action's hotkey; returns the ids that could not be registered.
    fn register(&mut self) -> Vec<String> {
        self.unregister_all();
        if self.suspended {
            return Vec::new();
        }
        let mut failed = Vec::new();
        for action in &self.actions {
            let Some(hk) = &action.hotkey else { continue };
            let Some(code) = keys::code(hk) else {
                failed.push(action.id.clone());
                continue;
            };
            let hotkey = HotKey::new(Some(keys::modifiers(hk)), code);
            let taken = self.registered.iter().any(|(h, _)| h.id() == hotkey.id());
            if taken || self.hotkeys.register(hotkey).is_err() {
                failed.push(action.id.clone());
            } else {
                self.registered.push((hotkey, action.id.clone()));
            }
        }
        failed
    }
}

/// Shows a notification through Notification Center. osascript is the one way that works for an
/// unsigned app without a bundle; macOS lists these under "Script Editor" in its settings.
fn notification(title: &str, body: &str, critical: bool) {
    let sound = if critical {
        r#" sound name "Basso""#
    } else {
        ""
    };
    let script = format!(
        "on run argv\ndisplay notification (item 2 of argv) with title (item 1 of argv){sound}\nend run"
    );
    let child = Command::new("osascript")
        .arg("-e")
        .arg(script)
        .arg(title)
        .arg(body)
        .stdin(Stdio::null())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .spawn();
    match child {
        Ok(mut child) => {
            std::thread::spawn(move || child.wait());
        }
        Err(e) => eprintln!("notification failed ({e}): {title}: {body}"),
    }
}

pub struct Ui {
    _private: (),
}

impl Ui {
    pub fn start(tx: Sender<UiEvent>) -> Result<Ui, String> {
        let (ready_tx, ready_rx) = mpsc::channel();
        DispatchQueue::main().exec_async(move || {
            let result = State::create(tx).map(|state| {
                STATE.with(|cell| *cell.borrow_mut() = Some(state));
            });
            let _ = ready_tx.send(result);
        });
        ready_rx
            .recv_timeout(Duration::from_secs(10))
            .map_err(|_| "The menu bar item did not start".to_string())??;
        Ok(Ui { _private: () })
    }

    pub fn update(&self, model: TrayModel) {
        on_main(move |s| s.set_model(model));
    }

    pub fn notify(&mut self, title: &str, body: &str, critical: bool, _timeout_ms: i32) {
        notification(title, body, critical);
    }

    /// Registers every shortcut. Returns the ids whose keys couldn't be registered.
    pub fn register_shortcuts(
        &mut self,
        actions: &[ShortcutAction],
    ) -> Result<Vec<String>, String> {
        let actions = actions.to_vec();
        let (tx, rx) = mpsc::channel();
        on_main(move |s| {
            s.actions = actions;
            let _ = tx.send(s.register());
        });
        rx.recv_timeout(Duration::from_secs(5))
            .map_err(|_| "The menu bar thread did not answer".to_string())
    }

    /// Releases the hotkeys while the Settings window is open, so it can record them.
    pub fn suspend_shortcuts(&mut self, suspended: bool) {
        on_main(move |s| {
            s.suspended = suspended;
            if suspended {
                s.unregister_all();
            } else {
                s.register();
            }
        });
    }

    pub fn shutdown(self) {
        DispatchQueue::main().exec_sync(|| {
            let state = STATE.with(|cell| cell.borrow_mut().take());
            if let Some(mut state) = state {
                state.unregister_all();
            }
        });
    }
}

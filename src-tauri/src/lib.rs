mod commands;
mod holster;
mod providers;
mod settings;
mod spend;
mod shell;
mod usage;

use std::sync::{Arc, OnceLock};

use tauri::menu::{Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Emitter, Manager, Theme, WindowEvent};
use tauri_plugin_autostart::ManagerExt as _;
use tauri_plugin_global_shortcut::{GlobalShortcutExt, ShortcutState};

use crate::settings::{Prefs, Settings, SettingsStore};
use crate::shell::Shell;
use crate::usage::Poller;

pub struct Core {
    pub prefs: Arc<Prefs>,
    pub poller: Arc<Poller>,
    shell: OnceLock<Arc<Shell>>,
}

impl Core {
    pub fn shell(&self) -> &Arc<Shell> {
        self.shell.get().expect("shell installed")
    }

    /// The shell, if setup got that far.
    pub fn try_shell(&self) -> Option<&Arc<Shell>> {
        self.shell.get()
    }

    /// Opens the panel and refreshes numbers that are getting old.
    pub fn open(&self) {
        self.shell().open();
        self.poller.refresh_if_old();
    }

    /// Side effects of a settings change.
    pub fn apply_settings_change(&self, app: &AppHandle, before: &Settings, after: &Settings) {
        if before.theme != after.theme {
            apply_theme(app, &after.theme);
        }
        if before.launch_at_startup != after.launch_at_startup {
            sync_autostart(app, after.launch_at_startup);
        }
        let providers_changed = before.providers.claude != after.providers.claude
            || before.providers.codex != after.providers.codex
            || before.providers.cursor != after.providers.cursor;
        if providers_changed || before.refresh_minutes != after.refresh_minutes {
            self.poller.apply_settings(after);
        }
    }
}

fn apply_theme(app: &AppHandle, theme: &str) {
    if let Some(w) = app.get_webview_window("main") {
        let _ = w.set_theme(match theme {
            "light" => Some(Theme::Light),
            "dark" => Some(Theme::Dark),
            _ => None,
        });
        apply_material(&w);
    }
}

/// Tints the frosted material for the effective (light/dark) theme.
fn apply_material(w: &tauri::WebviewWindow) {
    let dark = w.theme().map(|t| t == Theme::Dark).unwrap_or(false);
    if let Ok(hwnd) = w.hwnd() {
        if !shell::apply_material(hwnd.0 as isize, dark) {
            let tint = if dark { (28, 26, 26, 160) } else { (244, 244, 246, 160) };
            let _ = window_vibrancy::apply_blur(w, Some(tint));
        }
    }
}

fn sync_autostart(app: &AppHandle, enabled: bool) {
    // Dev builds would register target/debug; only touch autostart in release.
    if cfg!(debug_assertions) {
        return;
    }
    let al = app.autolaunch();
    let _ = if enabled { al.enable() } else { al.disable() };
}

static HOTKEY_ERROR: std::sync::Mutex<Option<String>> = std::sync::Mutex::new(None);

pub fn hotkey_error() -> Option<String> {
    HOTKEY_ERROR.lock().unwrap().clone()
}

pub fn register_hotkey(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    let result = register_hotkey_inner(app, hotkey);
    *HOTKEY_ERROR.lock().unwrap() =
        result.as_ref().err().map(|_| format!("{hotkey} is already used by another app. Record a different shortcut."));
    result
}

fn register_hotkey_inner(app: &AppHandle, hotkey: &str) -> Result<(), String> {
    let gs = app.global_shortcut();
    let _ = gs.unregister_all();
    if hotkey.trim().is_empty() {
        return Ok(());
    }
    gs.on_shortcut(hotkey, |app, _shortcut, event| {
        if event.state == ShortcutState::Pressed {
            if let Some(core) = app.try_state::<Arc<Core>>() {
                toggle(&core);
            }
        }
    })
    .map_err(|e| format!("Couldn't register {hotkey}: {e}"))
}

fn toggle(core: &Core) {
    match core.shell().mode() {
        shell::Mode::Closed | shell::Mode::Closing => core.open(),
        _ => core.shell().close(),
    }
}

fn open_settings(app: &AppHandle) {
    let _ = app.emit("navigate", "settings");
    if let Some(core) = app.try_state::<Arc<Core>>() {
        core.open();
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open Quota", true, None::<&str>)?;
    let settings = MenuItem::with_id(app, "settings", "Settings…", true, None::<&str>)?;
    let quit = MenuItem::with_id(app, "quit", "Quit Quota", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &settings, &PredefinedMenuItem::separator(app)?, &quit])?;
    TrayIconBuilder::with_id("quota")
        .icon(app.default_window_icon().cloned().expect("app icon"))
        .tooltip("Quota")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id.as_ref() {
            "open" => {
                if let Some(core) = app.try_state::<Arc<Core>>() {
                    core.open();
                }
            }
            "settings" => open_settings(app),
            "quit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                if let Some(core) = tray.app_handle().try_state::<Arc<Core>>() {
                    toggle(&core);
                }
            }
        })
        .build(app)?;
    Ok(())
}

#[cfg_attr(mobile, tauri::mobile_entry_point)]
pub fn run() {
    tauri::Builder::default()
        .plugin(tauri_plugin_single_instance::init(|app, args, _cwd| {
            // Launching Quota again opens it; `quota.exe --settings` opens Settings.
            if args.iter().any(|a| a == "--settings") {
                open_settings(app);
            } else if let Some(core) = app.try_state::<Arc<Core>>() {
                core.open();
            }
        }))
        .plugin(tauri_plugin_global_shortcut::Builder::new().build())
        .plugin(tauri_plugin_autostart::init(tauri_plugin_autostart::MacosLauncher::LaunchAgent, Some(vec!["--autostart"])))
        .plugin(tauri_plugin_opener::init())
        .setup(|app| {
            let handle = app.handle().clone();
            let data_dir = app.path().app_data_dir()?;
            std::fs::create_dir_all(&data_dir)?;

            let prefs = Arc::new(Prefs::new(SettingsStore::new(&data_dir)));
            let settings = prefs.get();
            let poller = Poller::new(handle.clone(), prefs.clone(), &data_dir);
            let core = Arc::new(Core { prefs: prefs.clone(), poller: poller.clone(), shell: OnceLock::new() });
            app.manage(core.clone());

            // Window: real acrylic behind a transparent webview. Hidden until opened.
            let window = app.get_webview_window("main").expect("main window");
            apply_theme(&handle, &settings.theme);
            let _ = core.shell.set(Shell::install(&handle, prefs.clone()));
            // Quota lives in Holster's dock: preview, open route, signals.
            holster::start(handle.clone(), &data_dir, &poller.snapshot());
            poller.start();

            let shell_for_events = core.shell().clone();
            let window_for_events = window.clone();
            window.on_window_event(move |event| match event {
                WindowEvent::ThemeChanged(_) => apply_material(&window_for_events),
                WindowEvent::Focused(false) => shell_for_events.on_blur(),
                WindowEvent::CloseRequested { api, .. } => {
                    api.prevent_close();
                    shell_for_events.close();
                }
                _ => {}
            });

            if let Err(e) = register_hotkey(&handle, &settings.hotkey) {
                eprintln!("{e}");
            }
            sync_autostart(&handle, settings.launch_at_startup);
            build_tray(&handle)?;
            Ok(())
        })
        .invoke_handler(tauri::generate_handler![
            commands::shell_state,
            commands::shell_close,
            commands::shell_set_height,
            commands::usage_snapshot,
            commands::usage_refresh,
            commands::get_settings,
            commands::update_settings,
            commands::holster_status,
            commands::app_info,
        ])
        .run(tauri::generate_context!())
        .expect("error while running Quota");
}

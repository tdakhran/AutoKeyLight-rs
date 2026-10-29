// Don't open a console window on Windows
#![windows_subsystem = "windows"]

mod camera;
mod controller;
mod discovery;
mod lights;
mod log;
mod settings;

use std::sync::{mpsc, Arc, Mutex};

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, LogicalSize, Manager, State, WebviewWindow, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use controller::{Event, Shared, Status};
use settings::Settings;

/// Passed when started at login, so the app starts in the tray
const MINIMIZED_ARG: &str = "--minimized";
const TRAY_ICON: &[u8] = include_bytes!("../icons/tray.png");
/// Matches the width in tauri.conf.json, the window only ever changes height
const WINDOW_WIDTH: f64 = 360.0;

#[tauri::command]
fn get_settings(shared: State<'_, Arc<Shared>>) -> Settings {
    shared.settings.lock().unwrap().clone()
}

#[tauri::command]
fn update_settings(shared: State<'_, Arc<Shared>>, settings: Settings) -> Result<Settings, String> {
    settings.save(&shared.settings_path).map_err(|error| error.to_string())?;

    let previous = std::mem::replace(&mut *shared.settings.lock().unwrap(), settings.clone());
    let newly_tracked = previous.untracked_lights.into_iter().filter(|id| settings.is_tracked(id)).collect();
    let _ = shared.events.send(Event::SettingsChanged { newly_tracked });

    Ok(settings)
}

#[tauri::command]
fn get_status(shared: State<'_, Arc<Shared>>) -> Status {
    shared.status.lock().unwrap().clone()
}

#[tauri::command]
fn get_autostart(app: AppHandle) -> bool {
    app.autolaunch().is_enabled().unwrap_or(false)
}

#[tauri::command]
fn set_autostart(app: AppHandle, enabled: bool) -> Result<(), String> {
    let autolaunch = app.autolaunch();
    let result = if enabled { autolaunch.enable() } else { autolaunch.disable() };

    result.map_err(|error| error.to_string())
}

/// Grows or shrinks the window by `difference`, the content height minus the height the page actually has.
/// Measured by the page, because what the window size covers differs between platforms, on macOS it includes
/// the title bar.
#[tauri::command]
fn fit_window(window: WebviewWindow, difference: f64) -> Result<(), String> {
    let scale = window.scale_factor().map_err(|error| error.to_string())?;
    let height = window.inner_size().map_err(|error| error.to_string())?.to_logical::<f64>(scale).height;
    let size = LogicalSize::new(WINDOW_WIDTH, height + difference);

    // macOS ignores resizing a window that isn't resizable, so allow it just for this. Not on Windows, where it
    // changes the frame thickness and with it the size
    if cfg!(target_os = "macos") {
        let _ = window.set_resizable(true);
        let result = window.set_size(size);
        let _ = window.set_resizable(false);
        result.map_err(|error| error.to_string())
    } else {
        window.set_size(size).map_err(|error| error.to_string())
    }
}

fn notify(app: &AppHandle, event: Event) {
    if let Some(shared) = app.try_state::<Arc<Shared>>() {
        let _ = shared.events.send(event);
    }
}

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window(controller::MAIN_WINDOW) {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
        notify(app, Event::WindowShown);
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    let exit = MenuItem::with_id(app, "exit", "Exit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &exit])?;

    TrayIconBuilder::new()
        .icon(Image::from_bytes(TRAY_ICON)?)
        .tooltip("AutoKeyLight")
        .menu(&menu)
        .show_menu_on_left_click(false)
        .on_menu_event(|app, event| match event.id().as_ref() {
            "open" => show_main_window(app),
            "exit" => app.exit(0),
            _ => {}
        })
        .on_tray_icon_event(|tray, event| {
            if let TrayIconEvent::Click { button: MouseButton::Left, button_state: MouseButtonState::Up, .. } = event {
                show_main_window(tray.app_handle());
            }
        })
        .build(app)?;

    Ok(())
}

fn main() {
    tauri::Builder::default()
        // Must be registered first, starting a second copy shows the running one instead
        .plugin(tauri_plugin_single_instance::init(|app, _args, _cwd| show_main_window(app)))
        .plugin(tauri_plugin_autostart::init(MacosLauncher::LaunchAgent, Some(vec![MINIMIZED_ARG])))
        .invoke_handler(tauri::generate_handler![
            get_settings,
            update_settings,
            get_status,
            get_autostart,
            set_autostart,
            fit_window
        ])
        .setup(|app| {
            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let (events, events_receiver) = mpsc::channel();

            let camera_events = events.clone();
            let camera_supported = camera::spawn_monitor(move |cameras| {
                let _ = camera_events.send(Event::Camera(cameras));
            });

            let shared = Arc::new(Shared {
                settings: Mutex::new(Settings::load(&settings_path)),
                status: Mutex::new(Status { camera_supported, ..Status::default() }),
                settings_path,
                events: events.clone(),
            });
            app.manage(shared.clone());

            if let Err(error) = discovery::spawn(events) {
                log::log(format_args!("discovery unavailable: {error}"));
            }

            build_tray(app.handle())?;
            controller::spawn(app.handle().clone(), shared, camera_supported, events_receiver);

            if !std::env::args().any(|arg| arg == MINIMIZED_ARG) {
                show_main_window(app.handle());
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            // Closing the window keeps the app running in the tray
            if let WindowEvent::CloseRequested { api, .. } = event {
                api.prevent_close();
                let _ = window.hide();
                notify(window.app_handle(), Event::WindowHidden);
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running AutoKeyLight");
}

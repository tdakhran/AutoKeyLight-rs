// Don't open a console window on Windows in release builds
#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

mod camera;
mod controller;
mod lights;
mod settings;

use std::sync::{Arc, Mutex};

use tauri::image::Image;
use tauri::menu::{Menu, MenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, State, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};

use controller::{Shared, Status};
use settings::Settings;

/// Passed when started at login, so the app starts in the tray
const MINIMIZED_ARG: &str = "--minimized";

#[tauri::command]
fn get_settings(shared: State<'_, Arc<Shared>>) -> Settings {
    shared.settings.lock().unwrap().clone()
}

#[tauri::command]
fn update_settings(shared: State<'_, Arc<Shared>>, settings: Settings) -> Result<Settings, String> {
    let settings = settings.normalized();
    settings.save(&shared.settings_path).map_err(|error| error.to_string())?;
    *shared.settings.lock().unwrap() = settings.clone();

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

fn show_main_window(app: &AppHandle) {
    if let Some(window) = app.get_webview_window("main") {
        let _ = window.unminimize();
        let _ = window.show();
        let _ = window.set_focus();
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    let exit = MenuItem::with_id(app, "exit", "Exit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &exit])?;

    TrayIconBuilder::with_id(controller::TRAY_ID)
        .icon(Image::from_bytes(controller::TRAY_ICON_UNLIT)?)
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
        .invoke_handler(tauri::generate_handler![get_settings, update_settings, get_status, get_autostart, set_autostart])
        .setup(|app| {
            let settings_path = app.path().app_config_dir()?.join("settings.json");
            let camera = camera::spawn_monitor();

            let shared = Arc::new(Shared {
                settings: Mutex::new(Settings::load(&settings_path)),
                status: Mutex::new(Status { camera_supported: camera.is_some(), ..Status::default() }),
                settings_path,
            });
            app.manage(shared.clone());

            build_tray(app.handle())?;
            controller::spawn(app.handle().clone(), shared, camera);

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
            }
        })
        .run(tauri::generate_context!())
        .expect("error while running AutoKeyLight");
}

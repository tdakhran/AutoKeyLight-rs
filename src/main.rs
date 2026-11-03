// Don't open a console window on Windows
#![windows_subsystem = "windows"]

mod calendar;
mod camera;
mod controller;
mod discovery;
mod lights;
mod lock;
mod log;
mod settings;

use std::sync::{mpsc, Arc, Mutex};

use tauri::image::Image;
use serde::Serialize;
use tauri::menu::{AboutMetadata, Menu, MenuItem, PredefinedMenuItem};
use tauri::tray::{MouseButton, MouseButtonState, TrayIconBuilder, TrayIconEvent};
use tauri::{AppHandle, Manager, RunEvent, State, WebviewWindowBuilder, WindowEvent};
use tauri_plugin_autostart::{MacosLauncher, ManagerExt};
use tauri_plugin_opener::OpenerExt;

use controller::{Event, Shared, Status};
use settings::Settings;

/// Passed when started at login, so the app starts in the tray
const MINIMIZED_ARG: &str = "--minimized";
const TRAY_ICON: &[u8] = include_bytes!("../icons/tray.png");
// Version, author and repository all come from Cargo.toml
const VERSION: &str = env!("CARGO_PKG_VERSION");
const REPOSITORY: &str = env!("CARGO_PKG_REPOSITORY");
const COPYRIGHT: &str = "© 2026 Tarek Dakhran";

/// Author names without email addresses
fn authors() -> Vec<String> {
    env!("CARGO_PKG_AUTHORS")
        .split(':')
        .map(|author| author.split('<').next().unwrap_or(author).trim().to_string())
        .collect()
}

#[derive(Serialize)]
struct About {
    version: &'static str,
    repository: &'static str,
}

#[tauri::command]
fn get_about() -> About {
    About { version: VERSION, repository: REPOSITORY }
}

#[tauri::command]
fn open_repository(app: AppHandle) -> Result<(), String> {
    app.opener().open_url(REPOSITORY, None::<&str>).map_err(|error| error.to_string())
}

#[tauri::command]
fn get_settings(shared: State<'_, Arc<Shared>>) -> Settings {
    shared.settings.lock().unwrap().clone()
}

#[tauri::command]
fn update_settings(shared: State<'_, Arc<Shared>>, settings: Settings) -> Result<Settings, String> {
    let settings = settings.clamped();
    settings.save(&shared.settings_path).map_err(|error| error.to_string())?;

    let previous = std::mem::replace(&mut *shared.settings.lock().unwrap(), settings.clone());
    let newly_tracked = previous.untracked_lights.into_iter().filter(|id| settings.is_tracked(id)).collect();
    let _ = shared.events.send(Event::SettingsChanged { newly_tracked });

    Ok(settings)
}

/// Names of the connected cameras, for choosing which ones count
#[tauri::command]
async fn list_cameras() -> Vec<String> {
    camera::connected_cameras()
}

/// Blinks the tracked lights now, to try the meeting reminder
#[tauri::command]
fn flash_lights(shared: State<'_, Arc<Shared>>) {
    let _ = shared.events.send(Event::FlashNow);
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

fn notify(app: &AppHandle, event: Event) {
    if let Some(shared) = app.try_state::<Arc<Shared>>() {
        let _ = shared.events.send(event);
    }
}

/// Shows the settings window, creating it if it's closed. It's destroyed when closed, so the web view doesn't keep
/// running while the app is in the tray.
fn show_main_window(app: &AppHandle) {
    let window = match app.get_webview_window(controller::MAIN_WINDOW) {
        Some(window) => window,
        None => {
            let Some(config) = app.config().app.windows.iter().find(|window| window.label == controller::MAIN_WINDOW)
            else {
                return;
            };

            // Before the web view's processes start, so they don't inherit the low priority
            #[cfg(windows)]
            set_efficiency_mode(false);

            match WebviewWindowBuilder::from_config(app, config).and_then(|builder| builder.build()) {
                Ok(window) => window,
                Err(error) => {
                    log::log(format_args!("window: couldn't create it: {error}"));
                    return;
                }
            }
        }
    };

    let _ = window.unminimize();
    let _ = window.show();
    let _ = window.set_focus();
    notify(app, Event::WindowShown);
}

/// Windows 11's efficiency mode while the app waits in the tray: throttled to efficient cores (EcoQoS) at low
/// priority, like Task Manager's own toggle. Turned off while the window is open, processes started meanwhile inherit
/// the priority, the web view's included, and the window would feel sluggish when the CPU is busy.
#[cfg(windows)]
fn set_efficiency_mode(enabled: bool) {
    use std::ffi::c_void;
    use windows::Win32::System::Threading::{
        GetCurrentProcess, ProcessPowerThrottling, SetPriorityClass, SetProcessInformation, IDLE_PRIORITY_CLASS,
        NORMAL_PRIORITY_CLASS, PROCESS_POWER_THROTTLING_CURRENT_VERSION, PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        PROCESS_POWER_THROTTLING_STATE,
    };

    let state = PROCESS_POWER_THROTTLING_STATE {
        Version: PROCESS_POWER_THROTTLING_CURRENT_VERSION,
        ControlMask: PROCESS_POWER_THROTTLING_EXECUTION_SPEED,
        StateMask: if enabled { PROCESS_POWER_THROTTLING_EXECUTION_SPEED } else { 0 },
    };

    unsafe {
        let throttling = SetProcessInformation(
            GetCurrentProcess(),
            ProcessPowerThrottling,
            (&state as *const PROCESS_POWER_THROTTLING_STATE).cast::<c_void>(),
            std::mem::size_of::<PROCESS_POWER_THROTTLING_STATE>() as u32,
        );
        let priority =
            SetPriorityClass(GetCurrentProcess(), if enabled { IDLE_PRIORITY_CLASS } else { NORMAL_PRIORITY_CLASS });
        log::log(format_args!("efficiency mode {enabled}: throttling {throttling:?}, priority {priority:?}"));
    }
}

fn build_tray(app: &AppHandle) -> tauri::Result<()> {
    let open = MenuItem::with_id(app, "open", "Open", true, None::<&str>)?;
    let about = PredefinedMenuItem::about(
        app,
        Some("About"),
        Some(AboutMetadata {
            name: Some("AutoKeyLight".into()),
            version: Some(VERSION.into()),
            // macOS adds the bundle version in parentheses unless there's a separate one, an empty one hides it.
            // Elsewhere it's appended to the version as is
            short_version: cfg!(target_os = "macos").then(String::new),
            authors: Some(authors()),
            comments: Some(env!("CARGO_PKG_DESCRIPTION").into()),
            copyright: Some(COPYRIGHT.into()),
            license: Some(env!("CARGO_PKG_LICENSE").into()),
            website: Some(REPOSITORY.into()),
            website_label: Some("GitHub".into()),
            // macOS shows credits instead of the website, as plain text
            credits: Some(REPOSITORY.into()),
            icon: Image::from_bytes(TRAY_ICON).ok(),
            ..Default::default()
        }),
    )?;
    let exit = MenuItem::with_id(app, "exit", "Exit", true, None::<&str>)?;
    let menu = Menu::with_items(app, &[&open, &about, &PredefinedMenuItem::separator(app)?, &exit])?;

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
        .plugin(tauri_plugin_opener::init())
        .invoke_handler(tauri::generate_handler![
            get_settings,
            update_settings,
            get_status,
            get_autostart,
            set_autostart,
            get_about,
            open_repository,
            flash_lights,
            list_cameras
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

            build_tray(app.handle())?;
            controller::spawn(app.handle().clone(), shared, camera_supported, events_receiver);

            #[cfg(windows)]
            set_efficiency_mode(true);

            if !std::env::args().any(|arg| arg == MINIMIZED_ARG) {
                show_main_window(app.handle());
            }

            Ok(())
        })
        .on_window_event(|window, event| {
            if let WindowEvent::Destroyed = event {
                notify(window.app_handle(), Event::WindowHidden);

                // Back in the tray
                #[cfg(windows)]
                set_efficiency_mode(true);
            }
        })
        .build(tauri::generate_context!())
        .expect("error while building AutoKeyLight")
        .run(|_app, event| {
            // Closing the window closes the last one, the app keeps running in the tray. Exit from the tray menu
            // exits with a code and isn't stopped
            if let RunEvent::ExitRequested { api, code: None, .. } = event {
                api.prevent_exit();
            }
        });
}

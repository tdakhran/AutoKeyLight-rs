//! Background loop that follows the camera state and switches the lights.

use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::image::Image;
use tauri::{AppHandle, Emitter, Manager};

use crate::lights::Lights;
use crate::settings::Settings;

pub const MAIN_WINDOW: &str = "main";
pub const TRAY_ID: &str = "main";
pub const TRAY_ICON_LIT: &[u8] = include_bytes!("../icons/tray-lit.png");
pub const TRAY_ICON_UNLIT: &[u8] = include_bytes!("../icons/tray-unlit.png");

const TICK: Duration = Duration::from_secs(1);

#[derive(Clone, Copy, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LightState {
    /// Not checked, lights are only queried while they're needed
    Unknown,
    On,
    Off,
    Unreachable,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LightStatus {
    pub address: String,
    pub state: LightState,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub camera_supported: bool,
    pub camera_on: bool,
    pub lights: Vec<LightStatus>,
}

pub struct Shared {
    pub settings: Mutex<Settings>,
    pub status: Mutex<Status>,
    pub settings_path: PathBuf,
}

pub fn spawn(app: AppHandle, shared: Arc<Shared>, camera: Option<Arc<AtomicBool>>) {
    thread::Builder::new()
        .name("controller".into())
        .spawn(move || run(app, shared, camera))
        .expect("failed to spawn controller thread");
}

fn run(app: AppHandle, shared: Arc<Shared>, camera: Option<Arc<AtomicBool>>) {
    let lights = Lights::new();
    let mut is_camera_on = false;
    let mut camera_changed_at = Instant::now();
    // Camera state the lights were last switched for, starts as off so the lights are only touched once the camera turns on
    let mut lights_camera_state = false;

    loop {
        let settings = shared.settings.lock().unwrap().clone();
        let camera_on = camera.as_ref().is_some_and(|streaming| streaming.load(Ordering::Relaxed));

        if camera_on != is_camera_on {
            is_camera_on = camera_on;
            camera_changed_at = Instant::now();
            update_tray(&app, is_camera_on);
        }

        // Light states are only needed to show them in the window, and for forced mode to find the lights that don't match
        let light_states = (is_window_open(&app) || !settings.allow_manual_override)
            .then(|| lights.are_on(&settings.lights));

        let delay_ms = if is_camera_on { settings.camera_on_delay_ms } else { settings.camera_off_delay_ms };

        if camera_changed_at.elapsed() >= Duration::from_millis(delay_ms) {
            if settings.allow_manual_override {
                // Only switch when the camera changed state, if a light didn't respond it's retried on the next tick
                if is_camera_on != lights_camera_state && lights.set_all_on(&settings.lights, is_camera_on) {
                    lights_camera_state = is_camera_on;
                }
            } else if let Some(light_states) = &light_states {
                // Force the lights to follow the camera, only sending requests to the ones that don't match
                let mismatched: Vec<String> = settings
                    .lights
                    .iter()
                    .zip(light_states)
                    .filter(|(_, on)| **on != Some(is_camera_on))
                    .map(|(address, _)| address.clone())
                    .collect();

                lights.set_all_on(&mismatched, is_camera_on);
                lights_camera_state = is_camera_on;
            }
        }

        let states: Vec<LightState> = match light_states {
            Some(light_states) => light_states
                .into_iter()
                .map(|on| match on {
                    Some(true) => LightState::On,
                    Some(false) => LightState::Off,
                    None => LightState::Unreachable,
                })
                .collect(),
            None => vec![LightState::Unknown; settings.lights.len()],
        };

        let status = Status {
            camera_supported: camera.is_some(),
            camera_on: is_camera_on,
            lights: settings
                .lights
                .iter()
                .zip(states)
                .map(|(address, state)| LightStatus { address: address.clone(), state })
                .collect(),
        };

        *shared.status.lock().unwrap() = status.clone();
        let _ = app.emit("status", &status);

        thread::sleep(TICK);
    }
}

fn is_window_open(app: &AppHandle) -> bool {
    app.get_webview_window(MAIN_WINDOW).is_some_and(|window| {
        window.is_visible().unwrap_or(false) && !window.is_minimized().unwrap_or(false)
    })
}

fn update_tray(app: &AppHandle, camera_on: bool) {
    let Some(tray) = app.tray_by_id(TRAY_ID) else {
        return;
    };

    let icon = if camera_on { TRAY_ICON_LIT } else { TRAY_ICON_UNLIT };
    if let Ok(image) = Image::from_bytes(icon) {
        let _ = tray.set_icon(Some(image));
    }

    let tooltip = if camera_on { "AutoKeyLight: camera is on" } else { "AutoKeyLight: camera is off" };
    let _ = tray.set_tooltip(Some(tooltip));
}

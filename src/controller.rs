//! Event driven loop that follows the camera state and switches the lights.
//!
//! The loop sleeps until something happens: the camera turns on or off, the window is shown or hidden,
//! the settings change, or a deadline passes (an on/off delay, or refreshing the light states while they're needed).
//! With manual override on and the window closed, it does nothing until the camera changes.

use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::image::Image;
use tauri::{AppHandle, Emitter};

use crate::lights::Lights;
use crate::settings::Settings;

pub const MAIN_WINDOW: &str = "main";
pub const TRAY_ID: &str = "main";
pub const TRAY_ICON_LIT: &[u8] = include_bytes!("../icons/tray-lit.png");
pub const TRAY_ICON_UNLIT: &[u8] = include_bytes!("../icons/tray-unlit.png");

/// How often light states are refreshed while they're needed
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const RETRY_DELAY: Duration = Duration::from_secs(1);

pub enum Event {
    /// Whether any camera is streaming, sent when it changes
    Camera(bool),
    WindowShown,
    WindowHidden,
    SettingsChanged,
}

#[derive(Clone, Copy, PartialEq, Serialize)]
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
    pub events: Sender<Event>,
}

pub fn spawn(app: AppHandle, shared: Arc<Shared>, camera_supported: bool, events: Receiver<Event>) {
    let controller = Controller {
        app,
        shared,
        lights: Lights::new(),
        camera_supported,
        camera_on: false,
        camera_changed_at: Instant::now(),
        lights_camera_state: false,
        window_open: false,
        next_refresh: Instant::now(),
        light_states: Vec::new(),
    };

    thread::Builder::new()
        .name("controller".into())
        .spawn(move || controller.run(events))
        .expect("failed to spawn controller thread");
}

struct Controller {
    app: AppHandle,
    shared: Arc<Shared>,
    lights: Lights,
    camera_supported: bool,
    camera_on: bool,
    camera_changed_at: Instant,
    /// Camera state the lights were last switched for, starts as off so the lights are only touched once the camera turns on
    lights_camera_state: bool,
    window_open: bool,
    next_refresh: Instant,
    /// Last queried light states
    light_states: Vec<LightStatus>,
}

impl Controller {
    fn run(mut self, events: Receiver<Event>) {
        loop {
            let settings = self.shared.settings.lock().unwrap().clone();
            self.update(&settings);

            let event = match self.next_deadline(&settings) {
                Some(deadline) => match events.recv_timeout(deadline.saturating_duration_since(Instant::now())) {
                    Ok(event) => Some(event),
                    Err(RecvTimeoutError::Timeout) => None,
                    Err(RecvTimeoutError::Disconnected) => return,
                },
                None => match events.recv() {
                    Ok(event) => Some(event),
                    Err(_) => return,
                },
            };

            if let Some(event) = event {
                self.handle(event);
            }
        }
    }

    fn handle(&mut self, event: Event) {
        match event {
            Event::Camera(on) => {
                if on != self.camera_on {
                    self.camera_on = on;
                    self.camera_changed_at = Instant::now();
                    update_tray(&self.app, on);
                }
            }
            Event::WindowShown => {
                self.window_open = true;
                self.next_refresh = Instant::now();
            }
            Event::WindowHidden => self.window_open = false,
            // Query right away so added lights show up
            Event::SettingsChanged => self.next_refresh = Instant::now(),
        }
    }

    /// Light states are only needed to show them in the window, and for forced mode to find the lights that don't match
    fn needs_light_states(&self, settings: &Settings) -> bool {
        self.window_open || !settings.allow_manual_override
    }

    /// When the lights should be switched to the camera state, if they haven't been yet
    fn switch_deadline(&self, settings: &Settings) -> Option<Instant> {
        let delay_ms = if self.camera_on { settings.camera_on_delay_ms } else { settings.camera_off_delay_ms };

        (self.camera_on != self.lights_camera_state)
            .then(|| self.camera_changed_at + Duration::from_millis(delay_ms))
    }

    fn next_deadline(&self, settings: &Settings) -> Option<Instant> {
        let refresh = self.needs_light_states(settings).then_some(self.next_refresh);

        [self.switch_deadline(settings), refresh].into_iter().flatten().min()
    }

    fn update(&mut self, settings: &Settings) {
        if self.switch_deadline(settings).is_some_and(|deadline| Instant::now() >= deadline) {
            let failed = self.lights.set_all_on(&settings.lights, self.camera_on);

            // Retry once, only the lights that didn't respond, then leave them until the camera changes again
            if !failed.is_empty() {
                thread::sleep(RETRY_DELAY);
                self.lights.set_all_on(&failed, self.camera_on);
            }

            self.lights_camera_state = self.camera_on;
        }

        if self.needs_light_states(settings) && Instant::now() >= self.next_refresh {
            self.refresh_light_states(settings);
            self.next_refresh = Instant::now() + REFRESH_INTERVAL;
        }

        let status = Status {
            camera_supported: self.camera_supported,
            camera_on: self.camera_on,
            lights: settings
                .lights
                .iter()
                .map(|address| LightStatus { address: address.clone(), state: self.light_state(address) })
                .collect(),
        };

        *self.shared.status.lock().unwrap() = status.clone();

        if self.window_open {
            let _ = self.app.emit("status", &status);
        }
    }

    fn refresh_light_states(&mut self, settings: &Settings) {
        let states = self.lights.are_on(&settings.lights);

        // Forced mode: switch back lights that don't match the camera, unless a delay is still running
        if !settings.allow_manual_override && self.camera_on == self.lights_camera_state {
            let mismatched: Vec<String> = settings
                .lights
                .iter()
                .zip(&states)
                .filter(|(_, on)| **on != Some(self.camera_on))
                .map(|(address, _)| address.clone())
                .collect();

            self.lights.set_all_on(&mismatched, self.camera_on);
        }

        self.light_states = settings
            .lights
            .iter()
            .zip(states)
            .map(|(address, on)| LightStatus {
                address: address.clone(),
                state: match on {
                    Some(true) => LightState::On,
                    Some(false) => LightState::Off,
                    None => LightState::Unreachable,
                },
            })
            .collect();
    }

    fn light_state(&self, address: &str) -> LightState {
        self.light_states
            .iter()
            .find(|light| light.address == address)
            .map_or(LightState::Unknown, |light| light.state)
    }
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

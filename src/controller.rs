//! Event driven loop that follows the camera state and switches the lights.
//!
//! The loop sleeps until something happens: the camera turns on or off, a light is found or lost on the network,
//! the window is shown or hidden, the settings change, light states come back from a refresh, or a deadline passes
//! (an on/off delay, a retry, or refreshing the light states while the window is open). With the window closed,
//! it does nothing until the camera changes. Light states are queried on a separate thread, so a slow light never
//! holds up the loop.

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::discovery::DiscoveredLight;
use crate::lights::Lights;
use crate::log::log;
use crate::settings::Settings;

pub const MAIN_WINDOW: &str = "main";

/// How often light states are refreshed while the window is open
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const RETRY_DELAY: Duration = Duration::from_secs(1);

pub enum Event {
    /// Names of the cameras that are streaming, empty when none are, sent when it changes
    Camera(Vec<String>),
    LightFound(DiscoveredLight),
    /// mDNS instance name of a light that left the network
    LightLost(String),
    WindowShown,
    WindowHidden,
    /// Ids of lights that were just tracked, they're switched to match the other tracked lights right away
    SettingsChanged { newly_tracked: Vec<String> },
    /// Result of a background refresh, whether each light is on by id, `None` if it couldn't be reached
    LightsRefreshed(Vec<(String, Option<bool>)>),
}

#[derive(Clone, Copy, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub enum LightState {
    /// Not checked, lights are only queried while the window is open
    Unknown,
    On,
    Off,
    Unreachable,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct LightStatus {
    pub id: String,
    pub name: String,
    pub address: String,
    pub state: LightState,
    pub tracked: bool,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct Status {
    pub camera_supported: bool,
    pub camera_on: bool,
    /// Names of the cameras that are streaming
    pub cameras: Vec<String>,
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
        found: Vec::new(),
        camera_supported,
        camera_on: false,
        cameras: Vec::new(),
        camera_changed_at: Instant::now(),
        lights_camera_state: false,
        retry: None,
        window_open: false,
        refreshing: false,
        next_refresh: Instant::now(),
        light_states: HashMap::new(),
    };

    thread::Builder::new()
        .name("controller".into())
        .spawn(move || controller.run(events))
        .expect("failed to spawn controller thread");
}

/// Lights that didn't respond to a switch, retried once
struct Retry {
    at: Instant,
    addresses: Vec<String>,
}

struct Controller {
    app: AppHandle,
    shared: Arc<Shared>,
    lights: Lights,
    /// Lights found on the network, sorted by name
    found: Vec<DiscoveredLight>,
    camera_supported: bool,
    camera_on: bool,
    cameras: Vec<String>,
    camera_changed_at: Instant,
    /// Camera state the lights were last switched for, starts as off so the lights are only touched once the camera turns on
    lights_camera_state: bool,
    retry: Option<Retry>,
    window_open: bool,
    /// Whether a background refresh of the light states is running
    refreshing: bool,
    next_refresh: Instant,
    /// Last queried light states by id
    light_states: HashMap<String, LightState>,
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
            Event::Camera(cameras) => {
                log(format_args!("controller: camera event {cameras:?}"));
                let on = !cameras.is_empty();
                self.cameras = cameras;

                if on != self.camera_on {
                    self.camera_on = on;
                    self.camera_changed_at = Instant::now();
                    // A pending retry was for the previous camera state
                    self.retry = None;
                }
            }
            Event::LightFound(light) => {
                match self.found.iter_mut().find(|found| found.id == light.id) {
                    Some(found) => *found = light,
                    None => self.found.push(light),
                }
                self.found.sort_by(|a, b| a.name.cmp(&b.name));
                self.next_refresh = Instant::now();
            }
            Event::LightLost(fullname) => self.found.retain(|light| light.fullname != fullname),
            Event::WindowShown => {
                self.window_open = true;
                self.next_refresh = Instant::now();
            }
            Event::WindowHidden => self.window_open = false,
            Event::SettingsChanged { newly_tracked } => {
                let addresses: Vec<String> = self
                    .found
                    .iter()
                    .filter(|light| newly_tracked.contains(&light.id))
                    .map(|light| light.address.clone())
                    .collect();

                if !addresses.is_empty() {
                    log(format_args!("controller: tracking {} lights on={}", addresses.len(), self.lights_camera_state));
                    self.lights.set_all_on(&addresses, self.lights_camera_state);
                    self.next_refresh = Instant::now();
                }
            }
            Event::LightsRefreshed(states) => {
                self.refreshing = false;
                self.next_refresh = Instant::now() + REFRESH_INTERVAL;
                self.light_states = states
                    .into_iter()
                    .map(|(id, on)| {
                        let state = match on {
                            Some(true) => LightState::On,
                            Some(false) => LightState::Off,
                            None => LightState::Unreachable,
                        };
                        (id, state)
                    })
                    .collect();
            }
        }
    }

    /// When the lights should be switched to the camera state, if they haven't been yet
    fn switch_deadline(&self, settings: &Settings) -> Option<Instant> {
        let delay_ms = if self.camera_on { settings.camera_on_delay_ms } else { settings.camera_off_delay_ms };

        (self.camera_on != self.lights_camera_state)
            .then(|| self.camera_changed_at + Duration::from_millis(delay_ms))
    }

    fn next_deadline(&self, settings: &Settings) -> Option<Instant> {
        // Light states are only needed to show them in the window
        let refresh = (self.window_open && !self.refreshing).then_some(self.next_refresh);
        let retry = self.retry.as_ref().map(|retry| retry.at);

        [self.switch_deadline(settings), retry, refresh].into_iter().flatten().min()
    }

    fn update(&mut self, settings: &Settings) {
        // Show camera changes right away, before talking to the lights
        self.publish_status(settings);

        if self.switch_deadline(settings).is_some_and(|deadline| Instant::now() >= deadline) {
            let tracked: Vec<String> = self
                .found
                .iter()
                .filter(|light| settings.is_tracked(&light.id))
                .map(|light| light.address.clone())
                .collect();

            log(format_args!("controller: switching {} lights on={}", tracked.len(), self.camera_on));
            let failed = self.lights.set_all_on(&tracked, self.camera_on);
            self.lights_camera_state = self.camera_on;

            // Retry once, only the lights that didn't respond, then leave them until the camera changes again
            self.retry = (!failed.is_empty()).then(|| Retry { at: Instant::now() + RETRY_DELAY, addresses: failed });

            // Show the new light states right away
            self.next_refresh = Instant::now();
        } else if let Some(retry) = self.retry.take_if(|retry| Instant::now() >= retry.at) {
            log(format_args!("controller: retrying {} lights", retry.addresses.len()));
            self.lights.set_all_on(&retry.addresses, self.camera_on);
            self.next_refresh = Instant::now();
        }

        if self.window_open && !self.refreshing && Instant::now() >= self.next_refresh {
            self.start_refresh();
        }
    }

    fn publish_status(&self, settings: &Settings) {
        let status = Status {
            camera_supported: self.camera_supported,
            camera_on: self.camera_on,
            cameras: self.cameras.clone(),
            lights: self
                .found
                .iter()
                .map(|light| LightStatus {
                    id: light.id.clone(),
                    name: light.name.clone(),
                    address: light.address.clone(),
                    state: self.light_states.get(&light.id).copied().unwrap_or(LightState::Unknown),
                    tracked: settings.is_tracked(&light.id),
                })
                .collect(),
        };

        *self.shared.status.lock().unwrap() = status.clone();

        if self.window_open {
            let _ = self.app.emit("status", &status);
        }
    }

    /// Queries all found lights on a separate thread, the result comes back as `Event::LightsRefreshed`.
    fn start_refresh(&mut self) {
        self.refreshing = true;

        let lights = self.lights.clone();
        let events = self.shared.events.clone();
        let (ids, addresses): (Vec<String>, Vec<String>) =
            self.found.iter().map(|light| (light.id.clone(), light.address.clone())).unzip();

        let spawned = thread::Builder::new().name("light-refresh".into()).spawn(move || {
            let started = Instant::now();
            let states = lights.are_on(&addresses);
            log(format_args!("refresh: {} lights took {}ms", addresses.len(), started.elapsed().as_millis()));

            let _ = events.send(Event::LightsRefreshed(ids.into_iter().zip(states).collect()));
        });

        if spawned.is_err() {
            self.refreshing = false;
        }
    }
}

//! Event driven loop that follows the camera state and switches the lights.
//!
//! The loop sleeps until something happens: the camera turns on or off, a light is found or lost on the network,
//! the window is shown or hidden, the settings change, light states come back from a refresh, or a deadline passes
//! (an on/off delay, a retry, or refreshing the light states while the window is open). With the window closed,
//! it does nothing until the camera changes. Light states are queried on a separate thread, so a slow light never
//! holds up the loop.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use chrono::Utc;
use serde::Serialize;
use tauri::{AppHandle, Emitter};

use crate::calendar::{self, Meeting};
use crate::discovery::DiscoveredLight;
use crate::lights::Lights;
use crate::log::log;
use crate::settings::Settings;

pub const MAIN_WINDOW: &str = "main";

/// How often light states are refreshed while the window is open
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const RETRY_DELAY: Duration = Duration::from_secs(1);
/// How soon the calendar is loaded again after it failed to, unless the refresh period is shorter
const CALENDAR_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// A meeting is still reminded of this long after its start, for example right after waking from sleep
const REMINDER_GRACE: chrono::Duration = chrono::Duration::minutes(2);

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
    /// Result of loading the calendar from `url`
    CalendarLoaded { url: String, result: Result<Vec<Meeting>, String> },
    /// Blink the lights now, from the Test button
    FlashNow,
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
    pub calendar: CalendarStatus,
}

#[derive(Clone, Default, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct CalendarStatus {
    pub loading: bool,
    pub error: Option<String>,
    /// Number of meetings found in the next day, once the calendar loaded
    pub upcoming: Option<usize>,
    pub next_meeting: Option<NextMeeting>,
}

#[derive(Clone, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct NextMeeting {
    pub summary: String,
    /// RFC 3339, the window shows it in local time
    pub start: String,
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
        calendar_url: String::new(),
        calendar_loading: false,
        calendar_last_load: None,
        calendar_error: None,
        meetings: None,
        next_calendar_load: Instant::now(),
        reminded: HashSet::new(),
        flash_requested: false,
        flash_cancel: None,
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
    /// Calendar link the meetings below are from
    calendar_url: String,
    calendar_loading: bool,
    /// When the calendar last finished loading, the next load is scheduled from it
    calendar_last_load: Option<Instant>,
    calendar_error: Option<String>,
    /// Meetings in the next day, `None` until the calendar loaded
    meetings: Option<Vec<Meeting>>,
    next_calendar_load: Instant,
    /// Meeting occurrences the lights already blinked for
    reminded: HashSet<String>,
    flash_requested: bool,
    /// Set to stop the blinking that's running
    flash_cancel: Option<Arc<AtomicBool>>,
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
                    // The camera wins over a reminder, restoring the lights afterwards would undo the switch
                    self.cancel_flash();
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

                // Only while the camera trigger is on, otherwise there's no camera state to match
                let camera_enabled = self.shared.settings.lock().unwrap().camera_enabled;
                if camera_enabled && !addresses.is_empty() {
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
            Event::CalendarLoaded { url, result } => {
                // Ignore results for a link that was changed meanwhile
                if url != self.calendar_url {
                    return;
                }

                self.calendar_loading = false;
                self.calendar_last_load = Some(Instant::now());
                match result {
                    Ok(meetings) => {
                        log(format_args!("controller: calendar loaded, {} meetings in the next day", meetings.len()));
                        self.calendar_error = None;
                        self.meetings = Some(meetings);
                    }
                    Err(error) => {
                        log(format_args!("controller: calendar failed to load: {error}"));
                        self.calendar_error = Some(error);
                    }
                }
            }
            Event::FlashNow => self.flash_requested = true,
        }
    }

    fn tracked_addresses(&self, settings: &Settings) -> Vec<String> {
        self.found
            .iter()
            .filter(|light| settings.is_tracked(&light.id))
            .map(|light| light.address.clone())
            .collect()
    }

    /// The next meeting that hasn't been reminded of yet
    fn next_meeting(&self) -> Option<&Meeting> {
        let earliest = Utc::now() - REMINDER_GRACE;
        self.meetings
            .iter()
            .flatten()
            .find(|meeting| meeting.start >= earliest && !self.reminded.contains(&meeting.key))
    }

    /// When the next meeting starts, as an `Instant` so the loop can wait for it
    fn next_meeting_deadline(&self) -> Option<Instant> {
        let until_start = (self.next_meeting()?.start - Utc::now()).to_std().unwrap_or(Duration::ZERO);
        Some(Instant::now() + until_start)
    }

    fn cancel_flash(&mut self) {
        if let Some(cancel) = self.flash_cancel.take() {
            cancel.store(true, Ordering::Relaxed);
        }
    }

    /// Blinks the tracked lights on a separate thread, replacing a blink that's still running
    fn start_flash(&mut self, settings: &Settings) {
        self.cancel_flash();
        let cancel = Arc::new(AtomicBool::new(false));
        self.flash_cancel = Some(cancel.clone());

        let lights = self.lights.clone();
        let addresses = self.tracked_addresses(settings);
        let (count, interval) = (settings.flash_count, Duration::from_millis(settings.flash_interval_ms));

        let _ = thread::Builder::new()
            .name("flash".into())
            .spawn(move || lights.flash(&addresses, count, interval, &cancel));
    }

    fn start_calendar_load(&mut self) {
        self.calendar_loading = true;

        let url = self.calendar_url.clone();
        let events = self.shared.events.clone();

        let spawned = thread::Builder::new().name("calendar".into()).spawn(move || {
            let result = calendar::fetch(&url);
            let _ = events.send(Event::CalendarLoaded { url, result });
        });

        if spawned.is_err() {
            self.calendar_loading = false;
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
        let calendar_load = (!self.calendar_url.is_empty() && !self.calendar_loading).then_some(self.next_calendar_load);

        [self.switch_deadline(settings), retry, refresh, calendar_load, self.next_meeting_deadline()]
            .into_iter()
            .flatten()
            .min()
    }

    fn update(&mut self, settings: &Settings) {
        // Only starts background work, so it doesn't hold up the status below
        self.update_calendar(settings);

        // With the camera trigger off, the lights are left alone. Once it's back on, they follow the camera's next
        // change, rather than switching right away
        if !settings.camera_enabled {
            self.lights_camera_state = self.camera_on;
            self.retry = None;
        }

        // Show camera changes right away, before talking to the lights
        self.publish_status(settings);

        if self.switch_deadline(settings).is_some_and(|deadline| Instant::now() >= deadline) {
            let tracked = self.tracked_addresses(settings);

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

    fn update_calendar(&mut self, settings: &Settings) {
        // With the calendar trigger off, the link is kept but nothing is loaded
        let url = if settings.calendar_enabled { settings.calendar_url.as_str() } else { "" };

        if url != self.calendar_url {
            self.calendar_url = url.to_string();
            self.calendar_loading = false;
            self.calendar_last_load = None;
            self.calendar_error = None;
            self.meetings = None;
        }

        // Scheduled from the last load, so changing the period applies right away
        let period = Duration::from_secs(settings.calendar_refresh_minutes * 60);
        let wait = if self.calendar_error.is_some() { period.min(CALENDAR_RETRY_INTERVAL) } else { period };
        self.next_calendar_load = self.calendar_last_load.map_or_else(Instant::now, |last| last + wait);

        if !self.calendar_url.is_empty() && !self.calendar_loading && Instant::now() >= self.next_calendar_load {
            self.start_calendar_load();
        }

        // Blink once per meeting, even when several start at the same time
        let started: Vec<String> = self
            .meetings
            .iter()
            .flatten()
            .filter(|meeting| {
                let since_start = Utc::now() - meeting.start;
                since_start >= chrono::Duration::zero() && since_start <= REMINDER_GRACE
            })
            .filter(|meeting| !self.reminded.contains(&meeting.key))
            .map(|meeting| meeting.key.clone())
            .collect();

        if !started.is_empty() {
            self.reminded.extend(started);

            // Nobody's there to see it
            if crate::lock::is_locked() {
                log("controller: meeting started while the computer is locked, not blinking");
            } else {
                log("controller: meeting started, blinking the lights");
                self.flash_requested = true;
            }
        }

        if std::mem::take(&mut self.flash_requested) {
            self.start_flash(settings);
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
            calendar: CalendarStatus {
                loading: self.calendar_loading,
                error: self.calendar_error.clone(),
                upcoming: self.meetings.as_ref().map(Vec::len),
                next_meeting: self.next_meeting().map(|meeting| NextMeeting {
                    summary: meeting.summary.clone(),
                    start: meeting.start.to_rfc3339(),
                }),
            },
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

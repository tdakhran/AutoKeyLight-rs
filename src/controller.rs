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
use crate::discovery::{self, DiscoveredLight, Discovery};
use crate::lights::{Lights, Reading, Target};
use crate::log::log;
use crate::settings::Settings;

pub const MAIN_WINDOW: &str = "main";

/// How often light states are refreshed while the window is open
const REFRESH_INTERVAL: Duration = Duration::from_secs(1);
const RETRY_DELAY: Duration = Duration::from_secs(1);
/// How long lights are looked for at startup, or after one stopped answering, unless all of them answer sooner
const DISCOVERY_WINDOW: Duration = Duration::from_secs(60);
/// Cameras are ignored this long after the computer was last seen locked, face recognition at login (Windows Hello)
/// keeps streaming for a moment after unlocking
const UNLOCK_GRACE: Duration = Duration::from_secs(5);
/// How soon the calendar is loaded again after it failed to, unless the refresh period is shorter
const CALENDAR_RETRY_INTERVAL: Duration = Duration::from_secs(5 * 60);
/// A meeting is still reminded of this long after its start, for example right after waking from sleep
const REMINDER_GRACE: chrono::Duration = chrono::Duration::minutes(2);
/// Freed memory is handed back this long after the window closed, once the web view is gone
const RELEASE_AFTER_WINDOW: Duration = Duration::from_secs(5);
/// And this long after starting, once everything is set up
const RELEASE_AFTER_START: Duration = Duration::from_secs(10);

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
    LightsRefreshed(Vec<(String, Option<Reading>)>),
    /// Result of loading the calendar from `url`
    CalendarLoaded { url: String, result: Result<Vec<Meeting>, String> },
    /// Blink the lights now, from the Test button
    FlashNow,
    /// Load the calendar now, from the Check now button
    CheckCalendar,
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
    /// Current brightness in percent, while the light can be reached
    pub brightness: Option<u8>,
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
    /// Number of meetings kept from the calendar, once it loaded
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

pub fn spawn(app: AppHandle, shared: Arc<Shared>, events: Receiver<Event>) {
    // Lights found before, so the camera can switch them right after login without discovering them first
    let known_lights_path = shared.settings_path.with_file_name("lights.json");
    let found = discovery::load_known(&known_lights_path);

    let controller = Controller {
        app,
        shared,
        lights: Lights::new(),
        found,
        known_lights_path,
        discovery: None,
        discover_until: Some(Instant::now() + DISCOVERY_WINDOW),
        found_this_discovery: HashSet::new(),
        expected_lights: HashSet::new(),
        camera_supported: false,
        camera_on: false,
        cameras: Vec::new(),
        camera_changed_at: Instant::now(),
        lights_camera_state: false,
        retry: None,
        ignore_cameras_until: None,
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
        release_memory_at: Some(Instant::now() + RELEASE_AFTER_START),
    };

    thread::Builder::new()
        .name("controller".into())
        .spawn(move || controller.run(events))
        .expect("failed to spawn controller thread");
}

/// Lights that didn't respond to a switch, retried once
struct Retry {
    at: Instant,
    targets: Vec<Target>,
}

struct Controller {
    app: AppHandle,
    shared: Arc<Shared>,
    lights: Lights,
    /// Lights found on the network, sorted by name
    found: Vec<DiscoveredLight>,
    /// Where `found` is saved
    known_lights_path: PathBuf,
    /// Running while the window is open, while no lights are known, and until `discover_until`
    discovery: Option<Discovery>,
    /// Look for lights until then, at startup or after a light stopped answering
    discover_until: Option<Instant>,
    /// Ids of the lights that answered since discovery started
    found_this_discovery: HashSet<String>,
    /// Ids of the lights known when discovery started
    expected_lights: HashSet<String>,
    camera_supported: bool,
    camera_on: bool,
    cameras: Vec<String>,
    camera_changed_at: Instant,
    /// Camera state the lights were last switched for, starts as off so the lights are only touched once the camera turns on
    lights_camera_state: bool,
    retry: Option<Retry>,
    /// Set while the computer is locked and shortly after, cameras don't count until then
    ignore_cameras_until: Option<Instant>,
    window_open: bool,
    /// Whether a background refresh of the light states is running
    refreshing: bool,
    next_refresh: Instant,
    /// Last queried light states and brightness by id
    light_states: HashMap<String, (LightState, Option<u8>)>,
    /// Calendar link the meetings below are from
    calendar_url: String,
    calendar_loading: bool,
    /// When the calendar last finished loading, the next load is scheduled from it
    calendar_last_load: Option<Instant>,
    calendar_error: Option<String>,
    /// Meetings within twice the check period, or the next one when there's none, `None` until the calendar loaded
    meetings: Option<Vec<Meeting>>,
    next_calendar_load: Instant,
    /// Meeting occurrences the lights already blinked for
    reminded: HashSet<String>,
    flash_requested: bool,
    /// Set to stop the blinking that's running
    flash_cancel: Option<Arc<AtomicBool>>,
    /// When to hand freed memory back to the system, after a burst of work. Only while the window is closed
    release_memory_at: Option<Instant>,
}

impl Controller {
    fn run(mut self, events: Receiver<Event>) {
        // Kept on this thread, which lives as long as the app, rather than a thread of its own
        let camera_events = self.shared.events.clone();
        let _camera_monitor = crate::camera::start_monitor(move |cameras| {
            let _ = camera_events.send(Event::Camera(cameras));
        });
        self.camera_supported = _camera_monitor.is_some();

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
                // Whether it turns the lights on depends on which cameras count, decided in `update`
                self.cameras = cameras;
            }
            Event::LightFound(light) => {
                self.found_this_discovery.insert(light.id.clone());

                let changed = match self.found.iter_mut().find(|found| found.id == light.id) {
                    Some(found) if *found == light => false,
                    Some(found) => {
                        *found = light;
                        true
                    }
                    None => {
                        self.found.push(light);
                        true
                    }
                };

                if changed {
                    self.found.sort_by(|a, b| a.name.cmp(&b.name));
                    discovery::save_known(&self.known_lights_path, &self.found);
                }
                self.next_refresh = Instant::now();
            }
            Event::LightLost(fullname) => {
                self.found.retain(|light| light.fullname != fullname);
                discovery::save_known(&self.known_lights_path, &self.found);
            }
            Event::WindowShown => {
                self.window_open = true;
                self.next_refresh = Instant::now();
            }
            Event::WindowHidden => {
                self.window_open = false;
                self.light_states.clear();
                self.release_memory_at = Some(Instant::now() + RELEASE_AFTER_WINDOW);
            }
            Event::SettingsChanged { newly_tracked } => {
                let settings = self.shared.settings.lock().unwrap().clone();
                let targets: Vec<Target> = self
                    .found
                    .iter()
                    .filter(|light| newly_tracked.contains(&light.id))
                    .map(|light| (light.address.clone(), settings.brightness_for(&light.id)))
                    .collect();

                // Only while the camera trigger is on, otherwise there's no camera state to match
                if settings.camera_enabled && !targets.is_empty() {
                    log(format_args!("controller: tracking {} lights on={}", targets.len(), self.lights_camera_state));
                    self.lights.set_all_on(&targets, self.lights_camera_state);
                    self.next_refresh = Instant::now();
                }
            }
            Event::LightsRefreshed(states) => {
                self.refreshing = false;
                self.next_refresh = Instant::now() + REFRESH_INTERVAL;
                self.light_states = states
                    .into_iter()
                    .map(|(id, reading)| {
                        let state = match reading {
                            Some(Reading { on: true, .. }) => LightState::On,
                            Some(Reading { on: false, .. }) => LightState::Off,
                            None => LightState::Unreachable,
                        };
                        (id, (state, reading.map(|reading| reading.brightness)))
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
                self.release_memory_at = Some(Instant::now());
                match result {
                    Ok(meetings) => {
                        log(format_args!("controller: calendar loaded, kept {} meetings", meetings.len()));
                        // Meetings that are no longer listed have passed, there's nothing left to remind of
                        self.reminded.retain(|key| meetings.iter().any(|meeting| &meeting.key == key));
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
            // Scheduled from the last load, without one it's due right away. One that's running already counts
            Event::CheckCalendar => {
                if !self.calendar_loading {
                    self.calendar_last_load = None;
                }
            }
        }
    }

    /// Tracked lights with the brightness the camera turns each on at
    fn tracked_targets(&self, settings: &Settings) -> Vec<Target> {
        self.found
            .iter()
            .filter(|light| settings.is_tracked(&light.id))
            .map(|light| (light.address.clone(), settings.brightness_for(&light.id)))
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
        let addresses: Vec<String> = self.tracked_targets(settings).into_iter().map(|(address, _)| address).collect();
        let (count, interval) = (settings.flash_count, Duration::from_millis(settings.flash_interval_ms));

        let _ = thread::Builder::new()
            .name("flash".into())
            .spawn(move || lights.flash(&addresses, count, interval, &cancel));
    }

    fn start_calendar_load(&mut self, settings: &Settings) {
        self.calendar_loading = true;

        let url = self.calendar_url.clone();
        let events = self.shared.events.clone();
        // Twice the check period, so a check that fails or comes late doesn't miss a meeting. When there's none in it,
        // the next meeting is kept instead, however far ahead it is
        let window = chrono::Duration::minutes(settings.calendar_refresh_minutes as i64 * 2);

        let spawned = thread::Builder::new().name("calendar".into()).spawn(move || {
            let result = calendar::fetch(&url, window);
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
        // Check the camera again once cameras stop being ignored after unlocking
        let unlock_grace = self.ignore_cameras_until.filter(|until| !self.cameras.is_empty() && *until > Instant::now());
        let release_memory = self.release_memory_at.filter(|_| !self.window_open);

        [
            self.switch_deadline(settings),
            retry,
            refresh,
            calendar_load,
            self.next_meeting_deadline(),
            unlock_grace,
            self.discover_until,
            release_memory,
        ]
            .into_iter()
            .flatten()
            .min()
    }

    /// Runs discovery only while it's needed, the rest of the time no mDNS traffic is received
    fn update_discovery(&mut self) {
        let now = Instant::now();

        // Done early once every light known when it started answered. With none known, it looks for the whole window, to
        // find every light rather than stopping at the first
        let all_answered = !self.expected_lights.is_empty() && self.expected_lights.is_subset(&self.found_this_discovery);
        if self.discover_until.is_some_and(|until| now >= until || all_answered) {
            self.discover_until = None;
        }

        let wanted = self.window_open || self.found.is_empty() || self.discover_until.is_some();

        if wanted && self.discovery.is_none() {
            self.found_this_discovery.clear();
            self.expected_lights = self.found.iter().map(|light| light.id.clone()).collect();
            match discovery::start(self.shared.events.clone()) {
                Ok(discovery) => self.discovery = Some(discovery),
                Err(error) => log(format_args!("discovery unavailable: {error}")),
            }
        } else if !wanted && self.discovery.is_some() {
            self.discovery = None;
            self.release_memory_at.get_or_insert(now);
        }
    }

    fn update(&mut self, settings: &Settings) {
        self.update_discovery();

        // Logging in with face recognition uses the camera, that mustn't turn the lights on. Only checked while a camera
        // is streaming
        let now = Instant::now();
        if settings.ignore_camera_when_locked && !self.cameras.is_empty() && crate::lock::is_locked() {
            self.ignore_cameras_until = Some(now + UNLOCK_GRACE);
        }
        let ignoring = self.ignore_cameras_until.is_some_and(|until| now < until);

        // Checked on every update, so unticking a camera that's in use applies right away
        let camera_on = !ignoring && self.cameras.iter().any(|camera| settings.counts_camera(camera));
        if camera_on != self.camera_on {
            self.camera_on = camera_on;
            self.camera_changed_at = Instant::now();
            // A pending retry was for the previous camera state
            self.retry = None;
            // The camera wins over a reminder, restoring the lights afterwards would undo the switch
            self.cancel_flash();
        }

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
            let tracked = self.tracked_targets(settings);

            log(format_args!("controller: switching {} lights on={}", tracked.len(), self.camera_on));
            let failed = self.lights.set_all_on(&tracked, self.camera_on);
            self.lights_camera_state = self.camera_on;

            // Retry once, only the lights that didn't respond, then leave them until the camera changes again
            self.retry = (!failed.is_empty()).then(|| Retry { at: Instant::now() + RETRY_DELAY, targets: failed });

            // Show the new light states right away
            self.next_refresh = Instant::now();
        } else if let Some(retry) = self.retry.take_if(|retry| Instant::now() >= retry.at) {
            log(format_args!("controller: retrying {} lights", retry.targets.len()));
            let failed = self.lights.set_all_on(&retry.targets, self.camera_on);
            // Most likely the light got a new address, look for it again
            if !failed.is_empty() {
                self.discover_until = Some(Instant::now() + DISCOVERY_WINDOW);
            }
            self.next_refresh = Instant::now();
        }

        if self.window_open && !self.refreshing && Instant::now() >= self.next_refresh {
            self.start_refresh();
        }

        // Last, after this update's own work. The window's own memory is far larger, it's not worth it while it's open
        if !self.window_open && self.release_memory_at.is_some_and(|at| Instant::now() >= at) {
            self.release_memory_at = None;
            crate::memory::release();
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
            self.start_calendar_load(settings);
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
                    state: self.light_states.get(&light.id).map_or(LightState::Unknown, |(state, _)| *state),
                    brightness: self.light_states.get(&light.id).and_then(|(_, brightness)| *brightness),
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
            let states = lights.read_all(&addresses);
            log(format_args!("refresh: {} lights took {}ms", addresses.len(), started.elapsed().as_millis()));

            let _ = events.send(Event::LightsRefreshed(ids.into_iter().zip(states).collect()));
        });

        if spawned.is_err() {
            self.refreshing = false;
        }
    }
}

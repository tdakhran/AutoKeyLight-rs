//! User settings, stored as JSON in the app's config directory.

use std::collections::HashMap;
use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Ids of discovered lights that are left alone, new lights are tracked
    pub untracked_lights: Vec<String>,
    /// Brightness in percent the camera turns each light on at, by light id. Lights without one keep their own
    pub light_brightness: HashMap<String, u8>,
    /// Use `brightness` for every light instead of `light_brightness`
    pub sync_brightness: bool,
    /// Brightness in percent for every light while synced, `None` keeps each light's own
    pub brightness: Option<u8>,
    /// Switch the tracked lights with the camera
    pub camera_enabled: bool,
    /// Names of cameras whose use doesn't count, new cameras count
    pub ignored_cameras: Vec<String>,
    /// Ignore cameras while the computer is locked, so face recognition at login doesn't turn the lights on
    pub ignore_camera_when_locked: bool,
    pub camera_on_delay_ms: u64,
    pub camera_off_delay_ms: u64,
    /// iCal (ICS) link of the calendar to remind of meetings from, the tracked lights blink when a meeting starts.
    /// Empty if none
    pub calendar_url: String,
    /// Blink the tracked lights when a meeting starts
    pub calendar_enabled: bool,
    /// How often the calendar is downloaded again to pick up changes
    pub calendar_refresh_minutes: u64,
    pub flash_count: u32,
    pub flash_interval_ms: u64,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            untracked_lights: Vec::new(),
            light_brightness: HashMap::new(),
            sync_brightness: true,
            brightness: Some(20),
            camera_enabled: true,
            ignored_cameras: Vec::new(),
            ignore_camera_when_locked: false,
            camera_on_delay_ms: 0,
            camera_off_delay_ms: 0,
            calendar_url: String::new(),
            calendar_enabled: true,
            calendar_refresh_minutes: 15,
            flash_count: 5,
            flash_interval_ms: 500,
        }
    }
}

impl Settings {
    /// Loads the settings, falling back to defaults on first run.
    pub fn load(path: &Path) -> Self {
        fs::read_to_string(path)
            .ok()
            .and_then(|contents| serde_json::from_str(&contents).ok())
            .unwrap_or_default()
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, serde_json::to_string_pretty(self)?)
    }

    /// Keeps values within sensible limits, whatever the window sent
    pub fn clamped(mut self) -> Self {
        self.brightness = self.brightness.map(|brightness| brightness.clamp(1, 100));
        for brightness in self.light_brightness.values_mut() {
            *brightness = (*brightness).clamp(1, 100);
        }
        self.calendar_url = self.calendar_url.trim().to_string();
        self.calendar_refresh_minutes = self.calendar_refresh_minutes.clamp(1, 120);
        self.flash_count = self.flash_count.clamp(1, 10);
        self.flash_interval_ms = self.flash_interval_ms.clamp(100, 5000);
        self
    }

    /// Brightness to turn the light on at, `None` keeps the light's own
    pub fn brightness_for(&self, id: &str) -> Option<u8> {
        if self.sync_brightness {
            self.brightness
        } else {
            self.light_brightness.get(id).copied()
        }
    }

    /// Whether this camera streaming turns the lights on
    pub fn counts_camera(&self, name: &str) -> bool {
        !self.ignored_cameras.iter().any(|ignored| ignored == name)
    }

    pub fn is_tracked(&self, id: &str) -> bool {
        !self.untracked_lights.iter().any(|untracked| untracked == id)
    }
}

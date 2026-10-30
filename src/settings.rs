//! User settings, stored as JSON in the app's config directory.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Ids of discovered lights that are left alone, new lights are tracked
    pub untracked_lights: Vec<String>,
    /// Switch the tracked lights with the camera
    pub camera_enabled: bool,
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
            camera_enabled: true,
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

    /// Keeps the blinking within sensible limits, whatever the window sent
    pub fn clamped(mut self) -> Self {
        self.calendar_url = self.calendar_url.trim().to_string();
        self.calendar_refresh_minutes = self.calendar_refresh_minutes.clamp(1, 120);
        self.flash_count = self.flash_count.clamp(1, 10);
        self.flash_interval_ms = self.flash_interval_ms.clamp(100, 5000);
        self
    }

    pub fn is_tracked(&self, id: &str) -> bool {
        !self.untracked_lights.iter().any(|untracked| untracked == id)
    }
}

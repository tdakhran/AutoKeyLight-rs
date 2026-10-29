//! User settings, stored as JSON in the app's config directory.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Ids of discovered lights that don't follow the camera, new lights are tracked
    pub untracked_lights: Vec<String>,
    pub camera_on_delay_ms: u64,
    pub camera_off_delay_ms: u64,
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

    pub fn is_tracked(&self, id: &str) -> bool {
        !self.untracked_lights.iter().any(|untracked| untracked == id)
    }
}

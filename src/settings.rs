//! User settings, stored as JSON in the app's config directory.

use std::fs;
use std::io;
use std::path::Path;

use serde::{Deserialize, Serialize};

#[derive(Clone, Default, Serialize, Deserialize)]
#[serde(default, rename_all = "camelCase")]
pub struct Settings {
    /// Light addresses, `<ip>` or `<ip>:<port>`
    pub lights: Vec<String>,
    pub camera_on_delay_ms: u64,
    pub camera_off_delay_ms: u64,
    /// Only switch the lights when the camera changes state, so lights switched manually are left alone
    pub allow_manual_override: bool,
}

impl Settings {
    /// Loads the settings, falling back to defaults (with lights imported from Elgato Control Center) on first run.
    pub fn load(path: &Path) -> Self {
        match fs::read_to_string(path) {
            Ok(contents) => serde_json::from_str(&contents).unwrap_or_default(),
            Err(_) => Self {
                lights: elgato_control_center_lights(),
                ..Self::default()
            },
        }
    }

    pub fn save(&self, path: &Path) -> io::Result<()> {
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        fs::write(path, serde_json::to_string_pretty(self)?)
    }

    /// Trims addresses and drops empty or duplicate ones.
    pub fn normalized(mut self) -> Self {
        let mut lights: Vec<String> = Vec::new();

        for light in self.lights.iter().map(|light| light.trim()) {
            if !light.is_empty() && !lights.iter().any(|existing| existing == light) {
                lights.push(light.to_string());
            }
        }

        self.lights = lights;
        self
    }
}

/// Reads the light IPs saved by an existing Elgato Control Center installation.
#[cfg(windows)]
fn elgato_control_center_lights() -> Vec<String> {
    let Some(app_data) = std::env::var_os("APPDATA") else {
        return Vec::new();
    };

    let path = Path::new(&app_data).join("Elgato").join("ControlCenter").join("Settings.xml");
    let Ok(contents) = fs::read_to_string(path) else {
        return Vec::new();
    };

    contents
        .split("<IpAddress>")
        .skip(1)
        .filter_map(|rest| rest.split_once("</IpAddress>"))
        .map(|(ip, _)| ip.trim().to_string())
        .filter(|ip| !ip.is_empty())
        .collect()
}

#[cfg(not(windows))]
fn elgato_control_center_lights() -> Vec<String> {
    Vec::new()
}

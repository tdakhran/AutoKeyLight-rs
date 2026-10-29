//! Talks to Elgato lights over their local HTTP API.

use std::thread;
use std::time::Duration;

use serde::Deserialize;
use serde_json::json;

const DEFAULT_PORT: u16 = 9123;
const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
struct LightsResponse {
    lights: Vec<LightState>,
}

#[derive(Deserialize)]
struct LightState {
    on: u8,
}

pub struct Lights {
    agent: ureq::Agent,
}

impl Lights {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .build()
            .into();

        Self { agent }
    }

    /// Accepts `<ip>` or `<ip>:<port>`
    fn url(address: &str) -> String {
        if address.contains(':') {
            format!("http://{address}/elgato/lights")
        } else {
            format!("http://{address}:{DEFAULT_PORT}/elgato/lights")
        }
    }

    /// Returns `None` if the light couldn't be reached.
    pub fn is_on(&self, address: &str) -> Option<bool> {
        let response: LightsResponse = self
            .agent
            .get(&Self::url(address))
            .call()
            .ok()?
            .body_mut()
            .read_json()
            .ok()?;

        Some(response.lights.first()?.on == 1)
    }

    /// Returns whether the light accepted the change.
    pub fn set_on(&self, address: &str, on: bool) -> bool {
        let payload = json!({ "numberOfLights": 1, "lights": [{ "on": u8::from(on) }] });

        self.agent.put(&Self::url(address)).send_json(&payload).is_ok()
    }

    /// Queries all lights in parallel, so an unreachable light doesn't hold up the others.
    pub fn are_on(&self, addresses: &[String]) -> Vec<Option<bool>> {
        thread::scope(|scope| {
            let handles: Vec<_> = addresses
                .iter()
                .map(|address| scope.spawn(move || self.is_on(address)))
                .collect();

            handles.into_iter().map(|handle| handle.join().ok().flatten()).collect()
        })
    }

    /// Switches all lights in parallel, returns whether every light accepted the change.
    pub fn set_all_on(&self, addresses: &[String], on: bool) -> bool {
        thread::scope(|scope| {
            let handles: Vec<_> = addresses
                .iter()
                .map(|address| scope.spawn(move || self.set_on(address, on)))
                .collect();

            handles.into_iter().all(|handle| handle.join().unwrap_or(false))
        })
    }
}

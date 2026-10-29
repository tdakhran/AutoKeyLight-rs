//! Talks to Elgato lights over their local HTTP API.

use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use crate::log::log;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);

#[derive(Deserialize)]
struct LightsResponse {
    lights: Vec<LightState>,
}

#[derive(Deserialize)]
struct LightState {
    on: u8,
}

/// Cheap to clone, clones share the connection pool
#[derive(Clone)]
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

    /// `address` is `<ip>:<port>`
    fn url(address: &str) -> String {
        format!("http://{address}/elgato/lights")
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

        let started = Instant::now();
        let result = self.agent.put(&Self::url(address)).send_json(&payload);

        let outcome = match &result {
            Ok(_) => "ok".to_string(),
            Err(error) => format!("failed: {error}"),
        };
        log(format_args!("lights: set {address} on={on} took {}ms, {outcome}", started.elapsed().as_millis()));

        result.is_ok()
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

    /// Switches all lights in parallel, returns the ones that didn't accept the change.
    pub fn set_all_on(&self, addresses: &[String], on: bool) -> Vec<String> {
        thread::scope(|scope| {
            let handles: Vec<_> = addresses
                .iter()
                .map(|address| (address, scope.spawn(move || self.set_on(address, on))))
                .collect();

            handles
                .into_iter()
                .filter_map(|(address, handle)| (!handle.join().unwrap_or(false)).then(|| address.clone()))
                .collect()
        })
    }
}

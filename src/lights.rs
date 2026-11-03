//! Talks to Elgato lights over their local HTTP API.

use std::sync::atomic::{AtomicBool, Ordering};
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
    #[serde(default)]
    brightness: u8,
}

/// What a light reported
#[derive(Clone, Copy)]
pub struct Reading {
    pub on: bool,
    /// In percent
    pub brightness: u8,
}

/// A light to switch, `brightness` in percent is only sent when turning it on, `None` leaves it as it is
pub type Target = (String, Option<u8>);

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
    pub fn read(&self, address: &str) -> Option<Reading> {
        let response: LightsResponse = self
            .agent
            .get(&Self::url(address))
            .call()
            .ok()?
            .body_mut()
            .read_json()
            .ok()?;

        let light = response.lights.first()?;
        Some(Reading { on: light.on == 1, brightness: light.brightness })
    }

    /// Returns whether the light accepted the change.
    pub fn set_on(&self, address: &str, on: bool, brightness: Option<u8>) -> bool {
        let mut light = json!({ "on": u8::from(on) });
        if let (true, Some(brightness)) = (on, brightness) {
            light["brightness"] = json!(brightness);
        }
        let payload = json!({ "numberOfLights": 1, "lights": [light] });

        let started = Instant::now();
        let result = self.agent.put(&Self::url(address)).send_json(&payload);

        let outcome = match &result {
            Ok(_) => "ok".to_string(),
            Err(error) => format!("failed: {error}"),
        };
        log(format_args!(
            "lights: set {address} on={on} brightness={brightness:?} took {}ms, {outcome}",
            started.elapsed().as_millis()
        ));

        result.is_ok()
    }

    /// Queries all lights in parallel, so an unreachable light doesn't hold up the others.
    pub fn read_all(&self, addresses: &[String]) -> Vec<Option<Reading>> {
        thread::scope(|scope| {
            let handles: Vec<_> = addresses
                .iter()
                .map(|address| scope.spawn(move || self.read(address)))
                .collect();

            handles.into_iter().map(|handle| handle.join().ok().flatten()).collect()
        })
    }

    /// Switches each light to its own state, in parallel.
    fn set_each(&self, targets: &[(String, bool)]) {
        thread::scope(|scope| {
            for (address, on) in targets {
                scope.spawn(move || self.set_on(address, *on, None));
            }
        });
    }

    /// Blinks the lights `count` times, `interval` apart, then leaves each light the way it was.
    /// Stops right away, without restoring, once `cancel` is set, for example when the camera turns on meanwhile.
    pub fn flash(&self, addresses: &[String], count: u32, interval: Duration, cancel: &AtomicBool) {
        // Lights that can't be reached are left out, there's no state to restore for them
        let original: Vec<(String, bool)> = addresses
            .iter()
            .zip(self.read_all(addresses))
            .filter_map(|(address, reading)| Some((address.clone(), reading?.on)))
            .collect();
        let toggled: Vec<(String, bool)> = original.iter().map(|(address, on)| (address.clone(), !on)).collect();

        log(format_args!("lights: flashing {} lights {count} times", original.len()));

        for _ in 0..count {
            for targets in [&toggled, &original] {
                if cancel.load(Ordering::Relaxed) {
                    log("lights: flashing cancelled");
                    return;
                }
                self.set_each(targets);
                thread::sleep(interval);
            }
        }

        // Again, in case a light missed one of the changes
        if !cancel.load(Ordering::Relaxed) {
            self.set_each(&original);
        }
    }

    /// Switches all lights in parallel, returns the ones that didn't accept the change.
    pub fn set_all_on(&self, targets: &[Target], on: bool) -> Vec<Target> {
        thread::scope(|scope| {
            let handles: Vec<_> = targets
                .iter()
                .map(|target| (target, scope.spawn(move || self.set_on(&target.0, on, target.1))))
                .collect();

            handles
                .into_iter()
                .filter_map(|(target, handle)| (!handle.join().unwrap_or(false)).then(|| target.clone()))
                .collect()
        })
    }
}

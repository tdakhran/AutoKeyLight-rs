//! Talks to Elgato lights over their local HTTP API.

use std::sync::atomic::{AtomicBool, Ordering};
use std::thread;
use std::time::{Duration, Instant};

use serde::Deserialize;
use serde_json::json;

use crate::log::log;

const REQUEST_TIMEOUT: Duration = Duration::from_secs(2);
/// Lights answer with a few hundred bytes, ureq's default buffers are 128 KB each way
const INPUT_BUFFER_BYTES: usize = 16 * 1024;
const OUTPUT_BUFFER_BYTES: usize = 4 * 1024;

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

/// Cheap to clone, clones share the agent
#[derive(Clone)]
pub struct Lights {
    agent: ureq::Agent,
}

impl Lights {
    pub fn new() -> Self {
        let agent = ureq::Agent::config_builder()
            .timeout_global(Some(REQUEST_TIMEOUT))
            .input_buffer_size(INPUT_BUFFER_BYTES)
            .output_buffer_size(OUTPUT_BUFFER_BYTES)
            // Idle connections are only dropped on the next request, which can be hours away in the tray. Connecting
            // to a light on the local network is quick
            .max_idle_connections(0)
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
        parallel(addresses, |address| self.read(address))
    }

    /// Switches each light to its own state, in parallel.
    fn set_each(&self, targets: &[(String, bool)]) {
        parallel(targets, |(address, on)| self.set_on(address, *on, None));
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
        let accepted = parallel(targets, |(address, brightness)| self.set_on(address, on, *brightness));

        targets.iter().zip(accepted).filter(|(_, accepted)| !accepted).map(|(target, _)| target.clone()).collect()
    }
}

/// Runs `task` on every item at the same time and returns the results in order. The first item runs on the calling
/// thread, so a single light needs no extra thread. A task that panicked counts as the default result.
fn parallel<T: Sync, R: Send + Default>(items: &[T], task: impl Fn(&T) -> R + Sync) -> Vec<R> {
    let Some((first, rest)) = items.split_first() else {
        return Vec::new();
    };

    thread::scope(|scope| {
        let task = &task;
        let others: Vec<_> = rest.iter().map(|item| scope.spawn(move || task(item))).collect();

        let mut results = vec![task(first)];
        results.extend(others.into_iter().map(|handle| handle.join().unwrap_or_default()));
        results
    })
}

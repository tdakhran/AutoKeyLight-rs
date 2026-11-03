//! Finds Elgato lights on the local network with mDNS, they announce themselves as `_elg._tcp`.
//!
//! Only runs while it's needed, the controller starts and stops it. While running, the mDNS socket receives every
//! announcement on the network, which shows up as constant network use.

use std::collections::HashSet;
use std::fs;
use std::path::Path;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use mdns_sd::{RecvTimeoutError, ServiceDaemon, ServiceEvent};
use serde::{Deserialize, Serialize};

use crate::controller::Event;
use crate::log::log;

const SERVICE_TYPE: &str = "_elg._tcp.local.";

/// mdns-sd doubles the time between searches, up to an hour. While no lights are known the search is restarted this
/// often, so lights show up soon after network access is allowed, for example after the macOS Local Network prompt
const RESTART_SEARCH_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
pub struct DiscoveredLight {
    /// Stable id announced by the light (its MAC address), falls back to the mDNS name
    pub id: String,
    /// Name set in Elgato Control Center
    pub name: String,
    /// `<ip>:<port>`
    pub address: String,
    /// mDNS instance name, used to match removals
    pub fullname: String,
}

/// Lights found before, so the app can switch them without discovering them again after a restart
pub fn load_known(path: &Path) -> Vec<DiscoveredLight> {
    fs::read_to_string(path).ok().and_then(|contents| serde_json::from_str(&contents).ok()).unwrap_or_default()
}

pub fn save_known(path: &Path, lights: &[DiscoveredLight]) {
    if let Ok(contents) = serde_json::to_string_pretty(lights) {
        let _ = fs::write(path, contents);
    }
}

/// A running discovery, stopped when dropped
pub struct Discovery {
    daemon: ServiceDaemon,
}

impl Drop for Discovery {
    fn drop(&mut self) {
        // Closes the mDNS sockets, the thread below ends once its receiver disconnects
        let _ = self.daemon.shutdown();
        log("discovery: stopped");
    }
}

/// Browses for lights in the background, sending `Event::LightFound` and `Event::LightLost` to the controller.
pub fn start(events: Sender<Event>) -> Result<Discovery, mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(SERVICE_TYPE)?;
    log("discovery: started");

    let thread_daemon = daemon.clone();
    let spawned = thread::Builder::new().name("discovery".into()).spawn(move || {
        let daemon = thread_daemon;
        let mut receiver = receiver;
        // mDNS instance names of the lights currently on the network
        let mut known: HashSet<String> = HashSet::new();

        loop {
            let event = match receiver.recv_timeout(RESTART_SEARCH_INTERVAL) {
                Ok(event) => event,
                Err(RecvTimeoutError::Timeout) => {
                    if known.is_empty() {
                        log("discovery: no lights yet, restarting the search");
                        let _ = daemon.stop_browse(SERVICE_TYPE);
                        match daemon.browse(SERVICE_TYPE) {
                            Ok(new_receiver) => receiver = new_receiver,
                            Err(error) => log(format_args!("discovery: restarting the search failed: {error}")),
                        }
                    }
                    continue;
                }
                Err(RecvTimeoutError::Disconnected) => return,
            };

            let event = match event {
                ServiceEvent::ServiceResolved(service) => {
                    let Some(ip) = service.get_addresses_v4().into_iter().next() else {
                        continue;
                    };

                    let fullname = service.get_fullname().to_string();
                    let name = fullname
                        .strip_suffix(&format!(".{SERVICE_TYPE}"))
                        .unwrap_or(&fullname)
                        .to_string();
                    let id = service.get_property_val_str("id").map_or_else(|| fullname.clone(), str::to_string);
                    let address = format!("{ip}:{}", service.get_port());

                    log(format_args!("discovery: found {name} ({id}) at {address}"));
                    known.insert(fullname.clone());
                    Event::LightFound(DiscoveredLight { id, name, address, fullname })
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    log(format_args!("discovery: lost {fullname}"));
                    known.remove(&fullname);
                    Event::LightLost(fullname)
                }
                _ => continue,
            };

            if events.send(event).is_err() {
                return;
            }
        }
    });

    match spawned {
        Ok(_) => Ok(Discovery { daemon }),
        Err(error) => {
            let _ = daemon.shutdown();
            Err(mdns_sd::Error::Msg(error.to_string()))
        }
    }
}

//! Finds Elgato lights on the local network with mDNS, they announce themselves as `_elg._tcp`.

use std::collections::HashSet;
use std::sync::mpsc::Sender;
use std::thread;
use std::time::Duration;

use mdns_sd::{RecvTimeoutError, ServiceDaemon, ServiceEvent};

use crate::controller::Event;
use crate::log::log;

const SERVICE_TYPE: &str = "_elg._tcp.local.";

/// mdns-sd doubles the time between searches, up to an hour. While no lights are known the search is restarted this
/// often, so lights show up soon after network access is allowed, for example after the macOS Local Network prompt
const RESTART_SEARCH_INTERVAL: Duration = Duration::from_secs(15);

#[derive(Clone)]
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

/// Browses for lights in the background, sending `Event::LightFound` and `Event::LightLost` to the controller.
pub fn spawn(events: Sender<Event>) -> Result<(), mdns_sd::Error> {
    let daemon = ServiceDaemon::new()?;
    let receiver = daemon.browse(SERVICE_TYPE)?;

    let spawned = thread::Builder::new().name("discovery".into()).spawn(move || {
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

    spawned.map(|_| ()).map_err(|error| mdns_sd::Error::Msg(error.to_string()))
}

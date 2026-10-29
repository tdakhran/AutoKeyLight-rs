//! Finds Elgato lights on the local network with mDNS, they announce themselves as `_elg._tcp`.

use std::sync::mpsc::Sender;
use std::thread;

use mdns_sd::{ServiceDaemon, ServiceEvent};

use crate::controller::Event;
use crate::log::log;

const SERVICE_TYPE: &str = "_elg._tcp.local.";

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
        // Keep the daemon alive as long as we're listening
        let _daemon = daemon;

        while let Ok(event) = receiver.recv() {
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
                    Event::LightFound(DiscoveredLight { id, name, address, fullname })
                }
                ServiceEvent::ServiceRemoved(_, fullname) => {
                    log(format_args!("discovery: lost {fullname}"));
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

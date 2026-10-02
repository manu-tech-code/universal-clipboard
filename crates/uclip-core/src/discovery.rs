//! mDNS discovery on the local network. Devices advertise `_uclip._tcp` with their id and
//! whether they are currently open for pairing.

use anyhow::Result;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use std::net::IpAddr;
use tokio::sync::mpsc::{UnboundedReceiver, unbounded_channel};

pub const SERVICE: &str = "_uclip._tcp.local.";

#[derive(Debug, Clone)]
pub struct Found {
    pub id: String,
    pub name: String,
    pub addrs: Vec<IpAddr>,
    pub port: u16,
    pub pairing: bool,
}

pub fn advertise(id: &str, name: &str, port: u16, pairing: bool) -> Result<ServiceDaemon> {
    let daemon = ServiceDaemon::new()?;
    let host = format!("uclip-{id}.local.");
    let props = [("id", id), ("name", name), ("pairing", if pairing { "1" } else { "0" })];
    let info = ServiceInfo::new(SERVICE, id, &host, "", port, &props[..])?.enable_addr_auto();
    daemon.register(info)?;
    Ok(daemon)
}

pub fn browse(daemon: &ServiceDaemon) -> Result<UnboundedReceiver<Found>> {
    let events = daemon.browse(SERVICE)?;
    let (tx, rx) = unbounded_channel();
    std::thread::spawn(move || {
        while let Ok(event) = events.recv() {
            if let ServiceEvent::ServiceResolved(info) = event {
                let id = info.get_property_val_str("id").unwrap_or_default().to_string();
                if id.is_empty() {
                    continue;
                }
                let addrs: Vec<IpAddr> = info.get_addresses().iter().map(|a| a.to_ip_addr()).filter(|a| a.is_ipv4()).collect();
                let found = Found {
                    name: info.get_property_val_str("name").unwrap_or_default().to_string(),
                    pairing: info.get_property_val_str("pairing") == Some("1"),
                    port: info.get_port(),
                    addrs,
                    id,
                };
                if tx.send(found).is_err() {
                    break;
                }
            }
        }
    });
    Ok(rx)
}

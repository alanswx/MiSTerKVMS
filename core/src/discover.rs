//! What the host editor offers as sources: capture server streams found on
//! the network (mDNS), and capture cards on this computer.

use std::net::Ipv4Addr;
use std::time::{Duration, Instant};

use mdns_sd::{ServiceDaemon, ServiceEvent};
use serde::Serialize;

/// A stream a capture server announces.
#[derive(Debug, Clone, PartialEq, Serialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct FoundStream {
    /// The stream's name, e.g. "MiSTer-SS1".
    pub name: String,
    /// `mkvm://host:port/stream`, what a host profile stores.
    pub address: String,
    /// The MiSTer the stream shows, if the server knows it.
    pub mister: String,
    /// The capture server's computer.
    pub server: String,
}

/// Capture server streams on the network. Blocks up to `wait_ms`; call it
/// off the main thread.
#[uniffi::export]
pub fn find_streams(wait_ms: u32) -> Vec<FoundStream> {
    let Ok(daemon) = ServiceDaemon::new() else { return Vec::new() };
    let Ok(rx) = daemon.browse(mkvm::SERVICE_TYPE) else { return Vec::new() };
    let until = Instant::now() + Duration::from_millis(wait_ms as u64);
    // A server answers on every network adapter it has (VPNs and VMs too):
    // gather each stream's addresses, then pick one this computer can reach.
    let mut seen: Vec<(FoundStream, u16, Vec<Ipv4Addr>)> = Vec::new();
    while let Some(left) = until.checked_duration_since(Instant::now()) {
        let Ok(event) = rx.recv_timeout(left) else { break };
        let ServiceEvent::ServiceResolved(info) = event else { continue };
        let name = info.get_property_val_str("stream").map(str::to_string).unwrap_or_else(|| {
            info.get_fullname().trim_end_matches(mkvm::SERVICE_TYPE).trim_end_matches('.').to_string()
        });
        let server = info.get_property_val_str("server").unwrap_or_default().to_string();
        let ips: Vec<Ipv4Addr> = info.get_addresses_v4().into_iter().copied().collect();
        match seen.iter_mut().find(|(f, _, _)| f.name == name && f.server == server) {
            Some((_, _, known)) => {
                for ip in ips {
                    if !known.contains(&ip) {
                        known.push(ip);
                    }
                }
            }
            None => {
                let stream = FoundStream {
                    name,
                    address: String::new(),
                    mister: info.get_property_val_str("mister").unwrap_or_default().to_string(),
                    server,
                };
                seen.push((stream, info.get_port(), ips));
            }
        }
    }
    let _ = daemon.shutdown();
    let local = local_networks();
    let mut found: Vec<FoundStream> = seen
        .into_iter()
        .filter_map(|(mut f, port, ips)| {
            let ip = best_address(&ips, &local)?;
            f.address = mkvm::StreamAddress { host: ip.to_string(), port, stream: f.name.clone() }.to_string();
            Some(f)
        })
        .collect();
    found.sort_by(|a, b| a.name.to_lowercase().cmp(&b.name.to_lowercase()));
    found
}

/// This computer's IPv4 addresses and netmasks.
fn local_networks() -> Vec<(Ipv4Addr, Ipv4Addr)> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) if !v4.ip.is_loopback() => Some((v4.ip, v4.netmask)),
            _ => None,
        })
        .collect()
}

/// The address on one of our own subnets; else loopback (server on this
/// computer); else a private address; else any.
fn best_address(ips: &[Ipv4Addr], local: &[(Ipv4Addr, Ipv4Addr)]) -> Option<Ipv4Addr> {
    let same_subnet = |ip: &Ipv4Addr| {
        local.iter().any(|(mine, mask)| u32::from(*ip) & u32::from(*mask) == u32::from(*mine) & u32::from(*mask))
    };
    ips.iter()
        .find(|ip| same_subnet(ip))
        .or_else(|| ips.iter().find(|ip| local.iter().any(|(mine, _)| mine == *ip)))
        .or_else(|| ips.iter().find(|ip| ip.is_private()))
        .or_else(|| ips.first())
        .copied()
}

/// A capture card on this computer, with the audio input that goes with it.
#[derive(Debug, Clone, PartialEq, Serialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct CaptureCard {
    pub id: String,
    pub name: String,
    /// The card's own audio input, if one matches by name.
    pub audio: String,
}

#[derive(Debug, Clone, PartialEq, Serialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct AudioInput {
    pub id: String,
    pub name: String,
}

/// Capture cards on this computer (for "Capture card on this computer").
#[uniffi::export]
pub fn capture_cards() -> Vec<CaptureCard> {
    let audio: Vec<String> = kvms_capture::audio_devices().into_iter().map(|a| a.name).collect();
    kvms_capture::devices()
        .unwrap_or_default()
        .into_iter()
        .map(|d| CaptureCard { audio: kvms_capture::matching_audio(&d.name, &audio).unwrap_or_default(), id: d.id, name: d.name })
        .collect()
}

/// Audio inputs on this computer.
#[uniffi::export]
pub fn audio_inputs() -> Vec<AudioInput> {
    kvms_capture::audio_devices().into_iter().map(|a| AudioInput { id: a.name.clone(), name: a.name }).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_the_address_on_our_subnet() {
        let local = [(Ipv4Addr::new(192, 168, 1, 20), Ipv4Addr::new(255, 255, 255, 0))];
        let ips = [Ipv4Addr::new(172, 19, 0, 1), Ipv4Addr::new(192, 168, 1, 7), Ipv4Addr::new(192, 168, 99, 82)];
        assert_eq!(best_address(&ips, &local), Some(Ipv4Addr::new(192, 168, 1, 7)));
        assert_eq!(best_address(&ips[..1], &local), Some(Ipv4Addr::new(172, 19, 0, 1)));
        assert_eq!(best_address(&[], &local), None);
    }
}

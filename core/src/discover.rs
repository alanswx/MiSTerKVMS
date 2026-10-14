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

/// One of this computer's IPv4 networks.
struct LocalNet {
    ip: Ipv4Addr,
    mask: Ipv4Addr,
    /// WSL, Hyper-V, Docker, VMware, VirtualBox...: networks that exist on
    /// many machines with the same numbers but don't connect them.
    is_virtual: bool,
}

fn local_networks() -> Vec<LocalNet> {
    if_addrs::get_if_addrs()
        .unwrap_or_default()
        .into_iter()
        .filter_map(|i| match i.addr {
            if_addrs::IfAddr::V4(v4) if !v4.ip.is_loopback() => {
                Some(LocalNet { ip: v4.ip, mask: v4.netmask, is_virtual: virtual_adapter(&i.name) })
            }
            _ => None,
        })
        .collect()
}

fn virtual_adapter(name: &str) -> bool {
    let n = name.to_lowercase();
    ["vethernet", "wsl", "hyper-v", "docker", "vmware", "vmnet", "virtualbox", "vboxnet", "virbr", "br-", "veth", "utun", "zerotier"]
        .iter()
        .any(|v| n.contains(v))
}

/// Which of a server's addresses to use: the server itself (this computer)
/// over loopback; else one on a real network we share; else a shared
/// virtual network; else a private address; else any.
fn best_address(ips: &[Ipv4Addr], local: &[LocalNet]) -> Option<Ipv4Addr> {
    // Virtual adapters reuse the same addresses on every machine (Docker's
    // 172.17.0.1), so only a real one proves the server is this computer.
    if ips.iter().any(|ip| local.iter().any(|n| !n.is_virtual && n.ip == *ip)) {
        return Some(Ipv4Addr::LOCALHOST);
    }
    let shared = |ip: &Ipv4Addr, want_virtual: bool| {
        local.iter().any(|n| n.is_virtual == want_virtual && u32::from(*ip) & u32::from(n.mask) == u32::from(n.ip) & u32::from(n.mask))
    };
    ips.iter()
        .find(|ip| shared(ip, false))
        .or_else(|| ips.iter().find(|ip| shared(ip, true)))
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
    let inputs = audio_inputs();
    let names: Vec<String> = inputs.iter().map(|a| a.name.clone()).collect();
    kvms_capture::devices()
        .unwrap_or_default()
        .into_iter()
        .map(|d| {
            let audio = kvms_capture::matching_audio(&d.name, &names)
                .and_then(|name| inputs.iter().find(|a| a.name == name).map(|a| a.id.clone()))
                .unwrap_or_default();
            CaptureCard { audio, id: d.id, name: d.name }
        })
        .collect()
}

/// Audio inputs on this computer.
#[uniffi::export]
pub fn audio_inputs() -> Vec<AudioInput> {
    // Card audio keeps its id (it names the card); system inputs go by name,
    // which survives reboots and driver updates better than WASAPI ids.
    kvms_capture::audio_devices()
        .into_iter()
        .map(|a| AudioInput { id: if kvms_capture::is_card_audio(&a.id) { a.id } else { a.name.clone() }, name: a.name })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn picks_a_reachable_address() {
        let net = |ip: [u8; 4], mask: [u8; 4], is_virtual| LocalNet { ip: ip.into(), mask: mask.into(), is_virtual };
        let local = [net([192, 168, 1, 20], [255, 255, 255, 0], false), net([172, 17, 0, 1], [255, 255, 0, 0], true)];
        let docker: Ipv4Addr = [172, 17, 0, 1].into();
        let lan: Ipv4Addr = [192, 168, 1, 7].into();
        let other: Ipv4Addr = [10, 9, 0, 3].into();
        // The LAN beats a Docker network both machines happen to have.
        assert_eq!(best_address(&[docker, other, lan], &local), Some(lan));
        assert_eq!(best_address(&[other], &local), Some(other));
        // This computer's own address: loopback.
        assert_eq!(best_address(&[[192, 168, 1, 20].into()], &local), Some(Ipv4Addr::LOCALHOST));
        assert_eq!(best_address(&[], &local), None);
        assert!(virtual_adapter("vEthernet (WSL (Hyper-V firewall))"));
        assert!(!virtual_adapter("Ethernet 2"));
    }
}

//! mDNS: each stream is announced on its own (`<stream>._mkvm._tcp.local.`),
//! so viewers list MiSTers, not servers (docs/CAPTURE-SERVER.md §5.1).

use mdns_sd::{ServiceDaemon, ServiceInfo};

use crate::log;

pub struct Discovery {
    daemon: ServiceDaemon,
    registered: Vec<String>,
}

impl Discovery {
    pub fn start() -> Result<Self, String> {
        Ok(Self { daemon: ServiceDaemon::new().map_err(|e| e.to_string())?, registered: Vec::new() })
    }

    /// Announce exactly these streams: (name, MiSTer address).
    pub fn announce(&mut self, streams: &[(String, String)], port: u16) {
        for full in self.registered.drain(..) {
            let _ = self.daemon.unregister(&full);
        }
        let host = hostname();
        for (name, mister) in streams {
            let props = [
                ("stream", name.as_str()),
                ("mister", mister.as_str()),
                ("protocol", "1"),
                ("server", host.as_str()),
            ];
            let info = ServiceInfo::new(mkvm::SERVICE_TYPE, &instance_name(name), &format!("{host}.local."), "", port, &props[..])
                .map(|i| i.enable_addr_auto());
            match info {
                Ok(info) => {
                    let full = info.get_fullname().to_string();
                    match self.daemon.register(info) {
                        Ok(()) => self.registered.push(full),
                        Err(e) => log(&format!("mDNS: can't announce '{name}': {e}")),
                    }
                }
                Err(e) => log(&format!("mDNS: '{name}': {e}")),
            }
        }
        if !streams.is_empty() {
            log(&format!("mDNS: announcing {} stream(s) as {}", streams.len(), mkvm::SERVICE_TYPE));
        }
    }
}

/// mDNS instance names can't contain dots; they're shown as is otherwise.
fn instance_name(stream: &str) -> String {
    stream.replace('.', "-")
}

fn hostname() -> String {
    let name = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    let name = if name.trim().is_empty() {
        std::process::Command::new("hostname")
            .output()
            .ok()
            .and_then(|o| String::from_utf8(o.stdout).ok())
            .unwrap_or_else(|| "misterkvms-capture".into())
    } else {
        name
    };
    name.trim().trim_end_matches(".local").to_lowercase().replace(' ', "-")
}

//! Paired devices (docs/IPHONE.md §2): one random key per phone, issued on
//! the home network and needed to connect from the internet. Kept in
//! `devices.json` in the config folder, readable only by the server's user
//! where the OS allows (like the setup password in `capture.ini`). Revoking
//! deletes the key and hangs up that device's open connections.

use std::net::TcpStream;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex, Weak};
use std::time::{SystemTime, UNIX_EPOCH};

use mkvm::secure::{self, DeviceKey, KEY_ID_LEN, KEY_LEN};
use serde::{Deserialize, Serialize};

use crate::log;

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase")]
struct Device {
    name: String,
    key_id: String,
    key: String,
    /// Unix seconds.
    paired: u64,
    #[serde(default)]
    last_seen: u64,
    #[serde(default)]
    last_address: String,
}

/// What the setup page shows: never the key.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(rename_all = "camelCase")]
pub struct DeviceInfo {
    pub name: String,
    pub key_id: String,
    pub paired: u64,
    pub last_seen: u64,
    pub last_address: String,
    pub connected: bool,
}

pub struct Devices {
    path: PathBuf,
    list: Mutex<Vec<Device>>,
    /// Open encrypted connections, by key id, to hang up on revoke.
    open: Mutex<Vec<(String, Weak<TcpStream>)>>,
}

fn now() -> u64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0)
}

impl Devices {
    pub fn load(dir: &Path) -> Self {
        let path = dir.join("devices.json");
        let list = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                log(&format!("devices: devices.json unreadable ({e}); starting with none"));
                Vec::new()
            }),
            Err(_) => Vec::new(),
        };
        Self { path, list: Mutex::new(list), open: Mutex::new(Vec::new()) }
    }

    fn save(&self, list: &[Device]) {
        let write = || -> std::io::Result<()> {
            if let Some(dir) = self.path.parent() {
                std::fs::create_dir_all(dir)?;
            }
            let tmp = self.path.with_extension("json.tmp");
            std::fs::write(&tmp, serde_json::to_vec_pretty(list).expect("serializable"))?;
            #[cfg(unix)]
            {
                use std::os::unix::fs::PermissionsExt;
                std::fs::set_permissions(&tmp, std::fs::Permissions::from_mode(0o600))?;
            }
            std::fs::rename(tmp, &self.path)
        };
        if let Err(e) = write() {
            log(&format!("devices: can't save {}: {e}", self.path.display()));
        }
    }

    /// Issue a key for a device on the home network. `replaces`: the key it
    /// had here before, dropped so pairing again leaves no stale entry.
    pub fn pair(&self, name: &str, address: &str, replaces: Option<&str>) -> std::io::Result<DeviceKey> {
        if let Some(old) = replaces {
            self.revoke(old);
        }
        let key = DeviceKey::generate()?;
        let name: String = name.trim().chars().take(64).collect();
        let name = if name.is_empty() { "Device".to_string() } else { name };
        let mut list = self.list.lock().unwrap();
        list.push(Device {
            name: name.clone(),
            key_id: key.id_hex(),
            key: key.key_hex(),
            paired: now(),
            last_seen: now(),
            last_address: address.to_string(),
        });
        self.save(&list);
        log(&format!("devices: paired '{name}' ({}) from {address}", key.id_hex()));
        Ok(key)
    }

    /// The key for an id a connection claims, if it's a paired device.
    pub fn key(&self, id: &[u8; KEY_ID_LEN]) -> Option<[u8; KEY_LEN]> {
        let id = secure::hex(id);
        let list = self.list.lock().unwrap();
        list.iter().find(|d| d.key_id == id).and_then(|d| secure::unhex::<KEY_LEN>(&d.key))
    }

    /// A paired device connected under another name (renamed on the phone):
    /// the list shows the new one.
    pub fn rename(&self, id: &[u8; KEY_ID_LEN], name: &str) {
        let id = secure::hex(id);
        let name: String = name.trim().chars().take(64).collect();
        if name.is_empty() {
            return;
        }
        let mut list = self.list.lock().unwrap();
        if let Some(d) = list.iter_mut().find(|d| d.key_id == id && d.name != name) {
            log(&format!("devices: '{}' is now '{name}' ({id})", d.name));
            d.name = name;
            self.save(&list);
        }
    }

    /// The device's name, for the log and the viewer list.
    pub fn name(&self, id: &[u8; KEY_ID_LEN]) -> Option<String> {
        let id = secure::hex(id);
        self.list.lock().unwrap().iter().find(|d| d.key_id == id).map(|d| d.name.clone())
    }

    /// A device connected: remember when and from where, and its socket.
    pub fn connected(&self, id: &[u8; KEY_ID_LEN], address: &str, sock: &Arc<TcpStream>) {
        let id = secure::hex(id);
        {
            let mut list = self.list.lock().unwrap();
            if let Some(d) = list.iter_mut().find(|d| d.key_id == id) {
                d.last_seen = now();
                d.last_address = address.to_string();
            }
            self.save(&list);
        }
        let mut open = self.open.lock().unwrap();
        open.retain(|(_, s)| s.strong_count() > 0);
        open.push((id, Arc::downgrade(sock)));
    }

    /// Forget a device and hang up its connections. False if unknown.
    pub fn revoke(&self, key_id: &str) -> bool {
        let removed = {
            let mut list = self.list.lock().unwrap();
            let before = list.len();
            let name = list.iter().find(|d| d.key_id == key_id).map(|d| d.name.clone());
            list.retain(|d| d.key_id != key_id);
            if list.len() != before {
                self.save(&list);
                log(&format!("devices: revoked '{}' ({key_id})", name.unwrap_or_default()));
            }
            list.len() != before
        };
        for (_, sock) in self.open.lock().unwrap().iter().filter(|(id, _)| id == key_id) {
            if let Some(sock) = sock.upgrade() {
                let _ = sock.shutdown(std::net::Shutdown::Both);
            }
        }
        removed
    }

    pub fn list(&self) -> Vec<DeviceInfo> {
        let open = self.open.lock().unwrap();
        let connected = |id: &str| open.iter().any(|(i, s)| i == id && s.strong_count() > 0);
        self.list
            .lock()
            .unwrap()
            .iter()
            .map(|d| DeviceInfo {
                name: d.name.clone(),
                key_id: d.key_id.clone(),
                paired: d.paired,
                last_seen: d.last_seen,
                last_address: d.last_address.clone(),
                connected: connected(&d.key_id),
            })
            .collect()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn pair_find_revoke_and_reload() {
        let dir = std::env::temp_dir().join(format!("mkvm-devices-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let devices = Devices::load(&dir);
        let key = devices.pair("  Dani's iPhone  ", "192.168.1.20", None).unwrap();
        assert_eq!(devices.key(&key.id), Some(key.key));
        assert_eq!(devices.name(&key.id).as_deref(), Some("Dani's iPhone"));
        // Survives a restart; the list never shows the key.
        let again = Devices::load(&dir);
        assert_eq!(again.key(&key.id), Some(key.key));
        let shown = serde_json::to_string(&again.list()).unwrap();
        assert!(!shown.contains(&key.key_hex()));
        // Renamed on the phone; pairing again replaces the old key.
        again.rename(&key.id, "Living room iPhone");
        assert_eq!(again.name(&key.id).as_deref(), Some("Living room iPhone"));
        let newer = again.pair("Living room iPhone", "192.168.1.20", Some(&key.id_hex())).unwrap();
        assert_eq!(again.key(&key.id), None);
        assert_eq!(again.list().len(), 1);
        let key = newer;
        assert!(again.revoke(&key.id_hex()));
        assert_eq!(again.key(&key.id), None);
        assert!(!again.revoke(&key.id_hex()));
        assert_eq!(Devices::load(&dir).key(&key.id), None);
        let _ = std::fs::remove_dir_all(&dir);
    }
}

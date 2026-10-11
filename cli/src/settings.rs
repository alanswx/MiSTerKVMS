//! Where to find the MiSTer and its video: `local.settings.json` (searched
//! from the current directory upwards, or `--settings`/`KVMS_SETTINGS`),
//! overridden by `--mister` / `--ndi`.
//!
//! Several MiSTers can be named under `machines`; `--host <name>` picks one
//! (default: `default`). A machine's fields override the top-level ones.
//!
//! ```json
//! { "default": "ss1", "obs_host": "obs-pc.local",
//!   "machines": { "ss1": { "mister_host": "192.168.1.50", "ndi_source": "OBSPC (MiSTer-1)" },
//!                 "pi":  { "mister_host": "192.168.1.51", "ndi_source": "OBSPC (MiSTer-2)" } } }
//! ```

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

/// Per-machine values; anything left out comes from the top level.
#[derive(serde::Deserialize, Default, Clone)]
#[serde(default)]
pub struct Machine {
    pub mister_host: Option<String>,
    pub mister_port: Option<u16>,
    pub ndi_source: Option<String>,
    pub obs_host: Option<String>,
    pub ssh_user: Option<String>,
    pub core_dest: Option<String>,
    pub type_delay_ms: Option<u64>,
    pub type_hold_ms: Option<u64>,
}

#[derive(serde::Deserialize, Default, Clone)]
#[serde(default)]
pub struct Settings {
    pub mister_host: String,
    pub mister_port: Option<u16>,
    pub ndi_source: String,
    /// OBS PC address, if NDI discovery can't find it.
    pub obs_host: String,
    pub ssh_user: Option<String>,
    /// Where `load-core` copies cores on the MiSTer.
    pub core_dest: Option<String>,
    /// Typing speed for `type` (ms between keys / how long each is held).
    pub type_delay_ms: Option<u64>,
    pub type_hold_ms: Option<u64>,
    /// Named MiSTers, chosen with `--host`.
    pub machines: BTreeMap<String, Machine>,
    /// Machine used when `--host` isn't given.
    pub default: Option<String>,
    #[serde(skip)]
    pub file: Option<PathBuf>,
    /// Which machine was applied, if any.
    #[serde(skip)]
    pub machine: Option<String>,
}

impl Settings {
    pub fn load(explicit: Option<&Path>) -> Result<Self, String> {
        let path = explicit
            .map(Path::to_path_buf)
            .or_else(|| std::env::var_os("KVMS_SETTINGS").map(PathBuf::from))
            .or_else(find_upwards);
        let Some(path) = path else { return Ok(Settings::default()) };
        let text = std::fs::read_to_string(&path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut s: Settings = serde_json::from_str(&text).map_err(|e| format!("{}: {e}", path.display()))?;
        s.file = Some(path);
        Ok(s)
    }

    /// Apply a named machine (`--host`), or the default one.
    pub fn select(&mut self, host: Option<&str>) -> Result<(), String> {
        let Some(name) = host.map(str::to_string).or_else(|| self.default.clone()) else {
            return Ok(());
        };
        let m = self.machines.get(&name).cloned().ok_or_else(|| {
            let known: Vec<&str> = self.machines.keys().map(String::as_str).collect();
            format!("no machine '{name}' in settings (known: {})", known.join(", "))
        })?;
        if let Some(v) = m.mister_host { self.mister_host = v; }
        if let Some(v) = m.ndi_source { self.ndi_source = v; }
        if let Some(v) = m.obs_host { self.obs_host = v; }
        self.mister_port = m.mister_port.or(self.mister_port);
        self.ssh_user = m.ssh_user.or(self.ssh_user.take());
        self.core_dest = m.core_dest.or(self.core_dest.take());
        self.type_delay_ms = m.type_delay_ms.or(self.type_delay_ms);
        self.type_hold_ms = m.type_hold_ms.or(self.type_hold_ms);
        self.machine = Some(name);
        Ok(())
    }

    pub fn mister(&self) -> Result<String, String> {
        if self.mister_host.is_empty() {
            return Err(missing("mister_host", "--mister"));
        }
        Ok(self.mister_host.clone())
    }

    pub fn port(&self) -> u16 {
        self.mister_port.unwrap_or(8182)
    }

    pub fn ndi(&self) -> Result<String, String> {
        if self.ndi_source.is_empty() {
            return Err(missing("ndi_source", "--ndi"));
        }
        Ok(self.ndi_source.clone())
    }
}

fn find_upwards() -> Option<PathBuf> {
    let mut dir = std::env::current_dir().ok()?;
    loop {
        let candidate = dir.join("local.settings.json");
        if candidate.is_file() {
            return Some(candidate);
        }
        if !dir.pop() {
            return None;
        }
    }
}

fn missing(field: &str, flag: &str) -> String {
    format!("no {field}: pass {flag} or set it in local.settings.json (see local.settings.example.json)")
}

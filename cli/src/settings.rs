//! Where to find the MiSTer and its video: `local.settings.json` (searched
//! from the current directory upwards, or `--settings`/`KVMS_SETTINGS`),
//! overridden by `--mister` / `--ndi`.

use std::path::{Path, PathBuf};

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
    #[serde(skip)]
    pub file: Option<PathBuf>,
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

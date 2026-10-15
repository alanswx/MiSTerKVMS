//! Configuration: `capture.json` (streams, written by the setup page) and
//! `capture.ini` (the few things a person may edit by hand: ports, LAN
//! access to the setup page, the setup password, internet access). Forgot
//! the password? Delete its line in `capture.ini` and restart. Paired
//! devices are in `devices.json` (devices.rs).

use std::path::{Path, PathBuf};

use kvms_capture::VideoFormat;
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct StreamConfig {
    /// Shown to viewers and used in the address (`mkvm://pc/<name>`).
    pub name: String,
    /// Video device id (hardware id; see `misterkvms-capture devices`).
    pub video: String,
    /// The device's name when it was chosen, to find it again if its id
    /// changes (moved to another USB port).
    pub video_name: String,
    /// None: the best format the card offers.
    pub format: Option<VideoFormat>,
    /// Audio device id or name; empty for none.
    pub audio: String,
    /// The MiSTer this card shows (mrext Remote); empty if not set.
    pub mister: String,
    pub mister_port: u16,
    /// JPEG quality for raw cards and scaled pictures (1–100).
    pub quality: u8,
}

impl Default for StreamConfig {
    fn default() -> Self {
        Self {
            name: String::new(),
            video: String::new(),
            video_name: String::new(),
            format: None,
            audio: String::new(),
            mister: String::new(),
            mister_port: 8182,
            quality: 85,
        }
    }
}

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct Streams {
    pub streams: Vec<StreamConfig>,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Settings {
    pub port: u16,
    pub setup_port: u16,
    /// Other computers may open the setup page.
    pub allow_lan: bool,
    /// Needed to change the configuration from the setup page (empty: none).
    pub password: String,
    /// Offer the built-in test pattern (rainbow bars and a beep) as a card.
    pub test_pattern: bool,
    /// Paired devices may connect from the internet (docs/IPHONE.md §3).
    /// Off: only the home network, as before.
    pub remote_enabled: bool,
    /// How the internet reaches this PC ("myhome.example.org:5970"), handed
    /// to devices when they pair.
    pub remote_address: String,
}

impl Default for Settings {
    fn default() -> Self {
        Self {
            port: mkvm::DEFAULT_PORT,
            setup_port: mkvm::SETUP_PORT,
            allow_lan: false,
            password: String::new(),
            test_pattern: false,
            remote_enabled: false,
            remote_address: String::new(),
        }
    }
}

const INI_TEMPLATE: &str = "\
; misterkvms-capture settings. Restart the service after editing.

[server]
; Viewers connect here (TCP).
port = 5970
; The setup page: http://this-pc:5971
setup_port = 5971

[setup]
; Let other computers open the setup page (true/false).
allow_lan = false
; Password to change the configuration from the setup page. Empty: none.
; Forgot it? Delete what's after the = and restart.
password =
; Offer a test pattern (rainbow bars and a beep) as a capture card (true/false).
test_pattern = false

[remote]
; Let paired phones connect from the internet (true/false). Needs the
; viewers' port forwarded to this PC on the router. Only changeable here or
; on the setup page opened on this PC.
enabled = false
; How the internet reaches this PC, e.g. myhome.duckdns.org:5970. Handed to
; phones when they pair; they can change it.
address =
";

/// The config folder: `MISTERKVMS_CAPTURE_CONFIG` if set; otherwise
/// `%ProgramData%\\MiSTerKVMS\\Capture` on Windows (shared by the service and
/// the tray), the user's config folder elsewhere.
pub fn dir() -> PathBuf {
    if let Ok(d) = std::env::var("MISTERKVMS_CAPTURE_CONFIG") {
        return PathBuf::from(d);
    }
    if cfg!(windows) {
        let base = std::env::var("ProgramData").unwrap_or_else(|_| r"C:\ProgramData".into());
        return PathBuf::from(base).join("MiSTerKVMS").join("Capture");
    }
    let home = std::env::var("HOME").unwrap_or_else(|_| ".".into());
    if cfg!(target_os = "macos") {
        PathBuf::from(home).join("Library/Application Support/MiSTerKVMS Capture")
    } else {
        std::env::var("XDG_CONFIG_HOME")
            .map(PathBuf::from)
            .unwrap_or_else(|_| PathBuf::from(home).join(".config"))
            .join("misterkvms-capture")
    }
}

pub fn load_streams(dir: &Path) -> Streams {
    match std::fs::read(dir.join("capture.json")) {
        Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
            let bad = dir.join("capture.json.bad");
            let _ = std::fs::copy(dir.join("capture.json"), &bad);
            crate::log(&format!("config: capture.json unreadable ({e}); copied to {}", bad.display()));
            Streams::default()
        }),
        Err(_) => Streams::default(),
    }
}

pub fn save_streams(dir: &Path, streams: &Streams) -> std::io::Result<()> {
    std::fs::create_dir_all(dir)?;
    let tmp = dir.join("capture.json.tmp");
    std::fs::write(&tmp, serde_json::to_vec_pretty(streams).expect("serializable"))?;
    std::fs::rename(tmp, dir.join("capture.json"))
}

/// Load `capture.ini`, writing the commented template first if it's missing.
pub fn load_settings(dir: &Path) -> Settings {
    let path = dir.join("capture.ini");
    if !path.exists() {
        let _ = std::fs::create_dir_all(dir);
        let _ = std::fs::write(&path, INI_TEMPLATE);
    }
    let text = std::fs::read_to_string(&path).unwrap_or_default();
    parse_settings(&text)
}

pub fn parse_settings(text: &str) -> Settings {
    let mut s = Settings::default();
    let mut section = String::new();
    for line in text.lines() {
        let line = line.trim();
        if line.is_empty() || line.starts_with(';') || line.starts_with('#') {
            continue;
        }
        if let Some(name) = line.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            section = name.trim().to_lowercase();
            continue;
        }
        let Some((key, value)) = line.split_once('=') else { continue };
        let (key, value) = (key.trim().to_lowercase(), value.trim());
        match (section.as_str(), key.as_str()) {
            ("server", "port") => s.port = value.parse().unwrap_or(s.port),
            ("server", "setup_port") => s.setup_port = value.parse().unwrap_or(s.setup_port),
            ("setup", "allow_lan") => s.allow_lan = matches!(value.to_lowercase().as_str(), "true" | "yes" | "1" | "on"),
            ("setup", "password") => s.password = value.to_string(),
            ("setup", "test_pattern") => s.test_pattern = matches!(value.to_lowercase().as_str(), "true" | "yes" | "1" | "on"),
            ("remote", "enabled") => s.remote_enabled = matches!(value.to_lowercase().as_str(), "true" | "yes" | "1" | "on"),
            ("remote", "address") => s.remote_address = value.to_string(),
            _ => {}
        }
    }
    s
}

/// Change one `key = value` in `capture.ini`, keeping comments and layout.
pub fn set_ini_value(dir: &Path, section: &str, key: &str, value: &str) -> std::io::Result<()> {
    let path = dir.join("capture.ini");
    let text = std::fs::read_to_string(&path).unwrap_or_else(|_| INI_TEMPLATE.to_string());
    let mut out = Vec::new();
    let mut current = String::new();
    let mut done = false;
    for line in text.lines() {
        let t = line.trim();
        if let Some(name) = t.strip_prefix('[').and_then(|l| l.strip_suffix(']')) {
            if current == section && !done {
                out.push(format!("{key} = {value}"));
                done = true;
            }
            current = name.trim().to_lowercase();
        } else if current == section && !done && !t.starts_with(';') {
            if let Some((k, _)) = t.split_once('=') {
                if k.trim().eq_ignore_ascii_case(key) {
                    out.push(format!("{key} = {value}"));
                    done = true;
                    continue;
                }
            }
        }
        out.push(line.to_string());
    }
    if !done {
        if current != section {
            out.push(format!("[{section}]"));
        }
        out.push(format!("{key} = {value}"));
    }
    std::fs::write(path, out.join("\n") + "\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn template_parses_to_defaults() {
        assert_eq!(parse_settings(INI_TEMPLATE), Settings::default());
    }

    #[test]
    fn parses_values_and_ignores_junk() {
        let s = parse_settings(
            "[server]\nport=6000\n[setup]\n allow_lan = Yes \npassword = hunter 2\nnonsense\n[other]\nport=1\n[remote]\nenabled = on\naddress = home.example.org:5970",
        );
        assert_eq!(
            s,
            Settings {
                port: 6000,
                setup_port: 5971,
                allow_lan: true,
                password: "hunter 2".into(),
                test_pattern: false,
                remote_enabled: true,
                remote_address: "home.example.org:5970".into(),
            }
        );
    }

    #[test]
    fn set_value_keeps_comments() {
        let dir = std::env::temp_dir().join(format!("mkvm-ini-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("capture.ini"), INI_TEMPLATE).unwrap();
        set_ini_value(&dir, "setup", "allow_lan", "true").unwrap();
        set_ini_value(&dir, "setup", "password", "pw").unwrap();
        let text = std::fs::read_to_string(dir.join("capture.ini")).unwrap();
        assert!(text.contains("; Forgot it?"));
        let s = parse_settings(&text);
        assert!(s.allow_lan);
        assert_eq!(s.password, "pw");
        std::fs::remove_dir_all(dir).ok();
    }
}

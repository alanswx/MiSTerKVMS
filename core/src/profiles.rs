//! Saved hosts and app settings, kept as JSON in the platform's config folder
//! so every shell (and a profile export) reads the same file:
//! `~/Library/Application Support/MiSTerKVMS`, `%APPDATA%\MiSTerKVMS`,
//! `~/.config/misterkvms`.
//!
//! Field names match what the macOS app stored in UserDefaults before, so
//! that data imports as is.

use std::collections::hash_map::RandomState;
use std::hash::{BuildHasher, Hasher};
use std::path::{Path, PathBuf};
use std::sync::Mutex;
use std::time::{SystemTime, UNIX_EPOCH};

use serde::{Deserialize, Serialize};

use crate::input::InputSettings;
use crate::video::{DisplaySettings, ScaleMode};

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase", default)]
pub struct HostProfile {
    pub id: String,
    pub name: String,
    /// Where video comes from: "capture" (a capture server stream) or
    /// "local" (a capture card on this computer). Empty: "local" if
    /// `local_video` is set, else "capture". Hosts saved with "ndi" (removed)
    /// are capture hosts; opening one finds its MiSTer's stream.
    pub source: String,
    /// Capture server stream, `mkvm://host/stream`.
    pub stream: String,
    /// Capture card on this computer: video and audio device ids.
    pub local_video: String,
    pub local_audio: String,
    /// "auto", "best", "balanced" (1080p) or "low" (720p). Capture server
    /// streams only.
    pub quality: String,
    /// MiSTer running mrext Remote. Not needed for capture server streams:
    /// the server talks to the MiSTer.
    pub mister_host: String,
    pub mister_port: u16,
    pub mouse_sensitivity: f64,
    /// Alt + click → middle click.
    pub alt_click_is_middle: bool,
    /// Send the mouse's raw movement, so only the vintage OS accelerates it.
    pub raw_mouse: bool,
    /// "fit", "integer" or "native".
    pub scale_mode: String,
    pub smooth_scaling: bool,
    pub auto_crop: bool,
    /// Draw a local cursor ahead of the guest's lagging one.
    pub predictive_cursor: bool,
    /// What the predictive cursor learned last time (0 = not yet).
    pub cursor_lag_ms: f64,
    pub cursor_gain: f64,
}

impl Default for HostProfile {
    fn default() -> Self {
        Self {
            id: new_id(),
            name: "New MiSTer".into(),
            source: String::new(),
            stream: String::new(),
            local_video: String::new(),
            local_audio: String::new(),
            quality: "auto".into(),
            mister_host: String::new(),
            mister_port: crate::default_mrext_port(),
            mouse_sensitivity: 1.0,
            alt_click_is_middle: true,
            raw_mouse: false,
            scale_mode: "fit".into(),
            smooth_scaling: false,
            auto_crop: true,
            predictive_cursor: true,
            cursor_lag_ms: 0.0,
            cursor_gain: 0.0,
        }
    }
}

/// Where a host's video comes from (see `HostProfile::source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceKind {
    Capture,
    Local,
}

impl HostProfile {
    pub fn source_kind(&self) -> SourceKind {
        match self.source.as_str() {
            "capture" => SourceKind::Capture,
            "local" => SourceKind::Local,
            // Including "ndi" (removed): the MiSTer's stream replaces it.
            _ if !self.stream.trim().is_empty() => SourceKind::Capture,
            _ if !self.local_video.is_empty() => SourceKind::Local,
            _ => SourceKind::Capture,
        }
    }

    /// The largest picture to ask a capture server for (None: native).
    pub fn max_size(&self) -> (Option<u32>, Option<u32>) {
        match self.quality.as_str() {
            "balanced" => (Some(1920), Some(1080)),
            "low" => (Some(1280), Some(720)),
            _ => (None, None),
        }
    }

    pub fn input_settings(&self) -> InputSettings {
        InputSettings {
            mouse_sensitivity: self.mouse_sensitivity,
            alt_click_is_middle: self.alt_click_is_middle,
        }
    }

    /// The display settings this host opens with.
    pub fn display_settings(&self) -> DisplaySettings {
        DisplaySettings {
            scale_mode: ScaleMode::from_name(&self.scale_mode),
            smooth: self.smooth_scaling,
            auto_crop: self.auto_crop,
            predictive_cursor: self.predictive_cursor,
            ..DisplaySettings::default()
        }
    }

    /// Remember the per-host parts of `display` (not zoom/pan).
    pub fn store_display(&mut self, display: &DisplaySettings) {
        self.scale_mode = display.scale_mode.name().into();
        self.smooth_scaling = display.smooth;
        self.auto_crop = display.auto_crop;
        self.predictive_cursor = display.predictive_cursor;
    }

    /// Call when saving an edited copy of `before`: if the mouse scaling
    /// changed, the predictive cursor must learn its gain again.
    pub fn reset_learned_if_mouse_changed(&mut self, before: &HostProfile) {
        if self.raw_mouse != before.raw_mouse || self.mouse_sensitivity != before.mouse_sensitivity {
            self.cursor_gain = 0.0;
        }
    }
}

/// A new profile with default values and a fresh id.
#[uniffi::export]
pub fn new_host_profile() -> HostProfile {
    HostProfile::default()
}

/// Everything in the settings file.
#[derive(Debug, Clone, Default, Serialize, Deserialize)]
#[serde(rename_all = "camelCase", default)]
pub struct AppData {
    pub profiles: Vec<HostProfile>,
    /// Stats overlay always shown.
    pub stats_pinned: bool,
    /// How this computer appears to other viewers of a capture server
    /// stream ("Dani's Mac has control"). Empty: the computer's name.
    pub viewer_name: String,
    /// Set once the shell has imported settings from an older storage
    /// (macOS UserDefaults), so it isn't done twice.
    pub imported_legacy: bool,
}

/// The settings file. Every change is written straight away.
pub struct ProfileStore {
    path: Option<PathBuf>,
    data: Mutex<AppData>,
}

impl ProfileStore {
    /// Load from the platform config folder (empty if there's no file yet).
    pub fn open_default() -> Self {
        match config_dir() {
            Some(dir) => Self::open(dir.join("profiles.json")),
            None => Self { path: None, data: Mutex::new(AppData::default()) },
        }
    }

    pub fn open(path: PathBuf) -> Self {
        let data = match std::fs::read(&path) {
            Ok(bytes) => serde_json::from_slice(&bytes).unwrap_or_else(|e| {
                // Keep the unreadable file for inspection rather than overwrite it.
                let backup = path.with_extension("json.bad");
                let _ = std::fs::copy(&path, &backup);
                crate::log(&format!("profiles: {} unreadable ({e}); copied to {}", path.display(), backup.display()));
                AppData::default()
            }),
            Err(_) => AppData::default(),
        };
        Self { path: Some(path), data: Mutex::new(data) }
    }

    /// Not saved anywhere (tests).
    pub fn in_memory() -> Self {
        Self { path: None, data: Mutex::new(AppData::default()) }
    }

    pub fn path(&self) -> Option<&Path> {
        self.path.as_deref()
    }

    pub fn data(&self) -> AppData {
        self.data.lock().unwrap().clone()
    }

    pub fn profiles(&self) -> Vec<HostProfile> {
        self.data.lock().unwrap().profiles.clone()
    }

    pub fn get(&self, id: &str) -> Option<HostProfile> {
        self.data.lock().unwrap().profiles.iter().find(|p| p.id == id).cloned()
    }

    /// Add or replace (by id).
    pub fn upsert(&self, profile: HostProfile) {
        self.change(|d| match d.profiles.iter_mut().find(|p| p.id == profile.id) {
            Some(p) => *p = profile,
            None => d.profiles.push(profile),
        });
    }

    pub fn delete(&self, id: &str) {
        self.change(|d| d.profiles.retain(|p| p.id != id));
    }

    /// Change one saved profile in place (no-op if it was deleted).
    pub fn update(&self, id: &str, f: impl FnOnce(&mut HostProfile)) {
        self.change(|d| {
            if let Some(p) = d.profiles.iter_mut().find(|p| p.id == id) {
                f(p);
            }
        });
    }

    pub fn change(&self, f: impl FnOnce(&mut AppData)) {
        let mut data = self.data.lock().unwrap();
        let before = serde_json::to_value(&*data).ok();
        f(&mut data);
        if serde_json::to_value(&*data).ok() == before {
            return;
        }
        if let (Some(path), Ok(bytes)) = (&self.path, serde_json::to_vec_pretty(&*data)) {
            if let Err(e) = write_atomic(path, &bytes) {
                crate::log(&format!("profiles: save {} failed: {e}", path.display()));
            }
        }
    }

    /// Profiles as JSON (an array), for export.
    pub fn export_json(&self) -> String {
        serde_json::to_string_pretty(&self.profiles()).unwrap_or_else(|_| "[]".into())
    }

    /// Add profiles from JSON: an array of profiles (an export, or what the
    /// macOS app kept in UserDefaults) or a whole settings file. Profiles
    /// with an id that's already present replace it. Returns how many.
    pub fn import_json(&self, json: &str) -> Result<u32, String> {
        let profiles: Vec<HostProfile> = match serde_json::from_str::<Vec<HostProfile>>(json) {
            Ok(p) => p,
            Err(_) => serde_json::from_str::<AppData>(json).map_err(|e| e.to_string())?.profiles,
        };
        let count = profiles.len() as u32;
        self.change(|d| {
            for profile in profiles {
                match d.profiles.iter_mut().find(|p| p.id == profile.id) {
                    Some(p) => *p = profile,
                    None => d.profiles.push(profile),
                }
            }
        });
        Ok(count)
    }
}

fn write_atomic(path: &Path, bytes: &[u8]) -> std::io::Result<()> {
    if let Some(dir) = path.parent() {
        std::fs::create_dir_all(dir)?;
    }
    let tmp = path.with_extension("json.tmp");
    std::fs::write(&tmp, bytes)?;
    std::fs::rename(&tmp, path)
}

/// Where the app keeps its settings and log.
pub fn config_dir() -> Option<PathBuf> {
    let env = |k| std::env::var_os(k).filter(|v| !v.is_empty()).map(PathBuf::from);
    if cfg!(windows) {
        env("APPDATA").map(|d| d.join("MiSTerKVMS"))
    } else if cfg!(target_os = "macos") {
        env("HOME").map(|h| h.join("Library/Application Support/MiSTerKVMS"))
    } else {
        env("XDG_CONFIG_HOME")
            .or_else(|| env("HOME").map(|h| h.join(".config")))
            .map(|d| d.join("misterkvms"))
    }
}

/// A random UUID (v4) string, upper-case like the macOS app's.
fn new_id() -> String {
    let mut bytes = [0u8; 16];
    for (i, chunk) in bytes.chunks_mut(8).enumerate() {
        let mut h = RandomState::new().build_hasher();
        h.write_u128(SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| d.as_nanos()));
        h.write_usize(i);
        chunk.copy_from_slice(&h.finish().to_le_bytes());
    }
    bytes[6] = (bytes[6] & 0x0f) | 0x40;
    bytes[8] = (bytes[8] & 0x3f) | 0x80;
    let hex: String = bytes.iter().map(|b| format!("{b:02X}")).collect();
    format!("{}-{}-{}-{}-{}", &hex[0..8], &hex[8..12], &hex[12..16], &hex[16..20], &hex[20..32])
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ids_are_unique_uuids() {
        let (a, b) = (new_id(), new_id());
        assert_ne!(a, b);
        assert_eq!(a.len(), 36);
        assert_eq!(&a[14..15], "4");
    }

    #[test]
    fn reads_macos_userdefaults_json() {
        // As JSONEncoder wrote it; older entries lack newer fields.
        let json = r#"[{"id":"0B4E2C9A-1111-4222-8333-444455556666","name":"HP","misterHost":"192.168.1.50",
            "misterPort":8182,"obsHost":"","ndiSource":"OBS (MiSTer-1)","mouseSensitivity":1.5,
            "altClickIsMiddle":true,"scaleMode":"integer"}]"#;
        let store = ProfileStore::in_memory();
        assert_eq!(store.import_json(json), Ok(1));
        let p = &store.profiles()[0];
        assert_eq!(p.name, "HP");
        assert_eq!(p.mouse_sensitivity, 1.5);
        assert_eq!(p.display_settings().scale_mode, ScaleMode::Integer);
        assert!(p.auto_crop && p.predictive_cursor && !p.raw_mouse);
    }

    #[test]
    fn saves_and_reloads() {
        let dir = std::env::temp_dir().join(format!("kvms-test-{}", new_id()));
        let path = dir.join("profiles.json");
        let store = ProfileStore::open(path.clone());
        let mut p = HostProfile { name: "One".into(), ..Default::default() };
        store.upsert(p.clone());
        p.name = "Renamed".into();
        store.upsert(p.clone());
        store.change(|d| d.stats_pinned = true);
        let again = ProfileStore::open(path);
        assert_eq!(again.profiles(), vec![p.clone()]);
        assert!(again.data().stats_pinned);
        again.delete(&p.id);
        assert!(again.profiles().is_empty());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn mouse_change_resets_gain() {
        let before = HostProfile { cursor_gain: 1.2, cursor_lag_ms: 80.0, ..Default::default() };
        let mut after = HostProfile { mouse_sensitivity: 2.0, ..before.clone() };
        after.reset_learned_if_mouse_changed(&before);
        assert_eq!((after.cursor_gain, after.cursor_lag_ms), (0.0, 80.0));
    }
}

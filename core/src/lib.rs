//! MiSTerKVMS core: everything that isn't OS-specific UI or input capture.

uniffi::setup_scaffolding!();

pub mod controller;
pub mod input;
pub mod keymap;
pub mod ndi;
pub mod profiles;
pub mod session;
pub mod text;
pub mod video;

pub use controller::{Controller, ControllerEvent, ControllerObserver, ControllerState};
pub use input::{InputSettings, KeyResult, MouseButton};
pub use session::{ConnectionState, InputStats, MisterSession, SessionObserver};
pub use profiles::{HostProfile, ProfileStore};
pub use video::{DisplaySettings, MediaSink, NativeSurface, ScaleMode, VideoError, VideoSession, VideoStats};

/// Default mrext Remote port.
#[uniffi::export]
pub fn default_mrext_port() -> u16 {
    8182
}

#[uniffi::export]
pub fn default_input_settings() -> InputSettings {
    InputSettings::default()
}

/// One timestamped line on stderr (the apps send it to their log file).
pub fn log(msg: &str) {
    let t = std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0.0, |d| d.as_secs_f64());
    eprintln!("[{t:.3}] {msg}");
}

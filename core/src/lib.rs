//! MiSTerKVMS core: everything that isn't OS-specific UI or input capture.

uniffi::setup_scaffolding!();

pub mod input;
pub mod keymap;
pub mod ndi;
pub mod session;
pub mod text;
pub mod video;

pub use input::{InputSettings, KeyResult, MouseButton};
pub use session::{ConnectionState, InputStats, MisterSession, SessionObserver};
pub use video::{DisplaySettings, MediaSink, ScaleMode, VideoError, VideoSession, VideoStats};

/// Default mrext Remote port.
#[uniffi::export]
pub fn default_mrext_port() -> u16 {
    8182
}

#[uniffi::export]
pub fn default_input_settings() -> InputSettings {
    InputSettings::default()
}

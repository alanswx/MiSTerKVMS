//! Capture cards: list them, open one with a chosen format, and read frames
//! (video) and samples (audio). Used by `misterkvms-capture` and by the
//! apps' "capture card on this computer" source, so both see cards the same
//! way (docs/CAPTURE-SERVER.md §1, §3).
//!
//! Video backends: Media Foundation (Windows). AVFoundation (macOS) and V4L2
//! (Linux) come later; on those systems `devices()` is empty for now.
//! Audio goes through `cpal` everywhere.

pub mod audio;
mod clock;
#[cfg(windows)]
mod mf;

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use audio::{audio_devices, AudioCapture, AudioChunk};
pub use clock::now_100ns;

/// How a card delivers pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum PixelFormat {
    /// One JPEG per frame (most USB capture sticks).
    Mjpeg,
    /// 8-bit Y plane, then interleaved UV at half resolution.
    Nv12,
    /// Packed Y0 U Y1 V.
    Yuy2,
    /// 32-bit BGRA/BGRX, top row first.
    Bgra,
}

impl PixelFormat {
    pub fn name(self) -> &'static str {
        match self {
            PixelFormat::Mjpeg => "mjpeg",
            PixelFormat::Nv12 => "nv12",
            PixelFormat::Yuy2 => "yuy2",
            PixelFormat::Bgra => "bgra",
        }
    }

    pub fn is_compressed(self) -> bool {
        self == PixelFormat::Mjpeg
    }
}

/// One mode a card offers.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VideoFormat {
    pub pixel: PixelFormat,
    pub width: u32,
    pub height: u32,
    pub fps: f64,
}

impl fmt::Display for VideoFormat {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}x{}@{:.2} {}", self.width, self.height, self.fps, self.pixel.name())
    }
}

/// A video capture device.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct VideoDevice {
    /// Stable hardware id (Windows: the device's symbolic link). Two
    /// identical cards have the same name but different ids.
    pub id: String,
    pub name: String,
    pub formats: Vec<VideoFormat>,
}

/// One captured frame.
pub struct VideoFrame {
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    /// Bytes per row for uncompressed formats (the Y plane's for NV12).
    pub stride: u32,
    pub data: Vec<u8>,
    /// Capture time, 100 ns units since the Unix epoch.
    pub timestamp: i64,
}

#[derive(Debug, Clone, PartialEq)]
pub enum CaptureError {
    NotFound(String),
    /// Another program (often OBS) has the card open.
    InUse(String),
    /// The card refused the format.
    Format(String),
    /// Unplugged or stopped while running.
    Lost(String),
    Unsupported(String),
    Other(String),
}

impl fmt::Display for CaptureError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            CaptureError::NotFound(s) => write!(f, "capture card not found: {s}"),
            CaptureError::InUse(s) => write!(f, "capture card in use by another app: {s}"),
            CaptureError::Format(s) => write!(f, "capture format refused: {s}"),
            CaptureError::Lost(s) => write!(f, "capture card stopped: {s}"),
            CaptureError::Unsupported(s) => write!(f, "{s}"),
            CaptureError::Other(s) => write!(f, "{s}"),
        }
    }
}

impl std::error::Error for CaptureError {}

/// What a capture should hand out.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Output {
    /// Frames as the card sends them (MJPEG stays compressed). For the
    /// server, which passes MJPEG through untouched.
    Native,
    /// Always BGRA, decoded or converted by the OS. For local viewing.
    Bgra,
}

/// List video capture devices and their formats.
pub fn devices() -> Result<Vec<VideoDevice>, CaptureError> {
    #[cfg(windows)]
    {
        mf::devices()
    }
    #[cfg(not(windows))]
    {
        Ok(Vec::new())
    }
}

/// An open video capture. Frames are read on the caller's thread.
pub struct VideoCapture {
    #[cfg(windows)]
    inner: mf::Reader,
    format: VideoFormat,
}

impl VideoCapture {
    /// Open the device with id `id` (or, failing that, the first whose name
    /// matches) in `format`. With `format` None the best format is chosen:
    /// largest, then fastest, then MJPEG over raw.
    pub fn open(id: &str, format: Option<VideoFormat>, output: Output) -> Result<Self, CaptureError> {
        #[cfg(windows)]
        {
            let (inner, format) = mf::Reader::open(id, format, output)?;
            Ok(Self { inner, format })
        }
        #[cfg(not(windows))]
        {
            let _ = (id, format, output);
            Err(CaptureError::Unsupported("video capture isn't available on this system yet".into()))
        }
    }

    /// The format the card is running in.
    pub fn format(&self) -> VideoFormat {
        self.format
    }

    /// The next frame, or None after `timeout` without one (no signal, or a
    /// card that only sends on change).
    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, CaptureError> {
        #[cfg(windows)]
        {
            self.inner.next_frame(timeout)
        }
        #[cfg(not(windows))]
        {
            let _ = timeout;
            Err(CaptureError::Unsupported("video capture isn't available on this system yet".into()))
        }
    }
}

/// Pick the format to use when none was chosen: 1920x1080 (what MiSTer
/// outputs over HDMI by default) at the highest rate; otherwise the largest
/// 16:9-or-wider format at 50 fps or more; otherwise the largest. MJPEG
/// beats raw at equal size and rate (raw formats run slower over USB).
pub fn best_format(formats: &[VideoFormat]) -> Option<VideoFormat> {
    let key = |f: &VideoFormat| {
        (
            f.width == 1920 && f.height == 1080,
            f.fps >= 49.0 && f.width as f64 / f.height.max(1) as f64 >= 1.7, // 16:9 give or take (1360x768)
            f.fps >= 49.0,
            f.width as u64 * f.height as u64,
            (f.fps * 1000.0) as u64,
            f.pixel == PixelFormat::Mjpeg,
        )
    };
    formats.iter().copied().max_by(|a, b| key(a).cmp(&key(b)))
}

/// The audio device that belongs to a video device: capture cards show up
/// as e.g. "UGREEN 25773" (video) and "Digital Audio Interface (UGREEN
/// 25773)" (audio).
pub fn matching_audio(video_name: &str, audio_names: &[String]) -> Option<String> {
    let v = video_name.to_lowercase();
    audio_names
        .iter()
        .find(|a| {
            let a = a.to_lowercase();
            a.contains(&v) || v.contains(a.trim())
        })
        .cloned()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn best_format_on_a_ugreen_stick() {
        // 1080p only at 30; 60 fps tops out lower.
        let f = |pixel, width, height, fps| VideoFormat { pixel, width, height, fps };
        let formats = [
            f(PixelFormat::Mjpeg, 2560, 1440, 30.0),
            f(PixelFormat::Nv12, 1920, 1080, 30.0),
            f(PixelFormat::Mjpeg, 1920, 1080, 30.0),
            f(PixelFormat::Mjpeg, 1280, 1024, 60.0),
            f(PixelFormat::Mjpeg, 1360, 768, 60.0),
        ];
        assert_eq!(best_format(&formats), Some(formats[2]));
        assert_eq!(best_format(&formats[3..]), Some(formats[4]), "16:9 at 60 over 5:4 at 60");
    }

    #[test]
    fn best_format_prefers_size_then_rate_then_mjpeg() {
        let f = |pixel, width, height, fps| VideoFormat { pixel, width, height, fps };
        let formats = [
            f(PixelFormat::Yuy2, 1920, 1080, 5.0),
            f(PixelFormat::Mjpeg, 1920, 1080, 60.0),
            f(PixelFormat::Nv12, 1920, 1080, 60.0),
            f(PixelFormat::Mjpeg, 1280, 720, 60.0),
        ];
        assert_eq!(best_format(&formats), Some(formats[1]));
    }

    #[test]
    fn audio_matches_by_card_name() {
        let audio = vec![
            "Microphone (Steam Streaming Microphone)".to_string(),
            "Digital Audio Interface (UGREEN 25773)".to_string(),
        ];
        assert_eq!(matching_audio("UGREEN 25773", &audio).as_deref(), Some("Digital Audio Interface (UGREEN 25773)"));
        assert_eq!(matching_audio("AVerMedia Live Gamer 4K", &audio), None);
    }
}

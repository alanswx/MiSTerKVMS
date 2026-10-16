//! Capture cards: list them, open one with a chosen format, and read frames
//! (video) and samples (audio). Used by `misterkvms-capture` and by the
//! apps' "capture card on this computer" source, so both see cards the same
//! way (docs/CAPTURE-SERVER.md §1, §3).
//!
//! Video backends: Media Foundation (Windows), with DirectShow for cards
//! Media Foundation can't open (AVerMedia GC573 and other PCIe cards);
//! AVFoundation (macOS); V4L2 (Linux).
//! Audio goes through `cpal` everywhere.

pub mod audio;
#[cfg(target_os = "macos")]
mod avf;
mod clock;
#[cfg(windows)]
mod ds;
#[cfg(windows)]
mod mf;
pub mod pattern;
#[cfg(target_os = "linux")]
mod v4l2;

use std::fmt;
use std::time::Duration;

use serde::{Deserialize, Serialize};

pub use audio::{audio_devices, is_card_audio, AudioCapture, AudioChunk, AudioDevice, CARD_AUDIO};
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
    /// BGRA if the OS can convert cheaply and safely (Media Foundation);
    /// some backends (DirectShow) still hand out the card's own format, so
    /// callers must handle every `PixelFormat`. For local viewing.
    Bgra,
}

/// Show the built-in test pattern (`test:pattern`, with its own audio) in
/// `devices()` and `audio_devices()`.
pub fn set_test_pattern(on: bool) {
    pattern::set_listed(on);
}

/// List video capture devices and their formats.
pub fn devices() -> Result<Vec<VideoDevice>, CaptureError> {
    // Each card once: Media Foundation's entry when it can open the card,
    // DirectShow's otherwise (and for cards only DirectShow sees).
    #[cfg(windows)]
    let mut list = {
        let mf = mf::devices()?;
        let working: Vec<String> = mf.iter().filter(|d| !d.formats.is_empty()).map(|d| d.id.clone()).collect();
        let ds = ds::devices(&working).unwrap_or_default();
        let mut list: Vec<VideoDevice> = mf.iter().filter(|d| !d.formats.is_empty()).cloned().collect();
        for d in mf.into_iter().filter(|d| d.formats.is_empty()) {
            if !ds.iter().any(|x| x.id.strip_prefix(ds::PREFIX).is_some_and(|p| ds::same_device(p, &d.id))) {
                list.push(d);
            }
        }
        list.extend(ds);
        list
    };
    #[cfg(target_os = "macos")]
    let mut list = avf::devices()?;
    #[cfg(target_os = "linux")]
    let mut list = v4l2::devices()?;
    #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
    let mut list = Vec::new();
    if pattern::listed() {
        list.push(pattern::device());
    }
    Ok(list)
}

/// An open video capture. Frames are read on the caller's thread.
pub struct VideoCapture {
    inner: Inner,
    format: VideoFormat,
    audio: Option<AudioCapture>,
}

enum Inner {
    #[cfg(windows)]
    Mf(mf::Reader),
    #[cfg(windows)]
    Ds(ds::Reader),
    #[cfg(target_os = "macos")]
    Avf(avf::Reader),
    #[cfg(target_os = "linux")]
    V4l2(v4l2::Reader),
    Pattern(pattern::Pattern),
}

impl VideoCapture {
    /// Open the device with id `id` (or, failing that, the first whose name
    /// matches) in `format`. With `format` None the best format is chosen:
    /// largest, then fastest, then MJPEG over raw.
    pub fn open(id: &str, format: Option<VideoFormat>, output: Output) -> Result<Self, CaptureError> {
        Self::open_inner(id, format, output, false)
    }

    /// Like `open`, also capturing the card's own audio (for cards whose
    /// sound is a pin on the video device, like the GC573); take it with
    /// `take_audio`.
    pub fn open_with_audio(id: &str, format: Option<VideoFormat>, output: Output) -> Result<Self, CaptureError> {
        Self::open_inner(id, format, output, true)
    }

    /// The card's own audio, if opened with `open_with_audio` and the card
    /// has it.
    pub fn take_audio(&mut self) -> Option<AudioCapture> {
        self.audio.take()
    }

    fn open_inner(id: &str, format: Option<VideoFormat>, output: Output, audio: bool) -> Result<Self, CaptureError> {
        if id == pattern::ID {
            let (p, format) = pattern::Pattern::open(output);
            let audio = audio.then(|| p.audio());
            return Ok(Self { inner: Inner::Pattern(p), format, audio });
        }
        #[cfg(windows)]
        {
            let ds_open = |id: &str| {
                ds::Reader::open(id, format, output, audio).map(|(mut reader, format)| {
                    let audio = reader.audio.take();
                    Self { inner: Inner::Ds(reader), format, audio }
                })
            };
            if id.starts_with(ds::PREFIX) {
                return ds_open(id);
            }
            match mf::Reader::open(id, format, output) {
                Ok((reader, format)) => Ok(Self { inner: Inner::Mf(reader), format, audio: None }),
                // Media Foundation can't open some cards at all; DirectShow
                // usually can. A card in use stays an error either way.
                Err(e @ CaptureError::InUse(_)) => Err(e),
                Err(e) => ds_open(id).map_err(|_| e),
            }
        }
        #[cfg(target_os = "macos")]
        {
            // Card audio on macOS is a normal audio input (CoreAudio).
            let _ = audio;
            let (reader, format) = avf::Reader::open(id, format, output)?;
            Ok(Self { inner: Inner::Avf(reader), format, audio: None })
        }
        #[cfg(target_os = "linux")]
        {
            // Card audio on Linux is a normal ALSA input.
            let _ = audio;
            let (reader, format) = v4l2::Reader::open(id, format, output)?;
            Ok(Self { inner: Inner::V4l2(reader), format, audio: None })
        }
        #[cfg(not(any(windows, target_os = "macos", target_os = "linux")))]
        {
            let _ = (format, output, audio);
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
        match &mut self.inner {
            #[cfg(windows)]
            Inner::Mf(reader) => reader.next_frame(timeout),
            #[cfg(windows)]
            Inner::Ds(reader) => reader.next_frame(timeout),
            #[cfg(target_os = "macos")]
            Inner::Avf(reader) => reader.next_frame(timeout),
            #[cfg(target_os = "linux")]
            Inner::V4l2(reader) => reader.next_frame(timeout),
            Inner::Pattern(p) => Ok(p.next_frame(timeout)),
        }
    }
}

/// DirectShow diagnostics (audio inputs, pins per video device).
#[doc(hidden)]
pub fn describe_directshow() -> String {
    #[cfg(windows)]
    return ds::describe();
    #[cfg(not(windows))]
    "DirectShow is Windows-only
".into()
}

/// Pick the format to use when none was chosen: 1920x1080 (what MiSTer
/// outputs over HDMI by default); otherwise the largest 16:9-or-wider
/// format at 50 fps or more; otherwise the largest. Among those, the rate
/// closest to 60 (MiSTer's; 240 Hz modes only cost bandwidth), then MJPEG
/// over raw (raw formats run slower over USB).
pub fn best_format(formats: &[VideoFormat]) -> Option<VideoFormat> {
    let key = |f: &VideoFormat| {
        (
            f.width == 1920 && f.height == 1080,
            f.fps >= 49.0 && f.width as f64 / f.height.max(1) as f64 >= 1.7, // 16:9 give or take (1360x768)
            f.fps >= 49.0,
            f.width as u64 * f.height as u64,
            // Closest to 60 fps wins; below 49 the faster the better.
            -((f.fps.min(1000.0) - 60.0).abs() * 1000.0) as i64,
            f.pixel == PixelFormat::Mjpeg,
        )
    };
    formats.iter().copied().max_by(|a, b| key(a).cmp(&key(b)))
}

/// Rates offered from a range a card reports (DirectShow, AVFoundation):
/// the common ones inside it, plus its ends. Rounded to 0.01.
pub fn rates_in_range(min_fps: f64, max_fps: f64, extra: &[f64]) -> Vec<f64> {
    const COMMON: [f64; 10] = [24.0, 25.0, 30.0, 50.0, 59.94, 60.0, 100.0, 120.0, 144.0, 240.0];
    let (lo, hi) = (min_fps.min(max_fps), max_fps.max(min_fps));
    let mut rates: Vec<f64> = COMMON.iter().copied().filter(|r| *r >= lo - 0.01 && *r <= hi + 0.01).collect();
    rates.push(hi);
    rates.extend(extra.iter().copied().filter(|r| *r > 0.0));
    let mut rates: Vec<f64> = rates.into_iter().filter(|r| *r > 0.0).map(|r| (r * 100.0).round() / 100.0).collect();
    rates.sort_by(|a, b| b.total_cmp(a));
    rates.dedup_by(|a, b| (*a - *b).abs() < 0.02);
    rates
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
        // A GC573: 1080p at 60 and 240; NV12 and YUY2.
        let gc = [f(PixelFormat::Yuy2, 1920, 1080, 240.0), f(PixelFormat::Nv12, 1920, 1080, 60.0), f(PixelFormat::Nv12, 3840, 2160, 60.0)];
        assert_eq!(best_format(&gc), Some(gc[1]));
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
    fn rates_from_a_range() {
        // GC573 1080p: 1 to 240 fps, default 60.
        assert_eq!(rates_in_range(1.0, 240.0, &[60.0]), vec![240.0, 144.0, 120.0, 100.0, 60.0, 59.94, 50.0, 30.0, 25.0, 24.0]);
        assert_eq!(rates_in_range(30.0, 30.0, &[]), vec![30.0]);
        assert_eq!(rates_in_range(5.0, 29.97, &[]), vec![29.97, 25.0, 24.0]);
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

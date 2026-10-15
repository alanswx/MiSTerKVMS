//! Video/audio from a capture server stream or a card on this computer,
//! through a whole app session: frames, PNGs, WAV and MP4.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::Arc;

use kvms_core::video::MediaSink;
use kvms_core::video::crop::{self, Crop};

pub struct Frame {
    pub data: Vec<u8>,
    pub stride: usize,
    pub width: u32,
    pub height: u32,
    /// 100 ns units, this computer's clock (audio uses the same).
    pub timestamp: i64,
}

impl Frame {
    /// The picture area: MiSTer's borders removed unless `full`. Even sizes,
    /// as encoders want.
    pub fn area(&self, full: bool) -> Crop {
        let c = if full {
            Crop::full(self.width, self.height)
        } else {
            crop::detect(&self.data, self.stride, self.width, self.height)
                .unwrap_or(Crop::full(self.width, self.height))
        };
        Crop { w: c.w & !1, h: c.h & !1, ..c }
    }

    /// Rows of the area as tightly packed BGRA.
    pub fn bgra(&self, area: Crop) -> Vec<u8> {
        let mut out = Vec::with_capacity((area.w * area.h * 4) as usize);
        for y in area.y..area.y + area.h {
            let start = y as usize * self.stride + area.x as usize * 4;
            out.extend_from_slice(&self.data[start..start + area.w as usize * 4]);
        }
        out
    }

    pub fn save_png(&self, path: &Path, area: Crop) -> Result<(), String> {
        let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
        let mut enc = png::Encoder::new(std::io::BufWriter::new(file), area.w, area.h);
        enc.set_color(png::ColorType::Rgb);
        enc.set_depth(png::BitDepth::Eight);
        let mut writer = enc.write_header().map_err(|e| e.to_string())?;
        let mut rgb = Vec::with_capacity((area.w * area.h * 3) as usize);
        for px in self.bgra(area).chunks_exact(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
        writer.write_image_data(&rgb).map_err(|e| e.to_string())
    }

    /// Changed pixels (green channel) between two frames inside `area`.
    pub fn changed_pixels(&self, other: &Frame, area: Crop) -> usize {
        if (self.width, self.height) != (other.width, other.height) {
            return usize::MAX;
        }
        let mut n = 0;
        for y in area.y..area.y + area.h {
            let row = y as usize * self.stride;
            for x in area.x..area.x + area.w {
                let i = row + x as usize * 4 + 1;
                if self.data[i].abs_diff(other.data[i]) > 24 {
                    n += 1;
                }
            }
        }
        n
    }
}

pub enum Item {
    Video(Frame),
    /// Interleaved samples.
    Audio { samples: Vec<f32>, channels: u32, rate: u32, timestamp: i64 },
}

/// Pictures and sound as the session receives them: the feed the apps'
/// recorders get. Video is bounded (a slow reader skips frames), audio
/// isn't (it's small, and gaps would be audible).
struct ChannelSink {
    video: SyncSender<Frame>,
    audio: mpsc::Sender<Item>,
}

impl MediaSink for ChannelSink {
    fn video_frame(&self, data: u64, stride: u32, width: u32, height: u32, timestamp: i64) {
        if width == 0 || height == 0 {
            return;
        }
        let len = stride as usize * (height as usize - 1) + width as usize * 4;
        // SAFETY: the session passes a picture of `height` rows `stride`
        // bytes apart, valid for the duration of this call.
        let pixels = unsafe { std::slice::from_raw_parts(data as *const u8, len) };
        let _ = self.video.try_send(Frame { data: pixels.to_vec(), stride: stride as usize, width, height, timestamp });
    }

    fn audio_frame(&self, samples: Vec<f32>, channels: u32, sample_rate: u32, timestamp: i64) {
        let _ = self.audio.send(Item::Audio { samples, channels, rate: sample_rate, timestamp });
    }
}

pub struct Source {
    session: crate::session::Session,
    video: Receiver<Frame>,
    audio: Receiver<Item>,
}

impl Source {
    /// Open the source and wait for its picture. Borders are left in
    /// (callers crop with `Frame::area`).
    pub fn open(source: crate::session::SessionSource) -> Result<Self, String> {
        let session = crate::session::open(source, false)?;
        session.wait_connected(Duration::from_secs(15))?;
        let (video_tx, video) = mpsc::sync_channel(8);
        let (audio_tx, audio) = mpsc::channel();
        session
            .core
            .start_recording_with_sink(Arc::new(ChannelSink { video: video_tx, audio: audio_tx }))
            .map_err(|e| e.to_string())?;
        Ok(Self { session, video, audio })
    }

    /// Frames per second arriving now (0 until measured).
    pub fn fps(&self) -> f64 {
        self.session.core.state().video_stats.map_or(0.0, |v| v.received_fps)
    }

    pub fn next(&self, timeout_ms: u32) -> Option<Item> {
        if let Ok(a) = self.audio.try_recv() {
            return Some(a);
        }
        let until = Instant::now() + Duration::from_millis(timeout_ms as u64);
        loop {
            match self.video.recv_timeout(Duration::from_millis(5)) {
                Ok(f) => return Some(Item::Video(f)),
                Err(RecvTimeoutError::Disconnected) => return None,
                Err(RecvTimeoutError::Timeout) => {}
            }
            if let Ok(a) = self.audio.try_recv() {
                return Some(a);
            }
            if Instant::now() >= until {
                return None;
            }
        }
    }

    /// The next video frame, waiting up to `timeout`.
    pub fn frame(&self, timeout: Duration) -> Result<Frame, String> {
        let until = Instant::now() + timeout;
        while Instant::now() < until {
            if let Some(Item::Video(f)) = self.next(100) {
                return Ok(f);
            }
        }
        Err("no video from the source".into())
    }

    /// A frame that's current now: skip whatever was queued, then take the next.
    pub fn fresh_frame(&self, timeout: Duration) -> Result<Frame, String> {
        self.frame(timeout)?;
        while self.next(0).is_some() {}
        self.frame(timeout)
    }
}

impl Drop for Source {
    fn drop(&mut self) {
        self.session.core.stop_recording();
        self.session.core.recording_finished(None, None);
    }
}
pub fn write_wav(path: &Path, samples: &[f32], channels: u32, rate: u32) -> Result<(), String> {
    let data_len = (samples.len() * 2) as u32;
    let mut out = Vec::with_capacity(44 + data_len as usize);
    out.extend_from_slice(b"RIFF");
    out.extend_from_slice(&(36 + data_len).to_le_bytes());
    out.extend_from_slice(b"WAVEfmt ");
    out.extend_from_slice(&16u32.to_le_bytes());
    out.extend_from_slice(&1u16.to_le_bytes()); // PCM
    out.extend_from_slice(&(channels as u16).to_le_bytes());
    out.extend_from_slice(&rate.to_le_bytes());
    out.extend_from_slice(&(rate * channels * 2).to_le_bytes());
    out.extend_from_slice(&((channels * 2) as u16).to_le_bytes());
    out.extend_from_slice(&16u16.to_le_bytes());
    out.extend_from_slice(b"data");
    out.extend_from_slice(&data_len.to_le_bytes());
    for s in samples {
        out.extend_from_slice(&((s.clamp(-1.0, 1.0) * 32767.0) as i16).to_le_bytes());
    }
    std::fs::write(path, out).map_err(|e| format!("{}: {e}", path.display()))
}

pub struct Recording {
    pub frames: u64,
    pub audio_seconds: f64,
    pub size: (u32, u32),
    pub fps: f64,
}

/// Record for `duration`. `.wav` writes audio only; `.mp4` (needs ffmpeg)
/// writes H.264 video + AAC audio.
pub fn record(src: &Source, duration: Duration, out: &Path, full: bool) -> Result<Recording, String> {
    let video = match out.extension().and_then(|e| e.to_str()) {
        Some("wav") => false,
        Some("mp4") | Some("mov") | Some("mkv") => true,
        _ => return Err("output must end in .wav, .mp4, .mov or .mkv".into()),
    };
    let first = src.fresh_frame(Duration::from_secs(5))?;
    let area = first.area(full);
    // What the source sends, rounded (59.94 stays 59.94); 60 until measured.
    let fps = match src.fps() {
        f if f > 1.0 => (f * 100.0).round() / 100.0,
        _ => 60.0,
    };
    let start_ts = first.timestamp;
    let tmp = std::env::temp_dir().join(format!("misterkvms-rec-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let tmp_video = tmp.join("video.mp4");
    let tmp_audio = tmp.join("audio.wav");

    let mut encoder = if video {
        Some(
            Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgra"])
                .args(["-s", &format!("{}x{}", area.w, area.h), "-framerate", &format!("{fps:.3}")])
                .args(["-i", "-", "-c:v", "libx264", "-preset", "veryfast", "-crf", "16", "-pix_fmt", "yuv420p"])
                .arg(&tmp_video)
                .stdin(Stdio::piped())
                .spawn()
                .map_err(|e| format!("ffmpeg (needed for video recording): {e}"))?,
        )
    } else {
        None
    };

    let (mut frames, mut audio, mut channels, mut rate) = (0u64, Vec::<f32>::new(), 2u32, 48_000u32);
    let feed = |f: &Frame, enc: &mut Option<std::process::Child>| -> Result<(), String> {
        if let Some(enc) = enc.as_mut() {
            if (f.width, f.height) == (first.width, first.height) {
                enc.stdin.as_mut().unwrap().write_all(&f.bgra(area)).map_err(|e| e.to_string())?;
            }
        }
        Ok(())
    };
    feed(&first, &mut encoder)?;
    frames += 1;
    let until = Instant::now() + duration;
    while Instant::now() < until {
        match src.next(100) {
            Some(Item::Video(f)) => {
                feed(&f, &mut encoder)?;
                frames += 1;
            }
            Some(Item::Audio { samples, channels: c, rate: r, timestamp }) => {
                // Start the audio where the video starts.
                let skip = if timestamp < start_ts {
                    (((start_ts - timestamp) as f64 / 1e7) * r as f64) as usize * c as usize
                } else {
                    0
                };
                if skip < samples.len() {
                    audio.extend_from_slice(&samples[skip..]);
                }
                channels = c;
                rate = r;
            }
            None => {}
        }
    }
    write_wav(if video { &tmp_audio } else { out }, &audio, channels, rate)?;
    if let Some(mut enc) = encoder {
        drop(enc.stdin.take());
        let status = enc.wait().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("ffmpeg failed encoding video".into());
        }
        let status = Command::new("ffmpeg")
            .args(["-hide_banner", "-loglevel", "error", "-y", "-i"])
            .arg(&tmp_video)
            .arg("-i")
            .arg(&tmp_audio)
            .args(["-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-shortest"])
            .arg(out)
            .status()
            .map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("ffmpeg failed adding audio".into());
        }
    }
    let _ = std::fs::remove_dir_all(&tmp);
    Ok(Recording {
        frames,
        audio_seconds: audio.len() as f64 / channels.max(1) as f64 / rate.max(1) as f64,
        size: (area.w, area.h),
        fps,
    })
}

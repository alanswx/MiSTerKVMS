//! Video/audio from the MiSTer's NDI stream: frames, PNGs, WAV and MP4.

use std::io::Write;
use std::path::Path;
use std::process::{Command, Stdio};
use std::time::{Duration, Instant};

use kvms_core::ndi::{self, Captured, Receiver};
use kvms_core::video::crop::{self, Crop};

pub struct Frame {
    pub data: Vec<u8>,
    pub stride: usize,
    pub width: u32,
    pub height: u32,
    /// 100 ns units, sender clock.
    pub timestamp: i64,
    pub fps: f64,
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

pub struct Source {
    rx: Receiver,
}

pub enum Item {
    Video(Frame),
    /// Interleaved samples.
    Audio { samples: Vec<f32>, channels: u32, rate: u32, timestamp: i64 },
}

impl Source {
    pub fn open(name: &str, extra_ip: &str) -> Result<Self, String> {
        let ndi = ndi::ndi()?;
        let rx = Receiver::new(ndi, name, extra_ip, "misterkvms-cli").ok_or("could not create NDI receiver")?;
        Ok(Self { rx })
    }

    pub fn next(&self, timeout_ms: u32) -> Option<Item> {
        match self.rx.capture(timeout_ms) {
            Captured::Video(v) => {
                let fps = v.frame.frame_rate_N as f64 / v.frame.frame_rate_D.max(1) as f64;
                Some(Item::Video(Frame {
                    data: v.data().to_vec(),
                    stride: v.stride(),
                    width: v.frame.xres as u32,
                    height: v.frame.yres as u32,
                    timestamp: v.frame.timestamp,
                    fps,
                }))
            }
            Captured::Audio(a) if a.frame.FourCC == ndi::sys::NDIlib_FourCC_audio_type_FLTP => {
                let ch = a.frame.no_channels.max(1) as usize;
                let planes: Vec<&[f32]> = (0..ch).map(|c| a.channel(c)).collect();
                let n = a.frame.no_samples as usize;
                let mut samples = Vec::with_capacity(n * ch);
                for i in 0..n {
                    for p in &planes {
                        samples.push(p[i]);
                    }
                }
                Some(Item::Audio {
                    samples,
                    channels: ch as u32,
                    rate: a.frame.sample_rate as u32,
                    timestamp: a.frame.timestamp,
                })
            }
            _ => None,
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
        Err("no video from the NDI source (is OBS sending it?)".into())
    }

    /// A frame that's current now: skip whatever was queued, then take the next.
    pub fn fresh_frame(&self, timeout: Duration) -> Result<Frame, String> {
        self.frame(timeout)?;
        while self.next(0).is_some() {}
        self.frame(timeout)
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
    let start_ts = first.timestamp;
    let tmp = std::env::temp_dir().join(format!("misterkvms-rec-{}", std::process::id()));
    std::fs::create_dir_all(&tmp).map_err(|e| e.to_string())?;
    let tmp_video = tmp.join("video.mp4");
    let tmp_audio = tmp.join("audio.wav");

    let mut encoder = if video {
        Some(
            Command::new("ffmpeg")
                .args(["-hide_banner", "-loglevel", "error", "-y", "-f", "rawvideo", "-pix_fmt", "bgra"])
                .args(["-s", &format!("{}x{}", area.w, area.h), "-framerate", &format!("{:.3}", first.fps)])
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
        fps: first.fps,
    })
}

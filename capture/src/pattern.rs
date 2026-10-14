//! A built-in test pattern, `test:pattern`: moving rainbow bars at 1280x720,
//! 60 fps, with a white box that steps once a second, and its own audio (a
//! short beep each time the box steps, to check sound and sync). For trying
//! out streams, viewers and soak runs without a capture card. Listed by
//! `devices()` when switched on (`set_listed`, the setup page's "test
//! pattern") or `MISTERKVMS_TEST_PATTERN` is set; it can always be opened by id.

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc};
use std::time::{Duration, Instant};

use crate::audio::{AudioCapture, AudioChunk, AudioDevice, CARD_AUDIO};
use crate::clock::now_100ns;
use crate::{Output, PixelFormat, VideoDevice, VideoFormat, VideoFrame};

pub const ID: &str = "test:pattern";
const FORMAT: VideoFormat = VideoFormat { pixel: PixelFormat::Bgra, width: 1280, height: 720, fps: 60.0 };
const RATE: u32 = 48_000;
const CHANNELS: u16 = 2;

static LISTED: AtomicBool = AtomicBool::new(false);

pub fn set_listed(on: bool) {
    LISTED.store(on, Ordering::Relaxed);
}

pub fn listed() -> bool {
    LISTED.load(Ordering::Relaxed) || std::env::var_os("MISTERKVMS_TEST_PATTERN").is_some()
}

pub fn audio_device() -> AudioDevice {
    AudioDevice { id: format!("{CARD_AUDIO}{ID}"), name: "Test pattern (card audio)".into() }
}

/// The pattern's sound: silence, with a 1 kHz beep for the first 100 ms of
/// every second since `start` (when the box steps). Chunks of 10 ms, in
/// real time.
fn tone(start: Instant, start_ts: i64) -> AudioCapture {
    let stop = Arc::new(AtomicBool::new(false));
    let (tx, rx) = mpsc::sync_channel(256);
    let thread_stop = stop.clone();
    let _ = std::thread::Builder::new().name("test-tone".into()).spawn(move || {
        const CHUNK: u64 = RATE as u64 / 100;
        let mut n: u64 = 0;
        while !thread_stop.load(Ordering::Relaxed) {
            let due = start + Duration::from_secs_f64((n + CHUNK) as f64 / RATE as f64);
            if let Some(wait) = due.checked_duration_since(Instant::now()) {
                std::thread::sleep(wait);
            }
            let mut samples = Vec::with_capacity(CHUNK as usize * CHANNELS as usize);
            for i in n..n + CHUNK {
                let in_second = i % RATE as u64;
                let v = if in_second < RATE as u64 / 10 {
                    ((i as f64 * 1000.0 * std::f64::consts::TAU / RATE as f64).sin() * 12_000.0) as i16
                } else {
                    0
                };
                samples.extend([v; CHANNELS as usize]);
            }
            let timestamp = start_ts + (n * 10_000_000 / RATE as u64) as i64;
            if tx.send(Ok(AudioChunk { channels: CHANNELS, rate: RATE, samples, timestamp })).is_err() {
                break;
            }
            n += CHUNK;
        }
    });
    AudioCapture::from_channel(rx, stop, CHANNELS, RATE)
}

pub fn device() -> VideoDevice {
    VideoDevice { id: ID.into(), name: "Test pattern".into(), formats: vec![FORMAT] }
}

pub struct Pattern {
    started: Instant,
    started_ts: i64,
    frame: u64,
}

impl Pattern {
    pub fn open(_output: Output) -> (Self, VideoFormat) {
        (Self { started: Instant::now(), started_ts: now_100ns(), frame: 0 }, FORMAT)
    }

    /// Its sound, in step with the picture.
    pub fn audio(&self) -> AudioCapture {
        tone(self.started, self.started_ts)
    }

    pub fn next_frame(&mut self, timeout: Duration) -> Option<VideoFrame> {
        let due = self.started + Duration::from_secs_f64(self.frame as f64 / FORMAT.fps);
        let now = Instant::now();
        if due > now + timeout {
            std::thread::sleep(timeout);
            return None;
        }
        if due > now {
            std::thread::sleep(due - now);
        }
        let (w, h) = (FORMAT.width as usize, FORMAT.height as usize);
        let mut data = vec![0u8; w * h * 4];
        let offset = (self.frame * 4) as usize;
        for y in 0..h {
            let row = &mut data[y * w * 4..(y + 1) * w * 4];
            for x in 0..w {
                let bar = ((x + offset) / 160) % 8;
                let [r, g, b] = [
                    if bar & 1 != 0 { 230 } else { 20 },
                    if bar & 2 != 0 { 230 } else { 20 },
                    if bar & 4 != 0 { 230 } else { 20 },
                ];
                // A white box that steps once a second, to see motion and
                // check for frozen pictures.
                let in_box = y >= 300 && y < 420 && x >= ((self.frame / 60) % 8) as usize * 160 && x < ((self.frame / 60) % 8) as usize * 160 + 120;
                let px = if in_box { [255, 255, 255, 255] } else { [b, g, r, 255] };
                row[x * 4..x * 4 + 4].copy_from_slice(&px);
            }
        }
        self.frame += 1;
        let timestamp = self.started_ts + ((self.frame - 1) as f64 * 10_000_000.0 / FORMAT.fps) as i64;
        Some(VideoFrame { format: PixelFormat::Bgra, width: FORMAT.width, height: FORMAT.height, stride: FORMAT.width * 4, data, timestamp })
    }
}

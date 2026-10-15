//! Plays the source's audio on the default output device with a small, bounded buffer
//! so sound stays close to the picture.

use std::collections::VecDeque;
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};

/// Buffer we aim to keep; more than MAX gets trimmed back to TARGET.
const TARGET_MS: u32 = 40;
const MAX_MS: u32 = 120;

pub struct AudioOut {
    shared: Arc<Shared>,
    pub rate: u32,
    pub channels: usize,
}

struct Shared {
    buffer: Mutex<VecDeque<f32>>,
    muted: AtomicBool,
    volume: AtomicU32,
    stop: AtomicBool,
    underruns: AtomicU32,
}

impl AudioOut {
    /// Opens the default output device on its own thread (cpal streams aren't
    /// Send on every platform).
    pub fn start() -> Result<Self, String> {
        let shared = Arc::new(Shared {
            buffer: Mutex::new(VecDeque::new()),
            muted: AtomicBool::new(false),
            volume: AtomicU32::new(1.0f32.to_bits()),
            stop: AtomicBool::new(false),
            underruns: AtomicU32::new(0),
        });
        let (tx, rx) = std::sync::mpsc::channel();
        let thread_shared = shared.clone();
        thread::Builder::new()
            .name("audio-out".into())
            .spawn(move || {
                let result = (|| {
                    let device = cpal::default_host()
                        .default_output_device()
                        .ok_or("no audio output device")?;
                    let config = device.default_output_config().map_err(|e| e.to_string())?;
                    let rate = config.sample_rate();
                    let channels = config.channels() as usize;
                    let cb_shared = thread_shared.clone();
                    let stream = device
                        .build_output_stream(
                            config.config(),
                            move |out: &mut [f32], _| cb_shared.fill(out),
                            |e| eprintln!("audio stream error: {e}"),
                            None,
                        )
                        .map_err(|e| e.to_string())?;
                    stream.play().map_err(|e| e.to_string())?;
                    Ok::<_, String>((stream, rate, channels))
                })();
                match result {
                    Ok((stream, rate, channels)) => {
                        let _ = tx.send(Ok((rate, channels)));
                        while !thread_shared.stop.load(Ordering::Relaxed) {
                            thread::park_timeout(std::time::Duration::from_millis(200));
                        }
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| e.to_string())?;
        let (rate, channels) = rx.recv().map_err(|e| e.to_string())??;
        Ok(Self { shared, rate, channels })
    }

    /// Queue planar input audio, converting channel count and sample rate.
    pub fn push(&self, planes: &[&[f32]], in_rate: u32) {
        if planes.is_empty() || planes[0].is_empty() {
            return;
        }
        let in_len = planes[0].len();
        let out_len = ((in_len as u64 * self.rate as u64) / in_rate.max(1) as u64) as usize;
        let step = in_len as f64 / out_len.max(1) as f64;
        let mut buf = self.shared.buffer.lock().unwrap();
        for i in 0..out_len {
            // Linear interpolation; fine for speech/system sounds and avoids a resampler dependency.
            let pos = i as f64 * step;
            let i0 = pos as usize;
            let i1 = (i0 + 1).min(in_len - 1);
            let t = (pos - i0 as f64) as f32;
            for ch in 0..self.channels {
                let src = planes[ch.min(planes.len() - 1)];
                buf.push_back(src[i0] + (src[i1] - src[i0]) * t);
            }
        }
        let frames = buf.len() / self.channels;
        if frames > self.ms_to_frames(MAX_MS) {
            let drop = (frames - self.ms_to_frames(TARGET_MS)) * self.channels;
            buf.drain(..drop);
        }
    }

    pub fn buffered_ms(&self) -> f64 {
        let frames = self.shared.buffer.lock().unwrap().len() / self.channels;
        frames as f64 * 1000.0 / self.rate as f64
    }

    pub fn underruns(&self) -> u32 {
        self.shared.underruns.load(Ordering::Relaxed)
    }

    pub fn set_muted(&self, muted: bool) {
        self.shared.muted.store(muted, Ordering::Relaxed);
        if muted {
            self.shared.buffer.lock().unwrap().clear();
        }
    }

    pub fn set_volume(&self, volume: f32) {
        self.shared.volume.store(volume.clamp(0.0, 1.0).to_bits(), Ordering::Relaxed);
    }

    pub fn is_muted(&self) -> bool {
        self.shared.muted.load(Ordering::Relaxed)
    }

    fn ms_to_frames(&self, ms: u32) -> usize {
        (self.rate as u64 * ms as u64 / 1000) as usize
    }
}

impl Shared {
    fn fill(&self, out: &mut [f32]) {
        let muted = self.muted.load(Ordering::Relaxed);
        let volume = f32::from_bits(self.volume.load(Ordering::Relaxed));
        let mut buf = self.buffer.lock().unwrap();
        let mut short = false;
        for s in out.iter_mut() {
            *s = match buf.pop_front() {
                Some(v) if !muted => v * volume,
                Some(_) => 0.0,
                None => {
                    short = true;
                    0.0
                }
            };
        }
        if short && !muted {
            self.underruns.fetch_add(1, Ordering::Relaxed);
        }
    }
}

impl Drop for AudioOut {
    fn drop(&mut self) {
        self.shared.stop.store(true, Ordering::Relaxed);
    }
}

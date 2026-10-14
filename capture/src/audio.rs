//! Audio from a capture card's own audio device, as 16-bit interleaved PCM.

use std::str::FromStr;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::Arc;
use std::thread;
use std::time::Duration;

use cpal::traits::{DeviceTrait, HostTrait, StreamTrait};
use serde::{Deserialize, Serialize};

use crate::clock::now_100ns;
use crate::CaptureError;

#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AudioDevice {
    /// cpal's id ("wasapi:{...}"), stable across restarts.
    pub id: String,
    pub name: String,
}

/// Audio ids starting with this come from a capture card's own audio pin,
/// opened together with its video (`VideoCapture::open_with_audio`).
pub const CARD_AUDIO: &str = "card:";

/// Whether an audio id (or name, as the setup page stores it) is a card's
/// own audio rather than a system input.
pub fn is_card_audio(id_or_name: &str) -> bool {
    id_or_name.starts_with(CARD_AUDIO) || id_or_name.ends_with(" (card audio)")
}

/// Input devices (microphones, capture card audio, ...), then audio that
/// comes from a card's own pin.
pub fn audio_devices() -> Vec<AudioDevice> {
    let mut list: Vec<AudioDevice> = match cpal::default_host().input_devices() {
        Ok(devices) => devices
            .filter_map(|d| {
                let name = d.description().ok()?.name().to_string();
                let id = d.id().ok()?.to_string();
                Some(AudioDevice { id, name })
            })
            .collect(),
        Err(_) => Vec::new(),
    };
    #[cfg(windows)]
    list.extend(crate::ds::audio_devices());
    list
}

pub struct AudioChunk {
    pub channels: u16,
    pub rate: u32,
    /// Interleaved.
    pub samples: Vec<i16>,
    /// Capture time of the first sample, 100 ns since the Unix epoch.
    pub timestamp: i64,
}

/// A running audio capture. Chunks arrive every few milliseconds; if the
/// reader falls behind by more than about a second, chunks are dropped.
pub struct AudioCapture {
    rx: Receiver<Result<AudioChunk, CaptureError>>,
    stop: Arc<AtomicBool>,
    pub channels: u16,
    pub rate: u32,
}

impl AudioCapture {
    /// Audio fed by a video backend (a card's own audio pin).
    #[cfg_attr(not(windows), allow(dead_code))]
    pub(crate) fn from_channel(rx: Receiver<Result<AudioChunk, CaptureError>>, stop: Arc<AtomicBool>, channels: u16, rate: u32) -> Self {
        Self { rx, stop, channels, rate }
    }

    /// Open the input device with id `id`, or else the first whose name is
    /// `id`.
    pub fn open(id: &str) -> Result<Self, CaptureError> {
        if is_card_audio(id) {
            return Err(CaptureError::Unsupported("card audio comes with its video (open_with_audio)".into()));
        }
        let stop = Arc::new(AtomicBool::new(false));
        let (tx, rx) = mpsc::sync_channel(256);
        let (ready_tx, ready_rx) = mpsc::channel();
        let id = id.to_string();
        let thread_stop = stop.clone();
        thread::Builder::new()
            .name("capture-audio".into())
            .spawn(move || {
                // cpal streams aren't Send everywhere: build and hold it here.
                match start(&id, tx.clone()) {
                    Ok((stream, channels, rate)) => {
                        let _ = ready_tx.send(Ok((channels, rate)));
                        while !thread_stop.load(Ordering::Relaxed) {
                            thread::park_timeout(Duration::from_millis(200));
                        }
                        drop(stream);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| CaptureError::Other(e.to_string()))?;
        let (channels, rate) = ready_rx.recv().map_err(|e| CaptureError::Other(e.to_string()))??;
        Ok(Self { rx, stop, channels, rate })
    }

    /// The next chunk, or None after `timeout`.
    pub fn next_chunk(&self, timeout: Duration) -> Result<Option<AudioChunk>, CaptureError> {
        match self.rx.recv_timeout(timeout) {
            Ok(r) => r.map(Some),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(CaptureError::Lost("audio stopped".into())),
        }
    }
}

impl Drop for AudioCapture {
    fn drop(&mut self) {
        self.stop.store(true, Ordering::Relaxed);
    }
}

fn find(id: &str) -> Result<cpal::Device, CaptureError> {
    let host = cpal::default_host();
    if let Ok(parsed) = cpal::DeviceId::from_str(id) {
        if let Some(d) = host.device_by_id(&parsed) {
            return Ok(d);
        }
    }
    host.input_devices()
        .map_err(|e| CaptureError::Other(e.to_string()))?
        .find(|d| d.description().is_ok_and(|desc| desc.name() == id))
        .ok_or_else(|| CaptureError::NotFound(id.to_string()))
}

type Tx = SyncSender<Result<AudioChunk, CaptureError>>;

fn start(id: &str, tx: Tx) -> Result<(cpal::Stream, u16, u32), CaptureError> {
    let device = find(id)?;
    let config = device.default_input_config().map_err(|e| CaptureError::Other(e.to_string()))?;
    let channels = config.channels();
    let rate = config.sample_rate();
    let format = config.sample_format();
    let send = move |tx: &Tx, samples: Vec<i16>| {
        let frames = samples.len() as i64 / channels.max(1) as i64;
        // The callback runs when the buffer is full: its first sample is
        // one buffer older than now.
        let timestamp = now_100ns() - frames * 10_000_000 / rate.max(1) as i64;
        if let Err(TrySendError::Disconnected(_)) = tx.try_send(Ok(AudioChunk { channels, rate, samples, timestamp })) {
            // Reader gone; the stream stops when the thread exits.
        }
    };
    let err_tx = tx.clone();
    let on_error = move |e: cpal::Error| {
        let _ = err_tx.try_send(Err(CaptureError::Lost(e.to_string())));
    };
    let stream = match format {
        cpal::SampleFormat::F32 => device.build_input_stream(
            config.config(),
            move |data: &[f32], _: &_| {
                send(&tx, data.iter().map(|s| (s.clamp(-1.0, 1.0) * 32767.0) as i16).collect());
            },
            on_error,
            None,
        ),
        cpal::SampleFormat::I16 => device.build_input_stream(
            config.config(),
            move |data: &[i16], _: &_| send(&tx, data.to_vec()),
            on_error,
            None,
        ),
        cpal::SampleFormat::I32 => device.build_input_stream(
            config.config(),
            move |data: &[i32], _: &_| send(&tx, data.iter().map(|s| (s >> 16) as i16).collect()),
            on_error,
            None,
        ),
        other => return Err(CaptureError::Format(format!("audio sample format {other:?}"))),
    }
    .map_err(|e| CaptureError::Other(e.to_string()))?;
    stream.play().map_err(|e| CaptureError::Other(e.to_string()))?;
    Ok((stream, channels, rate))
}

//! A capture card on this computer: frames straight from `kvms-capture`,
//! no server and no network (docs/CAPTURE-SERVER.md §3). Media Foundation
//! hands out BGRA; frames in the card's own format (DirectShow) are
//! converted here.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use std::sync::Mutex;

use kvms_capture::{is_card_audio, AudioCapture, CaptureError, Output, PixelFormat, VideoCapture, VideoFrame};
use kvms_codec::{convert, jpeg, Bgra};

use super::{deinterleave, deliver_audio, Deliver, Shared};
use crate::log;

const RETRY: Duration = Duration::from_secs(2);
/// No frame for this long: say so (the card has no signal).
const NO_SIGNAL_AFTER: Duration = Duration::from_secs(1);

fn set_state(shared: &Shared, state: &str) {
    let mut c = shared.counters.lock().unwrap();
    if c.state != state {
        log(&format!("video: local {} -> {state}", c.state));
        c.state = state.into();
    }
}

fn describe(e: &CaptureError) -> String {
    match e {
        CaptureError::InUse(_) => "capture card in use by another app (close OBS or the capture server)".into(),
        CaptureError::NotFound(_) => "capture card not found (unplugged?)".into(),
        other => other.to_string(),
    }
}

pub(super) fn receive_loop(video: String, audio: String, shared: Arc<Shared>) {
    let stopped = || shared.stop.load(Ordering::Relaxed);
    let card_audio = is_card_audio(&audio);
    // Card audio is opened with the video and handed to the audio thread.
    let handover: Arc<Mutex<Option<AudioCapture>>> = Arc::new(Mutex::new(None));
    let audio_thread = (!audio.is_empty()).then(|| {
        let shared = shared.clone();
        let handover = handover.clone();
        thread::Builder::new().name("capture-audio-receive".into()).spawn(move || audio_loop(audio, handover, shared)).ok()
    });
    let mut deliver = Deliver::default();
    let mut picture = Bgra::default();
    while !stopped() {
        let opened = if card_audio {
            VideoCapture::open_with_audio(&video, None, Output::Bgra)
        } else {
            VideoCapture::open(&video, None, Output::Bgra)
        };
        let mut cap = match opened {
            Ok(c) => c,
            Err(e) => {
                set_state(&shared, &describe(&e));
                wait(&shared, RETRY);
                continue;
            }
        };
        log(&format!("video: local capture {}", cap.format()));
        if card_audio {
            *handover.lock().unwrap() = cap.take_audio();
        }
        set_state(&shared, "connecting");
        let mut last_frame = Instant::now();
        while !stopped() {
            match cap.next_frame(Duration::from_millis(100)) {
                Ok(Some(f)) => {
                    last_frame = Instant::now();
                    if f.format == PixelFormat::Bgra {
                        deliver.video(&shared, &f.data, f.stride, f.width, f.height, f.timestamp);
                    } else if let Err(e) = to_bgra(&f, &mut picture) {
                        log(&format!("video: local {e}"));
                        continue;
                    } else {
                        deliver.video(&shared, &picture.data, picture.stride, picture.width, picture.height, f.timestamp);
                    }
                    set_state(&shared, "connected");
                }
                Ok(None) => {
                    if last_frame.elapsed() >= NO_SIGNAL_AFTER {
                        set_state(&shared, "no video");
                    }
                }
                Err(e) => {
                    set_state(&shared, &describe(&e));
                    break;
                }
            }
        }
        drop(cap);
        wait(&shared, RETRY);
    }
    if let Some(Some(t)) = audio_thread {
        let _ = t.join();
    }
}

/// A frame the backend didn't convert (DirectShow hands out the card's own
/// format).
fn to_bgra(f: &VideoFrame, out: &mut Bgra) -> Result<(), String> {
    match f.format {
        PixelFormat::Mjpeg => jpeg::decode(&f.data, out).map_err(|e| e.to_string()),
        PixelFormat::Nv12 => {
            convert::nv12_to_bgra(&f.data, f.width, f.height, f.stride, out);
            Ok(())
        }
        PixelFormat::Yuy2 => {
            convert::yuy2_to_bgra(&f.data, f.width, f.height, f.stride, out);
            Ok(())
        }
        PixelFormat::Bgra => Err("already BGRA".into()),
    }
}

fn audio_loop(device: String, handover: Arc<Mutex<Option<AudioCapture>>>, shared: Arc<Shared>) {
    let card_audio = is_card_audio(&device);
    while !shared.stop.load(Ordering::Relaxed) {
        let opened = if card_audio { handover.lock().unwrap().take().ok_or(None) } else { AudioCapture::open(&device).map_err(Some) };
        let cap = match opened {
            Ok(c) => c,
            Err(e) => {
                if let Some(e) = e {
                    log(&format!("audio: local capture: {e}"));
                }
                wait(&shared, if card_audio { Duration::from_millis(200) } else { RETRY });
                continue;
            }
        };
        while !shared.stop.load(Ordering::Relaxed) {
            match cap.next_chunk(Duration::from_millis(200)) {
                Ok(Some(chunk)) => {
                    let planes = deinterleave(&chunk.samples, chunk.channels as usize);
                    let refs: Vec<&[f32]> = planes.iter().map(|p| p.as_slice()).collect();
                    deliver_audio(&shared, &refs, chunk.rate, chunk.timestamp);
                }
                Ok(None) => {}
                Err(e) => {
                    log(&format!("audio: local capture: {e}"));
                    break;
                }
            }
        }
    }
}

/// Sleep, waking early when the session stops.
fn wait(shared: &Shared, d: Duration) {
    let until = Instant::now() + d;
    while Instant::now() < until && !shared.stop.load(Ordering::Relaxed) {
        thread::sleep(Duration::from_millis(50));
    }
}

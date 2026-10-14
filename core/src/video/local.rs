//! A capture card on this computer: frames straight from `kvms-capture`,
//! no server and no network (docs/CAPTURE-SERVER.md §3). The OS decodes
//! MJPEG and converts raw formats to BGRA.

use std::sync::atomic::Ordering;
use std::sync::Arc;
use std::thread;
use std::time::{Duration, Instant};

use kvms_capture::{AudioCapture, CaptureError, Output, VideoCapture};

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
    let audio_thread = (!audio.is_empty()).then(|| {
        let shared = shared.clone();
        thread::Builder::new().name("capture-audio-receive".into()).spawn(move || audio_loop(audio, shared)).ok()
    });
    let mut deliver = Deliver::default();
    while !stopped() {
        let mut cap = match VideoCapture::open(&video, None, Output::Bgra) {
            Ok(c) => c,
            Err(e) => {
                set_state(&shared, &describe(&e));
                wait(&shared, RETRY);
                continue;
            }
        };
        log(&format!("video: local capture {}", cap.format()));
        set_state(&shared, "connecting");
        let mut last_frame = Instant::now();
        while !stopped() {
            match cap.next_frame(Duration::from_millis(100)) {
                Ok(Some(f)) => {
                    last_frame = Instant::now();
                    deliver.video(&shared, &f.data, f.stride, f.width, f.height, f.timestamp);
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

fn audio_loop(device: String, shared: Arc<Shared>) {
    while !shared.stop.load(Ordering::Relaxed) {
        let cap = match AudioCapture::open(&device) {
            Ok(c) => c,
            Err(e) => {
                log(&format!("audio: local capture: {e}"));
                wait(&shared, RETRY);
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

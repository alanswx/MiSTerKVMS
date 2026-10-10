//! Live check of the predictive cursor: moves the MiSTer mouse in a zig-zag
//! (no clicks), detects the guest cursor in the NDI video and reports how well
//! the tracker follows it.
//! cargo run --release -p kvms-core --example cursor_probe [-- <mister-host> "<ndi source>"]
//! Defaults come from local.settings.json.
use std::sync::{Arc, Mutex};
#[path = "support/settings.rs"]
mod settings;

use std::time::{Duration, Instant};

use kvms_core::ndi::{self, Captured, Receiver};
use kvms_core::video::crop::{self, Crop};
use kvms_core::video::cursor::{CursorDetector, CursorTracker};
use kvms_core::{ConnectionState, MisterSession, SessionObserver};

struct Quiet;
impl SessionObserver for Quiet {
    fn on_state(&self, _: ConnectionState) {}
    fn on_status(&self, _: String, _: String) {}
}

fn main() {
    let settings = settings::Settings::load();
    let (host, source) = (settings.mister_host(), settings.ndi_source());
    let ndi = ndi::ndi().unwrap_or_else(|e| panic!("{e}"));
    let rx = Receiver::new(ndi, &source, "", "kvms-cursor").expect("receiver");
    let session = MisterSession::new(host, settings.mister_port(), Arc::new(Quiet));
    std::thread::sleep(Duration::from_secs(2));

    let tracker = Arc::new(Mutex::new(CursorTracker::default()));
    tracker.lock().unwrap().set_captured(true);
    let mut detector = CursorDetector::default();
    let mut bounds: Option<Crop> = None;

    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mover = {
        let (session, stop, tracker) = (session.clone(), stop.clone(), tracker.clone());
        std::thread::spawn(move || {
            std::thread::sleep(Duration::from_millis(500));
            let mut i = 0u64;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                // Zig-zag with pauses, like real use.
                // Small symmetric legs so the cursor stays clear of the edges.
                let phase = (i / 80) % 6;
                let (dx, dy) = match phase {
                    0 => (1.5, 0.5),
                    1 => (-1.5, -0.5),
                    2 => (0.0, 0.0),
                    3 => (0.5, 1.5),
                    4 => (-0.5, -1.5),
                    _ => (0.0, 0.0),
                };
                if dx != 0.0 || dy != 0.0 {
                    session.mouse_move(dx, dy);
                    tracker.lock().unwrap().input(dx, dy, Instant::now());
                }
                i += 1;
                std::thread::sleep(Duration::from_millis(4));
            }
            // Undo the last leg so the cursor ends near where it started.
            session.mouse_move(0.0, 0.0);
        })
    };

    let start = Instant::now();
    let (mut changed_frames, mut hits) = (0, 0);
    let mut errors = Vec::new();
    while start.elapsed() < Duration::from_secs(8) {
        let Captured::Video(v) = rx.capture(100) else { continue };
        let now = Instant::now();
        let (w, h, stride) = (v.frame.xres as u32, v.frame.yres as u32, v.stride());
        if bounds.is_none() {
            bounds = Some(crop::detect(v.data(), stride, w, h).unwrap_or(Crop::full(w, h)));
            tracker.lock().unwrap().set_bounds(bounds.unwrap());
            println!("picture area {:?}", bounds.unwrap());
        }
        let (before, expected) = {
            let t = tracker.lock().unwrap();
            (t.stats(now).detections_per_sec, t.expected_in_frame(now))
        };
        let _ = before;
        let guest_before = tracker.lock().unwrap().guest();
        if !detector.process(v.data(), stride, w, h, &tracker, now) {
            continue;
        }
        changed_frames += 1;
        let guest_after = tracker.lock().unwrap().guest();
        if guest_after.is_some() && guest_after != guest_before {
            hits += 1;
            let g = guest_after.unwrap();
            if std::env::var("DUMP").is_ok() && hits < 50 {
                println!("{:5.0}ms guest ({:.0},{:.0}) expected ({:.0},{:.0})",
                    start.elapsed().as_secs_f64() * 1000.0, g.0, g.1, expected.0, expected.1);
            }
            if start.elapsed() > Duration::from_secs(4) {
                errors.push((g.0 - expected.0).hypot(g.1 - expected.1));
            }
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    mover.join().unwrap();
    let s = tracker.lock().unwrap().stats(Instant::now());
    errors.sort_by(|a, b| a.total_cmp(b));
    println!("changed frames {changed_frames}, cursor found in {hits}");
    let size = tracker.lock().unwrap().size();
    println!("fitted lag {:.0} ms, gain {:.2}, cursor size {:?}", s.lag_ms, s.gain, size);
    if !errors.is_empty() {
        println!(
            "prediction error vs detected (last 4 s): median {:.1} px, 90th pct {:.1} px, max {:.1} px",
            errors[errors.len() / 2], errors[errors.len() * 9 / 10], errors[errors.len() - 1]
        );
    }
    session.disconnect();
    std::thread::sleep(Duration::from_millis(200));
}

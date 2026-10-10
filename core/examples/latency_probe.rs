//! Measure input→video latency and real motion frame rate.
//! Moves the MiSTer's mouse a little (no clicks) and watches the NDI video
//! for the change.
//! cargo run --release -p kvms-core --example latency_probe [-- <mister-host> "<ndi source>"]
//! Defaults come from local.settings.json.
use std::sync::Arc;
#[path = "support/settings.rs"]
mod settings;

use std::time::{Duration, Instant};

use kvms_core::ndi::{self, Captured, Receiver};
use kvms_core::{ConnectionState, MisterSession, SessionObserver};

struct Quiet;
impl SessionObserver for Quiet {
    fn on_state(&self, _: ConnectionState) {}
    fn on_status(&self, _: String, _: String) {}
}

/// Downsampled luma of a BGRA frame (every 2nd pixel each way).
fn sample(rx: &Receiver, timeout: u32) -> Option<(Instant, Vec<u8>)> {
    loop {
        match rx.capture(timeout) {
            Captured::Video(v) => {
                let at = Instant::now();
                let (w, h, stride) = (v.frame.xres as usize, v.frame.yres as usize, v.stride());
                let d = v.data();
                let mut out = Vec::with_capacity(w * h / 4);
                for y in (0..h).step_by(2) {
                    for x in (0..w).step_by(2) {
                        out.push(d[y * stride + x * 4 + 1]);
                    }
                }
                return Some((at, out));
            }
            Captured::Nothing => return None,
            _ => {}
        }
    }
}

fn changed(a: &[u8], b: &[u8]) -> usize {
    a.iter().zip(b).filter(|(x, y)| x.abs_diff(**y) > 24).count()
}

fn main() {
    let settings = settings::Settings::load();
    let (host, source) = (settings.mister_host(), settings.ndi_source());

    let ndi = ndi::ndi().unwrap_or_else(|e| panic!("{e}"));
    let rx = Receiver::new(ndi, &source, "", "kvms-latency").expect("receiver");
    let session = MisterSession::new(host, settings.mister_port(), Arc::new(Quiet));
    std::thread::sleep(Duration::from_secs(2));
    assert!(session.stats().connected, "mrext not connected");

    // Let video settle.
    let mut prev = None;
    let settle = Instant::now();
    while settle.elapsed() < Duration::from_secs(1) {
        if let Some((_, f)) = sample(&rx, 200) {
            prev = Some(f);
        }
    }
    let mut prev = prev.expect("no video");

    // 1. Latency: single 30px moves, alternating direction.
    let mut lat = Vec::new();
    for i in 0..12 {
        // Wait for a quiet screen (no changes for 300 ms).
        let mut quiet_since = Instant::now();
        let deadline = Instant::now() + Duration::from_secs(3);
        while quiet_since.elapsed() < Duration::from_millis(300) && Instant::now() < deadline {
            if let Some((_, f)) = sample(&rx, 100) {
                if changed(&prev, &f) > 2 {
                    quiet_since = Instant::now();
                }
                prev = f;
            }
        }
        let dx = if i % 2 == 0 { 30.0 } else { -30.0 };
        let sent = Instant::now();
        session.mouse_move(dx, 0.0);
        while sent.elapsed() < Duration::from_millis(1000) {
            if let Some((at, f)) = sample(&rx, 100) {
                let n = changed(&prev, &f);
                prev = f;
                if n > 2 {
                    lat.push(at.duration_since(sent).as_secs_f64() * 1000.0);
                    break;
                }
            }
        }
    }
    lat.sort_by(|a, b| a.partial_cmp(b).unwrap());
    if lat.is_empty() {
        println!("latency: no cursor change seen (is the cursor visible?)");
    } else {
        println!(
            "input→video (frame received on Mac): min {:.0} ms, median {:.0} ms, max {:.0} ms  ({} samples)",
            lat[0], lat[lat.len() / 2], lat[lat.len() - 1], lat.len()
        );
    }

    // 2. Motion frame rate: continuous back-and-forth movement for 3 s.
    let stop = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let mover = {
        let (session, stop) = (session.clone(), stop.clone());
        std::thread::spawn(move || {
            let mut t = 0u32;
            while !stop.load(std::sync::atomic::Ordering::Relaxed) {
                let dx = if (t / 125) % 2 == 0 { 2.0 } else { -2.0 }; // ~250 px each way per 0.5 s
                session.mouse_move(dx, 0.0);
                t += 1;
                std::thread::sleep(Duration::from_millis(4));
            }
        })
    };
    let start = Instant::now();
    let (mut frames, mut moving, mut gaps, mut last_change) = (0, 0, Vec::new(), None::<Instant>);
    while start.elapsed() < Duration::from_secs(3) {
        if let Some((at, f)) = sample(&rx, 100) {
            frames += 1;
            if changed(&prev, &f) > 2 {
                moving += 1;
                if let Some(l) = last_change {
                    gaps.push(at.duration_since(l).as_secs_f64() * 1000.0);
                }
                last_change = Some(at);
            }
            prev = f;
        }
    }
    stop.store(true, std::sync::atomic::Ordering::Relaxed);
    mover.join().unwrap();
    let secs = start.elapsed().as_secs_f64();
    gaps.sort_by(|a, b| a.partial_cmp(b).unwrap());
    println!(
        "while moving: {:.1} fps received, {:.1} fps actually changed",
        frames as f64 / secs,
        moving as f64 / secs
    );
    if !gaps.is_empty() {
        println!(
            "gap between changed frames: median {:.1} ms, 90th pct {:.1} ms, max {:.1} ms",
            gaps[gaps.len() / 2], gaps[gaps.len() * 9 / 10], gaps[gaps.len() - 1]
        );
    }
    println!("mrext messages sent: {}", session.stats().messages_sent);
    session.disconnect();
    std::thread::sleep(Duration::from_millis(200));
}

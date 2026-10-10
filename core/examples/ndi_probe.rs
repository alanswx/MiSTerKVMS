//! List NDI sources, then receive one for a few seconds and print what arrives.
//! cargo run --release -p kvms-core --example ndi_probe -- "MACHINE (Source)"
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};
use kvms_core::ndi::{self, Captured};

fn main() {
    let ndi = ndi::ndi().unwrap_or_else(|e| panic!("{e}"));
    println!("NDI {} from {}", ndi.version, ndi.path.display());
    let sources = ndi::find_sources(ndi, "", 2000);
    println!("sources: {sources:?}");
    let Some(name) = std::env::args().nth(1).or_else(|| sources.first().cloned()) else { return };
    let rx = ndi::Receiver::new(ndi, &name, "", "kvms-probe").expect("receiver");
    let start = Instant::now();
    let (mut frames, mut audio, mut ages) = (0u32, 0u32, Vec::new());
    while start.elapsed() < Duration::from_secs(4) {
        match rx.capture(200) {
            Captured::Video(v) => {
                frames += 1;
                let now = SystemTime::now().duration_since(UNIX_EPOCH).unwrap().as_nanos() as i64 / 100;
                ages.push((now - v.frame.timestamp) as f64 / 10_000.0);
                if frames == 30 {
                    println!("video {}x{} fourcc {:#x} {}/{} fps stride {}", v.frame.xres, v.frame.yres,
                        v.frame.FourCC, v.frame.frame_rate_N, v.frame.frame_rate_D, v.stride());
                }
            }
            Captured::Audio(a) => {
                audio += 1;
                if audio == 1 {
                    println!("audio {} Hz, {} ch, {} samples/frame", a.frame.sample_rate, a.frame.no_channels, a.frame.no_samples);
                }
            }
            _ => {}
        }
    }
    ages.sort_by(|a, b| a.partial_cmp(b).unwrap());
    let secs = start.elapsed().as_secs_f64();
    println!("{frames} video frames ({:.1} fps), {audio} audio frames, dropped {}",
        frames as f64 / secs, rx.video_performance().1);
    if !ages.is_empty() {
        println!("frame age (send→receive, needs synced clocks): median {:.1} ms, min {:.1} ms",
            ages[ages.len() / 2], ages[0]);
    }
}

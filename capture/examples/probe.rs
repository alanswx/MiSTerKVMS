//! List capture devices and, for each, try to read a few frames.
//! `cargo run -p kvms-capture --example probe [seconds]`

use std::time::{Duration, Instant};

use kvms_capture::{audio_devices, matching_audio, AudioCapture, Output, VideoCapture};

fn main() {
    let secs: f64 = std::env::args().nth(1).and_then(|s| s.parse().ok()).unwrap_or(2.0);
    let audio = audio_devices();
    let audio_names: Vec<String> = audio.iter().map(|a| a.name.clone()).collect();
    let devices = kvms_capture::devices().expect("devices");
    for d in &devices {
        println!("{} [{}]", d.name, d.id);
        for f in &d.formats {
            println!("    {f}");
        }
        let paired = matching_audio(&d.name, &audio_names);
        println!("  audio: {paired:?}");
        for output in [Output::Native, Output::Bgra] {
            match VideoCapture::open(&d.id, None, output) {
                Ok(mut cap) => {
                    let start = Instant::now();
                    let (mut n, mut bytes, mut last) = (0u32, 0usize, None);
                    while start.elapsed().as_secs_f64() < secs {
                        match cap.next_frame(Duration::from_millis(500)) {
                            Ok(Some(f)) => {
                                n += 1;
                                bytes += f.data.len();
                                last = Some((f.format, f.width, f.height, f.stride, f.timestamp));
                            }
                            Ok(None) => {}
                            Err(e) => {
                                println!("  {output:?}: error after {n} frames: {e}");
                                break;
                            }
                        }
                    }
                    let fps = n as f64 / start.elapsed().as_secs_f64();
                    println!(
                        "  {output:?} {}: {n} frames, {fps:.1} fps, avg {} KB, last {last:?}",
                        cap.format(),
                        if n > 0 { bytes / n as usize / 1024 } else { 0 }
                    );
                }
                Err(e) => println!("  {output:?}: {e}"),
            }
        }
        if let Some(name) = paired {
            match AudioCapture::open(&name) {
                Ok(a) => {
                    let start = Instant::now();
                    let (mut chunks, mut samples, mut peak) = (0, 0usize, 0i16);
                    while start.elapsed().as_secs_f64() < 1.0 {
                        if let Ok(Some(c)) = a.next_chunk(Duration::from_millis(200)) {
                            chunks += 1;
                            samples += c.samples.len();
                            peak = peak.max(c.samples.iter().map(|s| s.saturating_abs()).max().unwrap_or(0));
                        }
                    }
                    println!("  audio {} ch {} Hz: {chunks} chunks, {samples} samples/s, peak {peak}", a.channels, a.rate);
                }
                Err(e) => println!("  audio: {e}"),
            }
        }
    }
}

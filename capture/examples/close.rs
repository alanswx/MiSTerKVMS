//! Open the first card, read a few frames, close it; then the same as BGRA.
use std::time::{Duration, Instant};
use kvms_capture::{Output, VideoCapture};

fn main() {
    let id = std::env::args().nth(1).unwrap_or_else(|| kvms_capture::devices().unwrap()[0].id.clone());
    for output in [Output::Native, Output::Bgra] {
        let t = Instant::now();
        let mut cap = match VideoCapture::open(&id, None, output) {
            Ok(c) => c,
            Err(e) => { println!("{output:?}: open failed: {e}"); continue; }
        };
        println!("{output:?}: opened {} in {:?}", cap.format(), t.elapsed());
        for _ in 0..10 {
            match cap.next_frame(Duration::from_millis(1000)) {
                Ok(Some(f)) => print!("{:?} {}x{} {}KB; ", f.format, f.width, f.height, f.data.len() / 1024),
                Ok(None) => print!("timeout; "),
                Err(e) => { print!("error {e}; "); break; }
            }
        }
        println!();
        let t = Instant::now();
        drop(cap);
        println!("{output:?}: closed in {:?}", t.elapsed());
    }
}

//! Open a card (by name) with its own audio; report video and audio rates.
use std::time::{Duration, Instant};
use kvms_capture::{Output, VideoCapture};

fn main() {
    let want = std::env::args().nth(1).unwrap_or_default();
    for a in kvms_capture::audio_devices() {
        println!("audio input: {} [{}]", a.name, a.id);
    }
    let id = kvms_capture::devices().unwrap().into_iter().find(|d| d.name.contains(&want)).expect("no such card").id;
    let mut cap = VideoCapture::open_with_audio(&id, None, Output::Native).expect("open");
    let audio = cap.take_audio();
    println!("{} audio: {}", cap.format(), audio.as_ref().map_or("none".into(), |a| format!("{} ch {} Hz", a.channels, a.rate)));
    let audio_thread = audio.map(|a| std::thread::spawn(move || {
        let start = Instant::now();
        let (mut chunks, mut samples, mut peak, mut gaps) = (0, 0usize, 0i16, Vec::new());
        let mut last = Instant::now();
        while start.elapsed() < Duration::from_secs(3) {
            if let Ok(Some(c)) = a.next_chunk(Duration::from_millis(200)) {
                gaps.push(last.elapsed().as_secs_f64() * 1000.0);
                last = Instant::now();
                chunks += 1;
                samples += c.samples.len() / c.channels.max(1) as usize;
                peak = peak.max(c.samples.iter().map(|s| s.saturating_abs()).max().unwrap_or(0));
            }
        }
        gaps.sort_by(|a, b| a.total_cmp(b));
        println!("audio: {chunks} chunks in 3 s, {} frames/s, peak {peak}, median chunk interval {:.1} ms", samples / 3, gaps.get(gaps.len() / 2).copied().unwrap_or(0.0));
    }));
    let start = Instant::now();
    let mut frames = 0;
    while start.elapsed() < Duration::from_secs(3) {
        if let Ok(Some(_)) = cap.next_frame(Duration::from_millis(500)) {
            frames += 1;
        }
    }
    println!("video: {:.1} fps", frames as f64 / 3.0);
    if let Some(t) = audio_thread {
        t.join().unwrap();
    }
    let t = Instant::now();
    drop(cap);
    println!("closed in {:?}", t.elapsed());
}

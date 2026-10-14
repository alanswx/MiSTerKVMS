//! `soak`: run a whole app session (controller, video source, offscreen
//! renderer) for a long time and watch memory. Fails if memory keeps
//! growing after warm-up (docs/CAPTURE-SERVER.md §2).

use std::sync::atomic::Ordering;
use std::time::{Duration, Instant};

use serde_json::{json, Value};

pub use crate::session::SessionSource as SoakSource;

fn rss_mb() -> f64 {
    memory_stats::memory_stats().map_or(0.0, |m| m.physical_mem as f64 / 1_048_576.0)
}

/// Runs for `duration`, printing a JSON line every `every`. Memory is judged
/// from the end of warm-up (a fifth of the run, at most 2 minutes) on.
pub fn soak(source: SoakSource, duration: Duration, every: Duration, limit_mb: f64) -> Result<Value, String> {
    let session = crate::session::open(source, true)?;
    let core = &session.core;
    let pictures = session.pictures.clone();
    let start = Instant::now();
    let warmup = (duration / 5).min(Duration::from_secs(120));
    let mut samples: Vec<(f64, f64)> = Vec::new();
    let mut last_pictures = 0;
    let mut last = Instant::now();
    while start.elapsed() < duration {
        std::thread::sleep(every.min(duration.saturating_sub(start.elapsed())).max(Duration::from_millis(10)));
        let t = start.elapsed().as_secs_f64();
        let mem = rss_mb();
        let st = core.state();
        let p = pictures.load(Ordering::Relaxed);
        let shown_fps = (p - last_pictures) as f64 / last.elapsed().as_secs_f64();
        last_pictures = p;
        last = Instant::now();
        let video = st.video_stats.as_ref();
        println!(
            "{}",
            json!({
                "t": (t * 10.0).round() / 10.0,
                "rss_mb": (mem * 10.0).round() / 10.0,
                "state": video.map(|v| v.state.clone()),
                "received_fps": video.map(|v| (v.received_fps * 10.0).round() / 10.0),
                "shown_fps": (shown_fps * 10.0).round() / 10.0,
                "size": video.map(|v| [v.source_width, v.source_height]),
            })
        );
        if start.elapsed() >= warmup {
            samples.push((t, mem));
        }
    }
    drop(session);

    // Growth: median of the last third minus median of the first third.
    let third = (samples.len() / 3).max(1);
    let median = |v: &[(f64, f64)]| {
        let mut m: Vec<f64> = v.iter().map(|s| s.1).collect();
        m.sort_by(|a, b| a.total_cmp(b));
        m.get(m.len() / 2).copied().unwrap_or(0.0)
    };
    let (early, late) = if samples.len() >= 3 {
        (median(&samples[..third]), median(&samples[samples.len() - third..]))
    } else {
        (samples.first().map_or(0.0, |s| s.1), samples.last().map_or(0.0, |s| s.1))
    };
    let growth = late - early;
    let pictures = pictures.load(Ordering::Relaxed);
    let result = json!({
        "seconds": start.elapsed().as_secs_f64(),
        "pictures": pictures,
        "rss_after_warmup_mb": early,
        "rss_end_mb": late,
        "growth_mb": growth,
        "limit_mb": limit_mb,
        "ok": growth <= limit_mb && pictures > 0,
    });
    if pictures == 0 {
        return Err(format!("no pictures arrived: {result}"));
    }
    if growth > limit_mb {
        return Err(format!("memory grew {growth:.1} MB after warm-up (limit {limit_mb} MB): {result}"));
    }
    Ok(result)
}

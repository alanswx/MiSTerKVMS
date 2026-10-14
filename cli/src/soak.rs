//! `soak`: run a whole app session (controller, video source, offscreen
//! renderer) for a long time and watch memory. Fails if memory keeps
//! growing after warm-up (docs/CAPTURE-SERVER.md §2).

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::Arc;
use std::time::{Duration, Instant};

use kvms_core::video::FrameOutput;
use kvms_core::{Controller, ControllerEvent, ControllerObserver, HostProfile, NativeSurface};
use serde_json::{json, Value};

struct Quiet;

impl ControllerObserver for Quiet {
    fn on_event(&self, _: ControllerEvent) {}
}

fn rss_mb() -> f64 {
    memory_stats::memory_stats().map_or(0.0, |m| m.physical_mem as f64 / 1_048_576.0)
}

pub struct SoakSource {
    pub stream: Option<String>,
    pub local: Option<String>,
    pub ndi: Option<String>,
}

/// Runs for `duration`, printing a JSON line every `every`. Memory is judged
/// from the end of warm-up (a fifth of the run, at most 2 minutes) on.
pub fn soak(source: SoakSource, duration: Duration, every: Duration, limit_mb: f64) -> Result<Value, String> {
    let settings = std::env::temp_dir().join(format!("misterkvms-soak-{}.json", std::process::id()));
    let core = Controller::with_settings_file(settings.display().to_string(), Arc::new(Quiet));
    let mut profile = HostProfile { name: "soak".into(), ..Default::default() };
    match (source.stream, source.local, source.ndi) {
        (Some(s), _, _) => {
            profile.source = "capture".into();
            profile.stream = s;
        }
        (_, Some(l), _) => {
            profile.source = "local".into();
            profile.local_video = l;
        }
        (_, _, Some(n)) => {
            profile.source = "ndi".into();
            profile.ndi_source = n;
        }
        _ => return Err("give --stream, --local or --ndi".into()),
    }
    let id = profile.id.clone();
    core.save_profile(profile);

    let pictures = Arc::new(AtomicU64::new(0));
    let frames = {
        let pictures = pictures.clone();
        // Like a shell: take each picture and hand its buffer back.
        let out: Arc<std::sync::Mutex<Option<Arc<FrameOutput>>>> = Arc::new(std::sync::Mutex::new(None));
        let out2 = out.clone();
        let f = FrameOutput::new(move || {
            if let Some(o) = out2.lock().unwrap().as_ref() {
                if let Some(frame) = o.take() {
                    pictures.fetch_add(1, Ordering::Relaxed);
                    o.recycle(frame.data);
                }
            }
        });
        *out.lock().unwrap() = Some(f.clone());
        f
    };
    core.set_frame_output(Some(frames));
    core.attach_surface(NativeSurface::Offscreen, 1280, 720, 1.0);
    core.open(id);

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
    core.close();
    core.detach_surface();
    let _ = std::fs::remove_file(settings);

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

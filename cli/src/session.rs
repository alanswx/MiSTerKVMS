//! A whole app session (controller, video source, offscreen renderer) on a
//! capture server stream or a local capture card, as the apps run it. Used by `soak` and by `record` for streams and local cards,
//! so both go through the same code as the apps.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use kvms_core::video::FrameOutput;
use kvms_core::{Controller, ControllerEvent, ControllerObserver, HostProfile, NativeSurface};

struct Quiet;

impl ControllerObserver for Quiet {
    fn on_event(&self, _: ControllerEvent) {}
}

#[derive(Debug, Clone, Default)]
pub struct SessionSource {
    /// Capture server stream (mkvm://host/stream).
    pub stream: Option<String>,
    /// Capture card on this computer (id or name).
    pub local: Option<String>,
    /// Its audio (id or name); None: the card's own, if one matches.
    pub local_audio: Option<String>,
}

pub struct Session {
    pub core: Arc<Controller>,
    /// Pictures the renderer produced.
    pub pictures: Arc<AtomicU64>,
    settings: std::path::PathBuf,
}

impl Drop for Session {
    fn drop(&mut self) {
        self.core.close();
        self.core.detach_surface();
        let _ = std::fs::remove_file(&self.settings);
    }
}

/// The card's own audio input, matched by name ("UGREEN 25773" →
/// "Digital Audio Interface (UGREEN 25773)"; a card with an audio pin →
/// "<card> (card audio)").
fn matching_audio(video: &str) -> String {
    let name = kvms_capture::devices()
        .ok()
        .and_then(|d| d.into_iter().find(|d| d.id == video || d.name.eq_ignore_ascii_case(video)).map(|d| d.name))
        .unwrap_or_else(|| if video == "test:pattern" { "Test pattern".into() } else { video.to_string() });
    let audio = kvms_capture::audio_devices();
    let names: Vec<String> = audio.iter().map(|a| a.name.clone()).collect();
    match kvms_capture::matching_audio(&name, &names) {
        // Card audio is opened by id with its video; inputs by name.
        Some(n) => audio.into_iter().find(|a| a.name == n).map(|a| if kvms_capture::is_card_audio(&a.id) { a.id } else { a.name }).unwrap_or(n),
        None if video == "test:pattern" => "card:test:pattern".into(),
        None => String::new(),
    }
}

pub fn open(source: SessionSource, auto_crop: bool) -> Result<Session, String> {
    let settings = std::env::temp_dir().join(format!("misterkvms-session-{}-{}.json", std::process::id(), rand_suffix()));
    let core = Controller::with_settings_file(settings.display().to_string(), Arc::new(Quiet));
    let mut profile = HostProfile { name: "cli".into(), ..Default::default() };
    match source {
        SessionSource { stream: Some(s), .. } => {
            profile.source = "capture".into();
            profile.stream = s;
        }
        SessionSource { local: Some(l), local_audio, .. } => {
            profile.source = "local".into();
            profile.local_audio = local_audio.unwrap_or_else(|| matching_audio(&l));
            profile.local_video = l;
        }
        _ => return Err("give --stream or --local".into()),
    }
    profile.auto_crop = auto_crop;
    let id = profile.id.clone();
    core.save_profile(profile);

    let pictures = Arc::new(AtomicU64::new(0));
    let frames = {
        let pictures = pictures.clone();
        // Like a shell: take each picture and hand its buffer back.
        let out: Arc<Mutex<Option<Arc<FrameOutput>>>> = Arc::new(Mutex::new(None));
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
    Ok(Session { core, pictures, settings })
}

fn rand_suffix() -> u128 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| d.as_nanos())
}

impl Session {
    /// Wait until video arrives, as the apps do before enabling Record.
    pub fn wait_connected(&self, timeout: Duration) -> Result<(), String> {
        let until = Instant::now() + timeout;
        loop {
            let st = self.core.state();
            if st.video_stats.as_ref().is_some_and(|v| v.state == "connected") {
                return Ok(());
            }
            if Instant::now() > until {
                let why = st.video_stats.map(|v| v.state).unwrap_or_else(|| "no video".into());
                return Err(format!("no picture after {} s ({why})", timeout.as_secs()));
            }
            std::thread::sleep(Duration::from_millis(100));
        }
    }

    /// Record to `out` (.mp4) for `duration` with the apps' recorder.
    pub fn record(&self, duration: Duration, out: &std::path::Path) -> Result<String, String> {
        self.core.start_recording(out.display().to_string()).map_err(|e| e.to_string())?;
        std::thread::sleep(duration);
        self.core.stop_recording();
        // ffmpeg finishes the file in the background.
        let until = Instant::now() + Duration::from_secs(120);
        loop {
            let st = self.core.state();
            if let Some(e) = st.recording_error {
                return Err(e);
            }
            if let Some(p) = st.last_recording {
                return Ok(p);
            }
            if Instant::now() > until {
                return Err("the recording didn't finish within 2 minutes".into());
            }
            std::thread::sleep(Duration::from_millis(200));
        }
    }
}

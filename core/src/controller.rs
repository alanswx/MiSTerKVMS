//! The app's session logic, shared by every shell: saved hosts, the one live
//! MiSTer (input session + video), stats and the overlay rule, saving what
//! the predictive cursor learned, display settings per host, recording.
//!
//! A shell renders `state()` and forwards input. It's told about changes
//! through `ControllerObserver`, called from background threads.

use std::path::PathBuf;
use std::sync::{Arc, Mutex, Weak};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;

use crate::input::{KeyResult, MouseButton};
use crate::keymap::hid;
use crate::link::{MkvmLink, StreamState};
use crate::log;
use crate::profiles::{HostProfile, ProfileStore, SourceKind};
use crate::session::{ConnectionState, InputStats, MisterSession, SessionObserver};
use crate::video::ffmpeg::FfmpegRecorder;
use crate::video::{CursorModel, DisplaySettings, FrameOutput, MediaSink, NativeSurface, VideoSession, VideoSource, VideoStats};

const TICK: Duration = Duration::from_millis(500);
/// How long the stats overlay stays up after something changes.
const STATS_FLASH: Duration = Duration::from_secs(5);
/// How often what the predictive cursor learned is saved.
const MODEL_SAVE: Duration = Duration::from_secs(60);

#[derive(Debug, Clone, Serialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct ControllerState {
    /// The open host (None = nothing selected).
    pub profile_id: Option<String>,
    pub connection: ConnectionState,
    /// Reported by mrext, e.g. "HP9K715" ("" = MiSTer menu).
    pub core_running: String,
    pub game_running: String,
    /// The shell is capturing keyboard and mouse.
    pub capturing: bool,
    /// Show the stats overlay now (flashed on changes, or pinned).
    pub stats_visible: bool,
    pub stats_pinned: bool,
    pub input_stats: Option<InputStats>,
    pub video_stats: Option<VideoStats>,
    /// Why there's no video (NDI missing, GPU error).
    pub video_error: Option<String>,
    pub display: DisplaySettings,
    pub muted: bool,
    /// Seconds since recording started (None = not recording).
    pub recording_seconds: Option<f64>,
    /// Path of the last finished recording.
    pub last_recording: Option<String>,
    pub recording_error: Option<String>,
    /// Capture server hosts: the stream's connection and who has control.
    pub stream: Option<StreamState>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, uniffi::Enum)]
#[serde(rename_all = "camelCase")]
pub enum ControllerEvent {
    /// Something in `state()` changed.
    Changed,
    /// The saved hosts changed.
    ProfilesChanged,
    /// The connection just came up: start capturing input (auto-capture),
    /// if the window is active.
    CaptureRequested,
    /// The connection went down: stop capturing.
    ReleaseRequested,
}

/// Implemented by the shell. Called from background threads; hop to the UI
/// thread before touching UI.
#[uniffi::export(with_foreign)]
pub trait ControllerObserver: Send + Sync {
    fn on_event(&self, event: ControllerEvent);
}

#[derive(Debug, uniffi::Error)]
pub enum ControllerError {
    Failed { message: String },
}

impl std::fmt::Display for ControllerError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let ControllerError::Failed { message } = self;
        f.write_str(message)
    }
}

impl std::error::Error for ControllerError {}

fn failed(message: impl Into<String>) -> ControllerError {
    ControllerError::Failed { message: message.into() }
}

enum Recorder {
    /// The shell's own encoder; it reports the result itself.
    Shell,
    Ffmpeg(Arc<FfmpegRecorder>),
}

struct Recording {
    since: Instant,
    recorder: Recorder,
}

struct State {
    profile: Option<HostProfile>,
    session: Option<Arc<MisterSession>>,
    /// Capture server hosts: the stream connection (video, audio, input).
    link: Option<Arc<MkvmLink>>,
    stream: Option<StreamState>,
    video: Option<Arc<VideoSession>>,
    surface: Option<(NativeSurface, u32, u32, f32)>,
    /// Where pictures go for an `Offscreen` surface.
    frames: Option<Arc<FrameOutput>>,
    /// Bumped per session so late callbacks from a closed one are ignored.
    generation: u64,
    capture_on_connect: bool,
    connection: ConnectionState,
    core_running: String,
    game_running: String,
    capturing: bool,
    stats_visible: bool,
    stats_hide_at: Instant,
    stats_signature: String,
    input_stats: Option<InputStats>,
    video_stats: Option<VideoStats>,
    video_error: Option<String>,
    display: DisplaySettings,
    muted: bool,
    recording: Option<Recording>,
    last_recording: Option<String>,
    recording_error: Option<String>,
    last_model_save: Instant,
}

impl Default for State {
    fn default() -> Self {
        Self {
            profile: None,
            session: None,
            link: None,
            stream: None,
            video: None,
            surface: None,
            frames: None,
            generation: 0,
            capture_on_connect: false,
            connection: not_connected(),
            core_running: String::new(),
            game_running: String::new(),
            capturing: false,
            stats_visible: false,
            stats_hide_at: Instant::now(),
            stats_signature: String::new(),
            input_stats: None,
            video_stats: None,
            video_error: None,
            display: DisplaySettings::default(),
            muted: false,
            recording: None,
            last_recording: None,
            recording_error: None,
            last_model_save: Instant::now(),
        }
    }
}

fn not_connected() -> ConnectionState {
    ConnectionState::Disconnected { reason: "not connected".into() }
}

struct Inner {
    /// Ourselves, for background work and session observers.
    me: Weak<Inner>,
    observer: Arc<dyn ControllerObserver>,
    store: ProfileStore,
    state: Mutex<State>,
    #[cfg(windows)]
    win_capture: Mutex<Option<crate::win::capture::WinCapture>>,
}

#[derive(uniffi::Object)]
pub struct Controller {
    inner: Arc<Inner>,
}

#[uniffi::export]
impl Controller {
    /// Uses the settings file in the platform config folder.
    #[uniffi::constructor]
    pub fn new(observer: Arc<dyn ControllerObserver>) -> Arc<Self> {
        Self::with_store(ProfileStore::open_default(), observer)
    }

    /// Uses the settings file at `path` (tests, portable installs).
    #[uniffi::constructor]
    pub fn with_settings_file(path: String, observer: Arc<dyn ControllerObserver>) -> Arc<Self> {
        Self::with_store(ProfileStore::open(PathBuf::from(path)), observer)
    }

    // MARK: Hosts

    pub fn profiles(&self) -> Vec<HostProfile> {
        self.inner.store.profiles()
    }

    /// Save a new or edited host. If it's the open one, it reconnects with
    /// the new settings.
    pub fn save_profile(&self, profile: HostProfile) {
        let mut profile = profile;
        profile.mister_host = profile.mister_host.trim().to_string();
        profile.obs_host = profile.obs_host.trim().to_string();
        if let Some(before) = self.inner.store.get(&profile.id) {
            // Keep what was learned meanwhile (the editor had an older copy).
            profile.cursor_lag_ms = before.cursor_lag_ms;
            profile.cursor_gain = before.cursor_gain;
            profile.reset_learned_if_mouse_changed(&before);
        }
        self.inner.store.upsert(profile.clone());
        self.inner.notify(ControllerEvent::ProfilesChanged);
        let is_open = self.inner.state.lock().unwrap().profile.as_ref().is_some_and(|p| p.id == profile.id);
        if is_open {
            self.close();
            self.open(profile.id);
        }
    }

    pub fn delete_profile(&self, id: String) {
        if self.open_profile_id().as_deref() == Some(id.as_str()) {
            self.close();
        }
        self.inner.store.delete(&id);
        self.inner.notify(ControllerEvent::ProfilesChanged);
    }

    /// Hosts as JSON, to back up or move to another computer.
    pub fn export_profiles(&self) -> String {
        self.inner.store.export_json()
    }

    /// Add hosts from an export (or the macOS app's old storage). Returns
    /// how many.
    pub fn import_profiles(&self, json: String) -> Result<u32, ControllerError> {
        let count = self.inner.store.import_json(&json).map_err(failed)?;
        self.inner.notify(ControllerEvent::ProfilesChanged);
        Ok(count)
    }

    /// Whether settings from older storage were already imported.
    pub fn legacy_imported(&self) -> bool {
        self.inner.store.data().imported_legacy
    }

    pub fn mark_legacy_imported(&self) {
        self.inner.store.change(|d| d.imported_legacy = true);
    }

    /// Where hosts are saved.
    pub fn settings_path(&self) -> String {
        self.inner.store.path().map(|p| p.display().to_string()).unwrap_or_default()
    }

    // MARK: Session

    /// Connect to a saved host (closing the current one).
    pub fn open(&self, id: String) {
        let Some(profile) = self.inner.store.get(&id) else { return };
        if self.open_profile_id().as_deref() == Some(id.as_str()) {
            return;
        }
        log(&format!(
            "host: open '{}' source {:?} stream '{}' local '{}' mister {} ndi '{}'",
            profile.name,
            profile.source_kind(),
            profile.stream,
            profile.local_video,
            profile.mister_host,
            profile.ndi_source
        ));
        self.close();
        let viewer = self.viewer_name();
        {
            let mut s = self.inner.state.lock().unwrap();
            s.display = profile.display_settings();
            s.profile = Some(profile.clone());
            s.generation += 1;
            let observer = Arc::new(Observer { inner: Arc::downgrade(&self.inner), generation: s.generation });
            if profile.source_kind() == SourceKind::Capture && profile.stream.trim().is_empty() {
                s.connection = ConnectionState::Disconnected { reason: "no capture stream chosen".into() };
            } else if profile.source_kind() == SourceKind::Capture {
                match mkvm::StreamAddress::parse(&profile.stream) {
                    Ok(address) => {
                        let link = MkvmLink::connect(address, viewer);
                        let (w, h) = profile.max_size();
                        link.set_quality(w, h, None);
                        let session = MisterSession::with_link(link.clone(), observer);
                        session.set_settings(profile.input_settings());
                        s.session = Some(session);
                        s.link = Some(link);
                        s.capture_on_connect = true;
                    }
                    Err(e) => s.connection = ConnectionState::Disconnected { reason: e },
                }
            } else if profile.mister_host.is_empty() {
                s.connection = ConnectionState::Disconnected { reason: "no MiSTer address set".into() };
            } else {
                let session = MisterSession::new(profile.mister_host.clone(), profile.mister_port, observer);
                session.set_settings(profile.input_settings());
                s.session = Some(session);
                s.capture_on_connect = true;
            }
        }
        self.inner.start_video();
        self.inner.notify(ControllerEvent::Changed);
    }

    pub fn close(&self) {
        self.inner.stop_platform_capture();
        self.inner.stop_video();
        let mut s = self.inner.state.lock().unwrap();
        if let Some(session) = s.session.take() {
            session.disconnect();
        }
        if let Some(link) = s.link.take() {
            link.close();
        }
        s.stream = None;
        s.generation += 1;
        s.profile = None;
        s.capturing = false;
        s.capture_on_connect = false;
        s.core_running.clear();
        s.game_running.clear();
        s.input_stats = None;
        s.connection = not_connected();
        drop(s);
        self.inner.notify(ControllerEvent::Changed);
    }

    pub fn state(&self) -> ControllerState {
        let pinned = self.inner.store.data().stats_pinned;
        let s = self.inner.state.lock().unwrap();
        ControllerState {
            profile_id: s.profile.as_ref().map(|p| p.id.clone()),
            connection: s.connection.clone(),
            core_running: s.core_running.clone(),
            game_running: s.game_running.clone(),
            capturing: s.capturing,
            stats_visible: s.stats_visible || pinned,
            stats_pinned: pinned,
            input_stats: s.input_stats.clone(),
            video_stats: s.video_stats.clone(),
            video_error: s.video_error.clone(),
            display: s.display.clone(),
            muted: s.muted,
            recording_seconds: s.recording.as_ref().map(|r| r.since.elapsed().as_secs_f64()),
            last_recording: s.last_recording.clone(),
            recording_error: s.recording_error.clone(),
            stream: s.stream.clone(),
        }
    }

    /// Capture server hosts: take control of the MiSTer from whoever has it.
    /// Always granted.
    pub fn take_control(&self) {
        let link = self.inner.state.lock().unwrap().link.clone();
        if let Some(link) = link {
            link.take_control();
        }
    }

    /// How this computer appears to other viewers (the computer's name
    /// unless set).
    pub fn viewer_name(&self) -> String {
        let name = self.inner.store.data().viewer_name;
        if name.trim().is_empty() {
            computer_name()
        } else {
            name
        }
    }

    pub fn set_viewer_name(&self, name: String) {
        self.inner.store.change(|d| d.viewer_name = name.trim().to_string());
    }

    /// Change the open host's quality ("auto", "best", "balanced", "low")
    /// and remember it.
    pub fn set_quality(&self, quality: String) {
        let (link, id) = {
            let mut s = self.inner.state.lock().unwrap();
            let link = s.link.clone();
            let Some(p) = s.profile.as_mut() else { return };
            p.quality = quality.clone();
            (link, p.id.clone())
        };
        self.inner.store.update(&id, |p| p.quality = quality.clone());
        if let Some(link) = link {
            let (w, h) = HostProfile { quality, ..Default::default() }.max_size();
            link.set_quality(w, h, None);
        }
        self.inner.notify(ControllerEvent::Changed);
    }

    /// The open host's input session (shells whose capture talks to it
    /// directly, like macOS).
    pub fn input_session(&self) -> Option<Arc<MisterSession>> {
        self.inner.state.lock().unwrap().session.clone()
    }

    // MARK: Video surface

    /// The view the video is drawn into, its size in pixels and pixels per
    /// point. Video starts whenever a host with an NDI source is open. The
    /// view must stay valid until `detach_surface` (macOS: call on the main
    /// thread).
    pub fn attach_surface(&self, surface: NativeSurface, width_px: u32, height_px: u32, points_scale: f32) {
        let same = self.inner.state.lock().unwrap().surface.as_ref().is_some_and(|(s, ..)| *s == surface);
        if same {
            self.resize_surface(width_px, height_px, points_scale);
            return;
        }
        self.inner.stop_video();
        self.inner.state.lock().unwrap().surface = Some((surface, width_px, height_px, points_scale));
        self.inner.start_video();
        self.inner.notify(ControllerEvent::Changed);
    }

    /// Stop drawing into the view. Returns once the view can be released.
    pub fn detach_surface(&self) {
        self.inner.stop_video();
        self.inner.state.lock().unwrap().surface = None;
        self.inner.notify(ControllerEvent::Changed);
    }

    pub fn resize_surface(&self, width_px: u32, height_px: u32, points_scale: f32) {
        let video = {
            let mut s = self.inner.state.lock().unwrap();
            if let Some(surface) = s.surface.as_mut() {
                (surface.1, surface.2, surface.3) = (width_px, height_px, points_scale);
            }
            s.video.clone()
        };
        match video {
            Some(v) => v.resize(width_px, height_px, points_scale),
            // It may not have had a usable size before.
            None => self.inner.start_video(),
        }
    }

    // MARK: Input

    pub fn is_connected(&self) -> bool {
        self.inner.state.lock().unwrap().connection == ConnectionState::Connected
    }

    /// The shell started or stopped capturing keyboard and mouse.
    pub fn set_capturing(&self, capturing: bool) {
        self.inner.set_capturing(capturing);
    }

    /// A key changed state (USB HID usage). `ExitCapture` means the release
    /// chord was pressed: the shell should stop capturing.
    pub fn key_event(&self, hid_usage: u16, down: bool) -> KeyResult {
        match self.input_session() {
            Some(s) => s.key_event(hid_usage, down),
            None => KeyResult::Ignored,
        }
    }

    pub fn mouse_move(&self, dx: f64, dy: f64) {
        if let Some(s) = self.input_session() {
            s.mouse_move(dx, dy);
        }
    }

    pub fn mouse_button(&self, button: MouseButton, down: bool) {
        if let Some(s) = self.input_session() {
            s.mouse_button(button, down);
        }
    }

    pub fn release_all(&self) {
        if let Some(s) = self.input_session() {
            s.release_all();
        }
    }

    /// Press the MiSTer menu (OSD) key, F12.
    pub fn open_menu(&self) {
        if let Some(s) = self.input_session() {
            s.open_menu();
        }
    }

    pub fn send_ctrl_alt_del(&self) {
        if let Some(s) = self.input_session() {
            s.press_combo(vec![hid::LEFT_CTRL, hid::LEFT_ALT, hid::DELETE]);
        }
    }

    // MARK: Display

    /// Apply display settings; scale, smoothing, auto-crop and the
    /// predictive cursor are remembered for the open host.
    pub fn set_display(&self, settings: DisplaySettings) {
        let mut d = settings;
        d.zoom = d.zoom.clamp(0.25, 16.0);
        d.pan_x = d.pan_x.clamp(-0.5, 0.5);
        d.pan_y = d.pan_y.clamp(-0.5, 0.5);
        if d.zoom <= 1.0 {
            d.pan_x = 0.0;
            d.pan_y = 0.0;
        }
        let (video, id) = {
            let mut s = self.inner.state.lock().unwrap();
            s.display = d.clone();
            if let Some(p) = s.profile.as_mut() {
                p.store_display(&d);
            }
            (s.video.clone(), s.profile.as_ref().map(|p| p.id.clone()))
        };
        if let Some(id) = id {
            self.inner.store.update(&id, |p| p.store_display(&d));
        }
        if let Some(v) = video {
            v.set_display(d);
        }
        self.inner.notify(ControllerEvent::Changed);
    }

    pub fn zoom_by(&self, factor: f64) {
        let mut d = self.display();
        d.zoom *= factor;
        self.set_display(d);
    }

    pub fn reset_zoom(&self) {
        let mut d = self.display();
        d.zoom = 1.0;
        self.set_display(d);
    }

    /// Pan by a fraction of the picture (when zoomed in).
    pub fn pan_by(&self, dx: f64, dy: f64) {
        let mut d = self.display();
        d.pan_x += dx;
        d.pan_y += dy;
        self.set_display(d);
    }

    pub fn set_muted(&self, muted: bool) {
        let video = {
            let mut s = self.inner.state.lock().unwrap();
            s.muted = muted;
            s.video.clone()
        };
        if let Some(v) = video {
            v.set_muted(muted);
        }
        self.inner.notify(ControllerEvent::Changed);
    }

    /// Show the stats overlay for a few seconds.
    pub fn flash_stats(&self) {
        self.inner.state.lock().unwrap().flash();
        self.inner.notify(ControllerEvent::Changed);
    }

    pub fn set_stats_pinned(&self, pinned: bool) {
        self.inner.store.change(|d| d.stats_pinned = pinned);
        if !pinned {
            self.inner.state.lock().unwrap().flash();
        }
        self.inner.notify(ControllerEvent::Changed);
    }

    // MARK: Recording

    /// Record video and audio to `path` (.mp4) through ffmpeg.
    pub fn start_recording(&self, path: String) -> Result<(), ControllerError> {
        let video = self.recordable_video()?;
        let recorder = FfmpegRecorder::start(PathBuf::from(path)).map_err(failed)?;
        video.set_media_sink(Some(recorder.clone()));
        self.inner.recording_started(Recorder::Ffmpeg(recorder));
        Ok(())
    }

    /// Record with the shell's own encoder. The shell reports the result
    /// with `recording_finished` after `stop_recording`.
    pub fn start_recording_with_sink(&self, sink: Arc<dyn MediaSink>) -> Result<(), ControllerError> {
        let video = self.recordable_video()?;
        video.set_media_sink(Some(sink));
        self.inner.recording_started(Recorder::Shell);
        Ok(())
    }

    /// Stop recording. With ffmpeg, the file is finished in the background
    /// and `last_recording` (or `recording_error`) is set when it's done.
    pub fn stop_recording(&self) {
        self.inner.stop_recording();
    }

    pub fn recording_finished(&self, path: Option<String>, error: Option<String>) {
        self.inner.recording_finished(path, error);
    }
}

#[cfg(windows)]
impl Controller {
    /// Capture keyboard and mouse with the built-in Windows capture (see
    /// `win::capture`). `window` is the app's top-level window; the rest is
    /// the video area in screen pixels. Ends on the release chord, focus
    /// loss, disconnect or `stop_capture`.
    pub fn start_capture(&self, window: isize, x: i32, y: i32, width: i32, height: i32) -> Result<(), String> {
        use crate::win::capture::{CaptureTarget, WinCapture};
        let (session, raw_mouse) = {
            let s = self.inner.state.lock().unwrap();
            if s.connection != ConnectionState::Connected {
                return Err("not connected".into());
            }
            let session = s.session.clone().ok_or("not connected")?;
            (session, s.profile.as_ref().is_some_and(|p| p.raw_mouse))
        };
        let mut slot = self.inner.win_capture.lock().unwrap();
        if slot.as_ref().is_some_and(|c| c.is_running()) {
            return Ok(());
        }
        let target = CaptureTarget { window, x, y, width, height, raw_mouse };
        let me = self.inner.me.clone();
        let capture = WinCapture::start(
            target,
            session,
            Box::new(move || {
                if let Some(inner) = me.upgrade() {
                    inner.set_capturing(false);
                }
            }),
        )?;
        *slot = Some(capture);
        drop(slot);
        self.inner.set_capturing(true);
        Ok(())
    }

    pub fn stop_capture(&self) {
        self.inner.stop_platform_capture();
        self.inner.set_capturing(false);
    }
}

impl Controller {
    /// Draw into memory instead of a view (`NativeSurface::Offscreen`): each
    /// picture goes to `frames`. Call before `attach_surface`.
    pub fn set_frame_output(&self, frames: Option<Arc<FrameOutput>>) {
        self.inner.state.lock().unwrap().frames = frames;
    }

    fn with_store(store: ProfileStore, observer: Arc<dyn ControllerObserver>) -> Arc<Self> {
        let inner = Arc::new_cyclic(|me| Inner {
            me: me.clone(),
            observer,
            store,
            state: Mutex::new(State::default()),
            #[cfg(windows)]
            win_capture: Mutex::new(None),
        });
        let weak = Arc::downgrade(&inner);
        thread::Builder::new()
            .name("controller-tick".into())
            .spawn(move || tick_loop(weak))
            .expect("spawn controller thread");
        Arc::new(Self { inner })
    }

    fn open_profile_id(&self) -> Option<String> {
        self.inner.state.lock().unwrap().profile.as_ref().map(|p| p.id.clone())
    }

    fn display(&self) -> DisplaySettings {
        self.inner.state.lock().unwrap().display.clone()
    }

    fn recordable_video(&self) -> Result<Arc<VideoSession>, ControllerError> {
        let s = self.inner.state.lock().unwrap();
        if s.recording.is_some() {
            return Err(failed("already recording"));
        }
        s.video.clone().ok_or_else(|| failed("no video to record"))
    }
}

impl Drop for Controller {
    fn drop(&mut self) {
        self.close();
        self.detach_surface();
    }
}

impl State {
    fn flash(&mut self) {
        self.stats_visible = true;
        self.stats_hide_at = Instant::now() + STATS_FLASH;
    }

    /// Re-show the overlay when anything worth noticing changes. Audio
    /// underruns are left out: they also count while nothing is playing.
    fn check_signature(&mut self) {
        let connected = match self.connection {
            ConnectionState::Connected => "up",
            ConnectionState::Connecting => "connecting",
            ConnectionState::Disconnected { .. } => "down",
        };
        let mut parts = vec![connected.to_string(), self.core_running.clone()];
        if let Some(v) = &self.video_stats {
            parts.extend([
                v.state.clone(),
                format!("{}x{}", v.source_width, v.source_height),
                format!("{}x{}", v.crop_width, v.crop_height),
                v.dropped_frames.to_string(),
            ]);
        }
        let signature = parts.join("|");
        if signature != self.stats_signature {
            self.stats_signature = signature;
            self.flash();
        }
    }
}

impl Inner {
    fn notify(&self, event: ControllerEvent) {
        self.observer.on_event(event);
    }

    fn set_capturing(&self, capturing: bool) {
        let (video, session) = {
            let mut s = self.state.lock().unwrap();
            if s.capturing == capturing {
                return;
            }
            s.capturing = capturing;
            if capturing {
                s.capture_on_connect = false;
            }
            (s.video.clone(), s.session.clone())
        };
        if let (false, Some(session)) = (capturing, session) {
            session.release_all();
        }
        if let Some(v) = video {
            v.set_pointer_captured(capturing);
        }
        self.notify(ControllerEvent::Changed);
    }

    /// End platform capture (Windows), if running.
    fn stop_platform_capture(&self) {
        #[cfg(windows)]
        {
            let capture = self.win_capture.lock().unwrap().take();
            if let Some(c) = capture {
                c.stop();
            }
        }
    }

    /// Start video if a host with an NDI source is open and there's a view.
    fn start_video(&self) {
        let mut s = self.state.lock().unwrap();
        let (Some(profile), Some((surface, w, h, scale))) = (s.profile.clone(), s.surface.clone()) else {
            return;
        };
        if s.video.is_some() || w < 2 || h < 2 {
            return;
        }
        let source = match profile.source_kind() {
            SourceKind::Capture => match &s.link {
                Some(link) => VideoSource::Link(link.clone()),
                None => return,
            },
            SourceKind::Local if !profile.local_video.is_empty() => {
                VideoSource::Local { video: profile.local_video.clone(), audio: profile.local_audio.clone() }
            }
            SourceKind::Ndi if !profile.ndi_source.is_empty() => {
                VideoSource::Ndi { source: profile.ndi_source.clone(), extra_ip: profile.obs_host.clone() }
            }
            _ => return,
        };
        let frames = s.frames.clone();
        match VideoSession::start(source, surface, frames, w, h, scale) {
            Ok(video) => {
                video.set_display(s.display.clone());
                video.set_muted(s.muted);
                if profile.cursor_lag_ms > 0.0 {
                    // Gain 0 = mouse scaling changed since it was learned; start neutral.
                    let gain = if profile.cursor_gain > 0.0 { profile.cursor_gain } else { 1.0 };
                    video.set_cursor_model(CursorModel { lag_ms: profile.cursor_lag_ms, gain });
                }
                if let Some(session) = &s.session {
                    video.link_input(session.clone());
                }
                video.set_pointer_captured(s.capturing);
                s.video = Some(video);
                s.video_error = None;
                s.last_model_save = Instant::now();
            }
            Err(e) => {
                log(&format!("video: {e}"));
                s.video_error = Some(e.to_string());
            }
        }
    }

    fn stop_video(&self) {
        self.stop_recording();
        self.save_cursor_model();
        let video = {
            let mut s = self.state.lock().unwrap();
            s.video_stats = None;
            s.video.take()
        };
        if let Some(v) = video {
            v.stop();
        }
    }

    /// Remember what the predictive cursor learned for next time.
    fn save_cursor_model(&self) {
        let (video, id) = {
            let mut s = self.state.lock().unwrap();
            s.last_model_save = Instant::now();
            (s.video.clone(), s.profile.as_ref().map(|p| p.id.clone()))
        };
        let (Some(video), Some(id)) = (video, id) else { return };
        // Nothing measured this session: keep what was saved before.
        if video.stats().cursor_fit_samples == 0 {
            return;
        }
        let model = video.cursor_model();
        self.store.update(&id, |p| {
            p.cursor_lag_ms = model.lag_ms;
            p.cursor_gain = model.gain;
        });
    }

    fn recording_started(&self, recorder: Recorder) {
        let mut s = self.state.lock().unwrap();
        s.recording = Some(Recording { since: Instant::now(), recorder });
        s.recording_error = None;
        drop(s);
        self.notify(ControllerEvent::Changed);
    }

    fn stop_recording(&self) {
        let (recording, video) = {
            let mut s = self.state.lock().unwrap();
            (s.recording.take(), s.video.clone())
        };
        let Some(recording) = recording else { return };
        if let Some(v) = video {
            v.set_media_sink(None);
        }
        if let Recorder::Ffmpeg(recorder) = recording.recorder {
            // Finishing takes a moment (muxing); don't block the caller.
            let inner = self.me.clone();
            thread::spawn(move || {
                let result = recorder.finish();
                if let Some(inner) = inner.upgrade() {
                    match result {
                        Ok(path) => inner.recording_finished(Some(path.display().to_string()), None),
                        Err(e) => inner.recording_finished(None, Some(e)),
                    }
                }
            });
        }
        self.notify(ControllerEvent::Changed);
    }

    fn recording_finished(&self, path: Option<String>, error: Option<String>) {
        let mut s = self.state.lock().unwrap();
        if let Some(path) = path {
            log(&format!("record: saved {path}"));
            s.last_recording = Some(path);
        }
        if let Some(e) = &error {
            log(&format!("record: failed: {e}"));
        }
        s.recording_error = error;
        drop(s);
        self.notify(ControllerEvent::Changed);
    }

    /// The open host's NDI source was renamed (and video already moved to
    /// the new name): remember it.
    fn source_renamed(&self, renamed: String) {
        let id = {
            let mut s = self.state.lock().unwrap();
            let Some(profile) = s.profile.as_mut() else { return };
            profile.ndi_source = renamed.clone();
            profile.id.clone()
        };
        log(&format!("host: NDI source is now '{renamed}'"));
        self.store.update(&id, |p| p.ndi_source = renamed);
        self.notify(ControllerEvent::ProfilesChanged);
    }

    fn tick(&self) {
        let (session, video, link) = {
            let s = self.state.lock().unwrap();
            (s.session.clone(), s.video.clone(), s.link.clone())
        };
        let stream = link.map(|l| l.stream_state());
        let input_stats = session.map(|s| s.stats());
        let renamed = video.as_ref().and_then(|v| v.take_renamed_source());
        let video_stats = video.map(|v| v.stats());
        if let Some(renamed) = renamed {
            self.source_renamed(renamed);
        }
        let (save_model, lost_control) = {
            let mut s = self.state.lock().unwrap();
            // Someone took control: stop capturing here.
            let had = s.stream.as_ref().is_some_and(|st| st.have_control);
            let has = stream.as_ref().is_some_and(|st| st.have_control);
            let lost_control = had && !has && s.capturing;
            s.stream = stream;
            s.input_stats = input_stats;
            s.video_stats = video_stats;
            s.check_signature();
            if s.stats_visible && Instant::now() >= s.stats_hide_at {
                s.stats_visible = false;
            }
            (s.last_model_save.elapsed() >= MODEL_SAVE, lost_control)
        };
        if save_model {
            self.save_cursor_model();
        }
        if lost_control {
            log("host: control taken by another viewer");
            self.stop_platform_capture();
            self.notify(ControllerEvent::ReleaseRequested);
        }
        self.notify(ControllerEvent::Changed);
    }
}

fn tick_loop(inner: Weak<Inner>) {
    loop {
        thread::sleep(TICK);
        match inner.upgrade() {
            Some(inner) => inner.tick(),
            None => return,
        }
    }
}

/// The computer's name, for other viewers to see.
fn computer_name() -> String {
    let from_env = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_default();
    if !from_env.trim().is_empty() {
        return from_env.trim().to_string();
    }
    std::process::Command::new("hostname")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .map(|s| s.trim().trim_end_matches(".local").to_string())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| "MiSTerKVMS".into())
}

/// Session callbacks (worker thread) → controller state.
struct Observer {
    inner: Weak<Inner>,
    generation: u64,
}

impl SessionObserver for Observer {
    fn on_state(&self, state: ConnectionState) {
        let Some(inner) = self.inner.upgrade() else { return };
        let mut events = vec![ControllerEvent::Changed];
        {
            let mut s = inner.state.lock().unwrap();
            if s.generation != self.generation {
                return;
            }
            s.connection = state.clone();
            s.check_signature();
            match state {
                ConnectionState::Connected if s.capture_on_connect => {
                    s.capture_on_connect = false;
                    events.push(ControllerEvent::CaptureRequested);
                }
                ConnectionState::Disconnected { .. } if s.capturing => {
                    events.push(ControllerEvent::ReleaseRequested);
                }
                _ => {}
            }
        }
        if events.contains(&ControllerEvent::ReleaseRequested) {
            inner.stop_platform_capture();
        }
        for e in events {
            inner.notify(e);
        }
    }

    fn on_status(&self, key: String, value: String) {
        let Some(inner) = self.inner.upgrade() else { return };
        {
            let mut s = inner.state.lock().unwrap();
            if s.generation != self.generation {
                return;
            }
            match key.as_str() {
                "coreRunning" => s.core_running = value,
                "gameRunning" => s.game_running = value,
                _ => return,
            }
            s.check_signature();
        }
        inner.notify(ControllerEvent::Changed);
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::video::ScaleMode;

    struct Events(Mutex<Vec<ControllerEvent>>);

    impl ControllerObserver for Events {
        fn on_event(&self, event: ControllerEvent) {
            self.0.lock().unwrap().push(event);
        }
    }

    fn controller() -> (Arc<Controller>, Arc<Events>) {
        let events = Arc::new(Events(Mutex::new(Vec::new())));
        (Controller::with_store(ProfileStore::in_memory(), events.clone()), events)
    }

    #[test]
    fn display_changes_are_clamped_and_remembered_per_host() {
        let (c, _) = controller();
        let p = HostProfile { name: "A".into(), ..Default::default() };
        c.save_profile(p.clone());
        c.open(p.id.clone());
        assert_eq!(c.state().profile_id.as_deref(), Some(p.id.as_str()));
        c.set_display(DisplaySettings { scale_mode: ScaleMode::Integer, zoom: 100.0, pan_x: 3.0, ..Default::default() });
        let d = c.state().display;
        assert_eq!((d.zoom, d.pan_x), (16.0, 0.5));
        c.reset_zoom();
        assert_eq!(c.state().display.pan_x, 0.0);
        assert_eq!(c.profiles()[0].scale_mode, "integer");
        // Reopening starts from the saved settings, at zoom 1.
        c.close();
        c.open(p.id);
        assert_eq!(c.state().display.scale_mode, ScaleMode::Integer);
    }

    #[test]
    fn editing_keeps_learned_cursor_unless_mouse_changed() {
        let (c, events) = controller();
        let p = HostProfile::default();
        c.save_profile(p.clone());
        c.inner.store.update(&p.id, |p| (p.cursor_lag_ms, p.cursor_gain) = (90.0, 1.1));
        c.save_profile(HostProfile { name: "Renamed".into(), ..p.clone() });
        assert_eq!((c.profiles()[0].cursor_lag_ms, c.profiles()[0].cursor_gain), (90.0, 1.1));
        c.save_profile(HostProfile { mouse_sensitivity: 2.0, ..p });
        assert_eq!((c.profiles()[0].cursor_lag_ms, c.profiles()[0].cursor_gain), (90.0, 0.0));
        assert!(events.0.lock().unwrap().contains(&ControllerEvent::ProfilesChanged));
    }

    #[test]
    fn host_without_address_reports_why() {
        let (c, _) = controller();
        let p = HostProfile { source: "capture".into(), ..Default::default() };
        c.save_profile(p.clone());
        c.open(p.id);
        assert_eq!(c.state().connection, ConnectionState::Disconnected { reason: "no capture stream chosen".into() });
        let p = HostProfile::default();
        c.save_profile(p.clone());
        c.open(p.id);
        assert_eq!(
            c.state().connection,
            ConnectionState::Disconnected { reason: "no MiSTer address set".into() }
        );
        assert_eq!(c.key_event(0x04, true), KeyResult::Ignored);
    }

    /// A capture server stream end to end against a fake server: connect,
    /// input arrives as mkvm Input, control taken away releases capture.
    #[test]
    fn capture_stream_carries_input_and_control() {
        use mkvm::*;
        use std::net::TcpListener;

        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        let (tx, rx) = std::sync::mpsc::channel();
        thread::spawn(move || {
            let (mut sock, _) = listener.accept().unwrap();
            let mut read = sock.try_clone().unwrap();
            let Message::Hello(hello) = read_message(&mut read).unwrap().0 else { panic!("no hello") };
            assert_eq!(hello.stream, "MiSTer SS1");
            let info = StreamInfo {
                name: hello.stream,
                mister: Some("10.0.0.5".into()),
                width: 0,
                height: 0,
                fps: 0.0,
                audio_rate: 48000,
                audio_channels: 2,
                sizes: vec![],
                codecs: vec![Codec::Jpeg],
            };
            write_message(&mut sock, &Message::Welcome(Welcome { protocol: 1, server: "test".into(), viewer_id: 1, stream: info }), 0).unwrap();
            write_message(&mut sock, &Message::Mister(MisterState { connected: true, reason: String::new() }), 0).unwrap();
            write_message(&mut sock, &Message::Control(ControlState { holder: Some(Viewer { id: 1, name: hello.viewer }), you: true }), 0).unwrap();
            write_message(&mut sock, &Message::Status(Status { key: "coreRunning".into(), value: "HP9K715".into() }), 0).unwrap();
            loop {
                match read_message(&mut read) {
                    Ok((Message::Input(cmds), _)) => {
                        tx.send(cmds).unwrap();
                        // Someone else takes control.
                        let other = Viewer { id: 2, name: "Other PC".into() };
                        write_message(&mut sock, &Message::Control(ControlState { holder: Some(other), you: false }), 0).unwrap();
                    }
                    Ok(_) => {}
                    Err(_) => return,
                }
            }
        });

        let (c, events) = controller();
        let p = HostProfile { stream: format!("mkvm://127.0.0.1:{port}/MiSTer%20SS1"), ..Default::default() };
        c.save_profile(p.clone());
        c.open(p.id);
        let deadline = Instant::now() + Duration::from_secs(5);
        while (c.state().connection != ConnectionState::Connected || c.state().core_running.is_empty()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert_eq!(c.state().connection, ConnectionState::Connected);
        assert_eq!(c.state().core_running, "HP9K715");
        assert!(events.0.lock().unwrap().contains(&ControllerEvent::CaptureRequested));

        c.set_capturing(true);
        c.inner.tick();
        assert!(c.state().stream.unwrap().have_control);
        c.key_event(0x04, true); // A
        let cmds = rx.recv_timeout(Duration::from_secs(2)).unwrap();
        assert_eq!(cmds, vec![InputCmd::Send { m: "kbdRawDown:30".into() }]);

        let deadline = Instant::now() + Duration::from_secs(2);
        while c.state().stream.as_ref().is_some_and(|s| s.holder.is_empty()) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
            c.inner.tick();
        }
        assert_eq!(c.state().stream.unwrap().holder, "Other PC");
        assert!(events.0.lock().unwrap().contains(&ControllerEvent::ReleaseRequested));
        c.close();
    }

    #[test]
    fn unreachable_mister_disconnects_and_capture_releases() {
        let (c, events) = controller();
        // Port 9 on localhost: nothing listens, so the connect fails fast.
        let p = HostProfile { source: "ndi".into(), mister_host: "127.0.0.1".into(), mister_port: 9, ..Default::default() };
        c.save_profile(p.clone());
        c.open(p.id);
        let deadline = Instant::now() + Duration::from_secs(5);
        while !matches!(c.state().connection, ConnectionState::Disconnected { .. }) && Instant::now() < deadline {
            thread::sleep(Duration::from_millis(20));
        }
        assert!(matches!(c.state().connection, ConnectionState::Disconnected { .. }));
        assert!(!events.0.lock().unwrap().contains(&ControllerEvent::CaptureRequested));
        c.set_capturing(true);
        assert!(c.state().capturing);
        c.close();
        assert!(!c.state().capturing);
    }
}

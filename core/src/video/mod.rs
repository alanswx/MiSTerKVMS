//! Video + audio for one session: a source (a capture server stream or a
//! capture card on this computer) feeding a latest-frame slot and an
//! audio buffer, and a render thread (slot → GPU → view).

mod audio;
mod local;
pub mod crop;
pub mod cursor;
pub mod ffmpeg;
mod render;

use std::fmt;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant, SystemTime, UNIX_EPOCH};

use audio::AudioOut;
use crop::{Crop, Stabilizer};
use kvms_codec as codec;
use cursor::{CursorDetector, CursorTracker};

use serde::{Deserialize, Serialize};

use crate::link::{LinkMedia, MkvmLink};
use crate::log;
use crate::session::{CursorSink, MisterSession};

/// Where a session's pictures and sound come from.
#[derive(Clone)]
pub enum VideoSource {
    /// A stream on a capture server.
    Link(Arc<MkvmLink>),
    /// A capture card on this computer: video device id and audio device id
    /// (empty: no audio).
    Local { video: String, audio: String },
}

impl VideoSource {
    fn describe(&self) -> String {
        match self {
            VideoSource::Link(link) => link.address().to_string(),
            VideoSource::Local { video, audio } => format!("local '{video}' audio '{audio}'"),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize, uniffi::Enum)]
#[serde(rename_all = "camelCase")]
pub enum ScaleMode {
    /// Largest size that fits the view, keeping aspect ratio.
    Fit,
    /// Largest whole-number multiple that fits (sharpest for pixel art and text).
    Integer,
    /// One source pixel per point.
    Native,
}

impl ScaleMode {
    /// The name saved in profiles.
    pub fn name(self) -> &'static str {
        match self {
            ScaleMode::Fit => "fit",
            ScaleMode::Integer => "integer",
            ScaleMode::Native => "native",
        }
    }

    pub fn from_name(name: &str) -> Self {
        match name {
            "integer" => ScaleMode::Integer,
            "native" => ScaleMode::Native,
            _ => ScaleMode::Fit,
        }
    }
}

/// The native view the video is drawn into. Handles are pointers or ids
/// passed as integers; they must stay valid until the session is stopped.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, uniffi::Enum)]
#[serde(tag = "kind", rename_all = "camelCase", rename_all_fields = "camelCase")]
pub enum NativeSurface {
    /// An NSView (macOS). Create the session on the main thread.
    AppKit { ns_view: u64 },
    /// A UIView (iOS). Create the session on the main thread.
    UiKit { ui_view: u64 },
    /// A window (Windows). `hinstance` may be 0.
    Win32 { hwnd: u64, hinstance: u64 },
    /// A DirectComposition surface handle (Windows,
    /// `DCompositionCreateSurfaceHandle`). The shell shows it through a
    /// composition brush, so UI can draw on top of the video.
    CompositionHandle { handle: u64 },
    Wayland { display: u64, surface: u64 },
    Xlib { display: u64, window: u64 },
    /// No view: the picture is rendered into memory and handed to the shell
    /// through a `FrameOutput` (GTK, which can't give wgpu a surface).
    Offscreen,
}

/// One finished picture, as the shell should show it (BGRA, rows `stride`
/// bytes apart, sized to the view in pixels).
pub struct RenderedFrame {
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub data: Vec<u8>,
}

/// Where an `Offscreen` session puts its pictures. `notify` runs on the
/// render thread after each one; the shell then `take`s the latest.
pub struct FrameOutput {
    latest: Mutex<Option<RenderedFrame>>,
    spare: Mutex<Option<Vec<u8>>>,
    notify: Box<dyn Fn() + Send + Sync>,
}

impl FrameOutput {
    pub fn new(notify: impl Fn() + Send + Sync + 'static) -> Arc<Self> {
        Arc::new(Self { latest: Mutex::new(None), spare: Mutex::new(None), notify: Box::new(notify) })
    }

    /// The newest picture since the last call, if any.
    pub fn take(&self) -> Option<RenderedFrame> {
        self.latest.lock().unwrap().take()
    }

    /// Hand a picture's buffer back once it has been copied, to save an
    /// allocation per frame.
    pub fn recycle(&self, data: Vec<u8>) {
        *self.spare.lock().unwrap() = Some(data);
    }

    pub(crate) fn spare(&self) -> Vec<u8> {
        self.spare.lock().unwrap().take().unwrap_or_default()
    }

    pub(crate) fn put(&self, frame: RenderedFrame) {
        if let Some(old) = self.latest.lock().unwrap().replace(frame) {
            self.recycle(old.data); // the shell didn't get to it
        }
        (self.notify)();
    }
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize, uniffi::Record)]
#[serde(rename_all = "camelCase", default)]
pub struct DisplaySettings {
    pub scale_mode: ScaleMode,
    /// Linear filtering instead of nearest-neighbour.
    pub smooth: bool,
    /// Multiplier on top of the scale mode (1.0 = none).
    pub zoom: f64,
    /// Pan offset as a fraction of the picture size (0 = centred).
    pub pan_x: f64,
    pub pan_y: f64,
    /// Detect and remove MiSTer's black borders.
    pub auto_crop: bool,
    /// Draw a local cursor ahead of the guest's lagging one.
    pub predictive_cursor: bool,
}

impl Default for DisplaySettings {
    fn default() -> Self {
        Self {
            scale_mode: ScaleMode::Fit,
            smooth: false,
            zoom: 1.0,
            pan_x: 0.0,
            pan_y: 0.0,
            auto_crop: true,
            predictive_cursor: true,
        }
    }
}

#[derive(Debug, Clone, Default, Serialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct VideoStats {
    /// "searching", "connected", "no video", or an error.
    pub state: String,
    pub source_width: u32,
    pub source_height: u32,
    pub crop_width: u32,
    pub crop_height: u32,
    pub received_fps: f64,
    pub rendered_fps: f64,
    /// Frames the source dropped before we received them (0 if unknown).
    pub dropped_frames: i64,
    /// Received frames replaced by a newer one before they were drawn.
    pub skipped_frames: u64,
    /// Average time from capture to us presenting it (capture server
    /// streams correct for the two clocks' difference). Negative if unknown.
    pub frame_age_ms: f64,
    pub audio_buffer_ms: f64,
    pub audio_underruns: u32,
    pub audio_rate: u32,
    /// Predictive cursor: guest cursor currently tracked.
    pub cursor_locked: bool,
    /// Learned input→video latency and guest pixels per input pixel.
    pub cursor_lag_ms: f64,
    pub cursor_gain: f64,
    /// Last difference between prediction and the guest cursor.
    pub cursor_error_px: f64,
    pub cursor_detections_per_sec: f64,
    /// Good measurements behind the lag/gain (last minute).
    pub cursor_fit_samples: u32,
}

/// Receives the raw stream for recording. Implemented by the shell with the
/// platform's encoder (AVFoundation on macOS). Called on the receive thread;
/// keep it quick.
#[uniffi::export(with_foreign)]
pub trait MediaSink: Send + Sync {
    /// One BGRA video frame. `data` is a pointer to the first pixel, valid
    /// only for the duration of the call; rows are `stride` bytes apart.
    /// `timestamp` is in 100 ns units (same clock as audio).
    fn video_frame(&self, data: u64, stride: u32, width: u32, height: u32, timestamp: i64);
    /// Interleaved float audio.
    fn audio_frame(&self, samples: Vec<f32>, channels: u32, sample_rate: u32, timestamp: i64);
}

/// What the predictive cursor learned, to restore in the next session.
#[derive(Debug, Clone, uniffi::Record)]
pub struct CursorModel {
    pub lag_ms: f64,
    pub gain: f64,
}

#[derive(Debug, uniffi::Error)]
pub enum VideoError {
    Graphics { message: String },
}

impl fmt::Display for VideoError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        match self {
            VideoError::Graphics { message } => f.write_str(message),
        }
    }
}
impl std::error::Error for VideoError {}

struct Frame {
    data: Vec<u8>,
    stride: u32,
    width: u32,
    height: u32,
    /// Capture time, 100 ns units since the Unix epoch (0 if unknown).
    timestamp: i64,
}

#[derive(Default)]
struct Slot {
    frame: Option<Frame>,
    /// Spare buffer handed back by the renderer to avoid reallocating.
    spare: Option<Vec<u8>>,
    redraw: bool,
}

#[derive(Default)]
struct Counters {
    state: String,
    source: (u32, u32),
    crop: Option<Crop>,
    received: u64,
    rendered: u64,
    skipped: u64,
    dropped: i64,
    age_sum_ms: f64,
    age_count: u64,
    last_rates: Option<(Instant, u64, u64, f64, f64, f64)>,
}

struct Shared {
    stop: AtomicBool,
    slot: Mutex<Slot>,
    wake: Condvar,
    display: Mutex<DisplaySettings>,
    viewport: Mutex<(u32, u32, f32)>,
    counters: Mutex<Counters>,
    audio: Mutex<Option<AudioOut>>,
    cursor: Mutex<CursorTracker>,
    /// Recording: where frames go, and the crop fixed when recording started
    /// (so the file doesn't change size when borders are re-detected).
    sink: Mutex<Option<(Arc<dyn MediaSink>, Option<Crop>)>>,
}

/// Mouse movement from the input session → predictive cursor (+ redraw).
struct Sink(Arc<Shared>);

impl CursorSink for Sink {
    fn input(&self, dx: f64, dy: f64) {
        self.0.cursor.lock().unwrap().input(dx, dy, Instant::now());
        self.0.slot.lock().unwrap().redraw = true;
        self.0.wake.notify_all();
    }
}

#[derive(uniffi::Object)]
pub struct VideoSession {
    shared: Arc<Shared>,
    threads: Mutex<Vec<JoinHandle<()>>>,
    input: Mutex<Option<Arc<MisterSession>>>,
    /// Capture server stream: frames arrive on the link's own thread.
    link: Option<Arc<MkvmLink>>,
}

impl VideoSession {
    /// Start receiving `source` and drawing it into `surface`; `frames`:
    /// where `Offscreen` pictures go. `points_scale` is the display's pixels
    /// per point (Windows: DPI / 96).
    pub fn start(
        source: VideoSource,
        surface: NativeSurface,
        frames: Option<Arc<FrameOutput>>,
        width_px: u32,
        height_px: u32,
        points_scale: f32,
    ) -> Result<Arc<Self>, VideoError> {
        log(&format!("video: start {} ({width_px}x{height_px} px)", source.describe()));
        // SAFETY: the shell keeps the view alive until stop() returns.
        let (instance, surface) = unsafe { render::create_surface(&surface) }
            .map_err(|message| VideoError::Graphics { message })?;

        let shared = Arc::new(Shared {
            stop: AtomicBool::new(false),
            slot: Mutex::new(Slot::default()),
            wake: Condvar::new(),
            display: Mutex::new(DisplaySettings::default()),
            viewport: Mutex::new((width_px, height_px, points_scale)),
            counters: Mutex::new(Counters { state: "searching".into(), ..Default::default() }),
            audio: Mutex::new(None),
            cursor: Mutex::new(CursorTracker::default()),
            sink: Mutex::new(None),
        });

        match AudioOut::start() {
            Ok(out) => *shared.audio.lock().unwrap() = Some(out),
            Err(e) => eprintln!("audio disabled: {e}"),
        }

        let render_shared = shared.clone();
        let (ready_tx, ready_rx) = std::sync::mpsc::channel();
        let render = thread::Builder::new()
            .name("video-render".into())
            .spawn(move || {
                // wgpu reports setup mistakes by panicking; turn that into an
                // error message the UI can show.
                let created = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                    render::Renderer::new(&instance, surface, frames, width_px, height_px)
                }))
                .unwrap_or_else(|panic| Err(format!("renderer setup failed: {}", panic_message(&panic))));
                match created {
                    Ok(r) => {
                        let _ = ready_tx.send(Ok(()));
                        render_loop(r, render_shared);
                    }
                    Err(e) => {
                        let _ = ready_tx.send(Err(e));
                    }
                }
            })
            .map_err(|e| VideoError::Graphics { message: e.to_string() })?;
        ready_rx
            .recv()
            .map_err(|e| VideoError::Graphics { message: e.to_string() })?
            .map_err(|message| VideoError::Graphics { message })?;

        let mut threads = vec![render];
        let mut link = None;
        match source {
            VideoSource::Link(l) => {
                shared.counters.lock().unwrap().state = "connecting".into();
                let sink = LinkSink { shared: shared.clone(), deliver: Mutex::new((Deliver::default(), codec::Bgra::default())) };
                l.set_media(Some(Arc::new(sink)));
                link = Some(l);
            }
            VideoSource::Local { video, audio } => {
                shared.counters.lock().unwrap().state = "opening".into();
                let recv_shared = shared.clone();
                threads.push(
                    thread::Builder::new()
                        .name("capture-receive".into())
                        .spawn(move || local::receive_loop(video, audio, recv_shared))
                        .map_err(|e| VideoError::Graphics { message: e.to_string() })?,
                );
            }
        }

        Ok(Arc::new(Self {
            shared,
            threads: Mutex::new(threads),
            input: Mutex::new(None),
            link,
        }))
    }
}

#[uniffi::export]
impl VideoSession {

    /// The view's drawable size in pixels and its pixels-per-point.
    pub fn resize(&self, width_px: u32, height_px: u32, points_scale: f32) {
        *self.shared.viewport.lock().unwrap() = (width_px, height_px, points_scale);
        self.redraw();
    }

    pub fn set_display(&self, settings: DisplaySettings) {
        let auto_crop = settings.auto_crop;
        self.shared.cursor.lock().unwrap().enabled = settings.predictive_cursor;
        *self.shared.display.lock().unwrap() = settings;
        if !auto_crop {
            self.shared.counters.lock().unwrap().crop = None;
        }
        self.redraw();
    }

    pub fn set_muted(&self, muted: bool) {
        if let Some(a) = self.shared.audio.lock().unwrap().as_ref() {
            a.set_muted(muted);
        }
    }

    pub fn set_volume(&self, volume: f32) {
        if let Some(a) = self.shared.audio.lock().unwrap().as_ref() {
            a.set_volume(volume);
        }
    }

    /// Feed this session's mouse movement to the predictive cursor.
    pub fn link_input(&self, session: Arc<MisterSession>) {
        session.set_cursor_sink(Some(Arc::new(Sink(self.shared.clone()))));
        if let Some(old) = self.input.lock().unwrap().replace(session) {
            old.set_cursor_sink(None);
        }
    }

    /// Whether the mouse is captured (the predictive cursor only shows then).
    pub fn set_pointer_captured(&self, captured: bool) {
        self.shared.cursor.lock().unwrap().set_captured(captured);
        self.redraw();
    }

    /// Start (Some) or stop (None) passing frames and audio to a recorder.
    pub fn set_media_sink(&self, sink: Option<Arc<dyn MediaSink>>) {
        *self.shared.sink.lock().unwrap() = sink.map(|s| (s, None));
    }

    pub fn cursor_model(&self) -> CursorModel {
        let s = self.shared.cursor.lock().unwrap().stats(Instant::now());
        CursorModel { lag_ms: s.lag_ms, gain: s.gain }
    }

    /// Start from what a previous session learned.
    pub fn set_cursor_model(&self, model: CursorModel) {
        self.shared.cursor.lock().unwrap().set_model(model.lag_ms, model.gain);
    }

    pub fn stats(&self) -> VideoStats {
        let mut c = self.shared.counters.lock().unwrap();
        let now = Instant::now();
        let (rx_fps, render_fps, age) = match c.last_rates {
            Some((t, rx, rendered, rx_fps, render_fps, age)) if now - t < Duration::from_millis(500) => {
                let _ = (rx, rendered);
                (rx_fps, render_fps, age)
            }
            Some((t, rx, rendered, ..)) => {
                let secs = (now - t).as_secs_f64();
                let age = if c.age_count > 0 { c.age_sum_ms / c.age_count as f64 } else { -1.0 };
                let v = ((c.received - rx) as f64 / secs, (c.rendered - rendered) as f64 / secs, age);
                c.age_sum_ms = 0.0;
                c.age_count = 0;
                c.last_rates = Some((now, c.received, c.rendered, v.0, v.1, v.2));
                v
            }
            None => {
                c.last_rates = Some((now, c.received, c.rendered, 0.0, 0.0, -1.0));
                (0.0, 0.0, -1.0)
            }
        };
        let crop = c.crop.unwrap_or(Crop::full(c.source.0, c.source.1));
        let cursor = self.shared.cursor.lock().unwrap().stats(now);
        let audio = self.shared.audio.lock().unwrap();
        VideoStats {
            cursor_locked: cursor.locked,
            cursor_lag_ms: cursor.lag_ms,
            cursor_gain: cursor.gain,
            cursor_error_px: cursor.error_px,
            cursor_detections_per_sec: cursor.detections_per_sec,
            cursor_fit_samples: cursor.fit_samples,
            state: c.state.clone(),
            source_width: c.source.0,
            source_height: c.source.1,
            crop_width: crop.w,
            crop_height: crop.h,
            received_fps: rx_fps,
            rendered_fps: render_fps,
            dropped_frames: c.dropped,
            skipped_frames: c.skipped,
            frame_age_ms: age,
            audio_buffer_ms: audio.as_ref().map_or(0.0, |a| a.buffered_ms()),
            audio_underruns: audio.as_ref().map_or(0, |a| a.underruns()),
            audio_rate: audio.as_ref().map_or(0, |a| a.rate),
        }
    }

    /// Stop receiving and drawing. Blocks until the render thread has let go
    /// of the view, so the shell can safely release it afterwards.
    pub fn stop(&self) {
        if !self.shared.stop.load(Ordering::Relaxed) {
            log("video: stop");
        }
        if let Some(session) = self.input.lock().unwrap().take() {
            session.set_cursor_sink(None);
        }
        if let Some(link) = &self.link {
            link.set_media(None);
        }
        self.shared.stop.store(true, Ordering::Relaxed);
        self.shared.wake.notify_all();
        for t in self.threads.lock().unwrap().drain(..) {
            let _ = t.join();
        }
        self.shared.audio.lock().unwrap().take();
    }
}

impl VideoSession {
    fn redraw(&self) {
        self.shared.slot.lock().unwrap().redraw = true;
        self.shared.wake.notify_all();
    }
}

impl Drop for VideoSession {
    fn drop(&mut self) {
        self.stop();
    }
}

fn panic_message(panic: &Box<dyn std::any::Any + Send>) -> String {
    panic
        .downcast_ref::<String>()
        .cloned()
        .or_else(|| panic.downcast_ref::<&str>().map(|s| s.to_string()))
        .unwrap_or_else(|| "unknown error".into())
}

fn now_100ns() -> i64 {
    SystemTime::now().duration_since(UNIX_EPOCH).map_or(0, |d| (d.as_nanos() / 100) as i64)
}

/// Hand a frame to the recorder, cropped to the picture area that was
/// current when recording started (or the new full frame if the source
/// resolution changed since).
fn record_video(shared: &Shared, data: &[u8], stride: u32, w: u32, h: u32, timestamp: i64) {
    let mut sink = shared.sink.lock().unwrap();
    let Some((sink, fixed)) = sink.as_mut() else { return };
    let crop = match *fixed {
        Some(c) if c.x + c.w <= w && c.y + c.h <= h => c,
        _ => {
            let c = shared.counters.lock().unwrap().crop.unwrap_or(Crop::full(w, h));
            let c = if c.x + c.w <= w && c.y + c.h <= h { c } else { Crop::full(w, h) };
            *fixed = Some(c);
            c
        }
    };
    // Encoders want even dimensions.
    let (cw, ch) = (crop.w & !1, crop.h & !1);
    let offset = crop.y as usize * stride as usize + crop.x as usize * 4;
    let ptr = data[offset..].as_ptr() as u64;
    sink.video_frame(ptr, stride, cw, ch, timestamp);
}

/// Planar float audio from any source: to the speakers and the recorder.
fn deliver_audio(shared: &Shared, planes: &[&[f32]], rate: u32, timestamp: i64) {
    if planes.is_empty() {
        return;
    }
    if let Some(out) = shared.audio.lock().unwrap().as_ref() {
        if !out.is_muted() {
            out.push(planes, rate);
        }
    }
    let sink = shared.sink.lock().unwrap();
    let Some((sink, _)) = sink.as_ref() else { return };
    let samples = planes[0].len();
    let mut interleaved = Vec::with_capacity(samples * planes.len());
    for i in 0..samples {
        for plane in planes {
            interleaved.push(plane.get(i).copied().unwrap_or(0.0));
        }
    }
    sink.audio_frame(interleaved, planes.len() as u32, rate, timestamp);
}

/// Per-source state for handing frames to the session: border detection
/// and the predictive cursor's guest-cursor detector.
struct Deliver {
    stabilizer: Stabilizer,
    detector: CursorDetector,
    frames_since_crop: u32,
}

impl Default for Deliver {
    fn default() -> Self {
        Self { stabilizer: Stabilizer::default(), detector: CursorDetector::default(), frames_since_crop: u32::MAX }
    }
}

impl Deliver {
    /// One BGRA frame: borders, cursor, recording, then the slot for the
    /// renderer. `timestamp` is when it was captured, 100 ns since the Unix
    /// epoch on this machine's clock (0 if unknown).
    fn video(&mut self, shared: &Shared, data: &[u8], stride: u32, w: u32, h: u32, timestamp: i64) {
        let arrived = Instant::now();
        let auto_crop = shared.display.lock().unwrap().auto_crop;
        if auto_crop {
            // Re-check borders twice a second at 60 fps; cheap but not free.
            self.frames_since_crop = self.frames_since_crop.saturating_add(1);
            if self.frames_since_crop >= 30 {
                self.frames_since_crop = 0;
                let detected = crop::detect(data, stride as usize, w, h);
                let crop = self.stabilizer.update(detected);
                shared.counters.lock().unwrap().crop = crop;
            }
        } else {
            self.stabilizer.reset();
            self.frames_since_crop = u32::MAX;
        }

        // The predictive cursor works within the visible picture.
        let bounds = shared.counters.lock().unwrap().crop.unwrap_or(Crop::full(w, h));
        shared.cursor.lock().unwrap().set_bounds(bounds);
        self.detector.process(data, stride as usize, w, h, &shared.cursor, arrived);

        record_video(shared, data, stride, w, h, if timestamp > 0 { timestamp } else { now_100ns() });

        let mut slot = shared.slot.lock().unwrap();
        let mut buf = slot
            .frame
            .take()
            .map(|f| {
                shared.counters.lock().unwrap().skipped += 1;
                f.data
            })
            .or_else(|| slot.spare.take())
            .unwrap_or_default();
        buf.clear();
        buf.extend_from_slice(&data[..(stride * h) as usize]);
        slot.frame = Some(Frame { data: buf, stride, width: w, height: h, timestamp });
        drop(slot);
        shared.wake.notify_all();

        let mut c = shared.counters.lock().unwrap();
        c.received += 1;
        c.source = (w, h);
    }
}

/// A capture server stream's pictures and sound → this session.
struct LinkSink {
    shared: Arc<Shared>,
    /// The frame handler and a reused decode buffer.
    deliver: Mutex<(Deliver, codec::Bgra)>,
}

impl LinkMedia for LinkSink {
    fn video(&self, video: &mkvm::Video, timestamp: i64) {
        let mut guard = self.deliver.lock().unwrap();
        let (deliver, picture) = &mut *guard;
        match video.codec {
            mkvm::Codec::Jpeg => {
                if let Err(e) = codec::jpeg::decode(&video.data, picture) {
                    log(&format!("video: {e}"));
                    return;
                }
            }
            // Never asked for yet (Hello only lists JPEG).
            mkvm::Codec::H264 => return,
        }
        deliver.video(&self.shared, &picture.data, picture.stride, picture.width, picture.height, timestamp);
        let mut c = self.shared.counters.lock().unwrap();
        if c.state != "connected" {
            c.state = "connected".into();
        }
    }

    fn audio(&self, audio: &mkvm::Audio, timestamp: i64) {
        let planes = deinterleave(&audio.samples, audio.channels as usize);
        let refs: Vec<&[f32]> = planes.iter().map(|p| p.as_slice()).collect();
        deliver_audio(&self.shared, &refs, audio.rate, timestamp);
    }

    fn state(&self, state: &str) {
        let mut c = self.shared.counters.lock().unwrap();
        if c.state != state {
            log(&format!("video: stream {} -> {state}", c.state));
            c.state = state.into();
        }
    }
}

/// Interleaved 16-bit PCM → one float plane per channel.
fn deinterleave(samples: &[i16], channels: usize) -> Vec<Vec<f32>> {
    let channels = channels.max(1);
    (0..channels)
        .map(|ch| samples.iter().skip(ch).step_by(channels).map(|s| *s as f32 / 32768.0).collect())
        .collect()
}

fn render_loop(mut renderer: render::Renderer, shared: Arc<Shared>) {
    let mut texture: Option<(u32, u32)> = None;
    let mut last_size = (0, 0);
    // While our cursor is on screen, keep redrawing so it hides on time.
    let mut cursor_shown = false;
    loop {
        let frame = {
            let mut slot = shared.slot.lock().unwrap();
            while slot.frame.is_none() && !slot.redraw && !shared.stop.load(Ordering::Relaxed) {
                let wait = if cursor_shown { 16 } else { 250 };
                let (s, timeout) = shared.wake.wait_timeout(slot, Duration::from_millis(wait)).unwrap();
                slot = s;
                if timeout.timed_out() && cursor_shown {
                    break;
                }
            }
            if shared.stop.load(Ordering::Relaxed) {
                drop(slot);
                renderer.clear();
                return;
            }
            slot.redraw = false;
            slot.frame.take()
        };

        let (vw, vh, points_scale) = *shared.viewport.lock().unwrap();
        if (vw, vh) != last_size {
            renderer.resize(vw, vh);
            last_size = (vw, vh);
        }

        let mut timestamp = 0;
        if let Some(f) = frame {
            renderer.upload(&f.data, f.stride, f.width, f.height);
            texture = Some((f.width, f.height));
            timestamp = f.timestamp;
            shared.slot.lock().unwrap().spare = Some(f.data);
        }

        let (tw, th) = texture.unwrap_or((1, 1));
        let crop = match shared.counters.lock().unwrap().crop {
            Some(c) if c.x + c.w <= tw && c.y + c.h <= th => c,
            _ => Crop::full(tw, th),
        };
        let display = shared.display.lock().unwrap().clone();
        let now = Instant::now();
        let (pointer, pointer_size, state) = {
            let c = shared.cursor.lock().unwrap();
            (c.draw_position(now), c.size(), c.pointer_state(now))
        };
        cursor_shown = pointer.is_some();
        let pointer = pointer.map(|at| render::Pointer { at, size: pointer_size, state });
        if let Err(e) = renderer.draw(crop, &display, points_scale, pointer) {
            log(&format!("video: draw failed: {e}"));
            shared.counters.lock().unwrap().state = e;
            continue;
        }
        if texture.is_none() {
            continue;
        }
        let mut c = shared.counters.lock().unwrap();
        c.rendered += 1;
        if timestamp > 0 {
            let age = (now_100ns() - timestamp) as f64 / 10_000.0;
            if (0.0..10_000.0).contains(&age) {
                c.age_sum_ms += age;
                c.age_count += 1;
            }
        }
    }
}

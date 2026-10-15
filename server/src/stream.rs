//! One stream: a capture card (video + its audio) shown to any number of
//! viewers, and the MiSTer it belongs to. Threads: capture (newest frame
//! only), encoder (one picture per distinct viewer size), audio. One mrext
//! session per stream; input from the viewer in control goes to it.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Condvar, Mutex, Weak};
use std::thread::{self, JoinHandle};
use std::time::{Duration, Instant};

use kvms_capture::{is_card_audio, AudioCapture, CaptureError, Output, PixelFormat, VideoCapture, VideoFrame};
use kvms_codec::{convert, jpeg, scale, Bgra};
use kvms_core::{ConnectionState, MisterSession, SessionObserver};
use mkvm::{
    Codec, Control, ControlChange, ControlState, Format, HeldInput, InputCmd, Message, MisterState, Status, StreamInfo, Viewer, ViewerId,
};

use crate::config::StreamConfig;
use crate::log;

const RETRY: Duration = Duration::from_secs(2);
const NO_SIGNAL_AFTER: Duration = Duration::from_secs(1);
/// Distinct picture sizes encoded at once (docs/CAPTURE-SERVER.md §6).
const MAX_PIPELINES: usize = 3;
/// Encoder threads per stream. Encoding a raw 1080p frame takes 10–17 ms,
/// so one thread can't keep up with 60 fps whenever a frame runs long;
/// several take turns and frames still go out in order.
const ENCODERS: usize = 3;

#[derive(Default)]
struct Slot {
    frame: Option<VideoFrame>,
    /// Sequence number the next taken frame gets.
    next_seq: u64,
}

/// One encoded picture, shared by every viewer that asked for its size.
pub struct Encoded {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
    /// Capture time, server clock, 100 ns since the Unix epoch.
    pub timestamp: i64,
}

pub struct AudioPacket {
    pub channels: u8,
    pub rate: u32,
    pub samples: Vec<i16>,
    pub timestamp: i64,
}

/// What a viewer (or a setup-page preview) wants.
#[derive(Debug, Clone, Copy, Default)]
pub struct Want {
    pub video: bool,
    pub audio: bool,
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    /// Send only this many pictures a second (None: every one).
    pub max_fps: Option<f64>,
    /// JPEG quality for this viewer (None: the stream's own).
    pub quality: Option<u8>,
}

/// Someone watching a stream: an mkvm viewer or a setup-page preview.
pub trait Subscriber: Send + Sync {
    fn id(&self) -> ViewerId;
    fn name(&self) -> String;
    fn want(&self) -> Want;
    fn video(&self, frame: Arc<Encoded>);
    fn audio(&self, packet: Arc<AudioPacket>);
    fn message(&self, message: Message);
    /// The stream is going away.
    fn close(&self);
}

/// What the setup page shows about a stream.
#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct StreamStatus {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub pixel: String,
    pub signal: bool,
    /// Why there's no picture ("in use by another app", ...); empty if fine.
    pub error: String,
    pub measured_fps: f64,
    pub audio_rate: u32,
    pub audio_channels: u32,
    pub audio_error: String,
    pub mister_connected: bool,
    pub mister_error: String,
    pub viewers: Vec<ViewerStatus>,
    pub encode_ms: f64,
    /// Frames captured but never sent: every encoder was still busy.
    pub skipped: u64,
}

#[derive(Debug, Clone, Default, serde::Serialize)]
#[serde(rename_all = "camelCase")]
pub struct ViewerStatus {
    pub id: u32,
    pub name: String,
    pub in_control: bool,
    pub width: u32,
    pub height: u32,
}

pub struct Stream {
    pub config: StreamConfig,
    stop: AtomicBool,
    subscribers: Mutex<Vec<Arc<dyn Subscriber>>>,
    status: Mutex<StreamStatus>,
    /// Capture → encoders: the newest frame not yet taken.
    latest: Mutex<Slot>,
    wake: Condvar,
    /// Encoders finish out of order; frames go out in order: the sequence
    /// number to deliver next.
    delivered: Mutex<u64>,
    delivered_cv: Condvar,
    control: Mutex<(Control, HeldInput)>,
    mister: Mutex<Option<Arc<MisterSession>>>,
    mister_status: Mutex<BTreeMap<String, String>>,
    /// Card audio handed over by the capture thread (cards whose sound is a
    /// pin on the video device).
    card_audio: Mutex<Option<AudioCapture>>,
    /// Size each viewer got last (for the status page).
    sizes: Mutex<BTreeMap<ViewerId, (u32, u32)>>,
    threads: Mutex<Vec<JoinHandle<()>>>,
}

impl Stream {
    pub fn start(config: StreamConfig) -> Arc<Self> {
        let stream = Arc::new(Self {
            config,
            stop: AtomicBool::new(false),
            subscribers: Mutex::new(Vec::new()),
            status: Mutex::new(StreamStatus { error: "starting".into(), ..Default::default() }),
            latest: Mutex::new(Slot::default()),
            wake: Condvar::new(),
            delivered: Mutex::new(0),
            delivered_cv: Condvar::new(),
            control: Mutex::new((Control::default(), HeldInput::default())),
            mister: Mutex::new(None),
            mister_status: Mutex::new(BTreeMap::new()),
            sizes: Mutex::new(BTreeMap::new()),
            card_audio: Mutex::new(None),
            threads: Mutex::new(Vec::new()),
        });
        log(&format!("stream '{}': start (video '{}', audio '{}', MiSTer '{}')", stream.config.name, stream.config.video_name, stream.config.audio, stream.config.mister));
        let mut threads = Vec::new();
        let s = stream.clone();
        threads.push(spawn(&format!("capture-{}", s.config.name), move || capture_loop(s)));
        for i in 0..ENCODERS {
            let s = stream.clone();
            threads.push(spawn(&format!("encode-{}-{i}", s.config.name), move || encode_loop(s)));
        }
        if !stream.config.audio.is_empty() {
            let s = stream.clone();
            threads.push(spawn(&format!("audio-{}", s.config.name), move || audio_loop(s)));
        }
        *stream.threads.lock().unwrap() = threads;
        if !stream.config.mister.trim().is_empty() {
            let observer = Arc::new(MisterObserver(Arc::downgrade(&stream)));
            let session = MisterSession::new(stream.config.mister.trim().to_string(), stream.config.mister_port, observer);
            *stream.mister.lock().unwrap() = Some(session);
        } else {
            stream.status.lock().unwrap().mister_error = "no MiSTer address set".into();
        }
        stream
    }

    pub fn stop(&self) {
        log(&format!("stream '{}': stop", self.config.name));
        self.stop.store(true, Ordering::Relaxed);
        self.wake.notify_all();
        if let Some(m) = self.mister.lock().unwrap().take() {
            m.disconnect();
        }
        let subs: Vec<_> = self.subscribers.lock().unwrap().drain(..).collect();
        for s in subs {
            s.close();
        }
        for t in self.threads.lock().unwrap().drain(..) {
            let _ = t.join();
        }
    }

    pub fn info(&self) -> StreamInfo {
        let st = self.status.lock().unwrap();
        StreamInfo {
            name: self.config.name.clone(),
            mister: (!self.config.mister.trim().is_empty()).then(|| self.config.mister.trim().to_string()),
            width: st.width,
            height: st.height,
            fps: st.fps,
            audio_rate: st.audio_rate,
            audio_channels: st.audio_channels,
            sizes: scale::offered_sizes(st.width, st.height),
            codecs: vec![Codec::Jpeg],
        }
    }

    pub fn status(&self) -> StreamStatus {
        let mut st = self.status.lock().unwrap().clone();
        let holder = self.control.lock().unwrap().0.holder();
        let sizes = self.sizes.lock().unwrap().clone();
        st.viewers = self
            .subscribers
            .lock()
            .unwrap()
            .iter()
            .filter(|s| s.id() < PREVIEW_IDS)
            .map(|s| {
                let (width, height) = sizes.get(&s.id()).copied().unwrap_or_default();
                ViewerStatus { id: s.id(), name: s.name(), in_control: holder == Some(s.id()), width, height }
            })
            .collect();
        st
    }

    /// A viewer joined: tell it the current state.
    pub fn add(&self, sub: Arc<dyn Subscriber>) {
        let id = sub.id();
        sub.message(Message::Control(self.control_state_for(id)));
        let mister = self.mister_state();
        sub.message(Message::Mister(mister));
        for (key, value) in self.mister_status.lock().unwrap().iter() {
            sub.message(Message::Status(Status { key: key.clone(), value: value.clone() }));
        }
        let st = self.status.lock().unwrap().clone();
        if !st.signal {
            sub.message(Message::Format(Format { width: st.width, height: st.height, fps: st.fps, signal: false }));
        }
        self.subscribers.lock().unwrap().push(sub);
    }

    pub fn remove(&self, id: ViewerId) {
        // Release and announce while the viewer is still listed (for its name).
        let change = self.control.lock().unwrap().0.leave(id);
        if let Some(change) = change {
            self.control_changed(change);
        }
        self.subscribers.lock().unwrap().retain(|s| s.id() != id);
        self.sizes.lock().unwrap().remove(&id);
    }

    pub fn input(&self, viewer: ViewerId, cmds: Vec<InputCmd>) {
        let (forward, change) = self.control.lock().unwrap().0.input(viewer);
        if let Some(change) = change {
            self.control_changed(change);
        }
        if !forward {
            return;
        }
        let session = self.mister.lock().unwrap().clone();
        let mut control = self.control.lock().unwrap();
        for cmd in cmds {
            control.1.observe(&cmd);
            if let Some(s) = &session {
                match cmd {
                    InputCmd::Send { m } => s.send_message(m),
                    InputCmd::Move { dx, dy } => s.move_raw(dx, dy),
                    InputCmd::Delay { ms } => s.delay(ms),
                }
            }
        }
    }

    pub fn take_control(&self, viewer: ViewerId) {
        let change = self.control.lock().unwrap().0.take(viewer);
        if let Some(change) = change {
            self.control_changed(change);
        }
    }

    /// Release what the old holder held, then tell everyone.
    fn control_changed(&self, change: ControlChange) {
        let release = self.control.lock().unwrap().1.release_all();
        if let Some(s) = self.mister.lock().unwrap().clone() {
            for m in release {
                s.send_message(m);
            }
        }
        let name = |id: Option<ViewerId>| {
            id.and_then(|id| self.subscribers.lock().unwrap().iter().find(|s| s.id() == id).map(|s| s.name())).unwrap_or_else(|| "nobody".into())
        };
        log(&format!("stream '{}': control {} -> {}", self.config.name, name(change.from), name(change.to)));
        let subs = self.subscribers.lock().unwrap().clone();
        for s in subs {
            s.message(Message::Control(self.control_state_for(s.id())));
        }
    }

    fn control_state_for(&self, id: ViewerId) -> ControlState {
        let holder = self.control.lock().unwrap().0.holder();
        let holder_view = holder.map(|h| {
            let name = self.subscribers.lock().unwrap().iter().find(|s| s.id() == h).map(|s| s.name()).unwrap_or_default();
            Viewer { id: h, name }
        });
        ControlState { holder: holder_view, you: holder == Some(id) }
    }

    fn mister_state(&self) -> MisterState {
        let st = self.status.lock().unwrap();
        MisterState { connected: st.mister_connected, reason: st.mister_error.clone() }
    }

    fn broadcast(&self, message: Message) {
        let subs = self.subscribers.lock().unwrap().clone();
        for s in subs {
            s.message(message.clone());
        }
    }

    fn set_error(&self, error: &str) {
        let mut st = self.status.lock().unwrap();
        if st.error != error {
            if !error.is_empty() {
                log(&format!("stream '{}': {error}", self.config.name));
            }
            st.error = error.to_string();
        }
    }

    fn set_signal(&self, signal: bool) {
        let changed = {
            let mut st = self.status.lock().unwrap();
            let changed = st.signal != signal;
            st.signal = signal;
            changed.then(|| Format { width: st.width, height: st.height, fps: st.fps, signal })
        };
        if let Some(f) = changed {
            self.broadcast(Message::Format(f));
        }
    }

    fn stopped(&self) -> bool {
        self.stop.load(Ordering::Relaxed)
    }
}

/// Setup-page previews use ids from here up, so they never look like viewers.
pub const PREVIEW_IDS: ViewerId = 1 << 30;

fn spawn(name: &str, f: impl FnOnce() + Send + 'static) -> JoinHandle<()> {
    thread::Builder::new().name(name.into()).spawn(f).expect("spawn thread")
}

fn wait(stream: &Stream, d: Duration) {
    let until = Instant::now() + d;
    while Instant::now() < until && !stream.stopped() {
        thread::sleep(Duration::from_millis(50));
    }
}

fn describe(e: &CaptureError) -> String {
    match e {
        CaptureError::InUse(_) => "capture card in use by another app (OBS?)".into(),
        CaptureError::NotFound(_) => "capture card not found (unplugged?)".into(),
        other => other.to_string(),
    }
}

/// The device id to open: the configured one, or a device with the same
/// name if the id is gone (moved to another USB port).
fn device_id(config: &StreamConfig) -> String {
    let Ok(devices) = kvms_capture::devices() else { return config.video.clone() };
    if devices.iter().any(|d| d.id == config.video) {
        return config.video.clone();
    }
    devices
        .iter()
        .find(|d| !config.video_name.is_empty() && d.name == config.video_name)
        .map(|d| d.id.clone())
        .unwrap_or_else(|| config.video.clone())
}

fn capture_loop(stream: Arc<Stream>) {
    while !stream.stopped() {
        let id = device_id(&stream.config);
        let card_audio = is_card_audio(&stream.config.audio);
        let opened = if card_audio {
            VideoCapture::open_with_audio(&id, stream.config.format, Output::Native)
        } else {
            VideoCapture::open(&id, stream.config.format, Output::Native)
        };
        let mut cap = match opened {
            Ok(c) => c,
            Err(e) => {
                stream.set_error(&describe(&e));
                stream.set_signal(false);
                wait(&stream, RETRY);
                continue;
            }
        };
        let format = cap.format();
        log(&format!("stream '{}': capturing {format}", stream.config.name));
        if card_audio {
            match cap.take_audio() {
                Some(a) => *stream.card_audio.lock().unwrap() = Some(a),
                None => stream.status.lock().unwrap().audio_error = "this card has no audio of its own".into(),
            }
        }
        {
            let mut st = stream.status.lock().unwrap();
            st.fps = format.fps;
            st.pixel = format.pixel.name().into();
        }
        stream.set_error("");
        let mut last_frame = Instant::now();
        let mut rate = (Instant::now(), 0u32);
        while !stream.stopped() {
            match cap.next_frame(Duration::from_millis(200)) {
                Ok(Some(frame)) => {
                    last_frame = Instant::now();
                    rate.1 += 1;
                    if rate.0.elapsed() >= Duration::from_secs(1) {
                        stream.status.lock().unwrap().measured_fps = rate.1 as f64 / rate.0.elapsed().as_secs_f64();
                        rate = (Instant::now(), 0);
                    }
                    let resized = {
                        let mut st = stream.status.lock().unwrap();
                        let resized = (st.width, st.height) != (frame.width, frame.height);
                        st.width = frame.width;
                        st.height = frame.height;
                        resized.then(|| Format { width: frame.width, height: frame.height, fps: st.fps, signal: true })
                    };
                    if let Some(f) = resized {
                        stream.status.lock().unwrap().signal = true;
                        stream.broadcast(Message::Format(f));
                    }
                    stream.set_signal(true);
                    {
                        let mut slot = stream.latest.lock().unwrap();
                        if slot.frame.replace(frame).is_some() {
                            // Every encoder busy: this frame never went out.
                            stream.status.lock().unwrap().skipped += 1;
                        }
                    }
                    stream.wake.notify_all();
                }
                Ok(None) => {
                    if last_frame.elapsed() >= NO_SIGNAL_AFTER {
                        stream.set_signal(false);
                    }
                }
                Err(e) => {
                    stream.set_error(&describe(&e));
                    stream.set_signal(false);
                    break;
                }
            }
        }
        drop(cap);
        if !stream.stopped() {
            wait(&stream, RETRY);
        }
    }
}

/// An encoder thread: takes the newest frame, encodes one picture per
/// distinct size viewers want, and hands them out in capture order.
fn encode_loop(stream: Arc<Stream>) {
    let mut native_bgra = Bgra::default();
    loop {
        let (seq, frame) = {
            let mut slot = stream.latest.lock().unwrap();
            loop {
                if stream.stopped() {
                    return;
                }
                if let Some(f) = slot.frame.take() {
                    let seq = slot.next_seq;
                    slot.next_seq += 1;
                    break (seq, f);
                }
                slot = stream.wake.wait_timeout(slot, Duration::from_millis(200)).unwrap().0;
            }
        };
        let started = Instant::now();
        let out = encode_frame(&stream, frame, &mut native_bgra);
        let ms = started.elapsed().as_secs_f64() * 1000.0;
        // Wait for the frames taken before this one to go out first.
        let mut delivered = stream.delivered.lock().unwrap();
        while *delivered < seq && !stream.stopped() {
            delivered = stream.delivered_cv.wait_timeout(delivered, Duration::from_millis(200)).unwrap().0;
        }
        if let Some(out) = out {
            let mut sizes = stream.sizes.lock().unwrap();
            for (sub, target, e) in out {
                sub.video(e);
                sizes.insert(sub.id(), target);
            }
            let mut st = stream.status.lock().unwrap();
            st.encode_ms = if st.encode_ms == 0.0 { ms } else { st.encode_ms * 0.9 + ms * 0.1 };
        }
        *delivered = seq + 1;
        drop(delivered);
        stream.delivered_cv.notify_all();
    }
}

type Deliveries = Vec<(Arc<dyn Subscriber>, (u32, u32), Arc<Encoded>)>;

/// One picture to encode: size and JPEG quality (None: the stream's own).
type Target = (u32, u32, Option<u8>);

/// Whether a frame captured at `ts` (100 ns units) goes to a viewer that
/// wants at most `max_fps` from a card delivering `source_fps`: the first
/// frame of each 1/max_fps slot does. Decided from the timestamp alone, so
/// encoder threads may decide frames in any order.
fn due(ts: i64, source_fps: f64, max_fps: Option<f64>) -> bool {
    let Some(max) = max_fps.filter(|m| *m > 0.0) else { return true };
    if source_fps <= 0.0 || source_fps <= max * 1.05 {
        return true;
    }
    let slot = 1e7 / max;
    let frame = 1e7 / source_fps;
    (ts as f64 / slot).floor() != ((ts as f64 - frame) / slot).floor()
}

/// The pictures for one frame, per viewer; None if nobody wants video or
/// the frame couldn't be read.
fn encode_frame(stream: &Stream, frame: VideoFrame, native_bgra: &mut Bgra) -> Option<Deliveries> {
    let source_fps = stream.status.lock().unwrap().fps;
    let subs: Vec<Arc<dyn Subscriber>> = stream
        .subscribers
        .lock()
        .unwrap()
        .iter()
        .filter(|s| {
            let w = s.want();
            w.video && due(frame.timestamp, source_fps, w.max_fps)
        })
        .cloned()
        .collect();
    if subs.is_empty() {
        return None;
    }
    // Viewer → target (size and quality), at most MAX_PIPELINES distinct.
    let mut wanted: Vec<(Arc<dyn Subscriber>, Target)> = subs
        .into_iter()
        .map(|s| {
            let w = s.want();
            let (width, height) = scale::size_for(frame.width, frame.height, w.max_width, w.max_height);
            (s, (width, height, w.quality.map(|q| q.clamp(1, 100))))
        })
        .collect();
    let mut targets: Vec<Target> = wanted.iter().map(|(_, t)| *t).collect();
    targets.sort_by(|a, b| (b.0 * b.1).cmp(&(a.0 * a.1)).then(b.2.cmp(&a.2)));
    targets.dedup();
    if targets.len() > MAX_PIPELINES {
        let kept = targets[..MAX_PIPELINES].to_vec();
        for (_, t) in wanted.iter_mut() {
            if !kept.contains(t) {
                // The largest kept size that fits, else the smallest kept.
                *t = kept.iter().copied().find(|k| k.0 <= t.0 && k.1 <= t.1).unwrap_or(*kept.last().unwrap());
            }
        }
        targets = kept;
    }

    let native = (frame.width, frame.height);
    // A card's own JPEG passes through untouched at native size, unless the
    // viewer asked for another quality.
    let passthrough = |t: &Target| frame.format == PixelFormat::Mjpeg && (t.0, t.1) == native && t.2.is_none();
    // BGRA only to scale, re-encode a card's JPEG, or for a BGRA card:
    // native-size pictures of raw cards are encoded straight from their YUV.
    let needs_pixels = targets.iter().any(|t| (t.0, t.1) != native || (frame.format == PixelFormat::Mjpeg && !passthrough(t)))
        || frame.format == PixelFormat::Bgra;
    if needs_pixels {
        let ok = match frame.format {
            PixelFormat::Mjpeg => jpeg::decode(&frame.data, native_bgra).map_err(|e| e.to_string()),
            PixelFormat::Nv12 => {
                convert::nv12_to_bgra(&frame.data, frame.width, frame.height, frame.stride, native_bgra);
                Ok(())
            }
            PixelFormat::Yuy2 => {
                convert::yuy2_to_bgra(&frame.data, frame.width, frame.height, frame.stride, native_bgra);
                Ok(())
            }
            PixelFormat::Bgra => {
                *native_bgra = Bgra { width: frame.width, height: frame.height, stride: frame.stride, data: frame.data.clone() };
                Ok(())
            }
        };
        if let Err(e) = ok {
            log(&format!("stream '{}': {e}", stream.config.name));
            return None;
        }
    }
    let native_bgra = &*native_bgra;
    let encode = |target: Target| -> Option<Arc<Encoded>> {
        let quality = target.2.unwrap_or(stream.config.quality).clamp(1, 100);
        let data = if passthrough(&target) {
            frame.data.clone()
        } else {
            let mut out = Vec::new();
            let result = if (target.0, target.1) == native {
                let (data, width, height, stride) = (&frame.data[..], frame.width, frame.height, frame.stride);
                match frame.format {
                    PixelFormat::Nv12 => jpeg::encode_yuv(jpeg::Yuv::Nv12 { data, width, height, stride }, quality, &mut out),
                    PixelFormat::Yuy2 => jpeg::encode_yuv(jpeg::Yuv::Yuy2 { data, width, height, stride }, quality, &mut out),
                    _ => jpeg::encode(native_bgra, quality, &mut out),
                }
            } else {
                let mut scaled = Bgra::default();
                scale::scale(native_bgra, target.0, target.1, &mut scaled);
                jpeg::encode(&scaled, quality, &mut out)
            };
            if let Err(e) = result {
                log(&format!("stream '{}': {e}", stream.config.name));
                return None;
            }
            out
        };
        Some(Arc::new(Encoded { codec: Codec::Jpeg, width: target.0, height: target.1, data, timestamp: frame.timestamp }))
    };
    let encoded: Vec<(Target, Option<Arc<Encoded>>)> = if targets.len() == 1 {
        vec![(targets[0], encode(targets[0]))]
    } else {
        thread::scope(|scope| {
            let handles: Vec<_> = targets.iter().map(|t| (*t, scope.spawn(move || encode(*t)))).collect();
            handles.into_iter().map(|(t, h)| (t, h.join().ok().flatten())).collect()
        })
    };
    Some(
        wanted
            .into_iter()
            .filter_map(|(sub, target)| match encoded.iter().find(|(t, _)| *t == target) {
                Some((_, Some(e))) => Some((sub, (target.0, target.1), e.clone())),
                _ => None,
            })
            .collect(),
    )
}

fn audio_loop(stream: Arc<Stream>) {
    let card_audio = is_card_audio(&stream.config.audio);
    while !stream.stopped() {
        let opened = if card_audio {
            // Arrives with the video (the capture thread opens both).
            stream.card_audio.lock().unwrap().take().ok_or(None)
        } else {
            AudioCapture::open(&stream.config.audio).map_err(Some)
        };
        let cap = match opened {
            Ok(c) => c,
            Err(e) => {
                if let Some(e) = e {
                    stream.status.lock().unwrap().audio_error = e.to_string();
                }
                wait(&stream, if card_audio { Duration::from_millis(200) } else { RETRY });
                continue;
            }
        };
        {
            let mut st = stream.status.lock().unwrap();
            st.audio_rate = cap.rate;
            st.audio_channels = cap.channels as u32;
            st.audio_error.clear();
        }
        while !stream.stopped() {
            match cap.next_chunk(Duration::from_millis(200)) {
                Ok(Some(chunk)) => {
                    let packet = Arc::new(AudioPacket {
                        channels: chunk.channels as u8,
                        rate: chunk.rate,
                        samples: chunk.samples,
                        timestamp: chunk.timestamp,
                    });
                    let subs = stream.subscribers.lock().unwrap().clone();
                    for s in subs.iter().filter(|s| s.want().audio) {
                        s.audio(packet.clone());
                    }
                }
                Ok(None) => {}
                Err(e) => {
                    stream.status.lock().unwrap().audio_error = e.to_string();
                    break;
                }
            }
        }
    }
}

/// The stream's mrext session → status and viewers.
struct MisterObserver(Weak<Stream>);

impl SessionObserver for MisterObserver {
    fn on_state(&self, state: ConnectionState) {
        let Some(stream) = self.0.upgrade() else { return };
        {
            let mut st = stream.status.lock().unwrap();
            match &state {
                ConnectionState::Connected => {
                    st.mister_connected = true;
                    st.mister_error.clear();
                }
                ConnectionState::Connecting => st.mister_connected = false,
                ConnectionState::Disconnected { reason } => {
                    st.mister_connected = false;
                    st.mister_error = reason.clone();
                }
            }
        }
        let m = stream.mister_state();
        stream.broadcast(Message::Mister(m));
    }

    fn on_status(&self, key: String, value: String) {
        let Some(stream) = self.0.upgrade() else { return };
        stream.mister_status.lock().unwrap().insert(key.clone(), value.clone());
        stream.broadcast(Message::Status(Status { key, value }));
    }
}

#[cfg(test)]
mod tests {
    use super::due;

    /// Frames of a card at `fps` for one second, 100 ns timestamps from `start`.
    fn frames(fps: f64, start: i64) -> Vec<i64> {
        (0..fps as i64).map(|i| start + (i as f64 * 1e7 / fps) as i64).collect()
    }

    #[test]
    fn caps_the_frame_rate() {
        let start = 17_604_000_000_000_000; // an arbitrary wall-clock time
        let sent = |src: f64, max: Option<f64>| frames(src, start).iter().filter(|&&t| due(t, src, max)).count();
        assert_eq!(sent(60.0, Some(30.0)), 30);
        assert_eq!(sent(60.0, Some(20.0)), 20);
        assert_eq!(sent(59.94, Some(30.0)), 30);
        // No cap, or a card already at or under it: every frame.
        assert_eq!(sent(60.0, None), 60);
        assert_eq!(sent(30.0, Some(30.0)), 30);
        assert_eq!(sent(25.0, Some(30.0)), 25);
    }
}

//! A connection to one stream on a `misterkvms-capture` server (mkvm
//! protocol). One link carries a host's video, audio and input for as long
//! as the host is open; it reconnects by itself. The video session and the
//! input session both use it (docs/CAPTURE-SERVER.md §4, §7).

use std::collections::HashMap;
use std::io::BufReader;
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use mkvm::{
    read_message, write_message, Audio, Codec, ControlState, Hello, InputCmd, Message, MisterState, StreamAddress, Subscribe, Video,
    Welcome,
};

use crate::log;
use crate::session::{ConnectionState, SessionObserver};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const PING_EVERY: Duration = Duration::from_secs(2);
/// No message for this long: the server is gone even if TCP hasn't noticed.
const SILENT_LIMIT: Duration = Duration::from_secs(8);

/// Receives a stream's pictures and sound (the video session). Called on
/// the link's reader thread; timestamps are already on this machine's clock.
pub(crate) trait LinkMedia: Send + Sync {
    fn video(&self, video: &Video, timestamp: i64);
    fn audio(&self, audio: &Audio, timestamp: i64);
    /// "connected", "no signal", "connecting", or an error.
    fn state(&self, state: &str);
}

/// What the UI shows about a server stream.
#[derive(Debug, Clone, Default, PartialEq, serde::Serialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct StreamState {
    /// Connected to the capture server.
    pub connected: bool,
    /// Why not, when not connected.
    pub error: String,
    /// The server is connected to the stream's MiSTer.
    pub mister_connected: bool,
    pub mister: String,
    /// This viewer controls the MiSTer.
    pub have_control: bool,
    /// Who does, when someone else does (empty if nobody).
    pub holder: String,
    pub round_trip_ms: f64,
}

#[derive(Default)]
struct State {
    connected: bool,
    error: String,
    welcome: Option<Welcome>,
    control: Option<ControlState>,
    mister: Option<MisterState>,
    status: HashMap<String, String>,
    /// Server clock minus ours, 100 ns units.
    clock_offset: i64,
    round_trip_ms: f64,
}

pub struct MkvmLink {
    address: StreamAddress,
    viewer: String,
    stop: AtomicBool,
    state: Mutex<State>,
    writer: Mutex<Option<TcpStream>>,
    subscription: Mutex<Subscribe>,
    media: Mutex<Option<Arc<dyn LinkMedia>>>,
    observer: Mutex<Option<Arc<dyn SessionObserver>>>,
}

impl MkvmLink {
    /// Start connecting to `address` (`mkvm://host/stream`). `viewer` is the
    /// name other viewers see.
    pub fn connect(address: StreamAddress, viewer: String) -> Arc<Self> {
        let link = Arc::new(Self {
            address,
            viewer,
            stop: AtomicBool::new(false),
            state: Mutex::new(State::default()),
            writer: Mutex::new(None),
            subscription: Mutex::new(Subscribe { video: false, audio: false, ..Default::default() }),
            media: Mutex::new(None),
            observer: Mutex::new(None),
        });
        let weak = Arc::downgrade(&link);
        let name = format!("mkvm-{}", link.address.stream);
        thread::Builder::new()
            .name(name)
            .spawn(move || run(weak))
            .expect("spawn mkvm link");
        link
    }

    pub fn address(&self) -> &StreamAddress {
        &self.address
    }

    pub fn close(&self) {
        self.stop.store(true, Ordering::Relaxed);
        if let Some(w) = self.writer.lock().unwrap().take() {
            let _ = w.shutdown(Shutdown::Both);
        }
    }

    pub fn stream_state(&self) -> StreamState {
        let s = self.state.lock().unwrap();
        let control = s.control.clone();
        StreamState {
            connected: s.connected,
            error: s.error.clone(),
            mister_connected: s.mister.as_ref().is_some_and(|m| m.connected),
            mister: s.welcome.as_ref().and_then(|w| w.stream.mister.clone()).unwrap_or_default(),
            have_control: control.as_ref().is_some_and(|c| c.you),
            holder: control.and_then(|c| if c.you { None } else { c.holder.map(|h| h.name) }).unwrap_or_default(),
            round_trip_ms: s.round_trip_ms,
        }
    }

    /// Where pictures and sound go (None: stop receiving them).
    pub(crate) fn set_media(&self, media: Option<Arc<dyn LinkMedia>>) {
        let on = media.is_some();
        *self.media.lock().unwrap() = media;
        let mut sub = self.subscription.lock().unwrap();
        sub.video = on;
        sub.audio = on;
        let sub = sub.clone();
        self.send(&Message::Subscribe(sub));
    }

    /// Ask for at most this picture size (None: native), this many pictures
    /// a second and this JPEG quality (None: the server's).
    pub fn set_limits(&self, max_width: Option<u32>, max_height: Option<u32>, max_fps: Option<f64>, quality: Option<u8>) {
        let mut sub = self.subscription.lock().unwrap();
        sub.max_width = max_width;
        sub.max_height = max_height;
        sub.max_fps = max_fps;
        sub.quality = quality;
        let sub = sub.clone();
        self.send(&Message::Subscribe(sub));
    }

    /// Who hears about the connection and MiSTer status (the input session's
    /// observer, i.e. the controller).
    pub(crate) fn set_observer(&self, observer: Option<Arc<dyn SessionObserver>>) {
        *self.observer.lock().unwrap() = observer.clone();
        if let Some(o) = observer {
            o.on_state(self.connection_state());
            let status = self.state.lock().unwrap().status.clone();
            for (k, v) in status {
                o.on_status(k, v);
            }
        }
    }

    pub fn take_control(&self) {
        self.send(&Message::TakeControl);
    }

    /// Input for the MiSTer. Returns false if not connected.
    pub(crate) fn send_input(&self, cmds: Vec<InputCmd>) -> bool {
        !cmds.is_empty() && self.send(&Message::Input(cmds))
    }

    pub(crate) fn is_connected(&self) -> bool {
        self.state.lock().unwrap().connected
    }

    /// As the input session reports it: connected only when the server is
    /// connected to the MiSTer too.
    fn connection_state(&self) -> ConnectionState {
        let s = self.state.lock().unwrap();
        if !s.connected {
            if s.error.is_empty() {
                ConnectionState::Connecting
            } else {
                ConnectionState::Disconnected { reason: s.error.clone() }
            }
        } else {
            match &s.mister {
                Some(m) if m.connected => ConnectionState::Connected,
                Some(m) if !m.reason.is_empty() => ConnectionState::Disconnected { reason: format!("capture server: {}", m.reason) },
                _ => ConnectionState::Connecting,
            }
        }
    }

    fn send(&self, msg: &Message) -> bool {
        let mut w = self.writer.lock().unwrap();
        let Some(stream) = w.as_mut() else { return false };
        if let Err(e) = write_message(stream, msg, mkvm_now()) {
            log(&format!("mkvm: send failed: {e}"));
            let _ = stream.shutdown(Shutdown::Both);
            *w = None;
            return false;
        }
        true
    }

    fn notify_state(&self) {
        let state = self.connection_state();
        let observer = self.observer.lock().unwrap().clone();
        if let Some(o) = observer {
            o.on_state(state);
        }
    }

    fn media_state(&self, text: &str) {
        let media = self.media.lock().unwrap().clone();
        if let Some(m) = media {
            m.state(text);
        }
    }
}

fn mkvm_now() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| (d.as_nanos() / 100) as i64)
}

fn run(weak: std::sync::Weak<MkvmLink>) {
    loop {
        let Some(link) = weak.upgrade() else { return };
        if link.stop.load(Ordering::Relaxed) {
            return;
        }
        link.media_state("connecting");
        let result = session(&link);
        {
            let mut s = link.state.lock().unwrap();
            s.connected = false;
            s.control = None;
            s.mister = None;
            s.error = match &result {
                Ok(()) => "closed by the capture server".into(),
                Err(e) => e.clone(),
            };
        }
        *link.writer.lock().unwrap() = None;
        if link.stop.load(Ordering::Relaxed) {
            return;
        }
        log(&format!("mkvm: {} {}", link.address, link.state.lock().unwrap().error));
        link.media_state(&link.state.lock().unwrap().error.clone());
        link.notify_state();
        drop(link);
        thread::sleep(RECONNECT_DELAY);
    }
}

/// One connection, from connect to close.
fn session(link: &Arc<MkvmLink>) -> Result<(), String> {
    let addr = link
        .address
        .socket()
        .to_socket_addrs()
        .map_err(|e| format!("can't find {}: {e}", link.address.host))?
        .next()
        .ok_or_else(|| format!("can't find {}", link.address.host))?;
    let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT).map_err(|e| format!("can't reach the capture server: {e}"))?;
    stream.set_nodelay(true).ok();
    stream.set_read_timeout(Some(SILENT_LIMIT)).ok();
    let mut write = stream.try_clone().map_err(|e| e.to_string())?;
    let hello = Message::Hello(Hello {
        protocol: mkvm::PROTOCOL_VERSION,
        stream: link.address.stream.clone(),
        viewer: link.viewer.clone(),
        platform: std::env::consts::OS.into(),
        codecs: vec![Codec::Jpeg],
    });
    write_message(&mut write, &hello, mkvm_now()).map_err(|e| e.to_string())?;
    let mut read = BufReader::with_capacity(256 * 1024, stream);
    let welcome = match read_message(&mut read).map_err(|e| e.to_string())?.0 {
        Message::Welcome(w) => w,
        Message::Error(e) => return Err(e.message),
        other => return Err(format!("unexpected {other:?}")),
    };
    log(&format!(
        "mkvm: {} connected ({} {}x{}, MiSTer {:?})",
        link.address, welcome.server, welcome.stream.width, welcome.stream.height, welcome.stream.mister
    ));
    let sub = link.subscription.lock().unwrap().clone();
    write_message(&mut write, &Message::Subscribe(sub), mkvm_now()).map_err(|e| e.to_string())?;
    {
        let mut s = link.state.lock().unwrap();
        s.connected = true;
        s.error.clear();
        s.welcome = Some(welcome);
        s.status.clear();
    }
    *link.writer.lock().unwrap() = Some(write);
    link.notify_state();
    link.media_state("connected");

    let mut last_ping = Instant::now() - PING_EVERY;
    loop {
        if link.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        if last_ping.elapsed() >= PING_EVERY {
            last_ping = Instant::now();
            link.send(&Message::Ping);
        }
        let (msg, header) = read_message(&mut read).map_err(|e| e.to_string())?;
        match msg {
            Message::Video(v) => {
                let ts = header.timestamp - link.state.lock().unwrap().clock_offset;
                let media = link.media.lock().unwrap().clone();
                if let Some(m) = media {
                    m.video(&v, ts);
                }
            }
            Message::Audio(a) => {
                let ts = header.timestamp - link.state.lock().unwrap().clock_offset;
                let media = link.media.lock().unwrap().clone();
                if let Some(m) = media {
                    m.audio(&a, ts);
                }
            }
            Message::Ping => {
                link.send(&Message::Pong { echo: header.timestamp });
            }
            Message::Pong { echo } => {
                let now = mkvm_now();
                let rtt = now - echo;
                if (0..10_000_000).contains(&rtt) {
                    let mut s = link.state.lock().unwrap();
                    let offset = header.timestamp - (echo + rtt / 2);
                    // Smooth: one slow round trip shouldn't move the clock much.
                    s.clock_offset = if s.round_trip_ms == 0.0 { offset } else { (s.clock_offset * 7 + offset) / 8 };
                    s.round_trip_ms = rtt as f64 / 10_000.0;
                }
            }
            Message::Format(f) => {
                if !f.signal {
                    link.media_state("no signal");
                }
            }
            Message::Control(c) => {
                link.state.lock().unwrap().control = Some(c);
                link.notify_state();
            }
            Message::Mister(m) => {
                link.state.lock().unwrap().mister = Some(m);
                link.notify_state();
            }
            Message::Status(st) => {
                link.state.lock().unwrap().status.insert(st.key.clone(), st.value.clone());
                let observer = link.observer.lock().unwrap().clone();
                if let Some(o) = observer {
                    o.on_status(st.key, st.value);
                }
            }
            Message::Error(e) => return Err(e.message),
            _ => {}
        }
    }
}

//! A connection to one stream on a `misterkvms-capture` server (mkvm
//! protocol). One link carries a host's video, audio and input for as long
//! as the host is open; it reconnects by itself. The video session and the
//! input session both use it (docs/CAPTURE-SERVER.md §4, §7).
//!
//! A paired device connects encrypted with its key (mkvm::secure), at home
//! or, when home doesn't answer, at the server's internet address; from the
//! internet it asks for fewer, lighter pictures (docs/IPHONE.md §3–4).

use std::collections::HashMap;
use std::io::{BufReader, Read, Write};
use std::net::{Shutdown, TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use mkvm::secure::{self, DeviceKey, SecureReader, SecureWriter};
use mkvm::{
    read_message, write_message, Audio, Codec, ControlState, Hello, InputCmd, Message, MisterState, Pair, Paired, StreamAddress,
    Subscribe, Video, Welcome,
};

use crate::log;
use crate::session::{ConnectionState, SessionObserver};

const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const PING_EVERY: Duration = Duration::from_secs(2);
/// No message for this long: the server is gone even if TCP hasn't noticed.
const SILENT_LIMIT: Duration = Duration::from_secs(8);
/// With an internet address to fall back on, the other route gets this long.
const FIRST_TRY_TIMEOUT: Duration = Duration::from_millis(1500);
/// From the internet a viewer asks for at most this, unless it asked for less.
const INTERNET_FPS: f64 = 30.0;
const INTERNET_QUALITY: u8 = 70;

/// A paired device's key, and where the server is reached from the internet.
#[derive(Debug, Clone)]
pub struct Remote {
    pub key: DeviceKey,
    pub internet: Option<StreamAddress>,
}

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
    /// Connected from the internet (encrypted), not at home.
    pub internet: bool,
    /// The capture PC's permanent id (empty: an older server).
    pub server_id: String,
}

#[derive(Default)]
struct State {
    connected: bool,
    /// The connection goes through the internet address.
    internet: bool,
    /// The route that worked last, tried first next time.
    prefer_internet: bool,
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
    remote: Option<Remote>,
    /// The open connection: where messages are written, and its socket.
    conn: Mutex<Option<Conn>>,
    subscription: Mutex<Subscribe>,
    media: Mutex<Option<Arc<dyn LinkMedia>>>,
    observer: Mutex<Option<Arc<dyn SessionObserver>>>,
}

impl MkvmLink {
    /// Start connecting to `address` (`mkvm://host/stream`). `viewer` is the
    /// name other viewers see.
    pub fn connect(address: StreamAddress, viewer: String) -> Arc<Self> {
        Self::connect_with(address, viewer, None)
    }

    /// Like `connect`, as a paired device: encrypted, and from the internet
    /// when home doesn't answer.
    pub fn connect_with(address: StreamAddress, viewer: String, remote: Option<Remote>) -> Arc<Self> {
        let link = Arc::new(Self {
            address,
            viewer,
            stop: AtomicBool::new(false),
            state: Mutex::new(State::default()),
            remote,
            conn: Mutex::new(None),
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
        if let Some(c) = self.conn.lock().unwrap().take() {
            let _ = c.sock.shutdown(Shutdown::Both);
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
            internet: s.connected && s.internet,
            server_id: s.welcome.as_ref().map(|w| w.server_id.clone()).unwrap_or_default(),
        }
    }

    /// Where pictures and sound go (None: stop receiving them).
    pub(crate) fn set_media(&self, media: Option<Arc<dyn LinkMedia>>) {
        let on = media.is_some();
        *self.media.lock().unwrap() = media;
        let sub = {
            let mut sub = self.subscription.lock().unwrap();
            sub.video = on;
            sub.audio = on;
            sub.clone()
        };
        self.send(&Message::Subscribe(self.effective(sub)));
    }

    /// Ask for at most this picture size (None: native), this many pictures
    /// a second and this JPEG quality (None: the server's).
    pub fn set_limits(&self, max_width: Option<u32>, max_height: Option<u32>, max_fps: Option<f64>, quality: Option<u8>) {
        let sub = {
            let mut sub = self.subscription.lock().unwrap();
            sub.max_width = max_width;
            sub.max_height = max_height;
            sub.max_fps = max_fps;
            sub.quality = quality;
            sub.clone()
        };
        self.send(&Message::Subscribe(self.effective(sub)));
    }

    /// What to ask for on this connection: from the internet, at most
    /// INTERNET_FPS pictures at INTERNET_QUALITY.
    fn effective(&self, mut sub: Subscribe) -> Subscribe {
        if self.state.lock().unwrap().internet {
            sub.max_fps = Some(sub.max_fps.map_or(INTERNET_FPS, |f| f.min(INTERNET_FPS)));
            sub.quality = Some(sub.quality.map_or(INTERNET_QUALITY, |q| q.min(INTERNET_QUALITY)));
        }
        sub
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
        let mut c = self.conn.lock().unwrap();
        let Some(conn) = c.as_mut() else { return false };
        if let Err(e) = write_message(&mut conn.write, msg, mkvm_now()) {
            log(&format!("mkvm: send failed: {e}"));
            let _ = conn.sock.shutdown(Shutdown::Both);
            *c = None;
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
        *link.conn.lock().unwrap() = None;
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

struct Conn {
    write: Box<dyn Write + Send>,
    sock: TcpStream,
}

fn tcp(address: &StreamAddress, timeout: Duration) -> Result<TcpStream, String> {
    let addr = address
        .socket()
        .to_socket_addrs()
        .map_err(|e| format!("can't find {}: {e}", address.host))?
        .next()
        .ok_or_else(|| format!("can't find {}", address.host))?;
    TcpStream::connect_timeout(&addr, timeout).map_err(|e| format!("can't reach the capture server at {}: {e}", address.host))
}

/// Connect at home or from the internet, the route that worked last first.
/// Returns the socket and whether it goes through the internet.
fn open(link: &MkvmLink) -> Result<(TcpStream, bool), String> {
    let Some(internet) = link.remote.as_ref().and_then(|r| r.internet.clone()) else {
        return tcp(&link.address, CONNECT_TIMEOUT).map(|s| (s, false));
    };
    let mut routes = [(link.address.clone(), false), (internet, true)];
    if link.state.lock().unwrap().prefer_internet {
        routes.reverse();
    }
    let mut errors = Vec::new();
    for (i, (address, internet)) in routes.into_iter().enumerate() {
        match tcp(&address, if i == 0 { FIRST_TRY_TIMEOUT } else { CONNECT_TIMEOUT }) {
            Ok(s) => return Ok((s, internet)),
            Err(e) => errors.push(format!("{}: {e}", if internet { "from the internet" } else { "at home" })),
        }
    }
    Err(errors.join("; "))
}

/// Ask a capture server on the home network for a device key, to connect
/// from the internet with later (docs/IPHONE.md §2). `replaces`: the key
/// this device had for it, which the server then drops.
pub fn pair(address: &StreamAddress, device: &str, replaces: Option<String>) -> Result<Paired, String> {
    let sock = tcp(address, CONNECT_TIMEOUT)?;
    sock.set_read_timeout(Some(Duration::from_secs(10))).ok();
    let mut write = sock.try_clone().map_err(|e| e.to_string())?;
    write_message(&mut write, &Message::Pair(Pair { device: device.to_string(), replaces }), mkvm_now()).map_err(|e| e.to_string())?;
    match read_message(&mut BufReader::new(sock)).map_err(|e| e.to_string())?.0 {
        Message::Paired(p) => Ok(p),
        Message::Error(e) => Err(e.message),
        other => Err(format!("unexpected answer {other:?}")),
    }
}

/// The capture PC's permanent id, asked of a stream on the home network
/// (Hello, Welcome, hang up). Empty from servers older than server ids.
pub fn server_id(address: &StreamAddress, viewer: &str) -> Result<String, String> {
    let sock = tcp(address, CONNECT_TIMEOUT)?;
    sock.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut write = sock.try_clone().map_err(|e| e.to_string())?;
    let hello = Message::Hello(Hello {
        protocol: mkvm::PROTOCOL_VERSION,
        stream: address.stream.clone(),
        viewer: viewer.to_string(),
        platform: std::env::consts::OS.into(),
        codecs: vec![Codec::Jpeg],
    });
    write_message(&mut write, &hello, mkvm_now()).map_err(|e| e.to_string())?;
    let answer = read_message(&mut BufReader::new(sock.try_clone().map_err(|e| e.to_string())?)).map_err(|e| e.to_string())?.0;
    let _ = sock.shutdown(Shutdown::Both);
    match answer {
        Message::Welcome(w) => Ok(w.server_id),
        Message::Error(e) => Err(e.message),
        other => Err(format!("unexpected answer {other:?}")),
    }
}

/// One connection, from connect to close.
fn session(link: &Arc<MkvmLink>) -> Result<(), String> {
    let (sock, internet) = open(link)?;
    sock.set_nodelay(true).ok();
    sock.set_read_timeout(Some(SILENT_LIMIT)).ok();
    let clone = || sock.try_clone().map_err(|e| e.to_string());
    let (read, mut write): (Box<dyn Read + Send>, Box<dyn Write + Send>) = match &link.remote {
        Some(remote) => {
            let session = secure::connect(&mut clone()?, &remote.key).map_err(|e| e.to_string())?;
            (Box::new(SecureReader::new(clone()?, session.clone())), Box::new(SecureWriter::new(clone()?, session)))
        }
        None => (Box::new(clone()?), Box::new(clone()?)),
    };
    let hello = Message::Hello(Hello {
        protocol: mkvm::PROTOCOL_VERSION,
        stream: link.address.stream.clone(),
        viewer: link.viewer.clone(),
        platform: std::env::consts::OS.into(),
        codecs: vec![Codec::Jpeg],
    });
    write_message(&mut write, &hello, mkvm_now()).map_err(|e| e.to_string())?;
    let mut read = BufReader::with_capacity(256 * 1024, read);
    let welcome = match read_message(&mut read).map_err(|e| e.to_string())?.0 {
        Message::Welcome(w) => w,
        Message::Error(e) => return Err(e.message),
        other => return Err(format!("unexpected {other:?}")),
    };
    log(&format!(
        "mkvm: {} connected{} ({} {}x{}, MiSTer {:?})",
        link.address,
        match (internet, link.remote.is_some()) {
            (true, _) => " from the internet, encrypted",
            (false, true) => ", encrypted",
            _ => "",
        },
        welcome.server,
        welcome.stream.width,
        welcome.stream.height,
        welcome.stream.mister
    ));
    {
        let mut s = link.state.lock().unwrap();
        s.internet = internet;
        s.prefer_internet = internet;
    }
    let sub = link.subscription.lock().unwrap().clone();
    write_message(&mut write, &Message::Subscribe(link.effective(sub)), mkvm_now()).map_err(|e| e.to_string())?;
    {
        let mut s = link.state.lock().unwrap();
        s.connected = true;
        s.error.clear();
        s.welcome = Some(welcome);
        s.status.clear();
    }
    *link.conn.lock().unwrap() = Some(Conn { write, sock });
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

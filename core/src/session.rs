//! One MiSTer connection: a worker thread owns the mrext WebSocket, sends input
//! in order, coalesces mouse movement, reconnects, and reports status.

use std::net::{TcpStream, ToSocketAddrs};
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, Sender};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::{Duration, Instant};

use serde::Serialize;
use tungstenite::{Message, WebSocket};

use mkvm::InputCmd;

use crate::input::{InputSettings, InputState, KeyResult, MouseButton};
use crate::link::MkvmLink;
use crate::keymap::hid;

/// Mouse deltas are batched and sent at most this often (~250 Hz).
const MOUSE_FLUSH: Duration = Duration::from_millis(4);
/// How long the worker blocks waiting for input or server messages per turn.
/// Bounds the extra latency a command can see while the worker is reading.
const POLL: Duration = Duration::from_millis(1);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(3);
const RECONNECT_DELAY: Duration = Duration::from_secs(2);
const TAP_HOLD: Duration = Duration::from_millis(60);

#[derive(Debug, Clone, PartialEq, Serialize, uniffi::Enum)]
#[serde(tag = "state", rename_all = "camelCase")]
pub enum ConnectionState {
    Connecting,
    Connected,
    Disconnected { reason: String },
}

#[derive(Debug, Clone, Default, Serialize, uniffi::Record)]
#[serde(rename_all = "camelCase")]
pub struct InputStats {
    pub connected: bool,
    pub messages_sent: u64,
    pub messages_per_sec: f64,
    pub queued: u64,
}

/// Implemented by the shell. Called from the session's worker thread.
#[uniffi::export(with_foreign)]
pub trait SessionObserver: Send + Sync {
    fn on_state(&self, state: ConnectionState);
    /// Status pushed by mrext, e.g. ("coreRunning", "HP9K715").
    fn on_status(&self, key: String, value: String);
}

enum Cmd {
    Send(String),
    Move(f64, f64),
    Delay(Duration),
    Shutdown,
}

#[derive(Default)]
struct Counters {
    sent: AtomicU64,
    queued: AtomicU64,
    connected: AtomicU64,
}

#[derive(uniffi::Object)]
pub struct MisterSession {
    tx: Sender<Cmd>,
    input: Mutex<InputState>,
    counters: Arc<Counters>,
    rate: Mutex<(Instant, u64, f64)>,
    /// Also told about mouse movement (the video's predictive cursor).
    cursor_sink: Mutex<Option<Arc<dyn CursorSink>>>,
}

/// Receives the relative mouse movement that's sent to MiSTer.
pub(crate) trait CursorSink: Send + Sync {
    fn input(&self, dx: f64, dy: f64);
}

#[uniffi::export]
impl MisterSession {
    /// Connect to mrext Remote at `ws://{host}:{port}/api/ws`. Returns
    /// immediately; progress is reported through `observer`.
    #[uniffi::constructor]
    pub fn new(host: String, port: u16, observer: Arc<dyn SessionObserver>) -> Arc<Self> {
        let (tx, rx) = mpsc::channel();
        let counters = Arc::new(Counters::default());
        let worker_counters = counters.clone();
        thread::Builder::new()
            .name(format!("mrext-{host}"))
            .spawn(move || Worker::new(host, port, observer, rx, worker_counters).run())
            .expect("spawn session worker");
        Arc::new(Self {
            tx,
            input: Mutex::new(InputState::default()),
            counters,
            rate: Mutex::new((Instant::now(), 0, 0.0)),
            cursor_sink: Mutex::new(None),
        })
    }

    /// A key changed state. `hid_usage` is a USB HID keyboard usage (page 7).
    pub fn key_event(&self, hid_usage: u16, down: bool) -> KeyResult {
        let mut out = Vec::new();
        let result = self.input.lock().unwrap().key_event(hid_usage, down, &mut out);
        self.send_all(out);
        result
    }

    /// Press and release a key (toolbar buttons such as the MiSTer menu key).
    pub fn tap_key(&self, hid_usage: u16) {
        let mut out = Vec::new();
        let mut input = self.input.lock().unwrap();
        if input.key_event(hid_usage, true, &mut out) == KeyResult::Forwarded {
            self.send_all(std::mem::take(&mut out));
            self.push(Cmd::Delay(TAP_HOLD));
            input.key_event(hid_usage, false, &mut out);
            self.send_all(out);
        }
    }

    /// Press keys together and release them in reverse, e.g. Ctrl+Alt+Del
    /// (which Windows never lets an app capture).
    pub fn press_combo(&self, hid_usages: Vec<u16>) {
        let mut out = Vec::new();
        let mut input = self.input.lock().unwrap();
        for &usage in &hid_usages {
            input.key_event(usage, true, &mut out);
        }
        self.send_all(std::mem::take(&mut out));
        self.push(Cmd::Delay(TAP_HOLD));
        for &usage in hid_usages.iter().rev() {
            input.key_event(usage, false, &mut out);
        }
        self.send_all(out);
    }

    /// Press the MiSTer menu (OSD) key.
    pub fn open_menu(&self) {
        self.tap_key(hid::F12);
    }

    /// Relative mouse movement in host pixels.
    pub fn mouse_move(&self, dx: f64, dy: f64) {
        let sensitivity = self.input.lock().unwrap().settings.mouse_sensitivity;
        self.push(Cmd::Move(dx * sensitivity, dy * sensitivity));
        if let Some(sink) = self.cursor_sink.lock().unwrap().as_ref() {
            sink.input(dx * sensitivity, dy * sensitivity);
        }
    }

    /// Absolute pointer position in per-mille of the screen (0..=1000 each
    /// way). Only some cores react to it; most use relative movement.
    pub fn mouse_position(&self, x: u16, y: u16) {
        self.push(Cmd::Send(format!("mousePos:{},{}", x.min(1000), y.min(1000))));
    }

    pub fn mouse_button(&self, button: MouseButton, down: bool) {
        let mut out = Vec::new();
        self.input.lock().unwrap().mouse_button(button, down, &mut out);
        self.send_all(out);
    }

    /// Release every key and button MiSTer thinks is held. Call on capture
    /// exit, focus loss and before switching sessions.
    pub fn release_all(&self) {
        let mut out = Vec::new();
        self.input.lock().unwrap().release_all(&mut out);
        self.send_all(out);
    }

    pub fn set_settings(&self, settings: InputSettings) {
        self.input.lock().unwrap().settings = settings;
    }

    pub fn stats(&self) -> InputStats {
        let sent = self.counters.sent.load(Ordering::Relaxed);
        let mut rate = self.rate.lock().unwrap();
        let elapsed = rate.0.elapsed().as_secs_f64();
        if elapsed >= 0.5 {
            rate.2 = (sent - rate.1) as f64 / elapsed;
            *rate = (Instant::now(), sent, rate.2);
        }
        InputStats {
            connected: self.counters.connected.load(Ordering::Relaxed) != 0,
            messages_sent: sent,
            messages_per_sec: rate.2,
            queued: self.counters.queued.load(Ordering::Relaxed),
        }
    }

    /// Release held input and close the connection. The session can't be
    /// reused afterwards.
    pub fn disconnect(&self) {
        self.release_all();
        self.push(Cmd::Shutdown);
    }
}

impl MisterSession {
    /// Input through a capture server's stream instead of straight to mrext:
    /// the server forwards it to the MiSTer while this viewer has control.
    pub(crate) fn with_link(link: Arc<MkvmLink>, observer: Arc<dyn SessionObserver>) -> Arc<Self> {
        let (tx, rx) = mpsc::channel();
        let counters = Arc::new(Counters::default());
        let worker_counters = counters.clone();
        thread::Builder::new()
            .name(format!("input-{}", link.address().stream))
            .spawn(move || LinkWorker { link, observer, rx, counters: worker_counters }.run())
            .expect("spawn session worker");
        Arc::new(Self {
            tx,
            input: Mutex::new(InputState::default()),
            counters,
            rate: Mutex::new((Instant::now(), 0, 0.0)),
            cursor_sink: Mutex::new(None),
        })
    }

    /// For the capture server, which forwards input it received already
    /// translated: one mrext message, as is.
    pub fn send_message(&self, message: String) {
        self.push(Cmd::Send(message));
    }

    /// Capture server: relative movement already scaled by the viewer.
    pub fn move_raw(&self, dx: f64, dy: f64) {
        self.push(Cmd::Move(dx, dy));
    }

    /// Capture server: pause before the next message (key taps).
    pub fn delay(&self, ms: u32) {
        self.push(Cmd::Delay(Duration::from_millis(ms.min(1000) as u64)));
    }

    pub(crate) fn set_cursor_sink(&self, sink: Option<Arc<dyn CursorSink>>) {
        *self.cursor_sink.lock().unwrap() = sink;
    }

    fn push(&self, cmd: Cmd) {
        self.counters.queued.fetch_add(1, Ordering::Relaxed);
        if self.tx.send(cmd).is_err() {
            self.counters.queued.fetch_sub(1, Ordering::Relaxed);
        }
    }

    fn send_all(&self, messages: Vec<String>) {
        for m in messages {
            self.push(Cmd::Send(m));
        }
    }
}

impl Drop for MisterSession {
    fn drop(&mut self) {
        let _ = self.tx.send(Cmd::Shutdown);
    }
}

type Socket = WebSocket<TcpStream>;

/// Sends input over a capture server link, batched: mouse movement every
/// `MOUSE_FLUSH`, everything else at once and in order.
struct LinkWorker {
    link: Arc<MkvmLink>,
    observer: Arc<dyn SessionObserver>,
    rx: Receiver<Cmd>,
    counters: Arc<Counters>,
}

impl LinkWorker {
    fn run(self) {
        self.link.set_observer(Some(self.observer.clone()));
        let mut pending = (0.0f64, 0.0f64);
        let mut last_flush = Instant::now();
        loop {
            let mut batch = Vec::new();
            let mut next = match self.rx.recv_timeout(MOUSE_FLUSH) {
                Ok(cmd) => Some(cmd),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => break,
            };
            let mut shutdown = false;
            while let Some(cmd) = next.take() {
                self.counters.queued.fetch_sub(1, Ordering::Relaxed);
                match cmd {
                    Cmd::Shutdown => shutdown = true,
                    Cmd::Move(dx, dy) => {
                        pending.0 += dx;
                        pending.1 += dy;
                    }
                    Cmd::Send(m) => {
                        flush_move(&mut pending, &mut batch);
                        batch.push(InputCmd::Send { m });
                    }
                    Cmd::Delay(d) => {
                        flush_move(&mut pending, &mut batch);
                        batch.push(InputCmd::Delay { ms: d.as_millis() as u32 });
                    }
                }
                next = self.rx.try_recv().ok();
            }
            if last_flush.elapsed() >= MOUSE_FLUSH || shutdown {
                flush_move(&mut pending, &mut batch);
                last_flush = Instant::now();
            }
            let n = batch.iter().filter(|c| !matches!(c, InputCmd::Delay { .. })).count() as u64;
            if self.link.send_input(batch) {
                self.counters.sent.fetch_add(n, Ordering::Relaxed);
            }
            self.counters.connected.store(self.link.is_connected() as u64, Ordering::Relaxed);
            if shutdown {
                break;
            }
        }
        self.link.set_observer(None);
    }
}

fn flush_move(pending: &mut (f64, f64), batch: &mut Vec<InputCmd>) {
    if pending.0 != 0.0 || pending.1 != 0.0 {
        batch.push(InputCmd::Move { dx: pending.0, dy: pending.1 });
        *pending = (0.0, 0.0);
    }
}

struct Worker {
    host: String,
    port: u16,
    observer: Arc<dyn SessionObserver>,
    rx: Receiver<Cmd>,
    counters: Arc<Counters>,
    pending: (f64, f64),
    last_flush: Instant,
}

enum Exit {
    Shutdown,
    Lost(String),
}

impl Worker {
    fn new(
        host: String,
        port: u16,
        observer: Arc<dyn SessionObserver>,
        rx: Receiver<Cmd>,
        counters: Arc<Counters>,
    ) -> Self {
        Self {
            host,
            port,
            observer,
            rx,
            counters,
            pending: (0.0, 0.0),
            last_flush: Instant::now(),
        }
    }

    fn run(mut self) {
        loop {
            self.observer.on_state(ConnectionState::Connecting);
            let reason = match self.connect() {
                Ok(socket) => {
                    self.counters.connected.store(1, Ordering::Relaxed);
                    self.observer.on_state(ConnectionState::Connected);
                    let exit = self.serve(socket);
                    self.counters.connected.store(0, Ordering::Relaxed);
                    match exit {
                        Exit::Shutdown => {
                            self.observer.on_state(ConnectionState::Disconnected {
                                reason: "closed".into(),
                            });
                            return;
                        }
                        Exit::Lost(reason) => reason,
                    }
                }
                Err(reason) => reason,
            };
            self.observer
                .on_state(ConnectionState::Disconnected { reason });
            if self.idle(RECONNECT_DELAY) {
                return;
            }
        }
    }

    fn connect(&self) -> Result<Socket, String> {
        let addr = (self.host.as_str(), self.port)
            .to_socket_addrs()
            .map_err(|e| format!("resolve {}: {e}", self.host))?
            .next()
            .ok_or_else(|| format!("no address for {}", self.host))?;
        let stream = TcpStream::connect_timeout(&addr, CONNECT_TIMEOUT)
            .map_err(|e| format!("connect {addr}: {e}"))?;
        stream.set_nodelay(true).ok();
        stream.set_read_timeout(Some(CONNECT_TIMEOUT)).ok();
        stream.set_write_timeout(Some(CONNECT_TIMEOUT)).ok();
        let url = format!("ws://{}:{}/api/ws", self.host, self.port);
        let (socket, _) =
            tungstenite::client(url, stream).map_err(|e| format!("websocket handshake: {e}"))?;
        socket.get_ref().set_read_timeout(Some(POLL)).ok();
        Ok(socket)
    }

    /// While disconnected: discard input (MiSTer can't receive it) and wait.
    /// Returns true if the session was shut down.
    fn idle(&mut self, wait: Duration) -> bool {
        let until = Instant::now() + wait;
        loop {
            let left = until.saturating_duration_since(Instant::now());
            if left.is_zero() {
                return false;
            }
            match self.rx.recv_timeout(left) {
                Ok(Cmd::Shutdown) | Err(RecvTimeoutError::Disconnected) => return true,
                Ok(_) => {
                    self.counters.queued.fetch_sub(1, Ordering::Relaxed);
                }
                Err(RecvTimeoutError::Timeout) => {}
            }
        }
    }

    fn serve(&mut self, mut socket: Socket) -> Exit {
        self.pending = (0.0, 0.0);
        loop {
            // Input first: block briefly for the next command, then drain.
            let mut next = match self.rx.recv_timeout(POLL) {
                Ok(cmd) => Some(cmd),
                Err(RecvTimeoutError::Timeout) => None,
                Err(RecvTimeoutError::Disconnected) => return Exit::Shutdown,
            };
            while let Some(cmd) = next.take() {
                self.counters.queued.fetch_sub(1, Ordering::Relaxed);
                match cmd {
                    Cmd::Shutdown => {
                        let _ = self.flush_mouse(&mut socket);
                        let _ = socket.close(None);
                        let _ = socket.flush();
                        return Exit::Shutdown;
                    }
                    Cmd::Move(dx, dy) => {
                        self.pending.0 += dx;
                        self.pending.1 += dy;
                    }
                    Cmd::Send(msg) => {
                        // Keep ordering: movement before the click that follows it.
                        if let Err(e) = self.flush_mouse(&mut socket) {
                            return Exit::Lost(e);
                        }
                        if let Err(e) = self.send(&mut socket, msg) {
                            return Exit::Lost(e);
                        }
                    }
                    Cmd::Delay(d) => {
                        if let Err(e) = self.flush_mouse(&mut socket) {
                            return Exit::Lost(e);
                        }
                        thread::sleep(d);
                    }
                }
                next = self.rx.try_recv().ok();
            }

            if self.last_flush.elapsed() >= MOUSE_FLUSH {
                if let Err(e) = self.flush_mouse(&mut socket) {
                    return Exit::Lost(e);
                }
            }

            match socket.read() {
                Ok(Message::Text(text)) => {
                    if let Some((key, value)) = text.as_str().split_once(':') {
                        self.observer.on_status(key.into(), value.into());
                    }
                }
                Ok(Message::Close(_)) => return Exit::Lost("closed by MiSTer".into()),
                Ok(_) => {}
                Err(tungstenite::Error::Io(e))
                    if matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) => {}
                Err(e) => return Exit::Lost(e.to_string()),
            }
        }
    }

    fn flush_mouse(&mut self, socket: &mut Socket) -> Result<(), String> {
        // Send whole pixels; keep the fraction for next time.
        let (x, y) = (self.pending.0.trunc(), self.pending.1.trunc());
        if x == 0.0 && y == 0.0 {
            return Ok(());
        }
        self.pending.0 -= x;
        self.pending.1 -= y;
        self.last_flush = Instant::now();
        self.send(socket, format!("mouseMove:{},{}", x as i32, y as i32))
    }

    fn send(&self, socket: &mut Socket, msg: String) -> Result<(), String> {
        socket
            .send(Message::text(msg))
            .map_err(|e| format!("send: {e}"))?;
        self.counters.sent.fetch_add(1, Ordering::Relaxed);
        Ok(())
    }
}

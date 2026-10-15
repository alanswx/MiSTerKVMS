//! mkvm viewers: the TCP listener and one connection per viewer. Each viewer
//! has a writer thread fed through an outbox that keeps only the newest
//! picture and a short audio queue, so a slow viewer drops frames instead of
//! holding anyone up.
//!
//! Plain connections are accepted from the home network only. Paired
//! devices connect encrypted (mkvm::secure), from the home network or, when
//! internet access is on, from anywhere (docs/IPHONE.md §3). A plain
//! connection from the home network may ask to pair instead of watching.

use std::collections::VecDeque;
use std::io::{BufReader, BufWriter, Read, Write};
use std::net::{IpAddr, Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use mkvm::secure::{self, SecureReader, SecureWriter};
use mkvm::{read_message, write_message, write_video, ErrorMsg, Gap, Message, Pair, Paired, Subscribe, ViewerId, Welcome, PROTOCOL_VERSION};

use crate::stream::{AudioPacket, Encoded, Subscriber, Want};
use crate::{log, now_100ns, Server};

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const PING_EVERY: Duration = Duration::from_secs(2);
/// About half a second of 48 kHz audio in typical 10 ms chunks.
const MAX_AUDIO_QUEUE: usize = 50;

static NEXT_VIEWER: AtomicU32 = AtomicU32::new(1);

type Reader = BufReader<Box<dyn Read + Send>>;
type Writer = BufWriter<Box<dyn Write + Send>>;

/// Reads and writes through one shared socket, so the read timeout set on
/// it applies (on Windows each cloned handle has its own timeouts).
struct Sock(Arc<TcpStream>);

impl Read for Sock {
    fn read(&mut self, buf: &mut [u8]) -> std::io::Result<usize> {
        (&*self.0).read(buf)
    }
}

impl Write for Sock {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        (&*self.0).write(buf)
    }
    fn flush(&mut self) -> std::io::Result<()> {
        (&*self.0).flush()
    }
}

/// The home network: this PC, private and link-local addresses. Not CGNAT
/// (100.64/10): that's the carrier's network, not the home's.
pub fn is_home(ip: IpAddr) -> bool {
    match ip {
        IpAddr::V4(a) => a.is_loopback() || a.is_private() || a.is_link_local(),
        IpAddr::V6(a) => {
            if let Some(v4) = a.to_ipv4_mapped() {
                return is_home(IpAddr::V4(v4));
            }
            let first = a.segments()[0];
            a.is_loopback() || (first & 0xfe00) == 0xfc00 || (first & 0xffc0) == 0xfe80
        }
    }
}

pub fn listen(server: Arc<Server>, port: u16) -> std::io::Result<()> {
    let listener = TcpListener::bind(("0.0.0.0", port))?;
    log(&format!("viewers: listening on TCP {port}"));
    thread::Builder::new().name("mkvm-listen".into()).spawn(move || {
        for conn in listener.incoming() {
            match conn {
                Ok(sock) => {
                    let server = server.clone();
                    let _ = thread::Builder::new().name("mkvm-viewer".into()).spawn(move || {
                        let peer = sock.peer_addr().map(|a| a.to_string()).unwrap_or_default();
                        if let Err(e) = serve(server, sock) {
                            log(&format!("viewer {peer}: {e}"));
                        }
                    });
                }
                Err(e) => log(&format!("viewers: accept: {e}")),
            }
        }
    })?;
    Ok(())
}

#[derive(Default)]
struct Outbox {
    video: Option<Arc<Encoded>>,
    audio: VecDeque<Arc<AudioPacket>>,
    messages: VecDeque<Message>,
    dropped_video: u32,
    dropped_audio: u32,
}

struct ViewerConn {
    id: ViewerId,
    name: String,
    want: Mutex<Want>,
    outbox: Mutex<Outbox>,
    wake: Condvar,
    closed: AtomicBool,
    sock: Arc<TcpStream>,
}

impl ViewerConn {
    fn shut(&self) {
        self.closed.store(true, Ordering::Relaxed);
        let _ = self.sock.shutdown(Shutdown::Both);
        self.wake.notify_all();
    }
}

impl Subscriber for ViewerConn {
    fn id(&self) -> ViewerId {
        self.id
    }

    fn name(&self) -> String {
        self.name.clone()
    }

    fn want(&self) -> Want {
        *self.want.lock().unwrap()
    }

    fn video(&self, frame: Arc<Encoded>) {
        let mut o = self.outbox.lock().unwrap();
        if o.video.replace(frame).is_some() {
            o.dropped_video += 1;
        }
        drop(o);
        self.wake.notify_all();
    }

    fn audio(&self, packet: Arc<AudioPacket>) {
        let mut o = self.outbox.lock().unwrap();
        o.audio.push_back(packet);
        while o.audio.len() > MAX_AUDIO_QUEUE {
            o.audio.pop_front();
            o.dropped_audio += 1;
        }
        drop(o);
        self.wake.notify_all();
    }

    fn message(&self, message: Message) {
        self.outbox.lock().unwrap().messages.push_back(message);
        self.wake.notify_all();
    }

    fn close(&self) {
        self.message(Message::Error(ErrorMsg { message: "stream stopped".into() }));
        // Let the writer send it, then hang up.
        thread::sleep(Duration::from_millis(50));
        self.shut();
    }
}

fn serve(server: Arc<Server>, sock: TcpStream) -> Result<(), String> {
    sock.set_nodelay(true).ok();
    let peer = sock.peer_addr().ok();
    let address = peer.map(|p| p.to_string()).unwrap_or_default();
    let home = peer.is_some_and(|p| is_home(p.ip()));
    let sock = Arc::new(sock);
    sock.set_read_timeout(Some(HELLO_TIMEOUT)).ok();
    let mut first = [0u8; 4];
    match Sock(sock.clone()).read_exact(&mut first) {
        Ok(()) => {}
        // Connected and left without a word: an address lookup (the iPhone
        // resolving a stream it found) or a port check. Nothing to log.
        Err(e) if e.kind() == std::io::ErrorKind::UnexpectedEof => return Ok(()),
        Err(e) => return Err(e.to_string()),
    }
    // A paired device's encrypted connection, or a plain one.
    let mut device_id = None;
    let (mut read, mut write, mut device): (Reader, Writer, Option<String>) = if &first == secure::MAGIC {
        let id = secure::read_prelude(&mut Sock(sock.clone())).map_err(|e| e.to_string())?;
        if !home && !server.settings().remote_enabled {
            return Err("an encrypted connection from the internet, but internet access is off".into());
        }
        let Some(key) = server.devices.key(&id) else {
            // Slows down guessing; the key space makes it hopeless anyway.
            thread::sleep(Duration::from_secs(1));
            return Err(format!("unknown or revoked device key {}", secure::hex(&id)));
        };
        let session = secure::accept(&mut Sock(sock.clone()), &key).map_err(|e| e.to_string())?;
        server.devices.connected(&id, &address, &sock);
        device_id = Some(id);
        (
            BufReader::with_capacity(64 * 1024, Box::new(SecureReader::new(Sock(sock.clone()), session.clone()))),
            BufWriter::with_capacity(256 * 1024, Box::new(SecureWriter::new(Sock(sock.clone()), session))),
            server.devices.name(&id),
        )
    } else {
        let replay = std::io::Cursor::new(first.to_vec()).chain(Sock(sock.clone()));
        (BufReader::with_capacity(64 * 1024, Box::new(replay)), BufWriter::with_capacity(256 * 1024, Box::new(Sock(sock.clone()))), None)
    };
    let refuse = |write: &mut Writer, message: String| {
        let _ = write_message(write, &Message::Error(ErrorMsg { message: message.clone() }), now_100ns());
        let _ = write.flush();
        Err(message)
    };
    let first = read_message(&mut read).map_err(|e| e.to_string())?.0;
    if device.is_none() && !home {
        return refuse(
            &mut write,
            "This capture server lets only paired devices in from outside its home network. \
             Pair this device while it's on the home network first."
                .into(),
        );
    }
    let hello = match first {
        Message::Hello(h) => h,
        Message::Pair(p) if device.is_none() => return pair(&server, p, &address, &mut write),
        other => return Err(format!("expected hello, got {other:?}")),
    };
    if hello.protocol != PROTOCOL_VERSION {
        return refuse(&mut write, format!("protocol {} not supported (server speaks {PROTOCOL_VERSION})", hello.protocol));
    }
    let Some(stream) = server.stream(&hello.stream) else {
        let names = server.stream_names().join(", ");
        return refuse(&mut write, format!("no stream '{}' on this server (streams: {names})", hello.stream));
    };
    sock.set_read_timeout(None).ok();
    if let Some(key_id) = &device_id {
        // The phone's name may have changed since it paired.
        server.devices.rename(key_id, &hello.viewer);
        device = server.devices.name(key_id);
    }
    let id = NEXT_VIEWER.fetch_add(1, Ordering::Relaxed);
    let name = if hello.viewer.trim().is_empty() { format!("Viewer {id}") } else { hello.viewer.trim().to_string() };
    let via = match &device {
        Some(d) if home => format!(", paired device '{d}', encrypted"),
        Some(d) => format!(", paired device '{d}' from the internet ({address})"),
        None => String::new(),
    };
    log(&format!("viewer {id} '{name}' ({}{via}) joined '{}'", hello.platform, stream.config.name));
    let welcome =
        Welcome { protocol: PROTOCOL_VERSION, server: crate::server_name(), server_id: server.id.clone(), viewer_id: id, stream: stream.info() };
    write_message(&mut write, &Message::Welcome(welcome), now_100ns()).map_err(|e| e.to_string())?;
    write.flush().map_err(|e| e.to_string())?;

    let viewer = Arc::new(ViewerConn {
        id,
        name: name.clone(),
        want: Mutex::new(Want::default()),
        outbox: Mutex::new(Outbox::default()),
        wake: Condvar::new(),
        closed: AtomicBool::new(false),
        sock,
    });
    let writer_viewer = viewer.clone();
    let writer = thread::Builder::new()
        .name(format!("mkvm-write-{id}"))
        .spawn(move || write_loop(writer_viewer, write))
        .map_err(|e| e.to_string())?;
    stream.add(viewer.clone());

    let result = loop {
        let (msg, header) = match read_message(&mut read) {
            Ok(m) => m,
            Err(mkvm::WireError::Closed) => break Ok(()),
            Err(e) => break Err(e.to_string()),
        };
        match msg {
            Message::Subscribe(Subscribe { video, audio, max_width, max_height, max_fps, quality, .. }) => {
                *viewer.want.lock().unwrap() = Want { video, audio, max_width, max_height, max_fps, quality };
            }
            Message::Input(cmds) => stream.input(id, cmds),
            Message::TakeControl => stream.take_control(id),
            Message::Ping => viewer.message(Message::Pong { echo: header.timestamp }),
            _ => {}
        }
        if viewer.closed.load(Ordering::Relaxed) {
            break Ok(());
        }
    };
    stream.remove(id);
    viewer.shut();
    let _ = writer.join();
    log(&format!("viewer {id} '{name}' left '{}'", stream.config.name));
    result
}

/// A device on the home network asks for a key to connect from the internet.
fn pair(server: &Server, request: Pair, address: &str, write: &mut Writer) -> Result<(), String> {
    let settings = server.settings();
    let reply = if !settings.remote_enabled {
        Message::Error(ErrorMsg {
            message: "Internet access is off on this capture server. Turn on \"Allow connections from the internet\" \
                      on its setup page, on the capture PC, then try again."
                .into(),
        })
    } else {
        match server.devices.pair(&request.device, address, request.replaces.as_deref()) {
            Ok(key) => Message::Paired(Paired {
                key_id: key.id_hex(),
                key: key.key_hex(),
                internet_address: settings.remote_address,
                server_id: server.id.clone(),
            }),
            Err(e) => Message::Error(ErrorMsg { message: format!("can't make a key: {e}") }),
        }
    };
    write_message(write, &reply, now_100ns()).map_err(|e| e.to_string())?;
    write.flush().map_err(|e| e.to_string())
}

fn write_loop(viewer: Arc<ViewerConn>, mut w: Writer) {
    let mut last_sent = std::time::Instant::now();
    loop {
        let (messages, audio, video, gap) = {
            let mut o = viewer.outbox.lock().unwrap();
            while o.video.is_none() && o.audio.is_empty() && o.messages.is_empty() && !viewer.closed.load(Ordering::Relaxed) {
                let (guard, timeout) = viewer.wake.wait_timeout(o, PING_EVERY).unwrap();
                o = guard;
                if timeout.timed_out() {
                    break;
                }
            }
            if viewer.closed.load(Ordering::Relaxed) && o.messages.is_empty() {
                return;
            }
            let gap = (o.dropped_video > 0 || o.dropped_audio > 0).then(|| Gap { video: o.dropped_video, audio: o.dropped_audio });
            o.dropped_video = 0;
            o.dropped_audio = 0;
            (std::mem::take(&mut o.messages), std::mem::take(&mut o.audio), o.video.take(), gap)
        };
        let result = (|| -> std::io::Result<()> {
            if last_sent.elapsed() >= PING_EVERY {
                write_message(&mut w, &Message::Ping, now_100ns())?;
            }
            for m in &messages {
                write_message(&mut w, m, now_100ns())?;
            }
            if let Some(g) = gap {
                write_message(&mut w, &Message::Gap(g), now_100ns())?;
            }
            for a in &audio {
                let msg = Message::Audio(mkvm::Audio { channels: a.channels, rate: a.rate, samples: a.samples.clone() });
                write_message(&mut w, &msg, a.timestamp)?;
            }
            if let Some(v) = &video {
                write_video(&mut w, v.codec, true, v.width, v.height, &v.data, v.timestamp)?;
            }
            w.flush()
        })();
        last_sent = std::time::Instant::now();
        if result.is_err() {
            viewer.shut();
            return;
        }
    }
}

#[cfg(test)]
mod tests {
    use super::is_home;

    #[test]
    fn home_addresses() {
        for home in ["127.0.0.1", "192.168.99.82", "10.0.0.5", "172.20.1.1", "169.254.3.4", "::1", "fd12::1", "fe80::1", "::ffff:192.168.1.2"] {
            assert!(is_home(home.parse().unwrap()), "{home}");
        }
        for away in ["8.8.8.8", "100.64.1.2", "172.32.0.1", "2001:db8::1", "::ffff:1.1.1.1"] {
            assert!(!is_home(away.parse().unwrap()), "{away}");
        }
    }
}

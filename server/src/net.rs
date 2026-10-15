//! mkvm viewers: the TCP listener and one connection per viewer. Each viewer
//! has a writer thread fed through an outbox that keeps only the newest
//! picture and a short audio queue, so a slow viewer drops frames instead of
//! holding anyone up.

use std::collections::VecDeque;
use std::io::{BufReader, BufWriter, Write};
use std::net::{Shutdown, TcpListener, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use mkvm::{read_message, write_message, write_video, ErrorMsg, Gap, Message, Subscribe, ViewerId, Welcome, PROTOCOL_VERSION};

use crate::stream::{AudioPacket, Encoded, Subscriber, Want};
use crate::{log, now_100ns, Server};

const HELLO_TIMEOUT: Duration = Duration::from_secs(5);
const PING_EVERY: Duration = Duration::from_secs(2);
/// About half a second of 48 kHz audio in typical 10 ms chunks.
const MAX_AUDIO_QUEUE: usize = 50;

static NEXT_VIEWER: AtomicU32 = AtomicU32::new(1);

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
    sock: TcpStream,
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
    let mut write = BufWriter::with_capacity(256 * 1024, sock.try_clone().map_err(|e| e.to_string())?);
    let mut read = BufReader::with_capacity(64 * 1024, sock.try_clone().map_err(|e| e.to_string())?);
    // On the handle that reads: on Windows each cloned handle has its own
    // timeouts.
    read.get_ref().set_read_timeout(Some(HELLO_TIMEOUT)).ok();
    let hello = match read_message(&mut read).map_err(|e| e.to_string())?.0 {
        Message::Hello(h) => h,
        other => return Err(format!("expected hello, got {other:?}")),
    };
    let refuse = |write: &mut BufWriter<TcpStream>, message: String| {
        let _ = write_message(write, &Message::Error(ErrorMsg { message: message.clone() }), now_100ns());
        let _ = write.flush();
        Err(message)
    };
    if hello.protocol != PROTOCOL_VERSION {
        return refuse(&mut write, format!("protocol {} not supported (server speaks {PROTOCOL_VERSION})", hello.protocol));
    }
    let Some(stream) = server.stream(&hello.stream) else {
        let names = server.stream_names().join(", ");
        return refuse(&mut write, format!("no stream '{}' on this server (streams: {names})", hello.stream));
    };
    read.get_ref().set_read_timeout(None).ok();
    let id = NEXT_VIEWER.fetch_add(1, Ordering::Relaxed);
    let name = if hello.viewer.trim().is_empty() { format!("Viewer {id}") } else { hello.viewer.trim().to_string() };
    log(&format!("viewer {id} '{name}' ({}) joined '{}'", hello.platform, stream.config.name));
    let welcome = Welcome { protocol: PROTOCOL_VERSION, server: crate::server_name(), viewer_id: id, stream: stream.info() };
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

fn write_loop(viewer: Arc<ViewerConn>, mut w: BufWriter<TcpStream>) {
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

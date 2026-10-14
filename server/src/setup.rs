//! The setup page: `http://<pc>:5971`. Lists capture cards with live
//! previews, edits streams, shows who's watching. Reachable from the capture
//! PC only unless `allow_lan` is on; changing anything needs the setup
//! password when one is set (docs/CAPTURE-SERVER.md §5.4).

use std::io::{Cursor, Read};
use std::net::{IpAddr, TcpStream};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Condvar, Mutex};
use std::thread;
use std::time::Duration;

use kvms_capture::{Output, PixelFormat, VideoCapture};
use kvms_codec::{convert, jpeg, scale, Bgra};
use mkvm::{Message, ViewerId};
use serde_json::json;
use tiny_http::{Header, Method, Request, Response, StatusCode};

use crate::config::{self, Settings, Streams};
use crate::stream::{AudioPacket, Encoded, Subscriber, Want, PREVIEW_IDS};
use crate::{log, Server};

const PAGE: &str = include_str!("setup.html");
/// Previews are for checking a card works, not for watching: keep them light.
const PREVIEW_MAX: (u32, u32) = (960, 540);

static NEXT_PREVIEW: AtomicU32 = AtomicU32::new(PREVIEW_IDS);

pub fn start(server: Arc<Server>, settings: &Settings) -> Result<(), String> {
    let http = tiny_http::Server::http(("0.0.0.0", settings.setup_port)).map_err(|e| e.to_string())?;
    log(&format!("setup page: http://localhost:{}", settings.setup_port));
    thread::Builder::new()
        .name("setup-http".into())
        .spawn(move || {
            for request in http.incoming_requests() {
                let server = server.clone();
                let _ = thread::Builder::new().name("setup-request".into()).spawn(move || handle(server, request));
            }
        })
        .map_err(|e| e.to_string())?;
    Ok(())
}

fn header(name: &str, value: &str) -> Header {
    Header::from_bytes(name.as_bytes(), value.as_bytes()).expect("valid header")
}

fn json_response(value: serde_json::Value) -> Response<Cursor<Vec<u8>>> {
    Response::from_data(serde_json::to_vec(&value).unwrap())
        .with_header(header("Content-Type", "application/json"))
        .with_header(header("Cache-Control", "no-store"))
}

fn error(code: u16, message: &str) -> Response<Cursor<Vec<u8>>> {
    json_response(json!({ "error": message })).with_status_code(StatusCode(code))
}

fn is_local(addr: Option<IpAddr>) -> bool {
    match addr {
        Some(IpAddr::V4(a)) => a.is_loopback(),
        Some(IpAddr::V6(a)) => a.is_loopback() || a.to_ipv4_mapped().is_some_and(|v4| v4.is_loopback()),
        None => false,
    }
}

fn handle(server: Arc<Server>, mut request: Request) {
    let settings = server.settings();
    let peer = request.remote_addr().map(|a| a.ip());
    if !settings.allow_lan && !is_local(peer) {
        let body = "This setup page only opens on the capture PC itself.\n\
                    To open it from other computers, set allow_lan = true in capture.ini and restart.";
        let _ = request.respond(Response::from_string(body).with_status_code(StatusCode(403)));
        return;
    }
    let url = request.url().to_string();
    let (path, query) = url.split_once('?').unwrap_or((&url, ""));
    let path = path.to_string();
    let query = query.to_string();
    let authorised = settings.password.is_empty()
        || request.headers().iter().any(|h| h.field.equiv("X-Setup-Password") && h.value.as_str() == settings.password);

    let method = request.method().clone();
    let response = match (method, path.as_str()) {
        (Method::Get, "/") => Response::from_string(PAGE).with_header(header("Content-Type", "text/html; charset=utf-8")),
        (Method::Get, "/api/state") => json_response(state(&server)),
        (Method::Get, "/api/devices") => json_response(devices()),
        (Method::Post, "/api/password") => {
            if authorised {
                json_response(json!({ "ok": true }))
            } else {
                error(403, "wrong password")
            }
        }
        (Method::Post, "/api/streams") => {
            if !authorised {
                error(403, "wrong password")
            } else {
                match read_json::<Streams>(&mut request) {
                    Ok(streams) => match validate(&streams).and_then(|_| server.save(&streams).map_err(|e| e.to_string())) {
                        Ok(()) => json_response(json!({ "ok": true })),
                        Err(e) => error(400, &e),
                    },
                    Err(e) => error(400, &e),
                }
            }
        }
        (Method::Post, "/api/settings") => {
            if !authorised {
                error(403, "wrong password")
            } else {
                match read_json::<serde_json::Value>(&mut request) {
                    Ok(v) => match save_settings(&server, &v) {
                        Ok(()) => json_response(json!({ "ok": true })),
                        Err(e) => error(500, &e),
                    },
                    Err(e) => error(400, &e),
                }
            }
        }
        (Method::Get, "/api/test-mister") => {
            let host = param(&query, "host").unwrap_or_default();
            let port: u16 = param(&query, "port").and_then(|p| p.parse().ok()).unwrap_or(8182);
            json_response(test_mister(&host, port))
        }
        (Method::Get, p) if p.starts_with("/preview/stream/") => {
            let name = percent_decode(&p["/preview/stream/".len()..]);
            match server.stream(&name) {
                Some(stream) => {
                    preview_stream(&server, stream, request);
                    return;
                }
                None => error(404, "no such stream"),
            }
        }
        (Method::Get, "/preview/device") => {
            let id = param(&query, "id").unwrap_or_default();
            if server.streams().iter().any(|s| s.config.video == id) {
                error(409, "this card belongs to a stream: its preview is on the stream")
            } else {
                preview_device(id, request);
                return;
            }
        }
        _ => error(404, "not found"),
    };
    let _ = request.respond(response);
}

fn state(server: &Server) -> serde_json::Value {
    let settings = server.settings();
    let streams: Vec<serde_json::Value> = server
        .streams()
        .iter()
        .map(|s| json!({ "config": s.config, "status": s.status(), "address": format!("mkvm://{}/{}", hostname_for_address(), encode_path(&s.config.name)) }))
        .collect();
    json!({
        "server": crate::server_name(),
        "host": hostname_for_address(),
        "port": settings.port,
        "allowLan": settings.allow_lan,
        "passwordSet": !settings.password.is_empty(),
        "configDir": server.dir().display().to_string(),
        "streams": streams,
    })
}

fn devices() -> serde_json::Value {
    let video = kvms_capture::devices().unwrap_or_default();
    let audio = kvms_capture::audio_devices();
    let audio_names: Vec<String> = audio.iter().map(|a| a.name.clone()).collect();
    let video: Vec<serde_json::Value> = video
        .iter()
        .map(|d| {
            let mut formats = d.formats.clone();
            formats.sort_by(|a, b| (b.width * b.height, (b.fps * 100.0) as u32).cmp(&(a.width * a.height, (a.fps * 100.0) as u32)));
            json!({
                "id": d.id,
                "name": d.name,
                "formats": formats,
                "best": kvms_capture::best_format(&d.formats),
                "audio": kvms_capture::matching_audio(&d.name, &audio_names),
            })
        })
        .collect();
    json!({ "video": video, "audio": audio })
}

fn validate(streams: &Streams) -> Result<(), String> {
    let mut names = std::collections::HashSet::new();
    let mut cards = std::collections::HashSet::new();
    for s in &streams.streams {
        let name = s.name.trim();
        if name.is_empty() {
            return Err("every stream needs a name".into());
        }
        if name.contains('/') {
            return Err(format!("'{name}': names can't contain '/'"));
        }
        if !names.insert(name.to_lowercase()) {
            return Err(format!("two streams are called '{name}'"));
        }
        if s.video.is_empty() {
            return Err(format!("'{name}': choose a capture card"));
        }
        if !cards.insert(s.video.clone()) {
            return Err(format!("'{name}': that capture card is already used by another stream"));
        }
    }
    Ok(())
}

fn save_settings(server: &Server, v: &serde_json::Value) -> Result<(), String> {
    let dir = server.dir();
    if let Some(allow) = v.get("allowLan").and_then(|a| a.as_bool()) {
        config::set_ini_value(dir, "setup", "allow_lan", if allow { "true" } else { "false" }).map_err(|e| e.to_string())?;
    }
    if let Some(pw) = v.get("password").and_then(|p| p.as_str()) {
        config::set_ini_value(dir, "setup", "password", pw.trim()).map_err(|e| e.to_string())?;
    }
    let mut settings = server.settings.lock().unwrap();
    let reloaded = config::load_settings(dir);
    // Ports change only on restart.
    *settings = Settings { port: settings.port, setup_port: settings.setup_port, ..reloaded };
    log(&format!("setup page: allow_lan {}, password {}", settings.allow_lan, if settings.password.is_empty() { "off" } else { "on" }));
    Ok(())
}

fn read_json<T: for<'de> serde::Deserialize<'de>>(request: &mut Request) -> Result<T, String> {
    let mut body = Vec::new();
    request.as_reader().take(1 << 20).read_to_end(&mut body).map_err(|e| e.to_string())?;
    serde_json::from_slice(&body).map_err(|e| e.to_string())
}

fn param(query: &str, key: &str) -> Option<String> {
    query.split('&').find_map(|kv| {
        let (k, v) = kv.split_once('=')?;
        (k == key).then(|| percent_decode(v))
    })
}

fn percent_decode(s: &str) -> String {
    let s = s.replace('+', " ");
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            if let Some(b) = s.get(i + 1..i + 3).and_then(|h| u8::from_str_radix(h, 16).ok()) {
                out.push(b);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).into_owned()
}

fn encode_path(s: &str) -> String {
    s.bytes()
        .map(|b| if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) { (b as char).to_string() } else { format!("%{b:02X}") })
        .collect()
}

fn hostname_for_address() -> String {
    std::env::var("COMPUTERNAME")
        .or_else(|_| std::env::var("HOSTNAME"))
        .map(|h| h.trim().to_lowercase())
        .unwrap_or_else(|_| "localhost".into())
}

/// Can we reach mrext Remote there? (Just the TCP connect: quick and
/// enough to catch a wrong address.)
fn test_mister(host: &str, port: u16) -> serde_json::Value {
    use std::net::ToSocketAddrs;
    let host = host.trim();
    if host.is_empty() {
        return json!({ "ok": false, "error": "enter the MiSTer's address" });
    }
    let addr = match (host, port).to_socket_addrs().map(|mut a| a.next()) {
        Ok(Some(a)) => a,
        _ => return json!({ "ok": false, "error": format!("can't find '{host}'") }),
    };
    match TcpStream::connect_timeout(&addr, Duration::from_secs(2)) {
        Ok(_) => json!({ "ok": true }),
        Err(e) => json!({ "ok": false, "error": format!("no mrext Remote at {addr}: {e}") }),
    }
}

/// A setup-page preview of a running stream, as a viewer that only wants
/// small pictures.
struct Preview {
    id: ViewerId,
    latest: Mutex<Option<Arc<Encoded>>>,
    wake: Condvar,
    closed: AtomicBool,
}

impl Subscriber for Preview {
    fn id(&self) -> ViewerId {
        self.id
    }
    fn name(&self) -> String {
        "setup page".into()
    }
    fn want(&self) -> Want {
        Want { video: true, audio: false, max_width: Some(PREVIEW_MAX.0), max_height: Some(PREVIEW_MAX.1) }
    }
    fn video(&self, frame: Arc<Encoded>) {
        *self.latest.lock().unwrap() = Some(frame);
        self.wake.notify_all();
    }
    fn audio(&self, _: Arc<AudioPacket>) {}
    fn message(&self, _: Message) {}
    fn close(&self) {
        self.closed.store(true, Ordering::Relaxed);
        self.wake.notify_all();
    }
}

const BOUNDARY: &str = "mkvmframe";

fn multipart_part(jpeg: &[u8]) -> Vec<u8> {
    let mut part = format!("--{BOUNDARY}\r\nContent-Type: image/jpeg\r\nContent-Length: {}\r\n\r\n", jpeg.len()).into_bytes();
    part.extend_from_slice(jpeg);
    part.extend_from_slice(b"\r\n");
    part
}

/// A `Read` that yields multipart parts as `next` produces them, and ends
/// when it returns None.
struct PartReader<F: FnMut() -> Option<Vec<u8>>> {
    next: F,
    buf: Vec<u8>,
    pos: usize,
}

impl<F: FnMut() -> Option<Vec<u8>>> Read for PartReader<F> {
    fn read(&mut self, out: &mut [u8]) -> std::io::Result<usize> {
        if self.pos >= self.buf.len() {
            match (self.next)() {
                Some(part) => {
                    self.buf = part;
                    self.pos = 0;
                }
                None => return Ok(0),
            }
        }
        let n = out.len().min(self.buf.len() - self.pos);
        out[..n].copy_from_slice(&self.buf[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

fn mjpeg_response<R: Read>(reader: R) -> Response<R> {
    Response::new(
        StatusCode(200),
        vec![
            header("Content-Type", &format!("multipart/x-mixed-replace; boundary={BOUNDARY}")),
            header("Cache-Control", "no-store"),
        ],
        reader,
        None,
        None,
    )
}

fn preview_stream(_server: &Server, stream: Arc<crate::stream::Stream>, request: Request) {
    let preview = Arc::new(Preview {
        id: NEXT_PREVIEW.fetch_add(1, Ordering::Relaxed),
        latest: Mutex::new(None),
        wake: Condvar::new(),
        closed: AtomicBool::new(false),
    });
    stream.add(preview.clone());
    let p = preview.clone();
    let reader = PartReader {
        next: move || {
            let mut latest = p.latest.lock().unwrap();
            loop {
                if p.closed.load(Ordering::Relaxed) {
                    return None;
                }
                if let Some(f) = latest.take() {
                    return Some(multipart_part(&f.data));
                }
                latest = p.wake.wait_timeout(latest, Duration::from_secs(1)).unwrap().0;
            }
        },
        buf: Vec::new(),
        pos: 0,
    };
    // Returns when the browser goes away (the write fails).
    let _ = request.respond(mjpeg_response(reader));
    stream.remove(preview.id);
}

/// Preview a card that isn't in a stream: open it just for this page.
fn preview_device(id: String, request: Request) {
    let mut cap = match VideoCapture::open(&id, None, Output::Native) {
        Ok(c) => c,
        Err(e) => {
            let _ = request.respond(error(409, &e.to_string()));
            return;
        }
    };
    // A card held by another app opens fine and fails on the first frame:
    // find out before answering, so the page can say so.
    let mut first = None;
    for _ in 0..6 {
        match cap.next_frame(Duration::from_millis(500)) {
            Ok(Some(f)) => {
                first = Some(f);
                break;
            }
            Ok(None) => {}
            Err(e) => {
                let _ = request.respond(error(409, &e.to_string()));
                return;
            }
        }
    }
    let mut bgra = Bgra::default();
    let mut scaled = Bgra::default();
    let mut idle = 0;
    let reader = PartReader {
        next: move || loop {
            let next = match first.take() {
                Some(f) => Ok(Some(f)),
                None => cap.next_frame(Duration::from_millis(500)),
            };
            match next {
                Ok(Some(f)) => {
                    idle = 0;
                    let target = scale::size_for(f.width, f.height, Some(PREVIEW_MAX.0), Some(PREVIEW_MAX.1));
                    if f.format == PixelFormat::Mjpeg && target == (f.width, f.height) {
                        return Some(multipart_part(&f.data));
                    }
                    let ok = match f.format {
                        PixelFormat::Mjpeg => jpeg::decode(&f.data, &mut bgra).is_ok(),
                        PixelFormat::Nv12 => {
                            convert::nv12_to_bgra(&f.data, f.width, f.height, f.stride, &mut bgra);
                            true
                        }
                        PixelFormat::Yuy2 => {
                            convert::yuy2_to_bgra(&f.data, f.width, f.height, f.stride, &mut bgra);
                            true
                        }
                        PixelFormat::Bgra => {
                            bgra = Bgra { width: f.width, height: f.height, stride: f.stride, data: f.data };
                            true
                        }
                    };
                    if !ok {
                        continue;
                    }
                    scale::scale(&bgra, target.0, target.1, &mut scaled);
                    let mut out = Vec::new();
                    if jpeg::encode(&scaled, 80, &mut out).is_ok() {
                        return Some(multipart_part(&out));
                    }
                }
                Ok(None) => {
                    // No signal for a long while: end; the page retries.
                    idle += 1;
                    if idle > 20 {
                        return None;
                    }
                }
                Err(_) => return None,
            }
        },
        buf: Vec::new(),
        pos: 0,
    };
    let _ = request.respond(mjpeg_response(reader));
}

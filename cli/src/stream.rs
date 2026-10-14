//! `stream`: watch a capture server stream for a while like a viewer would,
//! and report what arrived (for testing servers and networks).

use std::io::{BufReader, Write};
use std::net::{TcpStream, ToSocketAddrs};
use std::path::Path;
use std::time::{Duration, Instant};

use kvms_codec::{jpeg, Bgra};
use mkvm::{read_message, write_message, Codec, Hello, Message, StreamAddress, Subscribe};
use serde_json::{json, Value};

fn now_100ns() -> i64 {
    std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map_or(0, |d| (d.as_nanos() / 100) as i64)
}

pub fn watch(address: &str, duration: Duration, max: Option<(u32, u32)>, save: Option<&Path>) -> Result<Value, String> {
    let address = StreamAddress::parse(address)?;
    let addr = address
        .socket()
        .to_socket_addrs()
        .map_err(|e| format!("{}: {e}", address.host))?
        .next()
        .ok_or("no address")?;
    let sock = TcpStream::connect_timeout(&addr, Duration::from_secs(3)).map_err(|e| format!("connect {addr}: {e}"))?;
    sock.set_nodelay(true).ok();
    sock.set_read_timeout(Some(Duration::from_secs(5))).ok();
    let mut w = sock.try_clone().map_err(|e| e.to_string())?;
    let mut r = BufReader::with_capacity(256 * 1024, sock);
    let send = |w: &mut TcpStream, m: &Message| write_message(w, m, now_100ns()).map_err(|e| e.to_string());
    send(
        &mut w,
        &Message::Hello(Hello {
            protocol: mkvm::PROTOCOL_VERSION,
            stream: address.stream.clone(),
            viewer: "misterkvms-cli".into(),
            platform: "cli".into(),
            codecs: vec![Codec::Jpeg],
        }),
    )?;
    let welcome = match read_message(&mut r).map_err(|e| e.to_string())?.0 {
        Message::Welcome(w) => w,
        Message::Error(e) => return Err(e.message),
        other => return Err(format!("unexpected {other:?}")),
    };
    send(
        &mut w,
        &Message::Subscribe(Subscribe {
            video: true,
            audio: true,
            max_width: max.map(|m| m.0),
            max_height: max.map(|m| m.1),
            ..Default::default()
        }),
    )?;
    send(&mut w, &Message::Ping)?;

    let start = Instant::now();
    let (mut frames, mut bytes, mut audio_samples, mut gaps) = (0u64, 0u64, 0u64, 0u64);
    // Loudest sample (0–32767): 0 means silence, e.g. Windows blocking the microphone.
    let mut audio_peak = 0i32;
    let mut size = (0u32, 0u32);
    let mut last_jpeg = Vec::new();
    let mut ages = Vec::new();
    let mut offset = 0i64;
    let mut rtt_ms = None;
    let mut control = None;
    let mut mister = None;
    let mut decode_ms = 0.0;
    while start.elapsed() < duration {
        let (msg, header) = read_message(&mut r).map_err(|e| e.to_string())?;
        match msg {
            Message::Video(v) => {
                frames += 1;
                bytes += v.data.len() as u64;
                size = (v.width, v.height);
                // Age: capture (server clock, corrected) to now.
                if rtt_ms.is_some() {
                    ages.push((now_100ns() - (header.timestamp - offset)) as f64 / 10_000.0);
                }
                if frames % 30 == 1 {
                    let t = Instant::now();
                    let mut pic = Bgra::default();
                    jpeg::decode(&v.data, &mut pic).map_err(|e| e.to_string())?;
                    decode_ms = t.elapsed().as_secs_f64() * 1000.0;
                }
                last_jpeg = v.data;
            }
            Message::Audio(a) => {
                audio_samples += a.samples.len() as u64 / a.channels.max(1) as u64;
                audio_peak = a.samples.iter().fold(audio_peak, |m, &s| m.max((s as i32).abs()));
            }
            Message::Pong { echo } => {
                let now = now_100ns();
                let rtt = now - echo;
                offset = header.timestamp - (echo + rtt / 2);
                rtt_ms = Some(rtt as f64 / 10_000.0);
            }
            Message::Ping => send(&mut w, &Message::Pong { echo: header.timestamp })?,
            Message::Gap(g) => gaps += g.video as u64,
            Message::Control(c) => control = Some(json!({ "you": c.you, "holder": c.holder.map(|h| h.name) })),
            Message::Mister(m) => mister = Some(json!({ "connected": m.connected, "reason": m.reason })),
            Message::Error(e) => return Err(e.message),
            _ => {}
        }
    }
    let secs = start.elapsed().as_secs_f64();
    if let (Some(path), false) = (save, last_jpeg.is_empty()) {
        let mut pic = Bgra::default();
        jpeg::decode(&last_jpeg, &mut pic).map_err(|e| e.to_string())?;
        save_png(path, &pic)?;
    }
    ages.sort_by(|a, b| a.total_cmp(b));
    let median_age = ages.get(ages.len() / 2).copied();
    let _ = w.flush();
    Ok(json!({
        "stream": welcome.stream.name,
        "server": welcome.server,
        "mister": welcome.stream.mister,
        "native": [welcome.stream.width, welcome.stream.height],
        "received": [size.0, size.1],
        "frames": frames,
        "fps": frames as f64 / secs,
        "mbit_per_s": bytes as f64 * 8.0 / secs / 1e6,
        "avg_frame_kb": if frames > 0 { bytes / frames / 1024 } else { 0 },
        "frames_dropped_by_server": gaps,
        "audio_seconds": audio_samples as f64 / welcome.stream.audio_rate.max(1) as f64,
        "audio_peak": audio_peak,
        "round_trip_ms": rtt_ms,
        "frame_age_ms": median_age,
        "decode_ms": decode_ms,
        "control": control,
        "mister_state": mister,
        "saved": save,
    }))
}

fn save_png(path: &Path, pic: &Bgra) -> Result<(), String> {
    let file = std::fs::File::create(path).map_err(|e| format!("{}: {e}", path.display()))?;
    let mut enc = png::Encoder::new(std::io::BufWriter::new(file), pic.width, pic.height);
    enc.set_color(png::ColorType::Rgb);
    enc.set_depth(png::BitDepth::Eight);
    let mut writer = enc.write_header().map_err(|e| e.to_string())?;
    let mut rgb = Vec::with_capacity((pic.width * pic.height * 3) as usize);
    for y in 0..pic.height {
        for px in pic.row(y).chunks_exact(4) {
            rgb.extend_from_slice(&[px[2], px[1], px[0]]);
        }
    }
    writer.write_image_data(&rgb).map_err(|e| e.to_string())
}

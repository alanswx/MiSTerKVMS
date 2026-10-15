// misterkvms-cli: drive a MiSTer from the command line. Screenshots, recording,
// keyboard/mouse and core loading, for testing and debugging cores.
// Every command prints one JSON object on stdout (errors: JSON on stderr,
// exit code 1), so it's easy to script.

mod media;
mod session;
mod settings;
mod soak;
mod stream;

use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use clap::{Parser, Subcommand};
use kvms_core::{text, ConnectionState, MisterSession, MouseButton, SessionObserver};
use serde_json::{json, Value};

use media::{Item, Source};
use settings::Settings;

#[derive(Parser)]
#[command(name = "misterkvms-cli", version, about = "Drive a MiSTer from the command line: screenshots, recording, input, cores.")]
struct Cli {
    /// Settings file (default: local.settings.json in this or a parent folder).
    #[arg(long, global = true)]
    settings: Option<PathBuf>,
    /// Named machine from the settings file's "machines" (default: its "default").
    #[arg(long = "host", short = 'H', global = true)]
    host: Option<String>,
    /// MiSTer address (overrides the settings file).
    #[arg(long, global = true)]
    mister: Option<String>,
    /// Video from a capture server stream, mkvm://host/stream (overrides
    /// the settings file).
    #[arg(long, global = true)]
    stream: Option<String>,
    /// Video from a capture card on this computer, id or name (test:pattern
    /// works; overrides the settings file).
    #[arg(long, global = true)]
    local: Option<String>,
    /// Audio for --local, id or name (default: the card's own).
    #[arg(long, global = true)]
    local_audio: Option<String>,
    #[command(subcommand)]
    cmd: Cmd,
}

#[derive(Subcommand)]
enum Cmd {
    /// What's running on the MiSTer, and whether its video is reachable.
    Status,
    /// List the named machines in the settings file.
    Machines,
    /// Save the current picture as PNG.
    Screenshot {
        out: PathBuf,
        /// Use MiSTer's own screenshot (core's native resolution, pixel-exact)
        /// instead of the capture card. Saved on the MiSTer too.
        #[arg(long)]
        native: bool,
        /// Keep MiSTer's black borders (capture only).
        #[arg(long)]
        full: bool,
    },
    /// Record for a duration: .mp4 = video + audio with the apps' recorder;
    /// .mov/.mkv likewise through ffmpeg directly; .wav = audio only. Needs ffmpeg.
    Record {
        /// e.g. 10s, 500ms, 2m
        duration: String,
        out: PathBuf,
        /// Keep MiSTer's black borders.
        #[arg(long)]
        full: bool,
    },
    /// Save each changed frame as PNG for a duration (to check animation/timing).
    Frames {
        duration: String,
        dir: PathBuf,
        /// Keep identical (repeated) frames too.
        #[arg(long)]
        all: bool,
        #[arg(long)]
        full: bool,
    },
    /// Type text. Escapes: \n Enter, \t Tab, \e Esc, \b Backspace.
    Type {
        text: String,
        /// ms between keys (default 50, or type_delay_ms in settings).
        #[arg(long)]
        delay: Option<u64>,
        /// ms each key is held (default 30, or type_hold_ms in settings).
        #[arg(long)]
        hold: Option<u64>,
    },
    /// Press a key or combo: F12, enter, ctrl+alt+del, shift+a, menu (= F12).
    Key {
        combo: String,
        #[arg(long, default_value_t = 80)]
        hold: u64,
    },
    /// Mouse: move, click, down, up, pos.
    Mouse {
        #[command(subcommand)]
        action: MouseCmd,
    },
    /// Wait until the screen changes (or, with --until-still, until it stops changing).
    WaitForChange {
        #[arg(long, default_value = "10s")]
        timeout: String,
        /// Instead wait for the picture to stay unchanged this long (e.g. 2s).
        #[arg(long)]
        until_still: Option<String>,
        /// Changed pixels that count as a change (ignores tiny blinks).
        #[arg(long, default_value_t = 50)]
        min_pixels: usize,
        #[arg(long)]
        full: bool,
    },
    /// Watch a capture server stream like a viewer and report what arrived
    /// (fps, size, bandwidth, latency). E.g. `stream mkvm://capture-pc/MiSTer-1`.
    Stream {
        address: String,
        #[arg(long, default_value = "5s")]
        duration: String,
        /// Largest picture to ask for, e.g. 1280x720 (default: native).
        #[arg(long)]
        max: Option<String>,
        /// Save the last picture as PNG.
        #[arg(long)]
        save: Option<PathBuf>,
    },
    /// List capture server streams found on the network.
    Streams {
        #[arg(long, default_value = "2s")]
        wait: String,
    },
    /// Run a full app session (offscreen) and watch memory; fails if it keeps
    /// growing. Video from --stream or --local (or the settings file).
    Soak {
        /// e.g. 10m, 8h
        duration: String,
        #[arg(long, default_value = "10s")]
        every: String,
        /// Allowed growth after warm-up, MB.
        #[arg(long, default_value_t = 50.0)]
        limit_mb: f64,
    },
    /// Copy a core (.rbf) to the MiSTer over SSH and load it.
    LoadCore {
        rbf: PathBuf,
        /// Folder on the MiSTer (default /media/fat/_Development, or core_dest in settings).
        #[arg(long)]
        dest: Option<String>,
    },
}

#[derive(Subcommand)]
enum MouseCmd {
    /// Relative move in pixels.
    #[command(allow_negative_numbers = true)]
    Move { dx: f64, dy: f64 },
    /// Click a button (left, right, middle).
    Click {
        #[arg(default_value = "left")]
        button: String,
    },
    Down {
        #[arg(default_value = "left")]
        button: String,
    },
    Up {
        #[arg(default_value = "left")]
        button: String,
    },
    /// Absolute position in per-mille of the screen (0-1000). Core-dependent.
    Pos { x: u16, y: u16 },
}

fn main() {
    warn_old_name();
    let cli = Cli::parse();
    match run(cli) {
        Ok(v) => println!("{v}"),
        Err(e) => {
            eprintln!("{}", json!({ "error": e }));
            std::process::exit(1);
        }
    }
}

/// Run as `kvms` (the old name): say so, but only to a person at a terminal,
/// so scripts reading stderr still get JSON only.
fn warn_old_name() {
    use std::io::IsTerminal;
    let invoked = std::env::args_os().next().map(PathBuf::from);
    let stem = invoked.as_deref().and_then(Path::file_stem).and_then(|s| s.to_str());
    if stem == Some("kvms") && std::io::stderr().is_terminal() {
        eprintln!("note: `kvms` is now `misterkvms-cli`; the old name goes away in the next release.");
    }
}

fn run(cli: Cli) -> Result<Value, String> {
    let mut s = Settings::load(cli.settings.as_deref())?;
    s.select(cli.host.as_deref())?;
    if let Some(m) = cli.mister {
        s.mister_host = m;
    }
    // Either source on the command line replaces the settings file's.
    if cli.stream.is_some() || cli.local.is_some() {
        s.stream = cli.stream.unwrap_or_default();
        s.local = cli.local.unwrap_or_default();
    }
    if let Some(a) = cli.local_audio {
        s.local_audio = a;
    }
    match cli.cmd {
        Cmd::Stream { address, duration: d, max, save } => {
            let max = match max {
                Some(m) => {
                    let (w, h) = m.split_once(['x', 'X']).ok_or("--max is WIDTHxHEIGHT")?;
                    Some((w.parse().map_err(|_| "bad --max width")?, h.parse().map_err(|_| "bad --max height")?))
                }
                None => None,
            };
            stream::watch(&address, duration(&d)?, max, save.as_deref())
        }
        Cmd::Streams { wait } => {
            let found = kvms_core::discover::find_streams(duration(&wait)?.as_millis() as u32);
            Ok(json!({ "streams": found }))
        }
        Cmd::Soak { duration: d, every, limit_mb } => soak::soak(video_source(&s)?, duration(&d)?, duration(&every)?, limit_mb),
        Cmd::Status => status(&s),
        Cmd::Machines => Ok(json!({
            "default": s.default,
            "machines": s.machines.iter().map(|(name, m)| json!({
                "name": name,
                "mister_host": m.mister_host,
                "stream": m.stream,
                "local": m.local,
            })).collect::<Vec<_>>(),
            "settings": s.file,
        })),
        Cmd::Screenshot { out, native, full } => {
            if native {
                native_screenshot(&s, &out)
            } else {
                let src = Source::open(video_source(&s)?)?;
                let f = src.fresh_frame(Duration::from_secs(5))?;
                let area = f.area(full);
                f.save_png(&out, area)?;
                Ok(json!({ "path": out, "source": "capture", "width": area.w, "height": area.h }))
            }
        }
        Cmd::Record { duration: d, out, full } if out.extension().is_some_and(|e| e.eq_ignore_ascii_case("mp4")) => {
            // The apps' recorder: what Record in the apps makes.
            let session = session::open(video_source(&s)?, !full)?;
            session.wait_connected(Duration::from_secs(15))?;
            let path = session.record(duration(&d)?, &out)?;
            let st = session.core.state();
            Ok(json!({ "path": path, "video": st.video_stats }))
        }
        Cmd::Record { duration: d, out, full } => {
            let src = Source::open(video_source(&s)?)?;
            let r = media::record(&src, duration(&d)?, &out, full)?;
            Ok(json!({
                "path": out, "frames": r.frames, "fps": r.fps,
                "width": r.size.0, "height": r.size.1, "audio_seconds": r.audio_seconds,
            }))
        }
        Cmd::Frames { duration: d, dir, all, full } => frames(&s, duration(&d)?, &dir, all, full),
        Cmd::Type { text: t, delay, hold } => {
            let delay = Duration::from_millis(delay.or(s.type_delay_ms).unwrap_or(50));
            let hold = Duration::from_millis(hold.or(s.type_hold_ms).unwrap_or(30));
            with_input(&s, |session| type_text(session, &text::unescape(&t), delay, hold))
        }
        Cmd::Key { combo, hold } => {
            let keys = text::parse_combo(&combo)?;
            with_input(&s, |session| {
                for &k in &keys {
                    session.key_event(k, true);
                }
                std::thread::sleep(Duration::from_millis(hold));
                for &k in keys.iter().rev() {
                    session.key_event(k, false);
                }
                Ok(json!({ "pressed": combo }))
            })
        }
        Cmd::Mouse { action } => with_input(&s, |session| mouse(session, action)),
        Cmd::WaitForChange { timeout, until_still, min_pixels, full } => {
            wait_for_change(&s, duration(&timeout)?, until_still.as_deref().map(duration).transpose()?, min_pixels, full)
        }
        Cmd::LoadCore { rbf, dest } => load_core(&s, &rbf, dest),
    }
}

/// "10s", "500ms", "2m", or plain seconds.
fn duration(s: &str) -> Result<Duration, String> {
    let s = s.trim();
    let (num, mult) = if let Some(n) = s.strip_suffix("ms") {
        (n, 0.001)
    } else if let Some(n) = s.strip_suffix('s') {
        (n, 1.0)
    } else if let Some(n) = s.strip_suffix('m') {
        (n, 60.0)
    } else {
        (s, 1.0)
    };
    let v: f64 = num.trim().parse().map_err(|_| format!("bad duration '{s}' (try 10s, 500ms, 2m)"))?;
    Ok(Duration::from_secs_f64(v * mult))
}

// MARK: - mrext input

#[derive(Default)]
struct Status {
    connected: bool,
    values: HashMap<String, String>,
}

struct Observer(Arc<Mutex<Status>>);

impl SessionObserver for Observer {
    fn on_state(&self, state: ConnectionState) {
        self.0.lock().unwrap().connected = state == ConnectionState::Connected;
    }
    fn on_status(&self, key: String, value: String) {
        self.0.lock().unwrap().values.insert(key, value);
    }
}

fn connect(s: &Settings) -> Result<(Arc<MisterSession>, Arc<Mutex<Status>>), String> {
    let status = Arc::new(Mutex::new(Status::default()));
    let session = MisterSession::new(s.mister()?, s.port(), Arc::new(Observer(status.clone())));
    let until = Instant::now() + Duration::from_secs(5);
    while !status.lock().unwrap().connected {
        if Instant::now() > until {
            session.disconnect();
            return Err(format!("could not reach mrext Remote at {}:{}", s.mister_host, s.port()));
        }
        std::thread::sleep(Duration::from_millis(20));
    }
    Ok((session, status))
}

/// Connect, run `f`, then wait for everything to be sent before closing.
fn with_input(s: &Settings, f: impl FnOnce(&MisterSession) -> Result<Value, String>) -> Result<Value, String> {
    let (session, _) = connect(s)?;
    let result = f(&session);
    let until = Instant::now() + Duration::from_secs(10);
    while session.stats().queued > 0 && Instant::now() < until {
        std::thread::sleep(Duration::from_millis(10));
    }
    session.disconnect();
    std::thread::sleep(Duration::from_millis(150));
    result
}

fn type_text(session: &MisterSession, t: &str, delay: Duration, hold: Duration) -> Result<Value, String> {
    let mut typed = 0;
    let mut skipped = String::new();
    for c in t.chars() {
        let Some((key, shift)) = text::key_for_char(c) else {
            skipped.push(c);
            continue;
        };
        if shift {
            session.key_event(text::LEFT_SHIFT, true);
        }
        session.key_event(key, true);
        std::thread::sleep(hold);
        session.key_event(key, false);
        if shift {
            session.key_event(text::LEFT_SHIFT, false);
        }
        typed += 1;
        std::thread::sleep(delay);
    }
    Ok(json!({ "typed": typed, "skipped": skipped }))
}

fn button(name: &str) -> Result<MouseButton, String> {
    match name.to_ascii_lowercase().as_str() {
        "left" | "l" => Ok(MouseButton::Left),
        "right" | "r" => Ok(MouseButton::Right),
        "middle" | "m" => Ok(MouseButton::Middle),
        other => Err(format!("unknown mouse button '{other}' (left, right, middle)")),
    }
}

fn mouse(session: &MisterSession, action: MouseCmd) -> Result<Value, String> {
    match action {
        MouseCmd::Move { dx, dy } => session.mouse_move(dx, dy),
        MouseCmd::Click { button: b } => {
            let b = button(&b)?;
            session.mouse_button(b, true);
            std::thread::sleep(Duration::from_millis(60));
            session.mouse_button(b, false);
        }
        MouseCmd::Down { button: b } => session.mouse_button(button(&b)?, true),
        MouseCmd::Up { button: b } => session.mouse_button(button(&b)?, false),
        MouseCmd::Pos { x, y } => session.mouse_position(x, y),
    }
    Ok(json!({ "ok": true }))
}

// MARK: - commands

/// Where video comes from: `stream`, else `local`, else the capture server
/// stream that shows `mister_host` (found on the network, like the apps do).
fn video_source(s: &Settings) -> Result<session::SessionSource, String> {
    let audio = (!s.local_audio.is_empty()).then(|| s.local_audio.clone());
    if !s.stream.is_empty() {
        return Ok(session::SessionSource { stream: Some(s.stream.clone()), ..Default::default() });
    }
    if !s.local.is_empty() {
        return Ok(session::SessionSource { local: Some(s.local.clone()), local_audio: audio, ..Default::default() });
    }
    if s.mister_host.is_empty() {
        return Err(settings::missing("stream", "--stream or --local"));
    }
    let host = s.mister_host.trim().to_lowercase();
    match kvms_core::discover::find_streams(2500).into_iter().find(|f| f.mister.trim().to_lowercase() == host) {
        Some(f) => Ok(session::SessionSource { stream: Some(f.address), ..Default::default() }),
        None => Err(format!("no capture stream shows {}: pass --stream or --local, or set one in local.settings.json", s.mister_host)),
    }
}

fn status(s: &Settings) -> Result<Value, String> {
    let mister = match connect(s) {
        Ok((session, status)) => {
            std::thread::sleep(Duration::from_millis(300)); // initial status messages
            let st = status.lock().unwrap();
            let v = json!({
                "host": s.mister_host, "connected": true,
                "core": st.values.get("coreRunning").cloned().unwrap_or_default(),
                "game": st.values.get("gameRunning").cloned().unwrap_or_default(),
            });
            drop(st);
            session.disconnect();
            v
        }
        Err(e) => json!({ "host": s.mister_host, "connected": false, "error": e }),
    };
    let video = match video_source(s) {
        Ok(session::SessionSource { stream: Some(stream), .. }) => {
            // Is the stream announced on the network? (Compared by name: an
            // address typed by hand can name the server differently.)
            let name = stream.rsplit('/').next().unwrap_or_default().to_string();
            let found = kvms_core::discover::find_streams(1500);
            json!({ "stream": stream, "found": found.iter().any(|f| f.name.eq_ignore_ascii_case(&name)), "streams": found })
        }
        Ok(session::SessionSource { local: Some(card), local_audio, .. }) => {
            let cards = kvms_capture::devices().unwrap_or_default();
            let found = cards.iter().any(|d| d.id == card || d.name.eq_ignore_ascii_case(&card));
            json!({ "local": card, "audio": local_audio, "found": found, "cards": cards.iter().map(|d| &d.name).collect::<Vec<_>>() })
        }
        Ok(_) => json!({}),
        Err(e) => json!({ "error": e }),
    };
    Ok(json!({ "machine": s.machine, "mister": mister, "video": video, "settings": s.file }))
}

#[derive(serde::Deserialize)]
struct Shot {
    path: String,
    core: String,
}

fn native_screenshot(s: &Settings, out: &Path) -> Result<Value, String> {
    let base = format!("http://{}:{}/api/screenshots", s.mister()?, s.port());
    let list = |base: &str| -> Result<Vec<Shot>, String> {
        ureq::get(base).call().map_err(|e| e.to_string())?.body_mut().read_json().map_err(|e| e.to_string())
    };
    let before: Vec<String> = list(&base)?.into_iter().map(|x| x.path).collect();
    ureq::post(&base).send_empty().map_err(|e| format!("MiSTer screenshot request failed: {e}"))?;
    let until = Instant::now() + Duration::from_secs(8);
    loop {
        if let Some(shot) = list(&base)?.into_iter().find(|x| !before.contains(&x.path)) {
            let url = format!("{base}/{}", shot.path.split('/').map(encode).collect::<Vec<_>>().join("/"));
            let bytes = ureq::get(&url)
                .call()
                .map_err(|e| e.to_string())?
                .body_mut()
                .with_config()
                .limit(64 * 1024 * 1024)
                .read_to_vec()
                .map_err(|e| e.to_string())?;
            std::fs::write(out, &bytes).map_err(|e| format!("{}: {e}", out.display()))?;
            return Ok(json!({ "path": out, "source": "mister", "core": shot.core, "mister_path": shot.path }));
        }
        if Instant::now() > until {
            return Err("MiSTer didn't save a screenshot (PS/2 keyboard mode can block Alt+ScrollLock)".into());
        }
        std::thread::sleep(Duration::from_millis(300));
    }
}

fn encode(segment: &str) -> String {
    segment
        .bytes()
        .map(|b| match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => (b as char).to_string(),
            _ => format!("%{b:02X}"),
        })
        .collect()
}

fn frames(s: &Settings, d: Duration, dir: &Path, all: bool, full: bool) -> Result<Value, String> {
    std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
    let src = Source::open(video_source(s)?)?;
    let first = src.fresh_frame(Duration::from_secs(5))?;
    let area = first.area(full);
    let start = Instant::now();
    let (mut saved, mut seen, mut prev) = (Vec::new(), 0, None::<media::Frame>);
    let mut consider = |f: media::Frame, at: Duration, prev: &mut Option<media::Frame>| -> Result<(), String> {
        seen += 1;
        let changed = prev.as_ref().is_none_or(|p| f.changed_pixels(p, area) > 0);
        if all || changed {
            let path = dir.join(format!("frame_{:05}_{:06}ms.png", saved.len(), at.as_millis()));
            f.save_png(&path, area)?;
            saved.push(path);
        }
        *prev = Some(f);
        Ok(())
    };
    consider(first, Duration::ZERO, &mut prev)?;
    while start.elapsed() < d {
        if let Some(Item::Video(f)) = src.next(100) {
            consider(f, start.elapsed(), &mut prev)?;
        }
    }
    Ok(json!({ "dir": dir, "frames_seen": seen, "saved": saved.len(), "width": area.w, "height": area.h }))
}

fn wait_for_change(s: &Settings, timeout: Duration, still: Option<Duration>, min: usize, full: bool) -> Result<Value, String> {
    let src = Source::open(video_source(s)?)?;
    let mut prev = src.fresh_frame(Duration::from_secs(5))?;
    let area = prev.area(full);
    let start = Instant::now();
    let mut last_change = Instant::now();
    while start.elapsed() < timeout {
        let Some(Item::Video(f)) = src.next(100) else { continue };
        let changed = f.changed_pixels(&prev, area) >= min;
        prev = f;
        match still {
            None if changed => {
                return Ok(json!({ "changed": true, "after_ms": start.elapsed().as_millis() }));
            }
            Some(_) if changed => last_change = Instant::now(),
            Some(need) if last_change.elapsed() >= need => {
                return Ok(json!({ "still": true, "after_ms": start.elapsed().as_millis() }));
            }
            _ => {}
        }
    }
    Ok(json!({ "changed": false, "still": false, "timed_out": true, "after_ms": start.elapsed().as_millis() }))
}

fn load_core(s: &Settings, rbf: &Path, dest: Option<String>) -> Result<Value, String> {
    let host = s.mister()?;
    let target = format!("{}@{host}", s.ssh_user.as_deref().unwrap_or("root"));
    let dest = dest.or(s.core_dest.clone()).unwrap_or_else(|| "/media/fat/_Development".into());
    let name = rbf.file_name().and_then(|n| n.to_str()).ok_or("not a file name")?;
    if !rbf.is_file() {
        return Err(format!("{} not found", rbf.display()));
    }
    let remote = format!("{dest}/{name}");
    let quote = |p: &str| format!("'{}'", p.replace('\'', r"'\''"));
    let ssh = |cmd: &str| -> Result<(), String> {
        let ok = std::process::Command::new("ssh")
            .args(["-o", "BatchMode=yes", "-o", "ConnectTimeout=5", &target, cmd])
            .status()
            .map_err(|e| e.to_string())?
            .success();
        ok.then_some(()).ok_or_else(|| format!("ssh {target}: '{cmd}' failed"))
    };
    ssh(&format!("mkdir -p {}", quote(&dest)))?;
    let ok = std::process::Command::new("scp")
        .args(["-q", "-o", "BatchMode=yes"])
        .arg(rbf)
        .arg(format!("{target}:{}", quote(&remote)))
        .status()
        .map_err(|e| e.to_string())?
        .success();
    if !ok {
        return Err(format!("copying {name} to {target} failed"));
    }
    ssh(&format!("echo load_core {} > /dev/MiSTer_cmd", quote(&remote)))?;
    Ok(json!({ "loaded": remote }))
}

//! misterkvms-capture: owns the capture cards and streams each MiSTer to any
//! number of MiSTerKVMS viewers over the mkvm protocol, with a setup page
//! for choosing cards and checking them (docs/CAPTURE-SERVER.md §5).

mod config;
mod discovery;
mod net;
mod setup;
mod stream;

use std::path::PathBuf;
use std::sync::{Arc, Mutex};

use clap::{Parser, Subcommand};

use config::{Settings, StreamConfig, Streams};
use stream::Stream;

#[derive(Parser)]
#[command(name = "misterkvms-capture", version, about = "Stream MiSTer capture cards to MiSTerKVMS viewers.")]
struct Cli {
    /// Config folder (default: the platform's; see `misterkvms-capture paths`).
    #[arg(long, global = true)]
    config: Option<PathBuf>,
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// Run the server (the default).
    Serve,
    /// List capture cards, their formats, and audio inputs (JSON).
    Devices,
    /// Show the configured streams (JSON).
    List,
    /// Add a stream for a capture card.
    Add {
        /// Stream name, e.g. "MiSTer-SS1".
        name: String,
        /// Video device: id or name (see `devices`).
        #[arg(long)]
        video: String,
        /// Audio device id or name; "auto" picks the card's own (default).
        #[arg(long, default_value = "auto")]
        audio: String,
        /// The MiSTer's address (mrext Remote).
        #[arg(long)]
        mister: Option<String>,
    },
    /// Remove a stream.
    Remove { name: String },
    /// Where the config files are.
    Paths,
}

pub fn log(msg: &str) {
    kvms_core::log(msg);
}

pub fn now_100ns() -> i64 {
    kvms_capture::now_100ns()
}

pub fn server_name() -> String {
    format!("misterkvms-capture {}", env!("CARGO_PKG_VERSION"))
}

/// The running server: settings and streams. Streams are restarted when the
/// setup page saves a change.
pub struct Server {
    dir: PathBuf,
    settings: Mutex<Settings>,
    streams: Mutex<Vec<Arc<Stream>>>,
    discovery: Mutex<Option<discovery::Discovery>>,
}

impl Server {
    pub fn stream(&self, name: &str) -> Option<Arc<Stream>> {
        let streams = self.streams.lock().unwrap();
        streams.iter().find(|s| s.config.name == name).or_else(|| streams.iter().find(|s| s.config.name.eq_ignore_ascii_case(name))).cloned()
    }

    pub fn stream_names(&self) -> Vec<String> {
        self.streams.lock().unwrap().iter().map(|s| s.config.name.clone()).collect()
    }

    pub fn streams(&self) -> Vec<Arc<Stream>> {
        self.streams.lock().unwrap().clone()
    }

    pub fn settings(&self) -> Settings {
        self.settings.lock().unwrap().clone()
    }

    pub fn dir(&self) -> &std::path::Path {
        &self.dir
    }

    /// Start, stop or restart streams to match `wanted` (unchanged ones keep
    /// running, and keep their viewers).
    pub fn apply(&self, wanted: &Streams) {
        let mut streams = self.streams.lock().unwrap();
        let mut keep = Vec::new();
        for s in streams.drain(..) {
            if wanted.streams.contains(&s.config) {
                keep.push(s);
            } else {
                s.stop();
            }
        }
        for c in &wanted.streams {
            if c.name.trim().is_empty() || keep.iter().any(|s| s.config == *c) {
                continue;
            }
            keep.push(Stream::start(c.clone()));
        }
        // Configured order.
        keep.sort_by_key(|s| wanted.streams.iter().position(|c| c.name == s.config.name));
        *streams = keep;
        let names: Vec<(String, String)> = streams.iter().map(|s| (s.config.name.clone(), s.config.mister.clone())).collect();
        drop(streams);
        if let Some(d) = self.discovery.lock().unwrap().as_mut() {
            d.announce(&names, self.settings().port);
        }
    }

    pub fn save(&self, wanted: &Streams) -> std::io::Result<()> {
        config::save_streams(&self.dir, wanted)?;
        self.apply(wanted);
        Ok(())
    }
}

fn main() {
    let cli = Cli::parse();
    let dir = cli.config.unwrap_or_else(config::dir);
    let result = match cli.cmd.unwrap_or(Cmd::Serve) {
        Cmd::Serve => serve(dir),
        Cmd::Devices => devices(),
        Cmd::List => {
            println!("{}", serde_json::to_string_pretty(&config::load_streams(&dir)).unwrap());
            Ok(())
        }
        Cmd::Add { name, video, audio, mister } => add(&dir, name, video, audio, mister),
        Cmd::Remove { name } => {
            let mut s = config::load_streams(&dir);
            let before = s.streams.len();
            s.streams.retain(|c| c.name != name);
            if s.streams.len() == before {
                Err(format!("no stream '{name}'"))
            } else {
                config::save_streams(&dir, &s).map_err(|e| e.to_string())
            }
        }
        Cmd::Paths => {
            println!("config folder: {}", dir.display());
            println!("streams:       {}", dir.join("capture.json").display());
            println!("settings:      {}", dir.join("capture.ini").display());
            Ok(())
        }
    };
    if let Err(e) = result {
        eprintln!("error: {e}");
        std::process::exit(1);
    }
}

fn serve(dir: PathBuf) -> Result<(), String> {
    let settings = config::load_settings(&dir);
    log(&format!("{} (config {})", server_name(), dir.display()));
    let server = Arc::new(Server {
        dir: dir.clone(),
        settings: Mutex::new(settings.clone()),
        streams: Mutex::new(Vec::new()),
        discovery: Mutex::new(discovery::Discovery::start().map_err(|e| log(&format!("mDNS: {e}"))).ok()),
    });
    net::listen(server.clone(), settings.port).map_err(|e| format!("can't listen on TCP {}: {e}", settings.port))?;
    setup::start(server.clone(), &settings).map_err(|e| format!("can't start the setup page on TCP {}: {e}", settings.setup_port))?;
    let streams = config::load_streams(&dir);
    if streams.streams.is_empty() {
        log(&format!("no streams yet: open http://localhost:{} to add one", settings.setup_port));
    }
    server.apply(&streams);
    loop {
        std::thread::park();
    }
}

fn devices() -> Result<(), String> {
    let video = kvms_capture::devices().map_err(|e| e.to_string())?;
    let audio = kvms_capture::audio_devices();
    println!("{}", serde_json::to_string_pretty(&serde_json::json!({ "video": video, "audio": audio })).unwrap());
    Ok(())
}

fn add(dir: &std::path::Path, name: String, video: String, audio: String, mister: Option<String>) -> Result<(), String> {
    let devices = kvms_capture::devices().map_err(|e| e.to_string())?;
    let device = devices
        .iter()
        .find(|d| d.id == video)
        .or_else(|| devices.iter().find(|d| d.name.eq_ignore_ascii_case(&video)))
        .ok_or_else(|| format!("no capture card '{video}' (see `misterkvms-capture devices`)"))?;
    let audio_names: Vec<String> = kvms_capture::audio_devices().into_iter().map(|a| a.name).collect();
    let audio = match audio.as_str() {
        "auto" => kvms_capture::matching_audio(&device.name, &audio_names).unwrap_or_default(),
        "none" => String::new(),
        other => other.to_string(),
    };
    let mut streams = config::load_streams(dir);
    streams.streams.retain(|c| c.name != name);
    streams.streams.push(StreamConfig {
        name: name.clone(),
        video: device.id.clone(),
        video_name: device.name.clone(),
        audio: audio.clone(),
        mister: mister.unwrap_or_default(),
        ..Default::default()
    });
    config::save_streams(dir, &streams).map_err(|e| e.to_string())?;
    println!("added '{name}': video '{}', audio '{audio}'", device.name);
    Ok(())
}

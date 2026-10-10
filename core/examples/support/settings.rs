//! Local test settings for the example tools, read from `local.settings.json`
//! in the repository root (git-ignored; copy `local.settings.example.json`).
//! Command-line arguments override the file.

#![allow(dead_code)] // shared by several examples; each uses a subset

use std::path::PathBuf;

#[derive(serde::Deserialize, Default)]
#[serde(default)]
pub struct Settings {
    /// MiSTer running mrext Remote.
    pub mister_host: String,
    pub mister_port: Option<u16>,
    /// NDI source carrying that MiSTer's video, e.g. "OBSPC (MiSTer-1)".
    pub ndi_source: String,
}

impl Settings {
    /// Settings from the file, with positional args overriding:
    /// `[mister-host] [ndi-source]`.
    pub fn load() -> Self {
        let path = PathBuf::from(env!("CARGO_MANIFEST_DIR")).join("../local.settings.json");
        let mut s: Settings = match std::fs::read_to_string(&path) {
            Ok(text) => serde_json::from_str(&text)
                .unwrap_or_else(|e| panic!("{}: {e}", path.display())),
            Err(_) => Settings::default(),
        };
        let mut args = std::env::args().skip(1);
        if let Some(host) = args.next() {
            s.mister_host = host;
        }
        if let Some(source) = args.next() {
            s.ndi_source = source;
        }
        s
    }

    pub fn mister_host(&self) -> String {
        if self.mister_host.is_empty() {
            missing("mister_host");
        }
        self.mister_host.clone()
    }

    pub fn mister_port(&self) -> u16 {
        self.mister_port.unwrap_or(8182)
    }

    pub fn ndi_source(&self) -> String {
        if self.ndi_source.is_empty() {
            missing("ndi_source");
        }
        self.ndi_source.clone()
    }
}

fn missing(field: &str) -> ! {
    eprintln!(
        "No {field} set. Copy local.settings.example.json to local.settings.json and fill it in, \
         or pass it on the command line."
    );
    std::process::exit(2);
}

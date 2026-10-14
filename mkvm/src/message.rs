//! The messages. Control messages carry JSON payloads (easy to extend,
//! cheap at their rate); video and audio are binary.

use serde::{Deserialize, Serialize};

use crate::wire::FLAG_KEYFRAME;

/// Message kinds on the wire. 0x0_: viewer → server, 0x1_: either way,
/// 0x2_: server → viewer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
#[repr(u8)]
pub enum Kind {
    Hello = 0x01,
    Subscribe = 0x02,
    Input = 0x03,
    TakeControl = 0x04,
    Ping = 0x10,
    Pong = 0x11,
    Welcome = 0x20,
    Video = 0x21,
    Audio = 0x22,
    Format = 0x23,
    Control = 0x24,
    Mister = 0x25,
    Gap = 0x26,
    Error = 0x27,
    Status = 0x28,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Codec {
    /// One JPEG per frame (every frame a keyframe).
    Jpeg,
    /// H.264 Annex B access units.
    H264,
}

impl Codec {
    pub(crate) fn id(self) -> u8 {
        match self {
            Codec::Jpeg => 1,
            Codec::H264 => 2,
        }
    }

    fn from_id(id: u8) -> Option<Self> {
        match id {
            1 => Some(Codec::Jpeg),
            2 => Some(Codec::H264),
            _ => None,
        }
    }
}

/// Viewer → server, first message on a connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Hello {
    pub protocol: u32,
    /// The stream's name, as in its address (`mkvm://pc/<stream>`).
    pub stream: String,
    /// Shown to other viewers ("*viewer* has control").
    pub viewer: String,
    /// "macos", "windows", "linux", "cli"...
    #[serde(default)]
    pub platform: String,
    /// Codecs the viewer can decode, best first.
    pub codecs: Vec<Codec>,
}

/// Viewer → server: what to send. May be sent again at any time to change
/// quality; the server switches at the next frame (keyframe for H.264).
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
#[serde(default)]
pub struct Subscribe {
    pub video: bool,
    pub audio: bool,
    /// Largest picture wanted; the server picks the best size it offers
    /// within it. None = native.
    pub max_width: Option<u32>,
    pub max_height: Option<u32>,
    pub max_fps: Option<f64>,
    /// Preferred codec; None lets the server choose (native JPEG on a LAN).
    pub codec: Option<Codec>,
}

/// Input for the MiSTer, already translated to mrext's terms by the viewer
/// (the same commands its own mrext session would send).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "t", rename_all = "snake_case")]
pub enum InputCmd {
    /// One mrext message, e.g. "kbdRawDown:30" or "mouseBtn:left_down".
    Send { m: String },
    /// Relative mouse movement (already scaled by the viewer's sensitivity).
    Move { dx: f64, dy: f64 },
    /// Wait before the next command (key taps hold for a moment).
    Delay { ms: u32 },
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StreamInfo {
    pub name: String,
    /// The MiSTer this stream shows (mrext address), if configured.
    pub mister: Option<String>,
    /// Current native picture size and rate (0 if no signal yet).
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub audio_rate: u32,
    pub audio_channels: u32,
    /// Sizes the server offers, largest first.
    pub sizes: Vec<(u32, u32)>,
    pub codecs: Vec<Codec>,
}

/// Server → viewer, answer to `Hello`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Welcome {
    pub protocol: u32,
    pub server: String,
    /// This viewer's id on the stream (matches `ControlState.holder.id`).
    pub viewer_id: u32,
    pub stream: StreamInfo,
}

#[derive(Debug, Clone, PartialEq)]
pub struct Video {
    pub codec: Codec,
    pub keyframe: bool,
    pub width: u32,
    pub height: u32,
    pub data: Vec<u8>,
}

/// Interleaved signed 16-bit PCM.
#[derive(Debug, Clone, PartialEq)]
pub struct Audio {
    pub channels: u8,
    pub rate: u32,
    pub samples: Vec<i16>,
}

/// The picture changed (MiSTer switched modes) or the signal went away.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Format {
    pub width: u32,
    pub height: u32,
    pub fps: f64,
    pub signal: bool,
}

#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Viewer {
    pub id: u32,
    pub name: String,
}

/// Who controls the stream. Sent to every viewer whenever it changes, and
/// after `Welcome`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ControlState {
    pub holder: Option<Viewer>,
    /// Whether the receiving viewer is the holder.
    pub you: bool,
}

/// The server's mrext connection to the stream's MiSTer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct MisterState {
    pub connected: bool,
    #[serde(default)]
    pub reason: String,
}

/// A status value mrext pushed, e.g. ("coreRunning", "HP9K715"). The
/// server sends the ones it knows after `Welcome`, then each change.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Status {
    pub key: String,
    pub value: String,
}

/// Frames or audio the server dropped for this viewer since the last `Gap`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Gap {
    pub video: u32,
    pub audio: u32,
}

/// Sent before the server closes the connection.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ErrorMsg {
    pub message: String,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Message {
    Hello(Hello),
    Subscribe(Subscribe),
    Input(Vec<InputCmd>),
    TakeControl,
    /// Keep-alive. The header timestamp is the sender's clock.
    Ping,
    /// Answer to `Ping`: `echo` is the ping's timestamp, the header's is the
    /// answerer's clock, so the pinger learns round trip and clock offset.
    Pong { echo: i64 },
    Welcome(Welcome),
    Video(Video),
    Audio(Audio),
    Format(Format),
    Control(ControlState),
    Mister(MisterState),
    Gap(Gap),
    Error(ErrorMsg),
    Status(Status),
    /// A kind this version doesn't know (from a newer peer); skip it.
    Unknown { kind: u8 },
}

fn json<T: Serialize>(v: &T) -> Vec<u8> {
    serde_json::to_vec(v).expect("protocol types always serialize")
}

fn parse<T: for<'de> Deserialize<'de>>(payload: &[u8]) -> Result<T, String> {
    serde_json::from_slice(payload).map_err(|e| e.to_string())
}

impl Message {
    pub fn kind(&self) -> Option<Kind> {
        Some(match self {
            Message::Hello(_) => Kind::Hello,
            Message::Subscribe(_) => Kind::Subscribe,
            Message::Input(_) => Kind::Input,
            Message::TakeControl => Kind::TakeControl,
            Message::Ping => Kind::Ping,
            Message::Pong { .. } => Kind::Pong,
            Message::Welcome(_) => Kind::Welcome,
            Message::Video(_) => Kind::Video,
            Message::Audio(_) => Kind::Audio,
            Message::Format(_) => Kind::Format,
            Message::Control(_) => Kind::Control,
            Message::Mister(_) => Kind::Mister,
            Message::Gap(_) => Kind::Gap,
            Message::Error(_) => Kind::Error,
            Message::Status(_) => Kind::Status,
            Message::Unknown { .. } => return None,
        })
    }

    /// (kind, flags, payload). Panics on `Unknown`, which is never sent.
    pub fn encode(&self) -> (Kind, u8, Vec<u8>) {
        let kind = self.kind().expect("Unknown messages are only received");
        let (flags, payload) = match self {
            Message::Hello(m) => (0, json(m)),
            Message::Subscribe(m) => (0, json(m)),
            Message::Input(m) => (0, json(m)),
            Message::TakeControl | Message::Ping => (0, Vec::new()),
            Message::Pong { echo } => (0, echo.to_le_bytes().to_vec()),
            Message::Welcome(m) => (0, json(m)),
            Message::Video(v) => {
                let mut p = Vec::with_capacity(9 + v.data.len());
                p.push(v.codec.id());
                p.extend_from_slice(&v.width.to_le_bytes());
                p.extend_from_slice(&v.height.to_le_bytes());
                p.extend_from_slice(&v.data);
                (if v.keyframe { FLAG_KEYFRAME } else { 0 }, p)
            }
            Message::Audio(a) => {
                let mut p = Vec::with_capacity(5 + a.samples.len() * 2);
                p.push(a.channels);
                p.extend_from_slice(&a.rate.to_le_bytes());
                for s in &a.samples {
                    p.extend_from_slice(&s.to_le_bytes());
                }
                (0, p)
            }
            Message::Format(m) => (0, json(m)),
            Message::Control(m) => (0, json(m)),
            Message::Mister(m) => (0, json(m)),
            Message::Gap(m) => (0, json(m)),
            Message::Error(m) => (0, json(m)),
            Message::Status(m) => (0, json(m)),
            Message::Unknown { .. } => unreachable!(),
        };
        (kind, flags, payload)
    }

    pub fn decode(kind: u8, flags: u8, payload: Vec<u8>) -> Result<Message, String> {
        Ok(match kind {
            0x01 => Message::Hello(parse(&payload)?),
            0x02 => Message::Subscribe(parse(&payload)?),
            0x03 => Message::Input(parse(&payload)?),
            0x04 => Message::TakeControl,
            0x10 => Message::Ping,
            0x11 => {
                let b: [u8; 8] = payload.as_slice().try_into().map_err(|_| "pong needs 8 bytes".to_string())?;
                Message::Pong { echo: i64::from_le_bytes(b) }
            }
            0x20 => Message::Welcome(parse(&payload)?),
            0x21 => {
                if payload.len() < 9 {
                    return Err("video header too short".into());
                }
                let codec = Codec::from_id(payload[0]).ok_or_else(|| format!("unknown codec {}", payload[0]))?;
                let width = u32::from_le_bytes(payload[1..5].try_into().unwrap());
                let height = u32::from_le_bytes(payload[5..9].try_into().unwrap());
                let mut data = payload;
                data.drain(..9);
                Message::Video(Video { codec, keyframe: flags & FLAG_KEYFRAME != 0, width, height, data })
            }
            0x22 => {
                if payload.len() < 5 || (payload.len() - 5) % 2 != 0 {
                    return Err("bad audio length".into());
                }
                let channels = payload[0];
                let rate = u32::from_le_bytes(payload[1..5].try_into().unwrap());
                let samples = payload[5..].chunks_exact(2).map(|c| i16::from_le_bytes([c[0], c[1]])).collect();
                Message::Audio(Audio { channels, rate, samples })
            }
            0x23 => Message::Format(parse(&payload)?),
            0x24 => Message::Control(parse(&payload)?),
            0x25 => Message::Mister(parse(&payload)?),
            0x26 => Message::Gap(parse(&payload)?),
            0x27 => Message::Error(parse(&payload)?),
            0x28 => Message::Status(parse(&payload)?),
            other => Message::Unknown { kind: other },
        })
    }
}

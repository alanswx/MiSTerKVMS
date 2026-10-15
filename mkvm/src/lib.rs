//! The mkvm protocol: video, audio and input for one MiSTer between
//! `misterkvms-capture` and its viewers. See docs/CAPTURE-SERVER.md §4.
//!
//! A viewer opens one TCP connection per stream, sends `Hello` naming the
//! stream, and gets `Welcome` back. After that both sides exchange framed
//! messages ([`wire`]) until either closes.
//!
//! Pure protocol logic (no sockets, no threads) so it can be tested and
//! shared by the server, the apps' core and the CLI.

pub mod address;
pub mod control;
pub mod held;
pub mod message;
pub mod secure;
pub mod wire;

pub use address::StreamAddress;
pub use control::{Control, ControlChange, ViewerId};
pub use held::HeldInput;
pub use message::*;
pub use wire::{read_message, write_message, write_video, Header, WireError};

/// Protocol version. A different major version is refused in `Hello`.
pub const PROTOCOL_VERSION: u32 = 1;
/// Viewers connect here (TCP; later also QUIC on UDP).
pub const DEFAULT_PORT: u16 = 5970;
/// The server's setup page (HTTP).
pub const SETUP_PORT: u16 = 5971;
/// mDNS service type; each stream registers its own instance.
pub const SERVICE_TYPE: &str = "_mkvm._tcp.local.";

//! Framing: every message is a 14-byte header and a payload.
//!
//! ```text
//! kind u8 | flags u8 | len u32 (LE) | timestamp i64 (LE, 100 ns, sender clock) | payload[len]
//! ```

use std::io::{self, Read, Write};

use crate::message::Message;

pub const HEADER_LEN: usize = 14;
/// Larger payloads are refused: a 4K raw BGRA frame is 33 MB; nothing we
/// send comes close to this as JPEG or H.264.
pub const MAX_PAYLOAD: u32 = 64 * 1024 * 1024;

/// `flags` bit on video: the frame is a keyframe (always set for JPEG).
pub const FLAG_KEYFRAME: u8 = 1;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Header {
    pub kind: u8,
    pub flags: u8,
    pub len: u32,
    pub timestamp: i64,
}

impl Header {
    pub fn encode(&self) -> [u8; HEADER_LEN] {
        let mut b = [0u8; HEADER_LEN];
        b[0] = self.kind;
        b[1] = self.flags;
        b[2..6].copy_from_slice(&self.len.to_le_bytes());
        b[6..14].copy_from_slice(&self.timestamp.to_le_bytes());
        b
    }

    pub fn decode(b: &[u8; HEADER_LEN]) -> Self {
        Self {
            kind: b[0],
            flags: b[1],
            len: u32::from_le_bytes(b[2..6].try_into().unwrap()),
            timestamp: i64::from_le_bytes(b[6..14].try_into().unwrap()),
        }
    }
}

#[derive(Debug)]
pub enum WireError {
    Io(io::Error),
    /// The peer closed the connection between messages.
    Closed,
    TooLarge(u32),
    /// A known message kind whose payload doesn't parse.
    Malformed { kind: u8, reason: String },
}

impl std::fmt::Display for WireError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            WireError::Io(e) => write!(f, "{e}"),
            WireError::Closed => f.write_str("connection closed"),
            WireError::TooLarge(n) => write!(f, "message too large ({n} bytes)"),
            WireError::Malformed { kind, reason } => write!(f, "bad message 0x{kind:02x}: {reason}"),
        }
    }
}

impl std::error::Error for WireError {}

impl From<io::Error> for WireError {
    fn from(e: io::Error) -> Self {
        if e.kind() == io::ErrorKind::UnexpectedEof {
            WireError::Closed
        } else {
            WireError::Io(e)
        }
    }
}

/// Write one message. `timestamp` is the sender's clock in 100 ns units
/// (the capture time for video and audio).
pub fn write_message(w: &mut impl Write, msg: &Message, timestamp: i64) -> io::Result<()> {
    let (kind, flags, payload) = msg.encode();
    let header = Header { kind: kind as u8, flags, len: payload.len() as u32, timestamp };
    // One write for small messages keeps them in one TCP segment.
    if payload.len() <= 1024 {
        let mut buf = Vec::with_capacity(HEADER_LEN + payload.len());
        buf.extend_from_slice(&header.encode());
        buf.extend_from_slice(&payload);
        w.write_all(&buf)
    } else {
        w.write_all(&header.encode())?;
        w.write_all(&payload)
    }
}

/// Write a video message from borrowed data (the server sends one encoded
/// frame to many viewers without copying it per viewer). Same bytes as
/// `write_message(Message::Video(..))`.
pub fn write_video(
    w: &mut impl Write,
    codec: crate::message::Codec,
    keyframe: bool,
    width: u32,
    height: u32,
    data: &[u8],
    timestamp: i64,
) -> io::Result<()> {
    let mut head = [0u8; HEADER_LEN + 9];
    let header = Header {
        kind: crate::message::Kind::Video as u8,
        flags: if keyframe { FLAG_KEYFRAME } else { 0 },
        len: (9 + data.len()) as u32,
        timestamp,
    };
    head[..HEADER_LEN].copy_from_slice(&header.encode());
    head[HEADER_LEN] = codec.id();
    head[HEADER_LEN + 1..HEADER_LEN + 5].copy_from_slice(&width.to_le_bytes());
    head[HEADER_LEN + 5..].copy_from_slice(&height.to_le_bytes());
    w.write_all(&head)?;
    w.write_all(data)
}

/// Read one message. Unknown kinds come back as `Message::Unknown` so newer
/// peers don't break older ones.
pub fn read_message(r: &mut impl Read) -> Result<(Message, Header), WireError> {
    let mut b = [0u8; HEADER_LEN];
    r.read_exact(&mut b)?;
    let header = Header::decode(&b);
    if header.len > MAX_PAYLOAD {
        return Err(WireError::TooLarge(header.len));
    }
    let mut payload = vec![0u8; header.len as usize];
    r.read_exact(&mut payload).map_err(|e| match WireError::from(e) {
        // EOF inside a message is an error, not a clean close.
        WireError::Closed => WireError::Io(io::ErrorKind::UnexpectedEof.into()),
        other => other,
    })?;
    let msg = Message::decode(header.kind, header.flags, payload)
        .map_err(|reason| WireError::Malformed { kind: header.kind, reason })?;
    Ok((msg, header))
}

/// Parse a message from a buffer that holds exactly one framed message
/// (for tests and fuzzing).
pub fn decode_one(bytes: &[u8]) -> Result<(Message, Header), WireError> {
    let mut r = bytes;
    let out = read_message(&mut r)?;
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::*;

    fn round_trip(msg: Message) {
        let mut buf = Vec::new();
        write_message(&mut buf, &msg, 1234).unwrap();
        let (back, header) = decode_one(&buf).unwrap();
        assert_eq!(back, msg);
        assert_eq!(header.timestamp, 1234);
    }

    #[test]
    fn every_message_round_trips() {
        round_trip(Message::Hello(Hello {
            protocol: 1,
            stream: "MiSTer SS1".into(),
            viewer: "Dani's Mac".into(),
            platform: "macos".into(),
            codecs: vec![Codec::Jpeg, Codec::H264],
        }));
        round_trip(Message::Subscribe(Subscribe {
            video: true,
            audio: true,
            max_width: Some(1920),
            max_height: Some(1080),
            max_fps: Some(30.0),
            codec: Some(Codec::H264),
            quality: Some(70),
        }));
        round_trip(Message::Pair(Pair { device: "Dani's iPhone".into() }));
        round_trip(Message::Paired(Paired {
            key_id: "00112233445566778899aabbccddeeff".into(),
            key: "ab".repeat(32),
            internet_address: "myhome.example.org:5970".into(),
        }));
        round_trip(Message::Input(vec![
            InputCmd::Send { m: "kbdRawDown:30".into() },
            InputCmd::Move { dx: 1.5, dy: -2.0 },
            InputCmd::Delay { ms: 60 },
        ]));
        round_trip(Message::TakeControl);
        round_trip(Message::Ping);
        round_trip(Message::Pong { echo: -77 });
        round_trip(Message::Welcome(Welcome {
            protocol: 1,
            server: "misterkvms-capture 0.9.0".into(),
            viewer_id: 3,
            stream: StreamInfo {
                name: "MiSTer SS1".into(),
                mister: Some("192.168.1.50".into()),
                width: 1920,
                height: 1080,
                fps: 60.0,
                audio_rate: 48000,
                audio_channels: 2,
                sizes: vec![(1920, 1080), (1280, 720)],
                codecs: vec![Codec::Jpeg],
            },
        }));
        round_trip(Message::Video(Video { codec: Codec::Jpeg, keyframe: true, width: 640, height: 480, data: vec![1, 2, 3] }));
        round_trip(Message::Video(Video { codec: Codec::H264, keyframe: false, width: 1920, height: 1080, data: vec![9; 3000] }));
        round_trip(Message::Audio(Audio { channels: 2, rate: 48000, samples: vec![0, -1, 32767, -32768] }));
        round_trip(Message::Format(Format { width: 1024, height: 768, fps: 60.0, signal: true }));
        round_trip(Message::Control(ControlState { holder: Some(Viewer { id: 2, name: "PC".into() }), you: false }));
        round_trip(Message::Mister(MisterState { connected: false, reason: "connect refused".into() }));
        round_trip(Message::Gap(Gap { video: 3, audio: 0 }));
        round_trip(Message::Error(ErrorMsg { message: "no such stream".into() }));
        round_trip(Message::Status(Status { key: "coreRunning".into(), value: "HP9K715".into() }));
    }

    #[test]
    fn write_video_matches_write_message() {
        let v = Video { codec: Codec::Jpeg, keyframe: true, width: 800, height: 600, data: vec![7; 50] };
        let mut a = Vec::new();
        write_message(&mut a, &Message::Video(v.clone()), 99).unwrap();
        let mut b = Vec::new();
        write_video(&mut b, v.codec, v.keyframe, v.width, v.height, &v.data, 99).unwrap();
        assert_eq!(a, b);
    }

    #[test]
    fn unknown_kinds_are_skipped_not_errors() {
        let header = Header { kind: 0x7e, flags: 0, len: 3, timestamp: 0 };
        let mut buf = header.encode().to_vec();
        buf.extend_from_slice(&[1, 2, 3]);
        buf.extend_from_slice(&{
            let mut b = Vec::new();
            write_message(&mut b, &Message::Ping, 5).unwrap();
            b
        });
        let mut r = &buf[..];
        assert!(matches!(read_message(&mut r).unwrap().0, Message::Unknown { kind: 0x7e }));
        assert_eq!(read_message(&mut r).unwrap().0, Message::Ping);
        assert!(matches!(read_message(&mut r), Err(WireError::Closed)));
    }

    #[test]
    fn oversized_and_truncated_messages_are_errors() {
        let header = Header { kind: 0x20, flags: 0, len: MAX_PAYLOAD + 1, timestamp: 0 };
        assert!(matches!(decode_one(&header.encode()), Err(WireError::TooLarge(_))));
        let header = Header { kind: 0x22, flags: 0, len: 100, timestamp: 0 };
        let mut buf = header.encode().to_vec();
        buf.extend_from_slice(&[0; 10]);
        assert!(matches!(decode_one(&buf), Err(WireError::Io(_))));
    }

    /// Random bytes must never panic the parser (a cheap stand-in for a
    /// fuzzer that runs on stable Rust).
    #[test]
    fn random_input_never_panics() {
        let mut seed: u64 = 0x9e3779b97f4a7c15;
        let mut next = || {
            seed ^= seed << 13;
            seed ^= seed >> 7;
            seed ^= seed << 17;
            seed
        };
        for _ in 0..20_000 {
            let len = (next() % 64) as usize;
            let mut buf: Vec<u8> = (0..HEADER_LEN + len).map(|_| next() as u8).collect();
            // Keep the length field honest half the time so payload parsers run.
            if next() % 2 == 0 {
                buf[2..6].copy_from_slice(&(len as u32).to_le_bytes());
                // Known kinds are more interesting.
                buf[0] = [0x01, 0x02, 0x03, 0x04, 0x10, 0x11, 0x20, 0x21, 0x22, 0x23, 0x24, 0x25, 0x26, 0x27, 0x28]
                    [(next() % 15) as usize];
            }
            let _ = decode_one(&buf);
        }
    }
}

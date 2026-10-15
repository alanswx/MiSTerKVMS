//! Encrypted mkvm, for viewers connecting from the internet (docs/IPHONE.md
//! §3). A paired device holds a 32-byte key the server issued it; the key
//! never crosses the network again.
//!
//! 1. The viewer opens with a clear prelude: [`MAGIC`], [`VERSION`], its
//!    16-byte key id. (A plain connection starts with a message kind, never
//!    `M`, so the server tells them apart from the first byte.)
//! 2. The server looks the key up and both run a Noise
//!    `NNpsk0_25519_ChaChaPoly_BLAKE2s` handshake keyed with it: one round
//!    trip; only a holder of the key completes it, and each connection gets
//!    fresh session keys (forward secrecy).
//! 3. After that the usual mkvm messages travel inside Noise transport
//!    messages: a u16 length, then the ciphertext (at most 64 KiB, so a
//!    video frame spans several).
//!
//! [`SecureReader`] and [`SecureWriter`] wrap the two halves of the socket,
//! so `read_message`/`write_message` work on them unchanged and the reading
//! and writing threads stay independent.

use std::io::{self, Read, Write};
use std::sync::Arc;

use snow::{Builder, StatelessTransportState};

/// First bytes of an encrypted connection.
pub const MAGIC: &[u8; 4] = b"MKVS";
/// Version of this prelude and handshake.
pub const VERSION: u8 = 1;
pub const KEY_LEN: usize = 32;
pub const KEY_ID_LEN: usize = 16;

const NOISE: &str = "Noise_NNpsk0_25519_ChaChaPoly_BLAKE2s";
const MAX_MESSAGE: usize = 65535;
const TAG_LEN: usize = 16;
const MAX_CHUNK: usize = MAX_MESSAGE - TAG_LEN;

/// A paired device's key, as the server issued it.
#[derive(Clone, PartialEq, Eq)]
pub struct DeviceKey {
    pub id: [u8; KEY_ID_LEN],
    pub key: [u8; KEY_LEN],
}

impl std::fmt::Debug for DeviceKey {
    // Never print the key itself.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "DeviceKey({})", self.id_hex())
    }
}

impl DeviceKey {
    /// A new random key and id (from the OS).
    pub fn generate() -> io::Result<Self> {
        let mut id = [0u8; KEY_ID_LEN];
        let mut key = [0u8; KEY_LEN];
        getrandom::fill(&mut id).map_err(io::Error::other)?;
        getrandom::fill(&mut key).map_err(io::Error::other)?;
        Ok(Self { id, key })
    }

    pub fn id_hex(&self) -> String {
        hex(&self.id)
    }

    pub fn key_hex(&self) -> String {
        hex(&self.key)
    }

    pub fn from_hex(id: &str, key: &str) -> Option<Self> {
        Some(Self { id: unhex(id)?, key: unhex(key)? })
    }
}

pub fn hex(bytes: &[u8]) -> String {
    bytes.iter().map(|b| format!("{b:02x}")).collect()
}

pub fn unhex<const N: usize>(s: &str) -> Option<[u8; N]> {
    let s = s.trim();
    if s.len() != N * 2 || !s.is_ascii() {
        return None;
    }
    let mut out = [0u8; N];
    for (i, o) in out.iter_mut().enumerate() {
        *o = u8::from_str_radix(&s[i * 2..i * 2 + 2], 16).ok()?;
    }
    Some(out)
}

#[derive(Debug)]
pub enum SecureError {
    Io(io::Error),
    /// Not an encrypted mkvm connection, or a version we don't speak.
    Prelude(String),
    /// The handshake failed: almost always a key the other side doesn't
    /// have (revoked, or paired with another server).
    Handshake(String),
}

impl std::fmt::Display for SecureError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SecureError::Io(e) => write!(f, "{e}"),
            SecureError::Prelude(e) => write!(f, "not an encrypted mkvm connection: {e}"),
            SecureError::Handshake(e) => write!(f, "encryption handshake failed: {e}"),
        }
    }
}

impl std::error::Error for SecureError {}

impl From<io::Error> for SecureError {
    fn from(e: io::Error) -> Self {
        SecureError::Io(e)
    }
}

/// The agreed session: shared by the reading and the writing half.
pub type Session = Arc<StatelessTransportState>;

fn noise_err(e: snow::Error) -> SecureError {
    SecureError::Handshake(e.to_string())
}

fn write_frame(w: &mut impl Write, data: &[u8]) -> io::Result<()> {
    let mut buf = Vec::with_capacity(2 + data.len());
    buf.extend_from_slice(&(data.len() as u16).to_be_bytes());
    buf.extend_from_slice(data);
    w.write_all(&buf)
}

fn read_frame(r: &mut impl Read, buf: &mut Vec<u8>) -> io::Result<()> {
    let mut len = [0u8; 2];
    r.read_exact(&mut len)?;
    buf.resize(u16::from_be_bytes(len) as usize, 0);
    r.read_exact(buf)
}

/// Viewer side: send the prelude and run the handshake on a fresh
/// connection. Then wrap the socket's halves in [`SecureReader`] and
/// [`SecureWriter`] with the returned session.
pub fn connect(sock: &mut (impl Read + Write), key: &DeviceKey) -> Result<Session, SecureError> {
    let mut prelude = Vec::with_capacity(5 + KEY_ID_LEN);
    prelude.extend_from_slice(MAGIC);
    prelude.push(VERSION);
    prelude.extend_from_slice(&key.id);
    sock.write_all(&prelude)?;
    let mut noise = Builder::new(NOISE.parse().expect("valid noise pattern"))
        .psk(0, &key.key)
        .map_err(noise_err)?
        .build_initiator()
        .map_err(noise_err)?;
    let mut buf = vec![0u8; MAX_MESSAGE];
    let n = noise.write_message(&[], &mut buf).map_err(noise_err)?;
    write_frame(sock, &buf[..n])?;
    let mut reply = Vec::new();
    read_frame(sock, &mut reply).map_err(|e| match e.kind() {
        // The server hangs up on an unknown or revoked key (a reset when our
        // handshake was still unread).
        io::ErrorKind::UnexpectedEof | io::ErrorKind::ConnectionReset | io::ErrorKind::ConnectionAborted => SecureError::Handshake(
            "the capture server doesn't accept this device's key any more (revoked?): pair again on the home network".into(),
        ),
        _ => SecureError::Io(e),
    })?;
    noise.read_message(&reply, &mut buf).map_err(noise_err)?;
    Ok(Arc::new(noise.into_stateless_transport_mode().map_err(noise_err)?))
}

/// Server side, after the first 4 bytes were found to be [`MAGIC`]: read
/// the rest of the prelude and return the key id the viewer claims.
pub fn read_prelude(sock: &mut impl Read) -> Result<[u8; KEY_ID_LEN], SecureError> {
    let mut version = [0u8; 1];
    sock.read_exact(&mut version)?;
    if version[0] != VERSION {
        return Err(SecureError::Prelude(format!("version {}", version[0])));
    }
    let mut id = [0u8; KEY_ID_LEN];
    sock.read_exact(&mut id)?;
    Ok(id)
}

/// Server side: run the handshake with the key looked up from the id.
pub fn accept(sock: &mut (impl Read + Write), key: &[u8; KEY_LEN]) -> Result<Session, SecureError> {
    let mut noise = Builder::new(NOISE.parse().expect("valid noise pattern"))
        .psk(0, key)
        .map_err(noise_err)?
        .build_responder()
        .map_err(noise_err)?;
    let mut first = Vec::new();
    read_frame(sock, &mut first)?;
    let mut buf = vec![0u8; MAX_MESSAGE];
    noise.read_message(&first, &mut buf).map_err(noise_err)?;
    let n = noise.write_message(&[], &mut buf).map_err(noise_err)?;
    write_frame(sock, &buf[..n])?;
    Ok(Arc::new(noise.into_stateless_transport_mode().map_err(noise_err)?))
}

/// The reading half: decrypts Noise messages into a byte stream.
pub struct SecureReader<R> {
    inner: R,
    session: Session,
    nonce: u64,
    cipher: Vec<u8>,
    plain: Vec<u8>,
    pos: usize,
}

impl<R: Read> SecureReader<R> {
    pub fn new(inner: R, session: Session) -> Self {
        Self { inner, session, nonce: 0, cipher: Vec::new(), plain: Vec::new(), pos: 0 }
    }
}

impl<R: Read> Read for SecureReader<R> {
    fn read(&mut self, out: &mut [u8]) -> io::Result<usize> {
        while self.pos >= self.plain.len() {
            read_frame(&mut self.inner, &mut self.cipher)?;
            self.plain.resize(MAX_MESSAGE, 0);
            let n = self
                .session
                .read_message(self.nonce, &self.cipher, &mut self.plain)
                .map_err(|e| io::Error::new(io::ErrorKind::InvalidData, format!("decrypt: {e}")))?;
            self.nonce += 1;
            self.plain.truncate(n);
            self.pos = 0;
        }
        let n = out.len().min(self.plain.len() - self.pos);
        out[..n].copy_from_slice(&self.plain[self.pos..self.pos + n]);
        self.pos += n;
        Ok(n)
    }
}

/// The writing half: every write goes out at once, encrypted (as plain TCP
/// writes do; wrap it in a `BufWriter` to batch).
pub struct SecureWriter<W> {
    inner: W,
    session: Session,
    nonce: u64,
    buf: Vec<u8>,
}

impl<W: Write> SecureWriter<W> {
    pub fn new(inner: W, session: Session) -> Self {
        Self { inner, session, nonce: 0, buf: vec![0u8; MAX_MESSAGE] }
    }

    pub fn get_ref(&self) -> &W {
        &self.inner
    }
}

impl<W: Write> Write for SecureWriter<W> {
    fn write(&mut self, data: &[u8]) -> io::Result<usize> {
        if data.is_empty() {
            return Ok(0);
        }
        let chunk = &data[..data.len().min(MAX_CHUNK)];
        let n = self
            .session
            .write_message(self.nonce, chunk, &mut self.buf)
            .map_err(|e| io::Error::other(format!("encrypt: {e}")))?;
        self.nonce += 1;
        write_frame(&mut self.inner, &self.buf[..n])?;
        Ok(chunk.len())
    }

    fn flush(&mut self) -> io::Result<()> {
        self.inner.flush()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::message::{Codec, Hello, Message, Video};
    use crate::wire::{read_message, write_message};
    use std::net::{TcpListener, TcpStream};
    use std::thread;

    fn pair() -> (TcpStream, TcpStream) {
        let l = TcpListener::bind("127.0.0.1:0").unwrap();
        let a = TcpStream::connect(l.local_addr().unwrap()).unwrap();
        let (b, _) = l.accept().unwrap();
        (a, b)
    }

    #[test]
    fn hex_round_trip() {
        let k = DeviceKey::generate().unwrap();
        assert_eq!(DeviceKey::from_hex(&k.id_hex(), &k.key_hex()), Some(k.clone()));
        assert_eq!(k.id_hex().len(), 32);
        assert!(!format!("{k:?}").contains(&k.key_hex()));
        assert_eq!(unhex::<2>("zz00"), None);
    }

    #[test]
    fn messages_pass_through_encrypted() {
        let key = DeviceKey::generate().unwrap();
        let (mut client, mut server) = pair();
        let server_key = key.key;
        let expected_id = key.id;
        let srv = thread::spawn(move || {
            let mut magic = [0u8; 4];
            server.read_exact(&mut magic).unwrap();
            assert_eq!(&magic, MAGIC);
            assert_eq!(read_prelude(&mut server).unwrap(), expected_id);
            let session = accept(&mut server, &server_key).unwrap();
            let mut r = SecureReader::new(server.try_clone().unwrap(), session.clone());
            let mut w = SecureWriter::new(server, session);
            // Echo two messages back.
            for _ in 0..2 {
                let (m, h) = read_message(&mut r).unwrap();
                write_message(&mut w, &m, h.timestamp).unwrap();
            }
        });
        let session = connect(&mut client, &key).unwrap();
        let mut r = SecureReader::new(client.try_clone().unwrap(), session.clone());
        let mut w = SecureWriter::new(client, session);
        let hello = Message::Hello(Hello {
            protocol: 1,
            stream: "MiSTer-SS1".into(),
            viewer: "iPhone".into(),
            platform: "ios".into(),
            codecs: vec![Codec::Jpeg],
        });
        // Larger than one Noise message: split and reassembled.
        let data: Vec<u8> = (0..300_000u32).map(|i| (i * 7) as u8).collect();
        let video = Message::Video(Video { codec: Codec::Jpeg, keyframe: true, width: 1280, height: 720, data });
        write_message(&mut w, &hello, 1).unwrap();
        write_message(&mut w, &video, 2).unwrap();
        assert_eq!(read_message(&mut r).unwrap().0, hello);
        assert_eq!(read_message(&mut r).unwrap().0, video);
        srv.join().unwrap();
    }

    #[test]
    fn a_wrong_key_fails_the_handshake() {
        let device = DeviceKey::generate().unwrap();
        let other = DeviceKey::generate().unwrap();
        let (mut client, mut server) = pair();
        let srv = thread::spawn(move || {
            let mut magic = [0u8; 4];
            server.read_exact(&mut magic).unwrap();
            read_prelude(&mut server).unwrap();
            // The server has a different key for this id.
            assert!(matches!(accept(&mut server, &other.key), Err(SecureError::Handshake(_))));
        });
        assert!(connect(&mut client, &device).is_err());
        srv.join().unwrap();
    }
}

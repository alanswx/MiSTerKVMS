//! Stream addresses: `mkvm://host[:port]/stream`. Stream names may contain
//! spaces and other characters; they are percent-encoded in the address.

use std::fmt;

use crate::DEFAULT_PORT;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StreamAddress {
    pub host: String,
    pub port: u16,
    pub stream: String,
}

impl StreamAddress {
    /// Parse `mkvm://host[:port]/stream`. The scheme may be left out
    /// (`host/stream`), as people type it. IPv6 hosts go in brackets.
    pub fn parse(s: &str) -> Result<Self, String> {
        let s = s.trim();
        let rest = s.strip_prefix("mkvm://").unwrap_or(s);
        let (authority, path) = rest.split_once('/').ok_or_else(|| format!("'{s}': no stream name (mkvm://host/stream)"))?;
        let stream = percent_decode(path.trim_end_matches('/'))?;
        if stream.is_empty() {
            return Err(format!("'{s}': no stream name (mkvm://host/stream)"));
        }
        let (host, port) = if let Some(v6) = authority.strip_prefix('[') {
            let (host, after) = v6.split_once(']').ok_or_else(|| format!("'{s}': unclosed '['"))?;
            let port = match after.strip_prefix(':') {
                Some(p) => p.parse().map_err(|_| format!("'{s}': bad port"))?,
                None if after.is_empty() => DEFAULT_PORT,
                None => return Err(format!("'{s}': bad address")),
            };
            (host.to_string(), port)
        } else {
            match authority.rsplit_once(':') {
                Some((h, p)) => (h.to_string(), p.parse().map_err(|_| format!("'{s}': bad port"))?),
                None => (authority.to_string(), DEFAULT_PORT),
            }
        };
        if host.is_empty() {
            return Err(format!("'{s}': no host"));
        }
        Ok(Self { host, port, stream })
    }

    /// `host:port` for connecting.
    pub fn socket(&self) -> String {
        if self.host.contains(':') {
            format!("[{}]:{}", self.host, self.port)
        } else {
            format!("{}:{}", self.host, self.port)
        }
    }
}

impl fmt::Display for StreamAddress {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let host = if self.host.contains(':') { format!("[{}]", self.host) } else { self.host.clone() };
        if self.port == DEFAULT_PORT {
            write!(f, "mkvm://{host}/{}", percent_encode(&self.stream))
        } else {
            write!(f, "mkvm://{host}:{}/{}", self.port, percent_encode(&self.stream))
        }
    }
}

fn percent_encode(s: &str) -> String {
    let mut out = String::new();
    for b in s.bytes() {
        if b.is_ascii_alphanumeric() || b"-_.~".contains(&b) {
            out.push(b as char);
        } else {
            out.push_str(&format!("%{b:02X}"));
        }
    }
    out
}

fn percent_decode(s: &str) -> Result<String, String> {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' {
            let hex = s.get(i + 1..i + 3).ok_or("bad % escape")?;
            out.push(u8::from_str_radix(hex, 16).map_err(|_| "bad % escape")?);
            i += 3;
        } else {
            out.push(bytes[i]);
            i += 1;
        }
    }
    String::from_utf8(out).map_err(|_| "stream name isn't UTF-8".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_common_forms() {
        let a = StreamAddress::parse("mkvm://capture-pc/MiSTer-SS1").unwrap();
        assert_eq!(a, StreamAddress { host: "capture-pc".into(), port: 5970, stream: "MiSTer-SS1".into() });
        let a = StreamAddress::parse("192.168.1.20:6000/MiSTer%20HP").unwrap();
        assert_eq!(a, StreamAddress { host: "192.168.1.20".into(), port: 6000, stream: "MiSTer HP".into() });
        let a = StreamAddress::parse("mkvm://[fe80::1]:5970/A").unwrap();
        assert_eq!(a.host, "fe80::1");
        assert_eq!(a.socket(), "[fe80::1]:5970");
    }

    #[test]
    fn display_round_trips() {
        for s in ["mkvm://pc/MiSTer%20SS1", "mkvm://pc:6000/x", "mkvm://[::1]/A%2FB"] {
            assert_eq!(StreamAddress::parse(s).unwrap().to_string(), s);
        }
    }

    #[test]
    fn rejects_missing_parts() {
        assert!(StreamAddress::parse("mkvm://pc").is_err());
        assert!(StreamAddress::parse("mkvm://pc/").is_err());
        assert!(StreamAddress::parse("mkvm:///x").is_err());
        assert!(StreamAddress::parse("pc:notaport/x").is_err());
    }
}

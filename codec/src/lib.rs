//! Pictures: JPEG decode/encode, raw format conversion and scaling. Shared
//! by `misterkvms-capture` (encode, scale) and the apps (decode), so both
//! ends treat pictures the same way. Pure Rust; H.264 through the OS's
//! encoders and decoders comes later (docs/CAPTURE-SERVER.md §5.2).
//!
//! Pictures in memory are BGRA (4 bytes per pixel, top row first), the
//! format the renderer and recorders take.

pub mod convert;
pub mod jpeg;
pub mod scale;

use std::fmt;

#[derive(Debug, Clone, PartialEq)]
pub struct CodecError(pub String);

impl fmt::Display for CodecError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for CodecError {}

/// A BGRA picture.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Bgra {
    pub width: u32,
    pub height: u32,
    /// Bytes per row (at least `width * 4`).
    pub stride: u32,
    pub data: Vec<u8>,
}

impl Bgra {
    pub fn new(width: u32, height: u32) -> Self {
        Self { width, height, stride: width * 4, data: vec![0; (width * height * 4) as usize] }
    }

    pub fn row(&self, y: u32) -> &[u8] {
        let start = (y * self.stride) as usize;
        &self.data[start..start + (self.width * 4) as usize]
    }
}

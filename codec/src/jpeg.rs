//! JPEG: decode (zune-jpeg, SIMD) and encode (jpeg-encoder, SIMD).

use jpeg_encoder::{ColorType, Encoder, SamplingFactor};
use zune_core::bytestream::ZCursor;
use zune_core::colorspace::ColorSpace;
use zune_core::options::DecoderOptions;
use zune_jpeg::JpegDecoder;

use crate::{Bgra, CodecError};

/// Decode a JPEG into `out`, reusing its buffer.
pub fn decode(data: &[u8], out: &mut Bgra) -> Result<(), CodecError> {
    let options = DecoderOptions::default()
        .jpeg_set_out_colorspace(ColorSpace::BGRA)
        // Capture sticks sometimes end a frame early; show what arrived.
        .set_strict_mode(false);
    let mut decoder = JpegDecoder::new_with_options(ZCursor::new(data), options);
    decoder.decode_headers().map_err(|e| CodecError(format!("jpeg: {e:?}")))?;
    let (w, h) = decoder.dimensions().ok_or_else(|| CodecError("jpeg: no size".into()))?;
    let len = decoder.output_buffer_size().ok_or_else(|| CodecError("jpeg: no size".into()))?;
    out.data.resize(len, 0);
    decoder.decode_into(&mut out.data).map_err(|e| CodecError(format!("jpeg: {e:?}")))?;
    out.width = w as u32;
    out.height = h as u32;
    out.stride = w as u32 * 4;
    Ok(())
}

/// Read a JPEG's size without decoding it.
pub fn size(data: &[u8]) -> Option<(u32, u32)> {
    let mut decoder = JpegDecoder::new(ZCursor::new(data));
    decoder.decode_headers().ok()?;
    decoder.dimensions().map(|(w, h)| (w as u32, h as u32))
}

/// Encode BGRA as JPEG into `out` (cleared first). 4:4:4 chroma: coloured
/// text and pixel art stay sharp, which is most of what MiSTer desktops
/// show. `quality` 1–100; 85 is a good LAN default.
pub fn encode(picture: &Bgra, quality: u8, out: &mut Vec<u8>) -> Result<(), CodecError> {
    if picture.width > u16::MAX as u32 || picture.height > u16::MAX as u32 {
        return Err(CodecError("jpeg: picture too large".into()));
    }
    out.clear();
    let tight;
    let pixels = if picture.stride == picture.width * 4 {
        &picture.data[..(picture.width * picture.height * 4) as usize]
    } else {
        tight = (0..picture.height).flat_map(|y| picture.row(y).iter().copied()).collect::<Vec<u8>>();
        &tight[..]
    };
    let mut encoder = Encoder::new(out, quality);
    encoder.set_sampling_factor(SamplingFactor::R_4_4_4);
    encoder
        .encode(pixels, picture.width as u16, picture.height as u16, ColorType::Bgra)
        .map_err(|e| CodecError(format!("jpeg: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pattern(w: u32, h: u32) -> Bgra {
        let mut p = Bgra::new(w, h);
        for y in 0..h {
            for x in 0..w {
                let i = ((y * w + x) * 4) as usize;
                // Flat blocks: JPEG keeps them almost exactly.
                let (b, g, r) = if (x / 16 + y / 16) % 2 == 0 { (200, 40, 10) } else { (10, 220, 90) };
                p.data[i..i + 4].copy_from_slice(&[b, g, r, 255]);
            }
        }
        p
    }

    #[test]
    fn round_trip_keeps_size_and_colours() {
        let src = pattern(64, 48);
        let mut jpeg = Vec::new();
        encode(&src, 95, &mut jpeg).unwrap();
        assert_eq!(size(&jpeg), Some((64, 48)));
        let mut back = Bgra::default();
        decode(&jpeg, &mut back).unwrap();
        assert_eq!((back.width, back.height, back.stride), (64, 48, 256));
        // Centre of the first block.
        let i = ((8 * 64 + 8) * 4) as usize;
        let px = &back.data[i..i + 3];
        for (got, want) in px.iter().zip([200u8, 40, 10]) {
            assert!((*got as i32 - want as i32).abs() <= 6, "{px:?}");
        }
    }

    #[test]
    fn encode_honours_stride() {
        let mut src = pattern(10, 4);
        // Re-lay with 8 bytes of padding per row.
        let mut padded = Vec::new();
        for y in 0..4 {
            padded.extend_from_slice(src.row(y));
            padded.extend_from_slice(&[0; 8]);
        }
        src.data = padded;
        src.stride = 48;
        let mut jpeg = Vec::new();
        encode(&src, 90, &mut jpeg).unwrap();
        assert_eq!(size(&jpeg), Some((10, 4)));
    }

    #[test]
    fn garbage_is_an_error_not_a_panic() {
        let mut out = Bgra::default();
        assert!(decode(&[0xff, 0xd8, 0xff, 0x00, 1, 2, 3], &mut out).is_err());
        assert!(decode(&[], &mut out).is_err());
    }
}

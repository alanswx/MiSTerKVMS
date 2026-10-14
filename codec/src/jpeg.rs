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

/// A raw capture frame as the card sent it (BT.709, limited range).
#[derive(Clone, Copy)]
pub enum Yuv<'a> {
    /// `stride`-byte Y rows, then interleaved UV rows at half height.
    Nv12 { data: &'a [u8], width: u32, height: u32, stride: u32 },
    /// Packed Y0 U Y1 V, `stride` bytes per row.
    Yuy2 { data: &'a [u8], width: u32, height: u32, stride: u32 },
}

/// BT.709 limited range → JPEG's BT.601 full range, 16.16 fixed point:
/// `Y = ky·(Y−16) + yb·(Cb−128) + yr·(Cr−128)`, `Cb = 128 + bb·(Cb−128) +
/// br·(Cr−128)`, `Cr = 128 + rb·(Cb−128) + rr·(Cr−128)` (the same colours
/// as decoding to RGB and encoding that, without the two conversions).
struct Matrix {
    ky: i32,
    yb: i32,
    yr: i32,
    bb: i32,
    br: i32,
    rb: i32,
    rr: i32,
}

impl Matrix {
    fn new() -> Self {
        // RGB from 709: R = y + 1.5748 pr, G = y − 0.187324 pb − 0.468124 pr, B = y + 1.8556 pb.
        let (r_pr, g_pb, g_pr, b_pb) = (1.5748, -0.187324, -0.468124, 1.8556);
        // JFIF from RGB.
        let (yr_, yg, yb_) = (0.299, 0.587, 0.114);
        let (cbr, cbg, cbb) = (-0.168736, -0.331264, 0.5);
        let (crr, crg, crb) = (0.5, -0.418688, -0.081312);
        let chroma = 255.0 / 224.0;
        let fx = |v: f64| (v * 65536.0).round() as i32;
        Self {
            ky: fx(255.0 / 219.0),
            yb: fx(chroma * (yg * g_pb + yb_ * b_pb)),
            yr: fx(chroma * (yr_ * r_pr + yg * g_pr)),
            bb: fx(chroma * (cbg * g_pb + cbb * b_pb)),
            br: fx(chroma * (cbr * r_pr + cbg * g_pr)),
            rb: fx(chroma * (crg * g_pb + crb * b_pb)),
            rr: fx(chroma * (crr * r_pr + crg * g_pr)),
        }
    }

    /// (Y offset from chroma, Cb, Cr) for one chroma sample.
    #[inline]
    fn chroma(&self, u: u8, v: u8) -> (i32, u8, u8) {
        let (d, e) = (u as i32 - 128, v as i32 - 128);
        let cb = (128 << 16) + self.bb * d + self.br * e + 32768;
        let cr = (128 << 16) + self.rb * d + self.rr * e + 32768;
        (self.yb * d + self.yr * e, (cb >> 16).clamp(0, 255) as u8, (cr >> 16).clamp(0, 255) as u8)
    }

    #[inline]
    fn luma(&self, y: u8, offset: i32) -> u8 {
        ((self.ky * (y as i32 - 16) + offset + 32768) >> 16).clamp(0, 255) as u8
    }
}

struct YuvImage<'a> {
    frame: Yuv<'a>,
    m: Matrix,
}

impl jpeg_encoder::ImageBuffer for YuvImage<'_> {
    fn get_jpeg_color_type(&self) -> jpeg_encoder::JpegColorType {
        jpeg_encoder::JpegColorType::Ycbcr
    }

    fn width(&self) -> u16 {
        match self.frame {
            Yuv::Nv12 { width, .. } | Yuv::Yuy2 { width, .. } => width as u16,
        }
    }

    fn height(&self) -> u16 {
        match self.frame {
            Yuv::Nv12 { height, .. } | Yuv::Yuy2 { height, .. } => height as u16,
        }
    }

    fn fill_buffers(&self, row: u16, buffers: &mut [Vec<u8>; 4]) {
        let row = row as usize;
        let [ys, cbs, crs, _] = buffers;
        // Pairs of pixels share their chroma in both layouts.
        let mut pair = |y0: u8, y1: Option<u8>, u: u8, v: u8| {
            let (off, cb, cr) = self.m.chroma(u, v);
            ys.push(self.m.luma(y0, off));
            cbs.push(cb);
            crs.push(cr);
            if let Some(y1) = y1 {
                ys.push(self.m.luma(y1, off));
                cbs.push(cb);
                crs.push(cr);
            }
        };
        match self.frame {
            Yuv::Nv12 { data, width, height, stride } => {
                let (w, h, s) = (width as usize, height as usize, stride as usize);
                let y_row = &data[row * s..row * s + w];
                let uv_row = &data[s * h + (row / 2) * s..][..w.div_ceil(2) * 2];
                for x in (0..w).step_by(2) {
                    pair(y_row[x], y_row.get(x + 1).copied(), uv_row[x], uv_row[x + 1]);
                }
            }
            Yuv::Yuy2 { data, width, stride, .. } => {
                let (w, s) = (width as usize, stride as usize);
                let p = &data[row * s..row * s + w.div_ceil(2) * 4];
                for x in (0..w).step_by(2) {
                    let q = &p[x * 2..x * 2 + 4];
                    pair(q[0], (x + 1 < w).then_some(q[2]), q[1], q[3]);
                }
            }
        }
    }
}

/// Encode a raw frame as JPEG straight from the card's YUV, keeping its
/// chroma subsampling (4:2:0 for NV12, 4:2:2 for YUY2): finer chroma than
/// the card sent adds bytes, not detail. Much cheaper than going through
/// BGRA and 4:4:4.
pub fn encode_yuv(frame: Yuv<'_>, quality: u8, out: &mut Vec<u8>) -> Result<(), CodecError> {
    let (width, height, sampling) = match frame {
        Yuv::Nv12 { width, height, .. } => (width, height, SamplingFactor::R_4_2_0),
        Yuv::Yuy2 { width, height, .. } => (width, height, SamplingFactor::R_4_2_2),
    };
    if width > u16::MAX as u32 || height > u16::MAX as u32 {
        return Err(CodecError("jpeg: picture too large".into()));
    }
    out.clear();
    let mut encoder = Encoder::new(out, quality);
    encoder.set_sampling_factor(sampling);
    encoder.encode_image(YuvImage { frame, m: Matrix::new() }).map_err(|e| CodecError(format!("jpeg: {e}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The direct YUV encode shows the same colours as converting to BGRA
    /// first and encoding that.
    #[test]
    fn yuv_encode_matches_the_bgra_path() {
        let (w, h) = (64u32, 32u32);
        let mut nv12 = Vec::new();
        for y in 0..h {
            for x in 0..w {
                nv12.push((16 + (x * 3 + y) % 220) as u8);
            }
        }
        for y in 0..h / 2 {
            for x in 0..w / 2 {
                // Flat 8x8 chroma blocks: JPEG keeps them nearly exactly.
                let (u, v): (u8, u8) = if (x / 4 + y / 4) % 2 == 0 { (90, 200) } else { (180, 60) };
                nv12.extend_from_slice(&[u, v]);
            }
        }
        let mut via_bgra = Bgra::default();
        crate::convert::nv12_to_bgra(&nv12, w, h, w, &mut via_bgra);
        let mut jpeg = Vec::new();
        encode_yuv(Yuv::Nv12 { data: &nv12, width: w, height: h, stride: w }, 95, &mut jpeg).unwrap();
        let mut back = Bgra::default();
        decode(&jpeg, &mut back).unwrap();
        assert_eq!((back.width, back.height), (w, h));
        let mut worst = 0;
        for y in 2..h - 2 {
            for x in 2..w - 2 {
                // Away from chroma block edges, where 4:2:0 blurs by design.
                if x % 8 < 2 || x % 8 > 5 || y % 8 < 2 || y % 8 > 5 {
                    continue;
                }
                let i = ((y * w + x) * 4) as usize;
                for c in 0..3 {
                    worst = worst.max((back.data[i + c] as i32 - via_bgra.data[i + c] as i32).abs());
                }
            }
        }
        assert!(worst <= 10, "worst channel difference {worst}");
    }

    #[test]
    fn yuy2_encodes() {
        let (w, h) = (16u32, 8u32);
        let yuy2: Vec<u8> = (0..w * h / 2).flat_map(|i| [100 + (i % 50) as u8, 110, 120, 150]).collect();
        let mut jpeg = Vec::new();
        encode_yuv(Yuv::Yuy2 { data: &yuy2, width: w, height: h, stride: w * 2 }, 90, &mut jpeg).unwrap();
        assert_eq!(size(&jpeg), Some((w, h)));
    }

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

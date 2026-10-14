//! Raw capture formats to BGRA (BT.709 limited range, what HDMI capture
//! cards deliver for HD; close enough for SD too).

use crate::Bgra;

#[inline]
fn yuv_to_bgr(y: i32, u: i32, v: i32) -> [u8; 3] {
    // Fixed point, 16.16: BT.709, limited range (16–235 / 16–240).
    let c = (y - 16).max(0) * 76309;
    let d = u - 128;
    let e = v - 128;
    let r = (c + 117489 * e + 32768) >> 16;
    let g = (c - 13975 * d - 34925 * e + 32768) >> 16;
    let b = (c + 138438 * d + 32768) >> 16;
    [b.clamp(0, 255) as u8, g.clamp(0, 255) as u8, r.clamp(0, 255) as u8]
}

/// NV12: `stride`-byte rows of Y (`height` rows), then interleaved UV rows
/// at half height.
pub fn nv12_to_bgra(src: &[u8], width: u32, height: u32, stride: u32, out: &mut Bgra) {
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    out.width = width;
    out.height = height;
    out.stride = width * 4;
    out.data.resize(w * h * 4, 0);
    let uv_plane = &src[s * h..];
    for y in 0..h {
        let y_row = &src[y * s..y * s + w];
        let uv_row = &uv_plane[(y / 2) * s..(y / 2) * s + w];
        let dst = &mut out.data[y * w * 4..(y + 1) * w * 4];
        for x in 0..w {
            let (u, v) = (uv_row[x & !1] as i32, uv_row[(x & !1) + 1] as i32);
            let [b, g, r] = yuv_to_bgr(y_row[x] as i32, u, v);
            dst[x * 4..x * 4 + 4].copy_from_slice(&[b, g, r, 255]);
        }
    }
}

/// YUY2: packed Y0 U Y1 V, `stride` bytes per row.
pub fn yuy2_to_bgra(src: &[u8], width: u32, height: u32, stride: u32, out: &mut Bgra) {
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    out.width = width;
    out.height = height;
    out.stride = width * 4;
    out.data.resize(w * h * 4, 0);
    for y in 0..h {
        let row = &src[y * s..y * s + w * 2];
        let dst = &mut out.data[y * w * 4..(y + 1) * w * 4];
        for pair in 0..w / 2 {
            let p = &row[pair * 4..pair * 4 + 4];
            let (u, v) = (p[1] as i32, p[3] as i32);
            let [b0, g0, r0] = yuv_to_bgr(p[0] as i32, u, v);
            let [b1, g1, r1] = yuv_to_bgr(p[2] as i32, u, v);
            dst[pair * 8..pair * 8 + 8].copy_from_slice(&[b0, g0, r0, 255, b1, g1, r1, 255]);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn limited_range_black_white_and_red() {
        assert_eq!(yuv_to_bgr(16, 128, 128), [0, 0, 0]);
        assert_eq!(yuv_to_bgr(235, 128, 128), [255, 255, 255]);
        // BT.709 red: Y 63, Cb 102, Cr 240.
        let [b, g, r] = yuv_to_bgr(63, 102, 240);
        assert!(r > 250 && g < 8 && b < 8, "{r} {g} {b}");
    }

    #[test]
    fn nv12_and_yuy2_agree() {
        let (w, h) = (4u32, 2u32);
        // Y ramp, one grey-ish chroma pair per 2x2 block.
        let ys = [16u8, 80, 150, 235, 30, 90, 160, 220];
        let mut nv12 = ys.to_vec();
        nv12.extend_from_slice(&[100, 150, 130, 120]);
        let mut a = Bgra::default();
        nv12_to_bgra(&nv12, w, h, w, &mut a);
        let mut yuy2 = Vec::new();
        for row in 0..2 {
            for pair in 0..2 {
                let (u, v) = if pair == 0 { (100, 150) } else { (130, 120) };
                yuy2.extend_from_slice(&[ys[row * 4 + pair * 2], u, ys[row * 4 + pair * 2 + 1], v]);
            }
        }
        let mut b = Bgra::default();
        yuy2_to_bgra(&yuy2, w, h, w * 2, &mut b);
        assert_eq!(a, b);
    }
}

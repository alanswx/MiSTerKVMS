//! Downscaling for viewers who ask for less than native
//! (docs/CAPTURE-SERVER.md §6). Integer ratios use an exact box filter, so
//! 4K → 1080p keeps pixel art crisp; other ratios average the source area
//! each output pixel covers.

use crate::Bgra;

/// The sizes a stream offers for a native size: native, then 1080p and 720p
/// (fitted, aspect kept) and native halved, thirded or quartered (exact
/// box filter) while at least 960 wide. Largest first, no duplicates, never larger than native.
pub fn offered_sizes(width: u32, height: u32) -> Vec<(u32, u32)> {
    if width == 0 || height == 0 {
        return Vec::new();
    }
    let mut sizes = vec![(width, height)];
    for (bw, bh) in [(1920u32, 1080u32), (1280, 720)] {
        sizes.push(fit(width, height, bw, bh));
    }
    for n in 2..=4 {
        if width % n == 0 && height % n == 0 && width / n >= 960 {
            sizes.push((width / n, height / n));
        }
    }
    sizes.retain(|&(w, h)| w <= width && h <= height && w > 0 && h > 0);
    sizes.sort_by(|a, b| (b.0 * b.1).cmp(&(a.0 * a.1)));
    sizes.dedup();
    sizes
}

/// Largest even size with the source's aspect that fits within max.
pub fn fit(width: u32, height: u32, max_w: u32, max_h: u32) -> (u32, u32) {
    if width <= max_w && height <= max_h {
        return (width, height);
    }
    let s = (max_w as f64 / width as f64).min(max_h as f64 / height as f64);
    let w = ((width as f64 * s) as u32) & !1;
    let h = ((height as f64 * s) as u32) & !1;
    (w.max(2), h.max(2))
}

/// The offered size a viewer gets for its limits (None = native): the
/// largest within them, or within 10% above them, so a viewer showing
/// 1206x680 gets 1280x720 (scaled down on its side) rather than 960x540.
pub fn size_for(width: u32, height: u32, max_w: Option<u32>, max_h: Option<u32>) -> (u32, u32) {
    let (mw, mh) = (max_w.unwrap_or(u32::MAX), max_h.unwrap_or(u32::MAX));
    let slack = |m: u32| m.saturating_add(m / 10);
    offered_sizes(width, height)
        .into_iter()
        .find(|&(w, h)| w <= slack(mw) && h <= slack(mh))
        .unwrap_or_else(|| fit(width, height, mw.max(2), mh.max(2)))
}

/// Scale `src` to `width` x `height` into `out`.
pub fn scale(src: &Bgra, width: u32, height: u32, out: &mut Bgra) {
    out.width = width;
    out.height = height;
    out.stride = width * 4;
    out.data.resize((width * height * 4) as usize, 0);
    if width == src.width && height == src.height {
        for y in 0..height {
            out.data[(y * width * 4) as usize..((y + 1) * width * 4) as usize].copy_from_slice(src.row(y));
        }
    } else if src.width % width == 0 && src.height % height == 0 {
        box_filter(src, src.width / width, src.height / height, out);
    } else {
        area_filter(src, out);
    }
}

fn box_filter(src: &Bgra, fx: u32, fy: u32, out: &mut Bgra) {
    let n = fx * fy;
    for oy in 0..out.height {
        for ox in 0..out.width {
            let mut acc = [0u32; 4];
            for sy in oy * fy..(oy + 1) * fy {
                let row = src.row(sy);
                for sx in ox * fx..(ox + 1) * fx {
                    let p = &row[(sx * 4) as usize..(sx * 4 + 4) as usize];
                    for c in 0..4 {
                        acc[c] += p[c] as u32;
                    }
                }
            }
            let i = ((oy * out.width + ox) * 4) as usize;
            for c in 0..4 {
                out.data[i + c] = ((acc[c] + n / 2) / n) as u8;
            }
        }
    }
}

/// Each output pixel is the average of the source rectangle it covers,
/// with fractional weights at the edges. Separable: a horizontal pass into
/// a narrow intermediate picture, then a vertical one.
fn area_filter(src: &Bgra, out: &mut Bgra) {
    let xs = weights(src.width, out.width);
    let ys = weights(src.height, out.height);
    let ow = out.width as usize;
    // Horizontal: every source row to output width, 4 channels as f32.
    let mut mid = vec![0f32; ow * 4 * src.height as usize];
    for y in 0..src.height {
        let row = src.row(y);
        let dst = &mut mid[y as usize * ow * 4..(y as usize + 1) * ow * 4];
        for (ox, (first, ws)) in xs.iter().enumerate() {
            let mut acc = [0f32; 4];
            for (k, w) in ws.iter().enumerate() {
                let p = &row[(*first + k) * 4..(*first + k) * 4 + 4];
                for c in 0..4 {
                    acc[c] += p[c] as f32 * w;
                }
            }
            dst[ox * 4..ox * 4 + 4].copy_from_slice(&acc);
        }
    }
    // Vertical.
    let mut acc = vec![0f32; ow * 4];
    for (oy, (first, ws)) in ys.iter().enumerate() {
        acc.iter_mut().for_each(|a| *a = 0.0);
        for (k, w) in ws.iter().enumerate() {
            let src_row = &mid[(*first + k) * ow * 4..(*first + k + 1) * ow * 4];
            for (a, v) in acc.iter_mut().zip(src_row) {
                *a += v * w;
            }
        }
        let dst = &mut out.data[oy * ow * 4..(oy + 1) * ow * 4];
        for (d, a) in dst.iter_mut().zip(&acc) {
            *d = (a + 0.5).clamp(0.0, 255.0) as u8;
        }
    }
}

/// For each output index: the first source index it covers and the
/// normalised weight of each source index from there.
fn weights(src: u32, dst: u32) -> Vec<(usize, Vec<f32>)> {
    let scale = src as f64 / dst as f64;
    (0..dst)
        .map(|o| {
            let a = o as f64 * scale;
            let b = (a + scale).min(src as f64);
            let first = a.floor() as usize;
            let last = (b.ceil() as usize).min(src as usize).max(first + 1);
            let ws: Vec<f64> = (first..last).map(|i| (b.min(i as f64 + 1.0) - a.max(i as f64)).max(0.0)).collect();
            let total: f64 = ws.iter().sum::<f64>().max(1e-9);
            (first, ws.into_iter().map(|w| (w / total) as f32).collect())
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn offered_sizes_for_common_natives() {
        assert_eq!(offered_sizes(3840, 2160), vec![(3840, 2160), (1920, 1080), (1280, 720), (960, 540)]);
        assert_eq!(offered_sizes(1920, 1080), vec![(1920, 1080), (1280, 720), (960, 540)]);
        assert_eq!(offered_sizes(1024, 768), vec![(1024, 768), (960, 720)]);
        assert_eq!(offered_sizes(640, 480), vec![(640, 480)]);
    }

    #[test]
    fn size_for_picks_largest_within_limits() {
        assert_eq!(size_for(3840, 2160, None, None), (3840, 2160));
        assert_eq!(size_for(3840, 2160, Some(1920), Some(1080)), (1920, 1080));
        assert_eq!(size_for(1920, 1080, Some(1280), Some(720)), (1280, 720));
        assert_eq!(size_for(1920, 1080, Some(800), Some(600)), (800, 450));
        // A phone's video area: the 720p picture, a little larger than asked.
        assert_eq!(size_for(1920, 1080, Some(1206), Some(680)), (1280, 720));
        assert_eq!(size_for(1920, 1080, Some(1000), Some(600)), (960, 540));
    }

    #[test]
    fn integer_box_filter_is_exact() {
        let mut src = Bgra::new(4, 2);
        // Two 2x2 blocks: one of 10/20/30/40 values, one solid 100.
        let vals = [[10u8, 20, 100, 100], [30, 40, 100, 100]];
        for y in 0..2 {
            for x in 0..4 {
                let i = (y * 16 + x * 4) as usize;
                src.data[i..i + 4].copy_from_slice(&[vals[y as usize][x as usize]; 4]);
            }
        }
        let mut out = Bgra::default();
        scale(&src, 2, 1, &mut out);
        assert_eq!(out.data, vec![25, 25, 25, 25, 100, 100, 100, 100]);
    }

    #[test]
    fn area_filter_keeps_flat_colour() {
        let mut src = Bgra::new(7, 5);
        src.data.chunks_mut(4).for_each(|p| p.copy_from_slice(&[9, 99, 199, 255]));
        let mut out = Bgra::default();
        scale(&src, 3, 2, &mut out);
        assert!(out.data.chunks(4).all(|p| p == [9, 99, 199, 255]));
    }
}

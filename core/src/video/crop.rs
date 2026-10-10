//! Finds the active picture inside MiSTer's letterbox/pillarbox borders, so a
//! 1280×1024 desktop inside a 1920×1080 frame is shown (and scaled) on its own.

/// Crop rectangle in source pixels.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Crop {
    pub x: u32,
    pub y: u32,
    pub w: u32,
    pub h: u32,
}

impl Crop {
    pub fn full(w: u32, h: u32) -> Self {
        Self { x: 0, y: 0, w, h }
    }
}

/// Anything brighter than this (max of R, G, B) counts as picture.
const THRESHOLD: u8 = 24;

/// Detect the content box in a BGRA/BGRX frame. Borders are taken as symmetric
/// (MiSTer centres its picture), which keeps dark content at one edge from
/// shrinking the crop. Returns None for an all-black frame.
pub fn detect(data: &[u8], stride: usize, w: u32, h: u32) -> Option<Crop> {
    let (w, h) = (w as usize, h as usize);
    let bright = |x: usize, y: usize| {
        let p = y * stride + x * 4;
        data[p].max(data[p + 1]).max(data[p + 2]) > THRESHOLD
    };
    let row_has = |y: usize| (0..w).step_by(4).any(|x| bright(x, y));

    let top = (0..h).find(|&y| row_has(y))?;
    let bottom = (top..h).rev().find(|&y| row_has(y))?;
    let rows: Vec<usize> = (top..=bottom).step_by(4).chain([bottom]).collect();
    let col_has = |x: usize| rows.iter().any(|&y| bright(x, y));
    let left = (0..w).find(|&x| col_has(x))?;
    let right = (left..w).rev().find(|&x| col_has(x))?;

    let mx = left.min(w - 1 - right);
    let my = top.min(h - 1 - bottom);
    // Keep sizes even so the centre stays on whole pixels.
    let mx = mx & !1;
    let my = my & !1;
    Some(Crop {
        x: mx as u32,
        y: my as u32,
        w: (w - 2 * mx) as u32,
        h: (h - 2 * my) as u32,
    })
}

/// Only switches to a new crop after seeing it twice in a row, so one odd
/// frame (fades, a black screen) doesn't make the picture jump.
#[derive(Default)]
pub struct Stabilizer {
    current: Option<Crop>,
    candidate: Option<Crop>,
}

impl Stabilizer {
    pub fn update(&mut self, detected: Option<Crop>) -> Option<Crop> {
        if let Some(c) = detected {
            if self.current.is_none() || self.candidate == Some(c) {
                self.current = Some(c);
            }
            self.candidate = Some(c);
        }
        self.current
    }

    pub fn reset(&mut self) {
        *self = Self::default();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn frame(w: usize, h: usize, rect: (usize, usize, usize, usize)) -> Vec<u8> {
        let mut d = vec![0u8; w * h * 4];
        for y in rect.1..rect.1 + rect.3 {
            for x in rect.0..rect.0 + rect.2 {
                d[(y * w + x) * 4..][..3].copy_from_slice(&[200, 200, 200]);
            }
        }
        d
    }

    #[test]
    fn finds_centred_desktop() {
        let d = frame(1920, 1080, (320, 28, 1280, 1024));
        assert_eq!(
            detect(&d, 1920 * 4, 1920, 1080),
            Some(Crop { x: 320, y: 28, w: 1280, h: 1024 })
        );
    }

    #[test]
    fn symmetric_when_one_edge_is_dark() {
        // Content only touches the right part of the real picture.
        let d = frame(1920, 1080, (700, 28, 900, 1024));
        let c = detect(&d, 1920 * 4, 1920, 1080).unwrap();
        assert_eq!((c.x, c.w), (320, 1280));
    }

    #[test]
    fn black_frame_is_none() {
        let d = vec![0u8; 64 * 32 * 4];
        assert_eq!(detect(&d, 64 * 4, 64, 32), None);
    }

    #[test]
    fn stabilizer_needs_two() {
        let a = Crop::full(10, 10);
        let b = Crop { x: 2, y: 2, w: 6, h: 6 };
        let mut s = Stabilizer::default();
        assert_eq!(s.update(Some(a)), Some(a));
        assert_eq!(s.update(Some(b)), Some(a));
        assert_eq!(s.update(None), Some(a));
        assert_eq!(s.update(Some(b)), Some(b));
    }
}

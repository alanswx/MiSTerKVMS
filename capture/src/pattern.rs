//! A built-in test pattern, `test:pattern`: moving bars at 1280x720, 60 fps.
//! For testing streams, viewers and soak runs without a capture card. Only
//! listed by `devices()` when `MISTERKVMS_TEST_PATTERN` is set; it can
//! always be opened by id.

use std::time::{Duration, Instant};

use crate::clock::now_100ns;
use crate::{Output, PixelFormat, VideoDevice, VideoFormat, VideoFrame};

pub const ID: &str = "test:pattern";
const FORMAT: VideoFormat = VideoFormat { pixel: PixelFormat::Bgra, width: 1280, height: 720, fps: 60.0 };

pub fn listed() -> bool {
    std::env::var_os("MISTERKVMS_TEST_PATTERN").is_some()
}

pub fn device() -> VideoDevice {
    VideoDevice { id: ID.into(), name: "Test pattern".into(), formats: vec![FORMAT] }
}

pub struct Pattern {
    started: Instant,
    frame: u64,
}

impl Pattern {
    pub fn open(_output: Output) -> (Self, VideoFormat) {
        (Self { started: Instant::now(), frame: 0 }, FORMAT)
    }

    pub fn next_frame(&mut self, timeout: Duration) -> Option<VideoFrame> {
        let due = self.started + Duration::from_secs_f64(self.frame as f64 / FORMAT.fps);
        let now = Instant::now();
        if due > now + timeout {
            std::thread::sleep(timeout);
            return None;
        }
        if due > now {
            std::thread::sleep(due - now);
        }
        let (w, h) = (FORMAT.width as usize, FORMAT.height as usize);
        let mut data = vec![0u8; w * h * 4];
        let offset = (self.frame * 4) as usize;
        for y in 0..h {
            let row = &mut data[y * w * 4..(y + 1) * w * 4];
            for x in 0..w {
                let bar = ((x + offset) / 160) % 8;
                let [r, g, b] = [
                    if bar & 1 != 0 { 230 } else { 20 },
                    if bar & 2 != 0 { 230 } else { 20 },
                    if bar & 4 != 0 { 230 } else { 20 },
                ];
                // A white box that steps once a second, to see motion and
                // check for frozen pictures.
                let in_box = y >= 300 && y < 420 && x >= ((self.frame / 60) % 8) as usize * 160 && x < ((self.frame / 60) % 8) as usize * 160 + 120;
                let px = if in_box { [255, 255, 255, 255] } else { [b, g, r, 255] };
                row[x * 4..x * 4 + 4].copy_from_slice(&px);
            }
        }
        self.frame += 1;
        Some(VideoFrame { format: PixelFormat::Bgra, width: FORMAT.width, height: FORMAT.height, stride: FORMAT.width * 4, data, timestamp: now_100ns() })
    }
}

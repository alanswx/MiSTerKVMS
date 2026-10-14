//! Time JPEG encoding of a 1080p NV12 frame: through BGRA at 4:4:4 (the old
//! path) and straight from YUV at 4:2:0. `cargo run --release -p kvms-codec --example jpeg_bench`

use std::time::Instant;

use kvms_codec::{convert, jpeg, Bgra};

fn main() {
    let (w, h) = (1920u32, 1080u32);
    // A busy picture (worst case for JPEG): gradients and fine detail.
    let mut nv12 = Vec::with_capacity((w * h * 3 / 2) as usize);
    for y in 0..h {
        for x in 0..w {
            nv12.push((16 + ((x ^ y) * 7 + x / 3) % 220) as u8);
        }
    }
    for y in 0..h / 2 {
        for x in 0..w / 2 {
            nv12.extend_from_slice(&[(64 + (x + y) % 128) as u8, (64 + (x * 3 + y) % 128) as u8]);
        }
    }
    let runs = 60;
    let mut out = Vec::new();
    let mut bgra = Bgra::default();
    let t = Instant::now();
    for _ in 0..runs {
        convert::nv12_to_bgra(&nv12, w, h, w, &mut bgra);
        jpeg::encode(&bgra, 85, &mut out).unwrap();
    }
    let old = t.elapsed().as_secs_f64() * 1000.0 / runs as f64;
    let old_kb = out.len() / 1024;
    let t = Instant::now();
    for _ in 0..runs {
        jpeg::encode_yuv(jpeg::Yuv::Nv12 { data: &nv12, width: w, height: h, stride: w }, 85, &mut out).unwrap();
    }
    let new = t.elapsed().as_secs_f64() * 1000.0 / runs as f64;
    println!("NV12 1080p → JPEG q85: via BGRA 4:4:4 {old:.1} ms ({old_kb} KB), straight from YUV 4:2:0 {new:.1} ms ({} KB)", out.len() / 1024);
}

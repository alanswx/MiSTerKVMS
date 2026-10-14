//! Rough timings for 1080p: `cargo run --release -p kvms-codec --example speed`

use std::time::Instant;

use kvms_codec::{convert, jpeg, scale, Bgra};

fn main() {
    let (w, h) = (1920u32, 1080u32);
    // Something like a desktop: flat areas, text-ish detail, gradients.
    let mut pic = Bgra::new(w, h);
    for y in 0..h {
        for x in 0..w {
            let i = ((y * w + x) * 4) as usize;
            let v = if (x / 8 + y / 12) % 7 == 0 { 255 } else { ((x + y) / 8) as u8 };
            pic.data[i..i + 4].copy_from_slice(&[v, (y / 5) as u8, (x / 8) as u8, 255]);
        }
    }
    let n = 30;
    let time = |name: &str, f: &mut dyn FnMut()| {
        f();
        let t = Instant::now();
        for _ in 0..n {
            f();
        }
        println!("{name}: {:.2} ms", t.elapsed().as_secs_f64() * 1000.0 / n as f64);
    };
    let mut jpg = Vec::new();
    time("jpeg encode q85 4:4:4", &mut || jpeg::encode(&pic, 85, &mut jpg).unwrap());
    println!("  size {} KB", jpg.len() / 1024);
    let mut back = Bgra::default();
    time("jpeg decode", &mut || jpeg::decode(&jpg, &mut back).unwrap());
    let nv12 = vec![128u8; (w * h * 3 / 2) as usize];
    let mut out = Bgra::default();
    time("nv12 -> bgra", &mut || convert::nv12_to_bgra(&nv12, w, h, w, &mut out));
    time("scale 1080p -> 720p (area)", &mut || scale::scale(&pic, 1280, 720, &mut out));
    time("scale 1080p -> 960x540 (box)", &mut || scale::scale(&pic, 960, 540, &mut out));
}

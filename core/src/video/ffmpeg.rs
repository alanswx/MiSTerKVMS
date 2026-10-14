//! Recording through ffmpeg, for shells without a native encoder: video is
//! piped to ffmpeg as raw BGRA (H.264 out), audio is kept in a temp file and
//! muxed in (AAC) when recording stops.

use std::fs::File;
use std::io::{BufWriter, Write};
use std::path::{Path, PathBuf};
use std::process::{Child, ChildStdin, Command, Stdio};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc::{self, Receiver, Sender};
use std::sync::{Arc, Mutex};
use std::thread::{self, JoinHandle};

use super::MediaSink;
use crate::log;

/// Frames per second written to the file. Each frame is placed by its
/// timestamp (repeated or dropped as needed) so audio stays in sync.
const FPS: i64 = 60;
const TICKS_PER_FRAME: i64 = 10_000_000 / FPS;
/// Video frames waiting for ffmpeg before new ones are dropped.
const MAX_QUEUED: usize = 8;

enum Msg {
    Video { data: Vec<u8>, width: u32, height: u32, timestamp: i64 },
    Audio { samples: Vec<f32>, channels: u32, rate: u32, timestamp: i64 },
}

pub struct FfmpegRecorder {
    tx: Mutex<Option<Sender<Msg>>>,
    queued: Arc<AtomicUsize>,
    worker: Mutex<Option<JoinHandle<Result<PathBuf, String>>>>,
}

/// The ffmpeg to run: next to our executable if bundled, else from PATH.
pub fn ffmpeg_path() -> PathBuf {
    let name = if cfg!(windows) { "ffmpeg.exe" } else { "ffmpeg" };
    std::env::current_exe()
        .ok()
        .and_then(|exe| exe.parent().map(|d| d.join(name)))
        .filter(|p| p.exists())
        .unwrap_or_else(|| PathBuf::from(name))
}

/// Whether ffmpeg can be started.
pub fn ffmpeg_available() -> bool {
    let mut cmd = Command::new(ffmpeg_path());
    cmd.arg("-version").stdout(Stdio::null()).stderr(Stdio::null());
    no_window(&mut cmd);
    cmd.status().is_ok_and(|s| s.success())
}

impl FfmpegRecorder {
    /// Start recording to `out` (an .mp4 path).
    pub fn start(out: PathBuf) -> Result<Arc<Self>, String> {
        if !ffmpeg_available() {
            return Err("Recording needs ffmpeg: install it (e.g. winget install ffmpeg) or put ffmpeg next to the app.".into());
        }
        if let Some(dir) = out.parent() {
            std::fs::create_dir_all(dir).map_err(|e| format!("{}: {e}", dir.display()))?;
        }
        let (tx, rx) = mpsc::channel();
        let queued = Arc::new(AtomicUsize::new(0));
        let worker_queued = queued.clone();
        let worker = thread::Builder::new()
            .name("ffmpeg-record".into())
            .spawn(move || Worker::new(out).run(rx, worker_queued))
            .map_err(|e| e.to_string())?;
        Ok(Arc::new(Self { tx: Mutex::new(Some(tx)), queued, worker: Mutex::new(Some(worker)) }))
    }

    /// Finish the file. Blocks while ffmpeg finishes encoding and muxing.
    pub fn finish(&self) -> Result<PathBuf, String> {
        self.tx.lock().unwrap().take();
        match self.worker.lock().unwrap().take() {
            Some(w) => w.join().unwrap_or_else(|_| Err("recorder crashed".into())),
            None => Err("already finished".into()),
        }
    }

    fn send(&self, msg: Msg) {
        if let Some(tx) = self.tx.lock().unwrap().as_ref() {
            let _ = tx.send(msg);
        }
    }
}

impl MediaSink for FfmpegRecorder {
    fn video_frame(&self, data: u64, stride: u32, width: u32, height: u32, timestamp: i64) {
        if self.queued.load(Ordering::Relaxed) >= MAX_QUEUED {
            return; // ffmpeg is behind; the frame before is repeated instead
        }
        let row = width as usize * 4;
        let mut pixels = Vec::with_capacity(row * height as usize);
        for y in 0..height as usize {
            // SAFETY: the caller guarantees `data` points at `height` rows of
            // `stride` bytes for the duration of this call.
            let line = unsafe { std::slice::from_raw_parts((data as usize + y * stride as usize) as *const u8, row) };
            pixels.extend_from_slice(line);
        }
        self.queued.fetch_add(1, Ordering::Relaxed);
        self.send(Msg::Video { data: pixels, width, height, timestamp });
    }

    fn audio_frame(&self, samples: Vec<f32>, channels: u32, sample_rate: u32, timestamp: i64) {
        self.send(Msg::Audio { samples, channels, rate: sample_rate, timestamp });
    }
}

struct Worker {
    out: PathBuf,
    video_path: PathBuf,
    audio_path: PathBuf,
    ffmpeg: Option<(Child, BufWriter<ChildStdin>)>,
    size: (u32, u32),
    first_video: Option<i64>,
    frames_written: i64,
    last_frame: Vec<u8>,
    audio: Option<(BufWriter<File>, u32, u32, i64)>,
}

impl Worker {
    fn new(out: PathBuf) -> Self {
        Self {
            video_path: out.with_extension("video.mp4"),
            audio_path: out.with_extension("audio.f32"),
            out,
            ffmpeg: None,
            size: (0, 0),
            first_video: None,
            frames_written: 0,
            last_frame: Vec::new(),
            audio: None,
        }
    }

    fn run(mut self, rx: Receiver<Msg>, queued: Arc<AtomicUsize>) -> Result<PathBuf, String> {
        let mut error = None;
        for msg in rx {
            let result = match msg {
                Msg::Video { data, width, height, timestamp } => {
                    queued.fetch_sub(1, Ordering::Relaxed);
                    self.video(data, width, height, timestamp)
                }
                Msg::Audio { samples, channels, rate, timestamp } => self.audio(&samples, channels, rate, timestamp),
            };
            if let Err(e) = result {
                log(&format!("record: {e}"));
                error.get_or_insert(e);
                break;
            }
        }
        let result = match error {
            Some(e) => Err(e),
            None => self.finish(),
        };
        let _ = std::fs::remove_file(&self.video_path);
        let _ = std::fs::remove_file(&self.audio_path);
        result
    }

    fn video(&mut self, data: Vec<u8>, width: u32, height: u32, timestamp: i64) -> Result<(), String> {
        if self.ffmpeg.is_none() {
            self.ffmpeg = Some(self.spawn_encoder(width, height)?);
            self.size = (width, height);
            self.first_video = Some(timestamp);
        }
        // The file keeps the size it started with; a resolution change
        // mid-recording repeats the last frame until it changes back.
        if (width, height) == self.size {
            self.last_frame = data;
        }
        let due = (timestamp - self.first_video.unwrap_or(timestamp)) / TICKS_PER_FRAME + 1;
        let (_, stdin) = self.ffmpeg.as_mut().unwrap();
        while self.frames_written < due {
            stdin.write_all(&self.last_frame).map_err(|e| format!("ffmpeg stopped: {e}"))?;
            self.frames_written += 1;
        }
        Ok(())
    }

    fn audio(&mut self, samples: &[f32], channels: u32, rate: u32, timestamp: i64) -> Result<(), String> {
        if self.audio.is_none() {
            let file = File::create(&self.audio_path).map_err(|e| format!("{}: {e}", self.audio_path.display()))?;
            self.audio = Some((BufWriter::new(file), channels, rate, timestamp));
        }
        let (file, ch, r, _) = self.audio.as_mut().unwrap();
        if (channels, rate) != (*ch, *r) {
            return Ok(()); // format changed; keep the first one
        }
        let bytes: Vec<u8> = samples.iter().flat_map(|s| s.to_le_bytes()).collect();
        file.write_all(&bytes).map_err(|e| e.to_string())
    }

    fn spawn_encoder(&self, width: u32, height: u32) -> Result<(Child, BufWriter<ChildStdin>), String> {
        let mut cmd = Command::new(ffmpeg_path());
        cmd.args(["-y", "-loglevel", "error", "-f", "rawvideo", "-pix_fmt", "bgra"])
            .args(["-s", &format!("{width}x{height}"), "-r", &FPS.to_string(), "-i", "-"])
            .args(["-c:v", "libx264", "-preset", "veryfast", "-crf", "18", "-pix_fmt", "yuv420p"])
            .arg(&self.video_path)
            .stdin(Stdio::piped())
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        no_window(&mut cmd);
        let mut child = cmd.spawn().map_err(|e| format!("ffmpeg: {e}"))?;
        let stdin = child.stdin.take().ok_or("ffmpeg: no stdin")?;
        log(&format!("record: {width}x{height} to {}", self.out.display()));
        Ok((child, BufWriter::with_capacity(1 << 20, stdin)))
    }

    fn finish(&mut self) -> Result<PathBuf, String> {
        let Some((mut child, stdin)) = self.ffmpeg.take() else {
            return Err("no video was received".into());
        };
        drop(stdin.into_inner().map_err(|e| e.to_string())?);
        let status = child.wait().map_err(|e| e.to_string())?;
        if !status.success() {
            return Err("ffmpeg failed encoding video".into());
        }
        let Some((mut file, channels, rate, audio_start)) = self.audio.take() else {
            std::fs::rename(&self.video_path, &self.out).map_err(|e| e.to_string())?;
            return Ok(self.out.clone());
        };
        file.flush().map_err(|e| e.to_string())?;
        drop(file);
        // Audio that started after the first video frame is delayed to match.
        let offset = (audio_start - self.first_video.unwrap_or(audio_start)) as f64 / 1e7;
        let mut cmd = Command::new(ffmpeg_path());
        cmd.args(["-y", "-loglevel", "error", "-i"])
            .arg(&self.video_path)
            .args(["-itsoffset", &format!("{offset:.4}")])
            .args(["-f", "f32le", "-ar", &rate.to_string(), "-ac", &channels.to_string(), "-i"])
            .arg(&self.audio_path)
            .args(["-map", "0:v", "-map", "1:a", "-c:v", "copy", "-c:a", "aac", "-b:a", "192k", "-shortest"])
            .arg(&self.out)
            .stdout(Stdio::null())
            .stderr(Stdio::inherit());
        no_window(&mut cmd);
        let status = cmd.status().map_err(|e| format!("ffmpeg: {e}"))?;
        if !status.success() {
            return Err("ffmpeg failed adding audio".into());
        }
        Ok(self.out.clone())
    }
}

/// Don't flash a console window when a GUI app starts ffmpeg.
fn no_window(cmd: &mut Command) {
    #[cfg(windows)]
    {
        use std::os::windows::process::CommandExt;
        const CREATE_NO_WINDOW: u32 = 0x0800_0000;
        cmd.creation_flags(CREATE_NO_WINDOW);
    }
    let _ = cmd;
}

/// A file name for a new recording: "<host> <date time>.mp4" in `dir`.
pub fn recording_path(dir: &Path, host: &str, stamp: &str) -> PathBuf {
    let safe: String = host
        .chars()
        .map(|c| if c.is_alphanumeric() || " -_().".contains(c) { c } else { '_' })
        .collect();
    dir.join(format!("{} {stamp}.mp4", safe.trim()))
}

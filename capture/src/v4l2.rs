//! Video4Linux2 capture (Linux): USB capture sticks (MJPEG, YUYV) and cards
//! with a V4L2 driver. Cards are identified by their `/dev/v4l/by-id` name
//! when they have one (USB cards do: stable across reboots and ports, and two
//! identical sticks differ by serial or port), else by `/dev/videoN`.
//!
//! Needs read/write access to `/dev/video*`: the `video` group on most
//! distributions (the service's user is put in it).

use std::fs;
use std::os::fd::AsRawFd;
use std::path::{Path, PathBuf};
use std::time::Duration;

use linuxvideo::format::{FrameIntervals, FrameSizes, PixFormat};
use linuxvideo::{BufType, CapabilityFlags, Device, Fract};

use crate::{best_format, clock, CaptureError, Output, PixelFormat, VideoDevice, VideoFormat, VideoFrame};

/// Sizes offered from a card with a range (stepwise) rather than a list.
const COMMON_SIZES: [(u32, u32); 4] = [(1920, 1080), (1280, 720), (1024, 768), (640, 480)];

fn pixel(fourcc: linuxvideo::format::PixelFormat) -> Option<PixelFormat> {
    use linuxvideo::format::PixelFormat as P;
    match fourcc {
        f if f == P::MJPG || f == P::from_fourcc(*b"JPEG") => Some(PixelFormat::Mjpeg),
        f if f == P::YUYV => Some(PixelFormat::Yuy2),
        f if f == P::from_fourcc(*b"NV12") => Some(PixelFormat::Nv12),
        // XR24 / AR24: B, G, R, X in memory.
        f if f == P::XBGR32 || f == P::ABGR32 => Some(PixelFormat::Bgra),
        _ => None,
    }
}

fn fourcc(pixel: PixelFormat) -> linuxvideo::format::PixelFormat {
    use linuxvideo::format::PixelFormat as P;
    match pixel {
        PixelFormat::Mjpeg => P::MJPG,
        PixelFormat::Yuy2 => P::YUYV,
        PixelFormat::Nv12 => P::from_fourcc(*b"NV12"),
        PixelFormat::Bgra => P::XBGR32,
    }
}

fn fps_of(interval: &Fract) -> f64 {
    if interval.numerator() == 0 {
        return 0.0;
    }
    // Two decimals: 59.94 stays 59.94, 60 stays 60.
    (interval.denominator() as f64 / interval.numerator() as f64 * 100.0).round() / 100.0
}

/// A video capture node (not a metadata or output node) that streams.
fn is_capture(device: &Device) -> bool {
    device.capabilities().is_ok_and(|c| {
        let caps = c.device_capabilities();
        caps.contains(CapabilityFlags::VIDEO_CAPTURE) && caps.contains(CapabilityFlags::STREAMING)
    })
}

/// `/dev/v4l/by-id/...` for a device node, if udev made one.
fn by_id(node: &Path) -> Option<PathBuf> {
    let node = fs::canonicalize(node).ok()?;
    fs::read_dir("/dev/v4l/by-id")
        .ok()?
        .flatten()
        .map(|e| e.path())
        .filter(|p| fs::canonicalize(p).ok().as_deref() == Some(node.as_path()))
        // Some cards have index0 (video) and index1 (metadata) links: the node
        // itself decides, but prefer the shortest name for the same node.
        .min_by_key(|p| p.as_os_str().len())
}

/// The modes a card offers, with the frame interval to ask for each.
fn modes(device: &Device) -> Vec<(VideoFormat, Fract)> {
    let mut out = Vec::new();
    for desc in device.formats(BufType::VIDEO_CAPTURE).flatten() {
        let Some(pixel) = pixel(desc.pixel_format()) else { continue };
        let sizes: Vec<(u32, u32)> = match device.frame_sizes(desc.pixel_format()) {
            Ok(FrameSizes::Discrete(list)) => list.iter().map(|s| (s.width(), s.height())).collect(),
            Ok(range @ (FrameSizes::Stepwise(_) | FrameSizes::Continuous(_))) => {
                let (max_w, max_h) = (range.max_width(), range.max_height());
                let mut sizes = vec![(max_w, max_h)];
                sizes.extend(COMMON_SIZES.iter().copied().filter(|&(w, h)| {
                    w <= max_w && h <= max_h && w >= range.min_width() && h >= range.min_height() && (w, h) != (max_w, max_h)
                }));
                sizes
            }
            Err(_) => continue,
        };
        for (width, height) in sizes {
            let intervals: Vec<Fract> = match device.frame_intervals(desc.pixel_format(), width, height) {
                Ok(FrameIntervals::Discrete(list)) => list.iter().map(|i| *i.fract()).collect(),
                // A range: its fastest end.
                Ok(range) => vec![range.min()],
                // Some drivers don't list intervals: assume 30.
                Err(_) => vec![Fract::new(1, 30)],
            };
            for interval in intervals {
                let fps = fps_of(&interval);
                if fps > 0.0 {
                    out.push((VideoFormat { pixel, width, height, fps }, interval));
                }
            }
        }
    }
    out
}

pub fn devices() -> Result<Vec<VideoDevice>, CaptureError> {
    let list = linuxvideo::list().map_err(|e| CaptureError::Other(format!("can't list video devices: {e}")))?;
    let mut out = Vec::new();
    for device in list.flatten() {
        if !is_capture(&device) {
            continue;
        }
        let Ok(node) = device.path() else { continue };
        let name = device.capabilities().map(|c| c.card().trim().to_string()).unwrap_or_default();
        let id = by_id(&node).unwrap_or(node).display().to_string();
        let mut formats: Vec<VideoFormat> = modes(&device).into_iter().map(|(f, _)| f).collect();
        formats.dedup();
        out.push(VideoDevice { id, name: if name.is_empty() { "Video capture".into() } else { name }, formats });
    }
    Ok(out)
}

fn open_error(e: std::io::Error, what: &str) -> CaptureError {
    match e.raw_os_error() {
        Some(libc::EBUSY) => CaptureError::InUse(what.to_string()),
        Some(libc::EACCES) | Some(libc::EPERM) => CaptureError::Other(format!(
            "no permission to open {what}: the user running MiSTerKVMS Capture needs to be in the 'video' group"
        )),
        Some(libc::ENOENT) | Some(libc::ENODEV) => CaptureError::NotFound(what.to_string()),
        _ => CaptureError::Other(format!("{what}: {e}")),
    }
}

pub struct Reader {
    stream: linuxvideo::stream::ReadStream,
    format: VideoFormat,
    stride: u32,
    what: String,
}

impl Reader {
    /// Open by id (a `/dev/...` path), or by name when the id isn't found
    /// (the card got another node).
    pub fn open(id: &str, wanted: Option<VideoFormat>, _output: Output) -> Result<(Self, VideoFormat), CaptureError> {
        let device = match Device::open(Path::new(id)) {
            Ok(d) if id.starts_with("/dev/") => d,
            Ok(_) | Err(_) => {
                let found = linuxvideo::list()
                    .map_err(|e| CaptureError::Other(e.to_string()))?
                    .flatten()
                    .filter(is_capture)
                    .find(|d| d.capabilities().is_ok_and(|c| c.card().trim() == id));
                match found {
                    Some(d) => d,
                    None => return Err(Device::open(Path::new(id)).err().map(|e| open_error(e, id)).unwrap_or(CaptureError::NotFound(id.into()))),
                }
            }
        };
        let what = device.capabilities().map(|c| c.card().trim().to_string()).unwrap_or_else(|_| id.to_string());
        let offered = modes(&device);
        let list: Vec<VideoFormat> = offered.iter().map(|(f, _)| *f).collect();
        let format = match wanted {
            Some(w) => list
                .iter()
                .copied()
                .find(|f| f.pixel == w.pixel && f.width == w.width && f.height == w.height && (f.fps - w.fps).abs() < 0.5)
                .ok_or_else(|| CaptureError::Format(format!("{w} not offered")))?,
            None => best_format(&list).ok_or_else(|| CaptureError::Format("no usable formats".into()))?,
        };
        let interval = offered.iter().find(|(f, _)| *f == format).map(|(_, i)| *i).unwrap_or(Fract::new(1, 30));
        let capture = device
            .video_capture(PixFormat::new(format.width, format.height, fourcc(format.pixel)))
            .map_err(|e| open_error(e, &what))?;
        let got = capture.format();
        if got.width() != format.width || got.height() != format.height || pixel(got.pixel_format()) != Some(format.pixel) {
            return Err(CaptureError::Format(format!(
                "{format} asked, the card set {}x{} {:?}",
                got.width(),
                got.height(),
                got.pixel_format()
            )));
        }
        let stride = got.bytes_per_line();
        // Not every driver lets the rate be chosen; the card's own then stands.
        let _ = capture.set_frame_interval(interval);
        let stream = capture.into_stream().map_err(|e| open_error(e, &what))?;
        Ok((Self { stream, format, stride, what }, format))
    }

    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, CaptureError> {
        let mut poll = libc::pollfd { fd: self.stream.as_raw_fd(), events: libc::POLLIN, revents: 0 };
        let ms = timeout.as_millis().min(i32::MAX as u128) as i32;
        let ready = unsafe { libc::poll(&mut poll, 1, ms) };
        if ready < 0 {
            let e = std::io::Error::last_os_error();
            if e.kind() == std::io::ErrorKind::Interrupted {
                return Ok(None);
            }
            return Err(CaptureError::Lost(format!("{}: {e}", self.what)));
        }
        if ready == 0 {
            return Ok(None);
        }
        if poll.revents & (libc::POLLERR | libc::POLLHUP) != 0 {
            return Err(CaptureError::Lost(format!("{}: unplugged", self.what)));
        }
        let timestamp = clock::now_100ns();
        let format = self.format;
        let stride = self.stride;
        let frame = self
            .stream
            .dequeue(|buf| {
                // A frame the driver flagged as damaged: skip it.
                if buf.is_error() || buf.is_empty() {
                    return Ok(None);
                }
                Ok(Some(VideoFrame {
                    format: format.pixel,
                    width: format.width,
                    height: format.height,
                    stride: if format.pixel == PixelFormat::Mjpeg { 0 } else { stride },
                    data: buf.to_vec(),
                    timestamp,
                }))
            })
            .map_err(|e| match e.raw_os_error() {
                Some(libc::ENODEV) => CaptureError::Lost(format!("{}: unplugged", self.what)),
                _ => CaptureError::Lost(format!("{}: {e}", self.what)),
            })?;
        Ok(frame)
    }
}

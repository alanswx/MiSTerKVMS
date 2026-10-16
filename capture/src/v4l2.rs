//! Linux: Video4Linux2, straight through ioctls (no extra libraries).
//! Cards are listed by their `/dev/v4l/by-id` link when they have one (the
//! same for a card across replugs and reboots), else `/dev/videoN`. Frames
//! come from memory-mapped driver buffers in the card's own format: MJPEG
//! stays compressed, so the server passes it through untouched.
//!
//! Needs read/write access to the device: the user logged in at the seat
//! has it, a service user needs the `video` group.

use std::ffi::CString;
use std::mem::{size_of, zeroed};
use std::os::raw::{c_int, c_ulong, c_void};
use std::path::{Path, PathBuf};
use std::time::Duration;

use crate::clock::DeviceClock;
use crate::{best_format, rates_in_range, CaptureError, Output, PixelFormat, VideoDevice, VideoFormat, VideoFrame};

const fn fourcc(s: &[u8; 4]) -> u32 {
    s[0] as u32 | (s[1] as u32) << 8 | (s[2] as u32) << 16 | (s[3] as u32) << 24
}

const PIX_MJPEG: u32 = fourcc(b"MJPG");
const PIX_JPEG: u32 = fourcc(b"JPEG");
const PIX_YUYV: u32 = fourcc(b"YUYV");
const PIX_NV12: u32 = fourcc(b"NV12");

fn pixel_format(code: u32) -> Option<PixelFormat> {
    match code {
        PIX_MJPEG | PIX_JPEG => Some(PixelFormat::Mjpeg),
        PIX_YUYV => Some(PixelFormat::Yuy2),
        PIX_NV12 => Some(PixelFormat::Nv12),
        _ => None,
    }
}

const BUF_TYPE_VIDEO_CAPTURE: u32 = 1;
const MEMORY_MMAP: u32 = 1;
const CAP_VIDEO_CAPTURE: u32 = 0x0000_0001;
const CAP_STREAMING: u32 = 0x0400_0000;
const CAP_DEVICE_CAPS: u32 = 0x8000_0000;
const FRMSIZE_TYPE_DISCRETE: u32 = 1;
const FRMIVAL_TYPE_DISCRETE: u32 = 1;
/// Driver buffers; a few so the card never waits for us.
const BUFFERS: u32 = 4;

#[repr(C)]
struct Capability {
    driver: [u8; 16],
    card: [u8; 32],
    bus_info: [u8; 32],
    version: u32,
    capabilities: u32,
    device_caps: u32,
    reserved: [u32; 3],
}

#[repr(C)]
struct FmtDesc {
    index: u32,
    kind: u32,
    flags: u32,
    description: [u8; 32],
    pixelformat: u32,
    mbus_code: u32,
    reserved: [u32; 3],
}

#[repr(C)]
struct FrmSizeEnum {
    index: u32,
    pixel_format: u32,
    kind: u32,
    /// Discrete: width, height. Stepwise: min/max/step width, then height.
    size: [u32; 6],
    reserved: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct Fract {
    numerator: u32,
    denominator: u32,
}

#[repr(C)]
struct FrmIvalEnum {
    index: u32,
    pixel_format: u32,
    width: u32,
    height: u32,
    kind: u32,
    /// Discrete: the interval. Stepwise: min, max, step.
    ival: [Fract; 3],
    reserved: [u32; 2],
}

#[repr(C)]
#[derive(Clone, Copy)]
struct PixFormat {
    width: u32,
    height: u32,
    pixelformat: u32,
    field: u32,
    bytesperline: u32,
    sizeimage: u32,
    colorspace: u32,
    private: u32,
    flags: u32,
    ycbcr_enc: u32,
    quantization: u32,
    xfer_func: u32,
}

#[repr(C)]
union FormatUnion {
    pix: PixFormat,
    // The kernel's union holds pointers (v4l2_window), so it is
    // pointer-aligned: 200 bytes at offset 8 on 64-bit.
    raw: [usize; 200 / size_of::<usize>()],
}

#[repr(C)]
struct Format {
    kind: u32,
    fmt: FormatUnion,
}

#[repr(C)]
#[derive(Clone, Copy)]
struct CaptureParm {
    capability: u32,
    capturemode: u32,
    timeperframe: Fract,
    extendedmode: u32,
    readbuffers: u32,
    reserved: [u32; 4],
}

#[repr(C)]
union ParmUnion {
    capture: CaptureParm,
    raw: [u8; 200],
}

#[repr(C)]
struct StreamParm {
    kind: u32,
    parm: ParmUnion,
}

#[repr(C)]
struct RequestBuffers {
    count: u32,
    kind: u32,
    memory: u32,
    capabilities: u32,
    flags: u8,
    reserved: [u8; 3],
}

#[repr(C)]
struct Timecode {
    kind: u32,
    flags: u32,
    frames: u8,
    seconds: u8,
    minutes: u8,
    hours: u8,
    userbits: [u8; 4],
}

#[repr(C)]
union BufferM {
    offset: u32,
    userptr: c_ulong,
    fd: i32,
}

#[repr(C)]
struct Buffer {
    index: u32,
    kind: u32,
    bytesused: u32,
    flags: u32,
    field: u32,
    timestamp: libc::timeval,
    timecode: Timecode,
    sequence: u32,
    memory: u32,
    m: BufferM,
    length: u32,
    reserved2: u32,
    request_fd: i32,
}

// _IOC encoding as on x86 and Arm: direction, size, type 'V', number.
const fn ioc(dir: c_ulong, nr: c_ulong, size: usize) -> c_ulong {
    dir << 30 | (size as c_ulong) << 16 | (b'V' as c_ulong) << 8 | nr
}
const fn ior<T>(nr: c_ulong) -> c_ulong {
    ioc(2, nr, size_of::<T>())
}
const fn iow<T>(nr: c_ulong) -> c_ulong {
    ioc(1, nr, size_of::<T>())
}
const fn iowr<T>(nr: c_ulong) -> c_ulong {
    ioc(3, nr, size_of::<T>())
}

const VIDIOC_QUERYCAP: c_ulong = ior::<Capability>(0);
const VIDIOC_ENUM_FMT: c_ulong = iowr::<FmtDesc>(2);
const VIDIOC_S_FMT: c_ulong = iowr::<Format>(5);
const VIDIOC_REQBUFS: c_ulong = iowr::<RequestBuffers>(8);
const VIDIOC_QUERYBUF: c_ulong = iowr::<Buffer>(9);
const VIDIOC_QBUF: c_ulong = iowr::<Buffer>(15);
const VIDIOC_DQBUF: c_ulong = iowr::<Buffer>(17);
const VIDIOC_STREAMON: c_ulong = iow::<c_int>(18);
const VIDIOC_STREAMOFF: c_ulong = iow::<c_int>(19);
const VIDIOC_S_PARM: c_ulong = iowr::<StreamParm>(22);
const VIDIOC_ENUM_FRAMESIZES: c_ulong = iowr::<FrmSizeEnum>(74);
const VIDIOC_ENUM_FRAMEINTERVALS: c_ulong = iowr::<FrmIvalEnum>(75);

fn errno() -> i32 {
    std::io::Error::last_os_error().raw_os_error().unwrap_or(0)
}

/// An open device node, closed on drop.
struct Fd(c_int);

impl Fd {
    fn open(path: &Path) -> Result<Self, CaptureError> {
        let c = CString::new(path.as_os_str().as_encoded_bytes()).map_err(|_| CaptureError::NotFound(path.display().to_string()))?;
        let fd = unsafe { libc::open(c.as_ptr(), libc::O_RDWR | libc::O_NONBLOCK | libc::O_CLOEXEC) };
        if fd < 0 {
            let e = std::io::Error::last_os_error();
            return Err(match e.raw_os_error() {
                Some(libc::ENOENT) | Some(libc::ENODEV) | Some(libc::ENXIO) => CaptureError::NotFound(path.display().to_string()),
                Some(libc::EACCES) | Some(libc::EPERM) => {
                    CaptureError::Other(format!("no permission to open {} (add the user to the `video` group)", path.display()))
                }
                Some(libc::EBUSY) => CaptureError::InUse(path.display().to_string()),
                _ => CaptureError::Other(format!("{}: {e}", path.display())),
            });
        }
        Ok(Fd(fd))
    }

    /// One ioctl, retried on EINTR; the errno on failure.
    fn ioctl<T>(&self, request: c_ulong, arg: &mut T) -> Result<(), i32> {
        loop {
            if unsafe { libc::ioctl(self.0, request as _, arg as *mut T as *mut c_void) } != -1 {
                return Ok(());
            }
            match errno() {
                libc::EINTR => continue,
                e => return Err(e),
            }
        }
    }
}

impl Drop for Fd {
    fn drop(&mut self) {
        unsafe { libc::close(self.0) };
    }
}

fn os_error(what: &str, e: i32) -> CaptureError {
    let text = format!("{what}: {}", std::io::Error::from_raw_os_error(e));
    match e {
        libc::EBUSY => CaptureError::InUse(text),
        libc::ENODEV | libc::EIO => CaptureError::Lost(text),
        libc::EINVAL => CaptureError::Format(text),
        _ => CaptureError::Other(text),
    }
}

fn c_str(bytes: &[u8]) -> String {
    let end = bytes.iter().position(|&b| b == 0).unwrap_or(bytes.len());
    String::from_utf8_lossy(&bytes[..end]).trim().to_string()
}

/// A capture node's name: "UGREEN 25773: UGREEN 25773" (UVC repeats the
/// product) becomes "UGREEN 25773", the name its audio device carries.
fn card_name(card: &str) -> String {
    match card.split_once(": ") {
        Some((a, b)) if a.trim() == b.trim() => a.trim().to_string(),
        _ => card.to_string(),
    }
}

/// Every /dev/videoN, with its by-id link when there is one.
fn nodes() -> Vec<(PathBuf, PathBuf)> {
    let mut by_id: Vec<(PathBuf, PathBuf)> = std::fs::read_dir("/dev/v4l/by-id")
        .into_iter()
        .flatten()
        .flatten()
        .filter_map(|e| Some((std::fs::canonicalize(e.path()).ok()?, e.path())))
        .collect();
    by_id.sort();
    let mut nodes: Vec<PathBuf> = std::fs::read_dir("/dev")
        .into_iter()
        .flatten()
        .flatten()
        .map(|e| e.path())
        .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.strip_prefix("video").is_some_and(|d| d.parse::<u32>().is_ok())))
        .collect();
    nodes.sort_by_key(|p| p.to_str().and_then(|s| s.trim_start_matches("/dev/video").parse::<u32>().ok()));
    nodes
        .into_iter()
        .map(|node| {
            let id = by_id.iter().find(|(target, _)| *target == node).map_or_else(|| node.clone(), |(_, link)| link.clone());
            (node, id)
        })
        .collect()
}

/// The card's name if this node captures video (UVC cards also have a
/// metadata node, which doesn't).
fn query(fd: &Fd) -> Option<String> {
    let mut cap: Capability = unsafe { zeroed() };
    fd.ioctl(VIDIOC_QUERYCAP, &mut cap).ok()?;
    let caps = if cap.capabilities & CAP_DEVICE_CAPS != 0 { cap.device_caps } else { cap.capabilities };
    (caps & CAP_VIDEO_CAPTURE != 0 && caps & CAP_STREAMING != 0).then(|| card_name(&c_str(&cap.card)))
}

fn fps_of(f: Fract) -> Option<f64> {
    (f.numerator > 0 && f.denominator > 0).then(|| ((f.denominator as f64 / f.numerator as f64) * 100.0).round() / 100.0)
}

/// Every format the card offers (in a pixel format we can read), with the
/// V4L2 pixel format code.
fn formats(fd: &Fd) -> Vec<(VideoFormat, u32)> {
    let mut out = Vec::new();
    for index in 0.. {
        let mut desc: FmtDesc = unsafe { zeroed() };
        desc.index = index;
        desc.kind = BUF_TYPE_VIDEO_CAPTURE;
        if fd.ioctl(VIDIOC_ENUM_FMT, &mut desc).is_err() {
            break;
        }
        let Some(pixel) = pixel_format(desc.pixelformat) else { continue };
        let mut sizes = Vec::new();
        for index in 0.. {
            let mut size: FrmSizeEnum = unsafe { zeroed() };
            size.index = index;
            size.pixel_format = desc.pixelformat;
            if fd.ioctl(VIDIOC_ENUM_FRAMESIZES, &mut size).is_err() {
                break;
            }
            if size.kind == FRMSIZE_TYPE_DISCRETE {
                sizes.push((size.size[0], size.size[1]));
            } else {
                // Stepwise or continuous: offer the largest.
                sizes.push((size.size[1], size.size[4]));
                break;
            }
        }
        for (width, height) in sizes {
            let mut rates = Vec::new();
            for index in 0.. {
                let mut ival: FrmIvalEnum = unsafe { zeroed() };
                ival.index = index;
                ival.pixel_format = desc.pixelformat;
                ival.width = width;
                ival.height = height;
                if fd.ioctl(VIDIOC_ENUM_FRAMEINTERVALS, &mut ival).is_err() {
                    break;
                }
                if ival.kind == FRMIVAL_TYPE_DISCRETE {
                    rates.extend(fps_of(ival.ival[0]));
                } else {
                    // Shortest interval is the highest rate.
                    if let (Some(max), Some(min)) = (fps_of(ival.ival[0]), fps_of(ival.ival[1])) {
                        rates.extend(rates_in_range(min, max, &[]));
                    }
                    break;
                }
            }
            for fps in rates {
                let f = VideoFormat { pixel, width, height, fps };
                if !out.iter().any(|(o, _)| *o == f) {
                    out.push((f, desc.pixelformat));
                }
            }
        }
    }
    out
}

pub fn devices() -> Result<Vec<VideoDevice>, CaptureError> {
    let mut list = Vec::new();
    for (node, id) in nodes() {
        let Ok(fd) = Fd::open(&node) else { continue };
        let Some(name) = query(&fd) else { continue };
        let formats = formats(&fd).into_iter().map(|(f, _)| f).collect();
        list.push(VideoDevice { id: id.display().to_string(), name, formats });
    }
    Ok(list)
}

struct Mapped {
    ptr: *mut c_void,
    len: usize,
}

pub struct Reader {
    fd: Fd,
    buffers: Vec<Mapped>,
    pixel: PixelFormat,
    width: u32,
    height: u32,
    stride: u32,
    clock: DeviceClock,
}

// SAFETY: the mappings belong to this reader alone and are only read
// between DQBUF and QBUF on the thread that owns it.
unsafe impl Send for Reader {}

impl Reader {
    /// Open by id (a by-id link or /dev/videoN) or, failing that, by name.
    pub fn open(id: &str, wanted: Option<VideoFormat>, _output: Output) -> Result<(Self, VideoFormat), CaptureError> {
        let node = if Path::new(id).exists() {
            PathBuf::from(id)
        } else {
            nodes()
                .into_iter()
                .find(|(node, _)| Fd::open(node).ok().and_then(|fd| query(&fd)).is_some_and(|n| n == id))
                .map(|(node, _)| node)
                .ok_or_else(|| CaptureError::NotFound(id.to_string()))?
        };
        let fd = Fd::open(&node)?;
        if query(&fd).is_none() {
            return Err(CaptureError::NotFound(format!("{id} isn't a video capture device")));
        }
        let offered = formats(&fd);
        let list: Vec<VideoFormat> = offered.iter().map(|(f, _)| *f).collect();
        let format = match wanted {
            Some(w) => list
                .iter()
                .copied()
                .find(|f| f.pixel == w.pixel && f.width == w.width && f.height == w.height && (f.fps - w.fps).abs() < 0.5)
                .ok_or_else(|| CaptureError::Format(format!("{w} not offered")))?,
            None => best_format(&list).ok_or_else(|| CaptureError::Format("no usable formats".into()))?,
        };
        let code = offered.iter().find(|(f, _)| *f == format).map(|(_, c)| *c).unwrap();

        let mut fmt: Format = unsafe { zeroed() };
        fmt.kind = BUF_TYPE_VIDEO_CAPTURE;
        fmt.fmt.pix = PixFormat { width: format.width, height: format.height, pixelformat: code, ..unsafe { zeroed() } };
        fd.ioctl(VIDIOC_S_FMT, &mut fmt).map_err(|e| os_error("set format", e))?;
        let pix = unsafe { fmt.fmt.pix };
        let pixel = pixel_format(pix.pixelformat).ok_or_else(|| CaptureError::Format(format!("{format} refused")))?;

        let mut parm: StreamParm = unsafe { zeroed() };
        parm.kind = BUF_TYPE_VIDEO_CAPTURE;
        parm.parm.capture.timeperframe = Fract { numerator: 1000, denominator: (format.fps * 1000.0).round() as u32 };
        // Not every driver can set the rate; the card then runs at its own.
        let _ = fd.ioctl(VIDIOC_S_PARM, &mut parm);

        let mut req = RequestBuffers { count: BUFFERS, kind: BUF_TYPE_VIDEO_CAPTURE, memory: MEMORY_MMAP, capabilities: 0, flags: 0, reserved: [0; 3] };
        fd.ioctl(VIDIOC_REQBUFS, &mut req).map_err(|e| os_error("request buffers", e))?;
        let mut reader = Reader {
            fd,
            buffers: Vec::new(),
            pixel,
            width: pix.width,
            height: pix.height,
            stride: if pixel.is_compressed() { 0 } else { pix.bytesperline },
            clock: DeviceClock::default(),
        };
        for index in 0..req.count {
            let mut buf = reader.buffer(index);
            reader.fd.ioctl(VIDIOC_QUERYBUF, &mut buf).map_err(|e| os_error("query buffer", e))?;
            let len = buf.length as usize;
            let ptr = unsafe {
                libc::mmap(std::ptr::null_mut(), len, libc::PROT_READ | libc::PROT_WRITE, libc::MAP_SHARED, reader.fd.0, buf.m.offset as libc::off_t)
            };
            if ptr == libc::MAP_FAILED {
                return Err(os_error("map buffer", errno()));
            }
            reader.buffers.push(Mapped { ptr, len });
            reader.fd.ioctl(VIDIOC_QBUF, &mut buf).map_err(|e| os_error("queue buffer", e))?;
        }
        let mut kind = BUF_TYPE_VIDEO_CAPTURE as c_int;
        reader.fd.ioctl(VIDIOC_STREAMON, &mut kind).map_err(|e| os_error("start streaming", e))?;
        let format = VideoFormat { pixel, width: pix.width, height: pix.height, fps: format.fps };
        Ok((reader, format))
    }

    fn buffer(&self, index: u32) -> Buffer {
        let mut buf: Buffer = unsafe { zeroed() };
        buf.index = index;
        buf.kind = BUF_TYPE_VIDEO_CAPTURE;
        buf.memory = MEMORY_MMAP;
        buf
    }

    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, CaptureError> {
        let mut pfd = libc::pollfd { fd: self.fd.0, events: libc::POLLIN, revents: 0 };
        let n = unsafe { libc::poll(&mut pfd, 1, timeout.as_millis().min(i32::MAX as u128) as c_int) };
        if n < 0 {
            return match errno() {
                libc::EINTR => Ok(None),
                e => Err(os_error("wait for frame", e)),
            };
        }
        if n == 0 {
            return Ok(None);
        }
        if pfd.revents & (libc::POLLERR | libc::POLLHUP | libc::POLLNVAL) != 0 && pfd.revents & libc::POLLIN == 0 {
            return Err(CaptureError::Lost("the capture card went away".into()));
        }
        let mut buf = self.buffer(0);
        match self.fd.ioctl(VIDIOC_DQBUF, &mut buf) {
            Ok(()) => {}
            Err(libc::EAGAIN) => return Ok(None),
            Err(e) => return Err(os_error("read frame", e)),
        }
        let mapped = &self.buffers[buf.index as usize];
        let used = (buf.bytesused as usize).min(mapped.len);
        let data = unsafe { std::slice::from_raw_parts(mapped.ptr as *const u8, used) }.to_vec();
        let device_ts = buf.timestamp.tv_sec as i64 * 10_000_000 + buf.timestamp.tv_usec as i64 * 10;
        self.fd.ioctl(VIDIOC_QBUF, &mut buf).map_err(|e| os_error("queue buffer", e))?;
        // A frame the card flagged as broken, or an empty one (no signal).
        const BUF_FLAG_ERROR: u32 = 0x40;
        if buf.flags & BUF_FLAG_ERROR != 0 || used == 0 {
            return Ok(None);
        }
        Ok(Some(VideoFrame {
            format: self.pixel,
            width: self.width,
            height: self.height,
            stride: self.stride,
            data,
            timestamp: self.clock.wall(device_ts),
        }))
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        let mut kind = BUF_TYPE_VIDEO_CAPTURE as c_int;
        let _ = self.fd.ioctl(VIDIOC_STREAMOFF, &mut kind);
        for m in &self.buffers {
            unsafe { libc::munmap(m.ptr, m.len) };
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn struct_sizes_match_the_kernel() {
        assert_eq!(size_of::<Capability>(), 104);
        assert_eq!(size_of::<FmtDesc>(), 64);
        assert_eq!(size_of::<FrmSizeEnum>(), 44);
        assert_eq!(size_of::<FrmIvalEnum>(), 52);
        assert_eq!(size_of::<StreamParm>(), 204);
        assert_eq!(size_of::<RequestBuffers>(), 20);
        if size_of::<usize>() == 8 {
            assert_eq!(size_of::<Format>(), 208);
            assert_eq!(size_of::<Buffer>(), 88);
            assert_eq!(VIDIOC_DQBUF, 0xc058_5611);
            assert_eq!(VIDIOC_S_FMT, 0xc0d0_5605);
        }
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
    }

    #[test]
    fn uvc_names() {
        assert_eq!(card_name("UGREEN 25773: UGREEN 25773"), "UGREEN 25773");
        assert_eq!(card_name("UVC Camera (046d:081b)"), "UVC Camera (046d:081b)");
    }
}

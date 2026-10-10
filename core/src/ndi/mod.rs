//! Minimal NDI receive wrapper. The NDI runtime is loaded at runtime (NDI's
//! recommended approach for open-source apps), so nothing proprietary is
//! linked at build time. Types come from the generated `sys` bindings.

pub mod sys;

use std::ffi::{c_char, CStr, CString};
use std::path::PathBuf;
use std::ptr;
use std::sync::OnceLock;

use libloading::Library;
use sys::*;

type FindCreate = unsafe extern "C" fn(*const NDIlib_find_create_t) -> NDIlib_find_instance_t;
type FindDestroy = unsafe extern "C" fn(NDIlib_find_instance_t);
type FindWait = unsafe extern "C" fn(NDIlib_find_instance_t, u32) -> bool;
type FindSources = unsafe extern "C" fn(NDIlib_find_instance_t, *mut u32) -> *const NDIlib_source_t;
type RecvCreate = unsafe extern "C" fn(*const NDIlib_recv_create_v3_t) -> NDIlib_recv_instance_t;
type RecvDestroy = unsafe extern "C" fn(NDIlib_recv_instance_t);
type RecvCapture = unsafe extern "C" fn(
    NDIlib_recv_instance_t,
    *mut NDIlib_video_frame_v2_t,
    *mut NDIlib_audio_frame_v3_t,
    *mut NDIlib_metadata_frame_t,
    u32,
) -> NDIlib_frame_type_e;
type RecvFreeVideo = unsafe extern "C" fn(NDIlib_recv_instance_t, *const NDIlib_video_frame_v2_t);
type RecvFreeAudio = unsafe extern "C" fn(NDIlib_recv_instance_t, *const NDIlib_audio_frame_v3_t);
type RecvPerformance = unsafe extern "C" fn(
    NDIlib_recv_instance_t,
    *mut NDIlib_recv_performance_t,
    *mut NDIlib_recv_performance_t,
);
type RecvConnections = unsafe extern "C" fn(NDIlib_recv_instance_t) -> i32;

pub struct Ndi {
    _lib: Library,
    pub path: PathBuf,
    pub version: String,
    find_create: FindCreate,
    find_destroy: FindDestroy,
    find_wait: FindWait,
    find_sources: FindSources,
    recv_create: RecvCreate,
    recv_destroy: RecvDestroy,
    recv_capture: RecvCapture,
    recv_free_video: RecvFreeVideo,
    recv_free_audio: RecvFreeAudio,
    recv_performance: RecvPerformance,
    recv_connections: RecvConnections,
}

static NDI: OnceLock<Result<Ndi, String>> = OnceLock::new();

/// The loaded NDI runtime, or a message explaining where we looked.
pub fn ndi() -> Result<&'static Ndi, String> {
    NDI.get_or_init(load).as_ref().map_err(Clone::clone)
}

fn candidates() -> Vec<PathBuf> {
    let mut paths = Vec::new();
    if let Ok(p) = std::env::var("KVMS_NDI_LIBRARY") {
        paths.push(PathBuf::from(p));
    }
    let file = if cfg!(target_os = "macos") {
        "libndi.dylib"
    } else if cfg!(windows) {
        "Processing.NDI.Lib.x64.dll"
    } else {
        "libndi.so.6"
    };
    // Bundled with the app (macOS: Contents/Frameworks).
    if let Ok(exe) = std::env::current_exe() {
        if let Some(dir) = exe.parent() {
            paths.push(dir.join("../Frameworks").join(file));
            paths.push(dir.join(file));
        }
    }
    if let Ok(dir) = std::env::var("NDI_RUNTIME_DIR_V6") {
        paths.push(PathBuf::from(dir).join(file));
    }
    if cfg!(target_os = "macos") {
        paths.push("/Library/NDI SDK for Apple/lib/macOS/libndi.dylib".into());
        paths.push("/usr/local/lib/libndi.dylib".into());
    }
    paths.push(PathBuf::from(file)); // system search path
    paths
}

fn load() -> Result<Ndi, String> {
    let mut tried = Vec::new();
    for path in candidates() {
        // SAFETY: loading the NDI runtime runs its initialisers, which is the
        // documented way to use it.
        let lib = match unsafe { Library::new(&path) } {
            Ok(lib) => lib,
            Err(_) => {
                tried.push(path.display().to_string());
                continue;
            }
        };
        return unsafe { bind(lib, path) };
    }
    Err(format!(
        "NDI runtime not found. Install NDI Tools or the NDI Runtime from https://ndi.video/tools/ (looked in: {})",
        tried.join(", ")
    ))
}

unsafe fn bind(lib: Library, path: PathBuf) -> Result<Ndi, String> {
    macro_rules! sym {
        ($name:literal) => {
            *lib.get(concat!($name, "\0").as_bytes())
                .map_err(|e| format!("{}: missing {}: {e}", path.display(), $name))?
        };
    }
    let initialize: unsafe extern "C" fn() -> bool = sym!("NDIlib_initialize");
    if !initialize() {
        return Err("NDI failed to initialise (unsupported CPU?)".into());
    }
    let version_fn: unsafe extern "C" fn() -> *const c_char = sym!("NDIlib_version");
    let version = CStr::from_ptr(version_fn()).to_string_lossy().into_owned();
    Ok(Ndi {
        find_create: sym!("NDIlib_find_create_v2"),
        find_destroy: sym!("NDIlib_find_destroy"),
        find_wait: sym!("NDIlib_find_wait_for_sources"),
        find_sources: sym!("NDIlib_find_get_current_sources"),
        recv_create: sym!("NDIlib_recv_create_v3"),
        recv_destroy: sym!("NDIlib_recv_destroy"),
        recv_capture: sym!("NDIlib_recv_capture_v3"),
        recv_free_video: sym!("NDIlib_recv_free_video_v2"),
        recv_free_audio: sym!("NDIlib_recv_free_audio_v3"),
        recv_performance: sym!("NDIlib_recv_get_performance"),
        recv_connections: sym!("NDIlib_recv_get_no_connections"),
        version,
        path,
        _lib: lib,
    })
}

/// Discover NDI sources on the network for up to `wait_ms`.
pub fn find_sources(ndi: &Ndi, extra_ips: &str, wait_ms: u32) -> Vec<String> {
    let extra = CString::new(extra_ips).unwrap_or_default();
    let settings = NDIlib_find_create_t {
        show_local_sources: true,
        p_groups: ptr::null(),
        p_extra_ips: if extra_ips.is_empty() { ptr::null() } else { extra.as_ptr() },
    };
    unsafe {
        let finder = (ndi.find_create)(&settings);
        if finder.is_null() {
            return Vec::new();
        }
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms as u64);
        while std::time::Instant::now() < deadline {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            (ndi.find_wait)(finder, left.as_millis().min(500) as u32);
        }
        let mut count = 0u32;
        let sources = (ndi.find_sources)(finder, &mut count);
        let names = (0..count as usize)
            .map(|i| CStr::from_ptr((*sources.add(i)).p_ndi_name).to_string_lossy().into_owned())
            .collect();
        (ndi.find_destroy)(finder);
        names
    }
}

pub struct Receiver {
    ndi: &'static Ndi,
    inst: NDIlib_recv_instance_t,
}

// SAFETY: NDI receivers may be used from any thread; we use each from one.
unsafe impl Send for Receiver {}

pub enum Captured<'a> {
    Video(VideoFrame<'a>),
    Audio(AudioFrame<'a>),
    Nothing,
    Other,
    Error,
}

pub struct VideoFrame<'a> {
    rx: &'a Receiver,
    pub frame: NDIlib_video_frame_v2_t,
}

pub struct AudioFrame<'a> {
    rx: &'a Receiver,
    pub frame: NDIlib_audio_frame_v3_t,
}

impl Drop for VideoFrame<'_> {
    fn drop(&mut self) {
        unsafe { (self.rx.ndi.recv_free_video)(self.rx.inst, &self.frame) }
    }
}

impl Drop for AudioFrame<'_> {
    fn drop(&mut self) {
        unsafe { (self.rx.ndi.recv_free_audio)(self.rx.inst, &self.frame) }
    }
}

impl VideoFrame<'_> {
    pub fn stride(&self) -> usize {
        unsafe { self.frame.__bindgen_anon_1.line_stride_in_bytes as usize }
    }

    pub fn data(&self) -> &[u8] {
        let len = self.stride() * self.frame.yres as usize;
        unsafe { std::slice::from_raw_parts(self.frame.p_data, len) }
    }
}

impl AudioFrame<'_> {
    /// Planar float samples for one channel (FourCC FLTP).
    pub fn channel(&self, ch: usize) -> &[f32] {
        unsafe {
            let stride = self.frame.__bindgen_anon_1.channel_stride_in_bytes as usize;
            let base = self.frame.p_data.add(ch * stride) as *const f32;
            std::slice::from_raw_parts(base, self.frame.no_samples as usize)
        }
    }
}

impl Receiver {
    /// Receive full-quality BGRA/BGRX video and float audio from `source`
    /// (the full NDI name, e.g. "MACHINE (Source)").
    pub fn new(ndi: &'static Ndi, source: &str, extra_ip: &str, recv_name: &str) -> Option<Self> {
        let name = CString::new(source).ok()?;
        let ip = CString::new(extra_ip).ok()?;
        let recv_name = CString::new(recv_name).ok()?;
        let settings = NDIlib_recv_create_v3_t {
            source_to_connect_to: NDIlib_source_t {
                p_ndi_name: name.as_ptr(),
                __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                    p_url_address: if extra_ip.is_empty() { ptr::null() } else { ip.as_ptr() },
                },
            },
            color_format: NDIlib_recv_color_format_BGRX_BGRA,
            bandwidth: NDIlib_recv_bandwidth_highest,
            allow_video_fields: false,
            p_ndi_recv_name: recv_name.as_ptr(),
        };
        let inst = unsafe { (ndi.recv_create)(&settings) };
        (!inst.is_null()).then_some(Self { ndi, inst })
    }

    #[allow(non_upper_case_globals)]
    pub fn capture(&self, timeout_ms: u32) -> Captured<'_> {
        let mut video: NDIlib_video_frame_v2_t = unsafe { std::mem::zeroed() };
        let mut audio: NDIlib_audio_frame_v3_t = unsafe { std::mem::zeroed() };
        let kind = unsafe {
            (self.ndi.recv_capture)(self.inst, &mut video, &mut audio, ptr::null_mut(), timeout_ms)
        };
        match kind {
            NDIlib_frame_type_video => Captured::Video(VideoFrame { rx: self, frame: video }),
            NDIlib_frame_type_audio => Captured::Audio(AudioFrame { rx: self, frame: audio }),
            NDIlib_frame_type_none => Captured::Nothing,
            NDIlib_frame_type_error => Captured::Error,
            _ => Captured::Other,
        }
    }

    /// (total, dropped) video frames since the receiver was created.
    pub fn video_performance(&self) -> (i64, i64) {
        let mut total: NDIlib_recv_performance_t = unsafe { std::mem::zeroed() };
        let mut dropped: NDIlib_recv_performance_t = unsafe { std::mem::zeroed() };
        unsafe { (self.ndi.recv_performance)(self.inst, &mut total, &mut dropped) };
        (total.video_frames, dropped.video_frames)
    }

    pub fn connections(&self) -> i32 {
        unsafe { (self.ndi.recv_connections)(self.inst) }
    }
}

impl Drop for Receiver {
    fn drop(&mut self) {
        unsafe { (self.ndi.recv_destroy)(self.inst) }
    }
}

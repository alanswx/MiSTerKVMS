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
    if cfg!(windows) {
        // The installer sets NDI_RUNTIME_DIR_V6 machine-wide, but processes
        // started before the install (or from an old shell) don't see it.
        let program_files = std::env::var("ProgramFiles").unwrap_or_else(|_| r"C:\Program Files".into());
        paths.push(PathBuf::from(&program_files).join(r"NDI\NDI 6 Runtime\v6").join(file));
        paths.push(PathBuf::from(&program_files).join(r"NDI\NDI 6 Tools\Runtime").join(file));
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

/// A discovered source: its full name ("MACHINE (Source)") and the address
/// NDI reports for it ("ip:port"), if any.
pub struct FoundSource {
    pub name: String,
    pub url: Option<String>,
}

/// Turn "obs-pc.local, 192.168.1.20" into the IP list NDI's finder wants,
/// resolving host names. Unresolvable names are skipped.
pub fn resolve_extra_ips(input: &str) -> String {
    use std::net::{IpAddr, ToSocketAddrs};
    input
        .split([',', ' '])
        .map(str::trim)
        .filter(|h| !h.is_empty())
        .filter_map(|h| {
            if h.parse::<IpAddr>().is_ok() {
                return Some(h.to_string());
            }
            let addrs: Vec<_> = (h, 0).to_socket_addrs().ok()?.collect();
            addrs.iter().find(|a| a.is_ipv4()).or(addrs.first()).map(|a| a.ip().to_string())
        })
        .collect::<Vec<_>>()
        .join(",")
}

/// Discover NDI sources for up to `wait_ms`, also asking the given OBS hosts
/// (IPs or names) directly. Returns early once `until` (a full source name)
/// is seen.
pub fn find(ndi: &Ndi, extra_hosts: &str, wait_ms: u32, until: Option<&str>) -> Vec<FoundSource> {
    let ips = resolve_extra_ips(extra_hosts);
    let extra = CString::new(ips.clone()).unwrap_or_default();
    let settings = NDIlib_find_create_t {
        show_local_sources: true,
        p_groups: ptr::null(),
        p_extra_ips: if ips.is_empty() { ptr::null() } else { extra.as_ptr() },
    };
    unsafe {
        let finder = (ndi.find_create)(&settings);
        if finder.is_null() {
            return Vec::new();
        }
        let read = || {
            let mut count = 0u32;
            let sources = (ndi.find_sources)(finder, &mut count);
            (0..count as usize)
                .map(|i| {
                    let s = &*sources.add(i);
                    let url = s.__bindgen_anon_1.p_url_address;
                    FoundSource {
                        name: CStr::from_ptr(s.p_ndi_name).to_string_lossy().into_owned(),
                        url: (!url.is_null()).then(|| CStr::from_ptr(url).to_string_lossy().into_owned()),
                    }
                })
                .collect::<Vec<_>>()
        };
        let deadline = std::time::Instant::now() + std::time::Duration::from_millis(wait_ms as u64);
        let mut found = Vec::new();
        while std::time::Instant::now() < deadline {
            let left = deadline.saturating_duration_since(std::time::Instant::now());
            (ndi.find_wait)(finder, left.as_millis().min(250) as u32);
            found = read();
            if until.is_some_and(|name| found.iter().any(|f| f.name == name)) {
                break;
            }
        }
        if found.is_empty() {
            found = read();
        }
        (ndi.find_destroy)(finder);
        found
    }
}

/// Names of the NDI sources visible on the network (and on `extra_hosts`).
pub fn find_sources(ndi: &Ndi, extra_hosts: &str, wait_ms: u32) -> Vec<String> {
    let mut names: Vec<String> = find(ndi, extra_hosts, wait_ms, None).into_iter().map(|f| f.name).collect();
    names.sort();
    names.dedup();
    names
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
    /// (the full NDI name, e.g. "MACHINE (Source)"). `obs_host` (IP or name,
    /// optional) is asked directly, for networks where discovery can't see it;
    /// we then connect to the address it reports for the source.
    pub fn new(ndi: &'static Ndi, source: &str, obs_host: &str, recv_name: &str) -> Option<Self> {
        Self::with_cancel(ndi, source, obs_host, recv_name, &|| false)
    }

    /// Like `new`, but gives up looking `source` up on `obs_host` as soon as
    /// `cancelled()` returns true (so closing a session never waits on it).
    pub fn with_cancel(
        ndi: &'static Ndi,
        source: &str,
        obs_host: &str,
        recv_name: &str,
        cancelled: &dyn Fn() -> bool,
    ) -> Option<Self> {
        let mut url = None;
        if !obs_host.trim().is_empty() {
            let until = std::time::Instant::now() + std::time::Duration::from_secs(3);
            while url.is_none() && std::time::Instant::now() < until && !cancelled() {
                url = find(ndi, obs_host, 250, Some(source))
                    .into_iter()
                    .find(|f| f.name == source)
                    .and_then(|f| f.url);
            }
        }
        if cancelled() {
            return None;
        }
        let name = CString::new(source).ok()?;
        let ip = CString::new(url.unwrap_or_default()).ok()?;
        let recv_name = CString::new(recv_name).ok()?;
        let settings = NDIlib_recv_create_v3_t {
            source_to_connect_to: NDIlib_source_t {
                p_ndi_name: name.as_ptr(),
                __bindgen_anon_1: NDIlib_source_t__bindgen_ty_1 {
                    p_url_address: if ip.as_bytes().is_empty() { ptr::null() } else { ip.as_ptr() },
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

/// "MACHINE (Source)" → ("MACHINE", "Source"). The source part may itself
/// contain parentheses, e.g. "PC (MiSTer-1 (Video Capture Device 2))".
fn split_name(full: &str) -> Option<(&str, &str)> {
    let (machine, rest) = full.split_once(" (")?;
    Some((machine, rest.strip_suffix(')')?))
}

/// When `saved` isn't on the network, the source it most likely became:
/// the only one from the same machine whose name extends or shortens it at a
/// " (" boundary. OBS can change a source's NDI name between sessions, e.g.
/// "PC (MiSTer-1 (Video Capture Device 2))" ↔ "PC (MiSTer-1)". None if
/// there's no single candidate.
pub fn renamed_source(saved: &str, available: &[String]) -> Option<String> {
    let (machine, name) = split_name(saved)?;
    let related = |a: &str, b: &str| a.strip_prefix(b).is_some_and(|rest| rest.starts_with(" ("));
    let mut candidates = available.iter().filter(|full| {
        split_name(full).is_some_and(|(m, n)| m == machine && n != name && (related(n, name) || related(name, n)))
    });
    let first = candidates.next()?;
    candidates.next().is_none().then(|| first.clone())
}

#[cfg(test)]
mod tests {
    use super::*;

    fn names(list: &[&str]) -> Vec<String> {
        list.iter().map(|s| s.to_string()).collect()
    }

    #[test]
    fn finds_a_renamed_source() {
        let seen = names(&["DANIHTPC (MiSTer-Pi)", "DANIHTPC (MiSTer-SS1)"]);
        assert_eq!(
            renamed_source("DANIHTPC (MiSTer-SS1 (Video Capture Device 2))", &seen).as_deref(),
            Some("DANIHTPC (MiSTer-SS1)")
        );
        // And the other way round.
        let seen = names(&["DANIHTPC (MiSTer-SS1 (Video Capture Device 2))"]);
        assert_eq!(renamed_source("DANIHTPC (MiSTer-SS1)", &seen).as_deref(), Some(seen[0].as_str()));
    }

    #[test]
    fn leaves_unclear_cases_alone() {
        // Another machine, a different name, or two candidates.
        assert_eq!(renamed_source("PC (MiSTer-1 (A))", &names(&["OTHER (MiSTer-1)"])), None);
        assert_eq!(renamed_source("PC (MiSTer-1)", &names(&["PC (MiSTer-10)", "PC (MiSTer-2)"])), None);
        assert_eq!(renamed_source("PC (MiSTer-1)", &names(&["PC (MiSTer-1 (A))", "PC (MiSTer-1 (B))"])), None);
        assert_eq!(renamed_source("not an NDI name", &names(&["PC (x)"])), None);
    }
}

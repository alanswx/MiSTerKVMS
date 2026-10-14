//! Windows: DirectShow, for cards Media Foundation can't open (AVerMedia
//! GC573 and other PCIe cards that OBS also reaches this way). Frames come
//! from a capture graph: card → Sample Grabber → Null Renderer, with the
//! grabber calling back for each frame.
//!
//! The Sample Grabber (qedit.dll) is deprecated but still part of Windows;
//! the `windows` crate doesn't describe it, so its two interfaces are
//! declared here.

#![allow(non_snake_case)] // COM method names, as Windows spells them.

use std::ffi::c_void;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use windows::core::{implement, interface, w, IUnknown, IUnknown_Vtbl, Interface, BOOL, GUID, HRESULT, PCWSTR};
use windows::Win32::Foundation::{E_POINTER, S_OK};
use windows::Win32::Media::Audio::WAVEFORMATEX;
use windows::Win32::Media::DirectShow::*;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::StructuredStorage::IPropertyBag;
use windows::Win32::System::Com::{CoCreateInstance, CoInitializeEx, CoTaskMemFree, IEnumMoniker, IMoniker, CLSCTX_INPROC_SERVER, COINIT_MULTITHREADED};
use windows::Win32::System::Variant::{VariantClear, VARIANT, VT_BSTR};

use crate::audio::{AudioCapture, AudioChunk, AudioDevice};
use crate::clock::now_100ns;
use crate::{best_format, CaptureError, Output, PixelFormat, VideoDevice, VideoFormat, VideoFrame};

/// Ids of devices opened through DirectShow carry this prefix.
pub const PREFIX: &str = "dshow:";

const CLSID_SAMPLE_GRABBER: GUID = GUID::from_u128(0xC1F400A0_3F08_11d3_9F0B_006008039E37);
const CLSID_NULL_RENDERER: GUID = GUID::from_u128(0xC1F400A4_3F08_11d3_9F0B_006008039E37);

#[interface("6B652FFF-11FE-4fce-92AD-0266B5D7C78F")]
unsafe trait ISampleGrabber: IUnknown {
    fn SetOneShot(&self, oneshot: BOOL) -> HRESULT;
    fn SetMediaType(&self, mt: *const AM_MEDIA_TYPE) -> HRESULT;
    fn GetConnectedMediaType(&self, mt: *mut AM_MEDIA_TYPE) -> HRESULT;
    fn SetBufferSamples(&self, buffer: BOOL) -> HRESULT;
    fn GetCurrentBuffer(&self, size: *mut i32, buffer: *mut i32) -> HRESULT;
    fn GetCurrentSample(&self, sample: *mut *mut c_void) -> HRESULT;
    fn SetCallback(&self, callback: *mut c_void, which: i32) -> HRESULT;
}

#[interface("0579154A-2B53-4994-B0D0-E773148EFF85")]
unsafe trait ISampleGrabberCB: IUnknown {
    fn SampleCB(&self, time: f64, sample: *mut c_void) -> HRESULT;
    fn BufferCB(&self, time: f64, buffer: *mut u8, len: i32) -> HRESULT;
}

fn err(context: &str, e: windows::core::Error) -> CaptureError {
    let code = e.code().0 as u32;
    let msg = format!("{context}: {} (0x{code:08x})", e.message());
    // VFW_E_CANNOT_CONNECT / device in use / access denied.
    if matches!(code, 0x80040217 | 0x800705AA | 0x80070005 | 0x8007001F) {
        CaptureError::InUse(msg)
    } else {
        CaptureError::Other(msg)
    }
}

fn hr(context: &str, h: HRESULT) -> Result<(), CaptureError> {
    h.ok().map_err(|e| err(context, e))
}

/// Step-by-step tracing for card problems: KVMS_CAPTURE_DEBUG=1.
fn dbg(step: &str) {
    static ON: std::sync::OnceLock<bool> = std::sync::OnceLock::new();
    if *ON.get_or_init(|| std::env::var_os("KVMS_CAPTURE_DEBUG").is_some()) {
        eprintln!("dshow: {step}");
    }
}

fn init() {
    unsafe {
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
    }
}

/// An AM_MEDIA_TYPE the system allocated: freed on drop.
struct MediaType(*mut AM_MEDIA_TYPE);

impl MediaType {
    fn get(&self) -> &AM_MEDIA_TYPE {
        unsafe { &*self.0 }
    }
}

impl Drop for MediaType {
    fn drop(&mut self) {
        unsafe {
            free_format(&mut *self.0);
            CoTaskMemFree(Some(self.0 as *const c_void));
        }
    }
}

/// Free what an AM_MEDIA_TYPE points to (FreeMediaType).
unsafe fn free_format(mt: &mut AM_MEDIA_TYPE) {
    if !mt.pbFormat.is_null() {
        CoTaskMemFree(Some(mt.pbFormat as *const c_void));
        mt.pbFormat = std::ptr::null_mut();
        mt.cbFormat = 0;
    }
    std::mem::ManuallyDrop::drop(&mut mt.pUnk);
}

fn pixel_format(subtype: &GUID) -> Option<PixelFormat> {
    if *subtype == MEDIASUBTYPE_MJPG {
        Some(PixelFormat::Mjpeg)
    } else if *subtype == MEDIASUBTYPE_NV12 {
        Some(PixelFormat::Nv12)
    } else if *subtype == MEDIASUBTYPE_YUY2 {
        Some(PixelFormat::Yuy2)
    } else if *subtype == MEDIASUBTYPE_RGB32 || *subtype == MEDIASUBTYPE_ARGB32 {
        Some(PixelFormat::Bgra)
    } else {
        None
    }
}

fn subtype_of(pixel: PixelFormat) -> GUID {
    match pixel {
        PixelFormat::Mjpeg => MEDIASUBTYPE_MJPG,
        PixelFormat::Nv12 => MEDIASUBTYPE_NV12,
        PixelFormat::Yuy2 => MEDIASUBTYPE_YUY2,
        PixelFormat::Bgra => MEDIASUBTYPE_RGB32,
    }
}

/// Size, signed height (negative: top row first) and frame time of a video
/// media type.
unsafe fn video_info(mt: &AM_MEDIA_TYPE) -> Option<(i32, i32, i64)> {
    if mt.pbFormat.is_null() {
        return None;
    }
    if mt.formattype == FORMAT_VideoInfo && mt.cbFormat as usize >= std::mem::size_of::<VIDEOINFOHEADER>() {
        let vi = &*(mt.pbFormat as *const VIDEOINFOHEADER);
        Some((vi.bmiHeader.biWidth, vi.bmiHeader.biHeight, vi.AvgTimePerFrame))
    } else if mt.formattype == FORMAT_VideoInfo2 && mt.cbFormat as usize >= std::mem::size_of::<VIDEOINFOHEADER2>() {
        let vi = &*(mt.pbFormat as *const VIDEOINFOHEADER2);
        Some((vi.bmiHeader.biWidth, vi.bmiHeader.biHeight, vi.AvgTimePerFrame))
    } else {
        None
    }
}

/// Set the frame time in a video media type we're about to apply.
unsafe fn set_frame_time(mt: &mut AM_MEDIA_TYPE, interval: i64) {
    if mt.formattype == FORMAT_VideoInfo {
        (*(mt.pbFormat as *mut VIDEOINFOHEADER)).AvgTimePerFrame = interval;
    } else if mt.formattype == FORMAT_VideoInfo2 {
        (*(mt.pbFormat as *mut VIDEOINFOHEADER2)).AvgTimePerFrame = interval;
    }
}

fn fps_of(interval: i64) -> f64 {
    if interval > 0 {
        (10_000_000.0 / interval as f64 * 100.0).round() / 100.0
    } else {
        0.0
    }
}

struct Device {
    path: String,
    name: String,
    moniker: IMoniker,
}

unsafe fn read_string(bag: &IPropertyBag, name: PCWSTR) -> Option<String> {
    let mut v = VARIANT::default();
    bag.Read(name, &mut v, None).ok()?;
    let s = if v.Anonymous.Anonymous.vt == VT_BSTR { Some(v.Anonymous.Anonymous.Anonymous.bstrVal.to_string()) } else { None };
    let _ = VariantClear(&mut v);
    s
}

fn enumerate(category: &GUID) -> Result<Vec<Device>, CaptureError> {
    init();
    unsafe {
        let devenum: ICreateDevEnum =
            CoCreateInstance(&CLSID_SystemDeviceEnum, None, CLSCTX_INPROC_SERVER).map_err(|e| err("SystemDeviceEnum", e))?;
        let mut monikers: Option<IEnumMoniker> = None;
        devenum.CreateClassEnumerator(category, &mut monikers, 0).map_err(|e| err("CreateClassEnumerator", e))?;
        // S_FALSE (no devices) leaves it empty.
        let Some(monikers) = monikers else { return Ok(Vec::new()) };
        let mut out = Vec::new();
        loop {
            let mut m = [None];
            let mut fetched = 0;
            if monikers.Next(&mut m, Some(&mut fetched)) != S_OK || fetched == 0 {
                break;
            }
            let Some(moniker) = m[0].take() else { break };
            let Ok(bag) = moniker.BindToStorage::<_, _, IPropertyBag>(None, None) else { continue };
            let name = read_string(&bag, w!("FriendlyName")).unwrap_or_default();
            // Software filters (virtual cameras) have no device path.
            let path = read_string(&bag, w!("DevicePath")).unwrap_or_else(|| name.clone());
            out.push(Device { path, name, moniker });
        }
        Ok(out)
    }
}

/// The capture pin's formats, each with every rate it offers (the default
/// and the fastest).
unsafe fn formats(config: &IAMStreamConfig) -> Vec<(VideoFormat, usize)> {
    let (mut count, mut size) = (0i32, 0i32);
    if config.GetNumberOfCapabilities(&mut count, &mut size).is_err() {
        return Vec::new();
    }
    let mut caps = vec![0u8; size.max(std::mem::size_of::<VIDEO_STREAM_CONFIG_CAPS>() as i32) as usize];
    let mut out = Vec::new();
    for i in 0..count {
        let mut pmt = std::ptr::null_mut();
        if config.GetStreamCaps(i, &mut pmt, caps.as_mut_ptr()).is_err() || pmt.is_null() {
            continue;
        }
        let mt = MediaType(pmt);
        let Some(pixel) = pixel_format(&mt.get().subtype) else { continue };
        let Some((w, h, interval)) = video_info(mt.get()) else { continue };
        let cc = &*(caps.as_ptr() as *const VIDEO_STREAM_CONFIG_CAPS);
        let mut rates = vec![fps_of(interval)];
        if cc.MinFrameInterval > 0 {
            rates.push(fps_of(cc.MinFrameInterval));
        }
        for fps in rates {
            let f = VideoFormat { pixel, width: w as u32, height: h.unsigned_abs(), fps };
            if fps > 0.0 && !out.iter().any(|(o, _): &(VideoFormat, usize)| *o == f) {
                out.push((f, i as usize));
            }
        }
    }
    out
}

unsafe fn source_filter(d: &Device) -> Result<IBaseFilter, CaptureError> {
    d.moniker.BindToObject::<_, _, IBaseFilter>(None, None).map_err(|e| err("open device", e))
}

unsafe fn stream_config(builder: &ICaptureGraphBuilder2, source: &IBaseFilter) -> Result<IAMStreamConfig, CaptureError> {
    let mut p = std::ptr::null_mut();
    builder
        .FindInterface(Some(&PIN_CATEGORY_CAPTURE), Some(&MEDIATYPE_Video), source, &IAMStreamConfig::IID, &mut p)
        .map_err(|e| err("find capture pin", e))?;
    Ok(IAMStreamConfig::from_raw(p))
}

unsafe fn graph() -> Result<(IGraphBuilder, ICaptureGraphBuilder2), CaptureError> {
    let graph: IGraphBuilder = CoCreateInstance(&CLSID_FilterGraph, None, CLSCTX_INPROC_SERVER).map_err(|e| err("FilterGraph", e))?;
    let builder: ICaptureGraphBuilder2 =
        CoCreateInstance(&CLSID_CaptureGraphBuilder2, None, CLSCTX_INPROC_SERVER).map_err(|e| err("CaptureGraphBuilder2", e))?;
    builder.SetFiltergraph(&graph).map_err(|e| err("SetFiltergraph", e))?;
    Ok((graph, builder))
}

/// Video capture devices DirectShow sees, with their formats. `skip` lists
/// device paths already handled by Media Foundation.
pub fn devices(skip: &[String]) -> Result<Vec<VideoDevice>, CaptureError> {
    let mut out = Vec::new();
    for d in enumerate(&CLSID_VideoInputDeviceCategory)? {
        if skip.iter().any(|s| s.eq_ignore_ascii_case(&d.path)) {
            continue;
        }
        let formats = unsafe {
            (|| {
                let source = source_filter(&d).ok()?;
                let (graph, builder) = graph().ok()?;
                graph.AddFilter(&source, w!("source")).ok()?;
                let config = stream_config(&builder, &source).ok()?;
                Some(formats(&config).into_iter().map(|(f, _)| f).collect::<Vec<_>>())
            })()
            .unwrap_or_default()
        };
        out.push(VideoDevice { id: format!("{PREFIX}{}", d.path), name: d.name, formats });
    }
    Ok(out)
}

/// Does the device have an audio output pin (a card that carries its
/// sound, like the GC573)?
unsafe fn has_audio_pin(filter: &IBaseFilter) -> bool {
    let Ok(pins) = filter.EnumPins() else { return false };
    loop {
        let mut p = [None];
        if pins.Next(&mut p, None) != S_OK {
            return false;
        }
        let Some(pin) = p[0].take() else { return false };
        if pin.QueryDirection().is_ok_and(|d| d == PINDIR_OUTPUT) {
            if let Ok(types) = pin.EnumMediaTypes() {
                loop {
                    let mut t = [std::ptr::null_mut()];
                    if types.Next(&mut t, None) != S_OK || t[0].is_null() {
                        break;
                    }
                    if MediaType(t[0]).get().majortype == MEDIATYPE_Audio {
                        return true;
                    }
                }
            }
        }
    }
}

/// Audio that comes from a capture card's own pin (offered alongside
/// WASAPI inputs). Ids are `card:` + the video device's id.
pub fn audio_devices() -> Vec<AudioDevice> {
    enumerate(&CLSID_VideoInputDeviceCategory)
        .unwrap_or_default()
        .into_iter()
        .filter(|d| unsafe { source_filter(d).is_ok_and(|f| has_audio_pin(&f)) })
        .map(|d| AudioDevice { id: format!("{}{PREFIX}{}", crate::audio::CARD_AUDIO, d.path), name: format!("{} (card audio)", d.name) })
        .collect()
}

type FrameResult = Result<VideoFrame, CaptureError>;

struct Shared {
    tx: SyncSender<FrameResult>,
    stop: AtomicBool,
    /// What arrives: pixel format, width, height, top row first.
    layout: Mutex<(PixelFormat, u32, u32, bool)>,
}

#[implement(ISampleGrabberCB)]
struct Callback(Arc<Shared>);

impl ISampleGrabberCB_Impl for Callback_Impl {
    unsafe fn SampleCB(&self, _time: f64, _sample: *mut c_void) -> HRESULT {
        S_OK
    }

    unsafe fn BufferCB(&self, _time: f64, buffer: *mut u8, len: i32) -> HRESULT {
        let st = &self.0;
        if st.stop.load(Ordering::Relaxed) {
            return S_OK;
        }
        if buffer.is_null() || len <= 0 {
            return E_POINTER;
        }
        dbg(&format!("buffer {len}"));
        let data = std::slice::from_raw_parts(buffer, len as usize);
        let (pixel, width, height, top_down) = *st.layout.lock().unwrap();
        let (data, stride) = match pixel {
            PixelFormat::Mjpeg => (data.to_vec(), 0),
            PixelFormat::Bgra => {
                let row = (width * 4) as usize;
                let rows = height as usize;
                if data.len() < row * rows {
                    return S_OK;
                }
                if top_down {
                    (data[..row * rows].to_vec(), row as u32)
                } else {
                    // RGB in DirectShow is bottom-up unless the height is negative.
                    let mut out = Vec::with_capacity(row * rows);
                    for r in (0..rows).rev() {
                        out.extend_from_slice(&data[r * row..(r + 1) * row]);
                    }
                    (out, row as u32)
                }
            }
            PixelFormat::Yuy2 => (data.to_vec(), width * 2),
            PixelFormat::Nv12 => (data.to_vec(), width),
        };
        let frame = VideoFrame { format: pixel, width, height, stride, data, timestamp: now_100ns() };
        let _ = st.tx.try_send(Ok(frame));
        S_OK
    }
}

/// Card audio: one PCM buffer per callback, converted to 16-bit.
#[implement(ISampleGrabberCB)]
struct AudioCallback {
    tx: SyncSender<Result<AudioChunk, CaptureError>>,
    stop: Arc<AtomicBool>,
    channels: u16,
    rate: u32,
    bits: u16,
    float: bool,
}

impl ISampleGrabberCB_Impl for AudioCallback_Impl {
    unsafe fn SampleCB(&self, _time: f64, _sample: *mut c_void) -> HRESULT {
        S_OK
    }

    unsafe fn BufferCB(&self, _time: f64, buffer: *mut u8, len: i32) -> HRESULT {
        if self.stop.load(Ordering::Relaxed) || buffer.is_null() || len <= 0 {
            return S_OK;
        }
        let data = std::slice::from_raw_parts(buffer, len as usize);
        let samples: Vec<i16> = match (self.bits, self.float) {
            (16, false) => data.chunks_exact(2).map(|b| i16::from_le_bytes([b[0], b[1]])).collect(),
            (24, false) => data.chunks_exact(3).map(|b| i16::from_le_bytes([b[1], b[2]])).collect(),
            (32, false) => data.chunks_exact(4).map(|b| i16::from_le_bytes([b[2], b[3]])).collect(),
            (32, true) => data
                .chunks_exact(4)
                .map(|b| (f32::from_le_bytes([b[0], b[1], b[2], b[3]]).clamp(-1.0, 1.0) * 32767.0) as i16)
                .collect(),
            _ => return S_OK,
        };
        let frames = samples.len() as i64 / self.channels.max(1) as i64;
        let timestamp = now_100ns() - frames * 10_000_000 / self.rate.max(1) as i64;
        let _ = self.tx.try_send(Ok(AudioChunk { channels: self.channels, rate: self.rate, samples, timestamp }));
        S_OK
    }
}

/// Connect the card's audio pin to a second grabber, with small buffers
/// (DirectShow's default is about half a second, far behind the picture).
unsafe fn connect_audio(graph: &IGraphBuilder, builder: &ICaptureGraphBuilder2, source: &IBaseFilter) -> Result<(AudioCapture, ISampleGrabber, ISampleGrabberCB), CaptureError> {
    let mut p = std::ptr::null_mut();
    builder
        .FindInterface(Some(&PIN_CATEGORY_CAPTURE), Some(&MEDIATYPE_Audio), source, &IAMStreamConfig::IID, &mut p)
        .map_err(|e| err("find audio pin", e))?;
    let config = IAMStreamConfig::from_raw(p);
    let current = MediaType(config.GetFormat().map_err(|e| err("audio format", e))?);
    let wf = (current.get().formattype == FORMAT_WaveFormatEx && current.get().cbFormat as usize >= std::mem::size_of::<WAVEFORMATEX>())
        .then(|| std::ptr::read_unaligned(current.get().pbFormat as *const WAVEFORMATEX))
        .ok_or_else(|| CaptureError::Format("audio pin has no wave format".into()))?;
    drop(current);
    let block = wf.nBlockAlign.max(1) as i32;
    let ten_ms = (wf.nSamplesPerSec as i32 / 100).max(1) * block;
    let mut p = std::ptr::null_mut();
    if builder
        .FindInterface(Some(&PIN_CATEGORY_CAPTURE), Some(&MEDIATYPE_Audio), source, &IAMBufferNegotiation::IID, &mut p)
        .is_ok()
    {
        let neg = IAMBufferNegotiation::from_raw(p);
        let props = ALLOCATOR_PROPERTIES { cBuffers: 8, cbBuffer: ten_ms, cbAlign: -1, cbPrefix: -1 };
        let _ = neg.SuggestAllocatorProperties(&props);
    }

    let filter: IBaseFilter = CoCreateInstance(&CLSID_SAMPLE_GRABBER, None, CLSCTX_INPROC_SERVER).map_err(|e| err("Sample Grabber", e))?;
    let grabber: ISampleGrabber = filter.cast().map_err(|e| err("ISampleGrabber", e))?;
    let want = AM_MEDIA_TYPE { majortype: MEDIATYPE_Audio, ..Default::default() };
    hr("audio grabber type", grabber.SetMediaType(&want))?;
    hr("audio grabber", grabber.SetBufferSamples(false.into()))?;
    graph.AddFilter(&filter, w!("audio grabber")).map_err(|e| err("add audio grabber", e))?;
    let null: IBaseFilter = CoCreateInstance(&CLSID_NULL_RENDERER, None, CLSCTX_INPROC_SERVER).map_err(|e| err("Null Renderer", e))?;
    graph.AddFilter(&null, w!("audio null")).map_err(|e| err("add audio renderer", e))?;
    builder
        .RenderStream(Some(&PIN_CATEGORY_CAPTURE), &MEDIATYPE_Audio, source, &filter, &null)
        .map_err(|e| err("connect audio", e))?;

    let mut connected = AM_MEDIA_TYPE::default();
    hr("audio connected type", grabber.GetConnectedMediaType(&mut connected))?;
    let wf = if connected.formattype == FORMAT_WaveFormatEx && connected.cbFormat as usize >= std::mem::size_of::<WAVEFORMATEX>() {
        std::ptr::read_unaligned(connected.pbFormat as *const WAVEFORMATEX)
    } else {
        wf
    };
    free_format(&mut connected);
    // WAVE_FORMAT_IEEE_FLOAT = 3; EXTENSIBLE (0xFFFE) at 32 bits is float in practice.
    let float = wf.wFormatTag == 3 || (wf.wFormatTag == 0xFFFE && wf.wBitsPerSample == 32);
    let (tx, rx) = mpsc::sync_channel(256);
    let stop = Arc::new(AtomicBool::new(false));
    let cb: ISampleGrabberCB = AudioCallback {
        tx,
        stop: stop.clone(),
        channels: wf.nChannels,
        rate: wf.nSamplesPerSec,
        bits: wf.wBitsPerSample,
        float,
    }
    .into();
    hr("audio callback", grabber.SetCallback(cb.as_raw(), 1))?;
    Ok((AudioCapture::from_channel(rx, stop, wf.nChannels, wf.nSamplesPerSec), grabber, cb))
}

pub struct Reader {
    rx: Receiver<FrameResult>,
    shared: Arc<Shared>,
    /// Card audio, until the caller takes it.
    pub audio: Option<AudioCapture>,
    _audio_parts: Option<(ISampleGrabber, ISampleGrabberCB)>,
    control: IMediaControl,
    // Kept alive while running.
    _graph: IGraphBuilder,
    _grabber: ISampleGrabber,
    _callback: ISampleGrabberCB,
}

impl Reader {
    pub fn open(id: &str, wanted: Option<VideoFormat>, output: Output, audio: bool) -> Result<(Self, VideoFormat), CaptureError> {
        let path = id.strip_prefix(PREFIX).unwrap_or(id);
        let devices = enumerate(&CLSID_VideoInputDeviceCategory)?;
        let d = devices
            .iter()
            .find(|d| d.path.eq_ignore_ascii_case(path))
            .or_else(|| devices.iter().find(|d| d.name == path))
            .ok_or_else(|| CaptureError::NotFound(id.to_string()))?;
        unsafe { open_device(d, wanted, output, audio) }
    }

    pub fn next_frame(&mut self, timeout: Duration) -> Result<Option<VideoFrame>, CaptureError> {
        match self.rx.recv_timeout(timeout) {
            Ok(r) => r.map(Some),
            Err(RecvTimeoutError::Timeout) => Ok(None),
            Err(RecvTimeoutError::Disconnected) => Err(CaptureError::Lost("capture stopped".into())),
        }
    }
}

impl Drop for Reader {
    fn drop(&mut self) {
        init();
        self.shared.stop.store(true, Ordering::Relaxed);
        unsafe {
            let _ = self.control.Stop();
            let _ = self._grabber.SetCallback(std::ptr::null_mut(), 1);
            if let Some((g, _)) = &self._audio_parts {
                let _ = g.SetCallback(std::ptr::null_mut(), 1);
            }
        }
    }
}

unsafe fn open_device(d: &Device, wanted: Option<VideoFormat>, output: Output, audio: bool) -> Result<(Reader, VideoFormat), CaptureError> {
    let source = source_filter(d)?;
    let (graph, builder) = graph()?;
    graph.AddFilter(&source, w!("source")).map_err(|e| err("add source", e))?;
    let config = stream_config(&builder, &source)?;
    let offered = formats(&config);
    let list: Vec<VideoFormat> = offered.iter().map(|(f, _)| *f).collect();
    let format = match wanted {
        Some(w) => list
            .iter()
            .copied()
            .find(|f| f.pixel == w.pixel && f.width == w.width && f.height == w.height && (f.fps - w.fps).abs() < 0.5)
            .ok_or_else(|| CaptureError::Format(format!("{w} not offered")))?,
        None => best_format(&list).ok_or_else(|| CaptureError::Format("no usable formats".into()))?,
    };
    let index = offered.iter().find(|(f, _)| *f == format).map(|(_, i)| *i).unwrap();
    let mut caps = vec![0u8; 512];
    let mut pmt = std::ptr::null_mut();
    config.GetStreamCaps(index as i32, &mut pmt, caps.as_mut_ptr()).map_err(|e| err("GetStreamCaps", e))?;
    let mt = MediaType(pmt);
    set_frame_time(&mut *mt.0, (10_000_000.0 / format.fps).round() as i64);
    config.SetFormat(mt.0).map_err(|e| err("set capture format", e))?;
    drop(mt);

    dbg("grabber");
    let grabber_filter: IBaseFilter =
        CoCreateInstance(&CLSID_SAMPLE_GRABBER, None, CLSCTX_INPROC_SERVER).map_err(|e| err("Sample Grabber", e))?;
    let grabber: ISampleGrabber = grabber_filter.cast().map_err(|e| err("ISampleGrabber", e))?;
    // Always the card's own format: asking DirectShow to convert makes it
    // insert converter filters, and some cards' filters crash in there
    // (GC573). Callers convert (`Output::Bgra` is a preference, see lib.rs).
    let _ = output;
    let out_pixel = format.pixel;
    let want = AM_MEDIA_TYPE { majortype: MEDIATYPE_Video, subtype: subtype_of(out_pixel), ..Default::default() };
    hr("grabber type", grabber.SetMediaType(&want))?;
    hr("grabber", grabber.SetBufferSamples(false.into()))?;
    hr("grabber", grabber.SetOneShot(false.into()))?;
    graph.AddFilter(&grabber_filter, w!("grabber")).map_err(|e| err("add grabber", e))?;
    let null: IBaseFilter = CoCreateInstance(&CLSID_NULL_RENDERER, None, CLSCTX_INPROC_SERVER).map_err(|e| err("Null Renderer", e))?;
    graph.AddFilter(&null, w!("null")).map_err(|e| err("add renderer", e))?;
    dbg("render");
    builder
        .RenderStream(Some(&PIN_CATEGORY_CAPTURE), &MEDIATYPE_Video, &source, &grabber_filter, &null)
        .map_err(|e| err(&format!("connect {}", out_pixel.name()), e))?;

    dbg("connected");
    // What actually connected (size and orientation).
    let mut connected = AM_MEDIA_TYPE::default();
    hr("connected type", grabber.GetConnectedMediaType(&mut connected))?;
    let (w, h, _) = video_info(&connected).unwrap_or((format.width as i32, format.height as i32, 0));
    let pixel = pixel_format(&connected.subtype).unwrap_or(out_pixel);
    free_format(&mut connected);

    dbg("channel");
    let (tx, rx) = mpsc::sync_channel(3);
    let shared = Arc::new(Shared {
        tx,
        stop: AtomicBool::new(false),
        // YUV is always top row first; RGB unless the height is negative.
        layout: Mutex::new((pixel, w as u32, h.unsigned_abs(), h < 0 || pixel != PixelFormat::Bgra)),
    });
    let callback: ISampleGrabberCB = Callback(shared.clone()).into();
    hr("grabber callback", grabber.SetCallback(callback.as_raw(), 1))?;
    // No reference clock: deliver frames as they arrive, never held back.
    if let Ok(filter) = graph.cast::<IMediaFilter>() {
        let _ = filter.SetSyncSource(None);
    }
    // Card audio in the same graph (the card can only be opened once).
    let (audio_capture, audio_parts) = if audio {
        match connect_audio(&graph, &builder, &source) {
            Ok((capture, g, cb)) => (Some(capture), Some((g, cb))),
            Err(e) => {
                dbg(&format!("audio: {e}"));
                (None, None)
            }
        }
    } else {
        (None, None)
    };
    let control: IMediaControl = graph.cast().map_err(|e| err("IMediaControl", e))?;
    dbg("run");
    control.Run().map_err(|e| err("start", e))?;
    let format = VideoFormat { width: w as u32, height: h.unsigned_abs(), ..format };
    let reader = Reader {
        rx,
        shared,
        audio: audio_capture,
        _audio_parts: audio_parts,
        control,
        _graph: graph,
        _grabber: grabber,
        _callback: callback,
    };
    Ok((reader, format))
}

/// Diagnostics: DirectShow audio inputs, and each video device's output pins
/// with the major types they offer.
#[doc(hidden)]
pub fn describe() -> String {
    let mut out = String::new();
    out += "audio inputs:\n";
    for d in enumerate(&CLSID_AudioInputDeviceCategory).unwrap_or_default() {
        out += &format!("  {} [{}]\n", d.name, d.path);
    }
    for d in enumerate(&CLSID_VideoInputDeviceCategory).unwrap_or_default() {
        out += &format!("video device {}\n", d.name);
        unsafe {
            let Ok(filter) = source_filter(&d) else { continue };
            let Ok(pins) = filter.EnumPins() else { continue };
            loop {
                let mut p = [None];
                if pins.Next(&mut p, None) != S_OK {
                    break;
                }
                let Some(pin) = p[0].take() else { break };
                let mut info = PIN_INFO::default();
                let _ = pin.QueryPinInfo(&mut info);
                let name = String::from_utf16_lossy(&info.achName).trim_end_matches('\0').to_string();
                std::mem::ManuallyDrop::drop(&mut info.pFilter);
                let mut majors = Vec::new();
                if let Ok(types) = pin.EnumMediaTypes() {
                    loop {
                        let mut t = [std::ptr::null_mut()];
                        if types.Next(&mut t, None) != S_OK || t[0].is_null() {
                            break;
                        }
                        let mt = MediaType(t[0]);
                        let m = if mt.get().majortype == MEDIATYPE_Video {
                            "video"
                        } else if mt.get().majortype == MEDIATYPE_Audio {
                            "audio"
                        } else {
                            "other"
                        };
                        if !majors.contains(&m) {
                            majors.push(m);
                        }
                    }
                }
                out += &format!("  pin '{name}' dir {:?} types {majors:?}\n", info.dir);
            }
        }
    }
    out
}

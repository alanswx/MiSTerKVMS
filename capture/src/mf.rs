//! Windows: Media Foundation. Devices through `MFEnumDeviceSources`, frames
//! through an `IMFSourceReader` in async mode (a callback per frame).

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::sync::{Arc, Condvar, Mutex};
use std::time::Duration;

use windows::core::{implement, Interface, Ref, GUID, HRESULT, PWSTR};
use windows::Win32::Foundation::E_ACCESSDENIED;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

use crate::clock::DeviceClock;
use crate::{best_format, CaptureError, Output, PixelFormat, VideoDevice, VideoFormat, VideoFrame};

/// MF_E_HW_MFT_FAILED_START_STREAMING and friends: what a card in use by
/// another program (usually OBS) reports.
const IN_USE: [i32; 3] = [0xC00D3704u32 as i32, 0xC00D3EA3u32 as i32, E_ACCESSDENIED.0];
/// MF_E_VIDEO_RECORDING_DEVICE_INVALIDATED / MF_E_SHUTDOWN etc.
const LOST: [i32; 2] = [0xC00D4E86u32 as i32, 0xC00D3E85u32 as i32];
const VIDEO_STREAM: u32 = MF_SOURCE_READER_FIRST_VIDEO_STREAM.0 as u32;

fn err(context: &str, e: windows::core::Error) -> CaptureError {
    let code = e.code().0;
    let msg = format!("{context}: {} (0x{:08x})", e.message(), code as u32);
    if IN_USE.contains(&code) {
        CaptureError::InUse(msg)
    } else if LOST.contains(&code) {
        CaptureError::Lost(msg)
    } else {
        CaptureError::Other(msg)
    }
}

/// COM + Media Foundation for this thread. Both are reference counted, so
/// calling this on every thread that uses MF is fine.
fn init() -> Result<(), CaptureError> {
    unsafe {
        // S_FALSE (already initialised) is fine; RPC_E_CHANGED_MODE means the
        // thread is STA, which MF also accepts.
        let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
        MFStartup(MF_VERSION, MFSTARTUP_NOSOCKET).map_err(|e| err("MFStartup", e))
    }
}

fn pixel_format(subtype: &GUID) -> Option<PixelFormat> {
    if *subtype == MFVideoFormat_MJPG {
        Some(PixelFormat::Mjpeg)
    } else if *subtype == MFVideoFormat_NV12 {
        Some(PixelFormat::Nv12)
    } else if *subtype == MFVideoFormat_YUY2 {
        Some(PixelFormat::Yuy2)
    } else if *subtype == MFVideoFormat_RGB32 || *subtype == MFVideoFormat_ARGB32 {
        Some(PixelFormat::Bgra)
    } else {
        None
    }
}

fn subtype_of(pixel: PixelFormat) -> GUID {
    match pixel {
        PixelFormat::Mjpeg => MFVideoFormat_MJPG,
        PixelFormat::Nv12 => MFVideoFormat_NV12,
        PixelFormat::Yuy2 => MFVideoFormat_YUY2,
        PixelFormat::Bgra => MFVideoFormat_RGB32,
    }
}

fn split(v: u64) -> (u32, u32) {
    ((v >> 32) as u32, v as u32)
}

fn video_format(t: &IMFMediaType) -> Option<VideoFormat> {
    unsafe {
        let pixel = pixel_format(&t.GetGUID(&MF_MT_SUBTYPE).ok()?)?;
        let (width, height) = split(t.GetUINT64(&MF_MT_FRAME_SIZE).ok()?);
        let (num, den) = split(t.GetUINT64(&MF_MT_FRAME_RATE).unwrap_or(0));
        let fps = if den > 0 { num as f64 / den as f64 } else { 0.0 };
        Some(VideoFormat { pixel, width, height, fps })
    }
}

fn string(a: &IMFActivate, key: &GUID) -> Option<String> {
    unsafe {
        let mut p = PWSTR::null();
        let mut len = 0;
        a.GetAllocatedString(key, &mut p, &mut len).ok()?;
        let s = p.to_string().ok();
        CoTaskMemFree(Some(p.0 as *const _));
        s
    }
}

fn activates() -> Result<Vec<IMFActivate>, CaptureError> {
    unsafe {
        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 1).map_err(|e| err("MFCreateAttributes", e))?;
        let attrs = attrs.unwrap();
        attrs
            .SetGUID(&MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_GUID)
            .map_err(|e| err("SetGUID", e))?;
        let mut list: *mut Option<IMFActivate> = std::ptr::null_mut();
        let mut count = 0u32;
        MFEnumDeviceSources(&attrs, &mut list, &mut count).map_err(|e| err("MFEnumDeviceSources", e))?;
        let mut out = Vec::with_capacity(count as usize);
        for i in 0..count as usize {
            if let Some(a) = (*list.add(i)).take() {
                out.push(a);
            }
        }
        CoTaskMemFree(Some(list as *const _));
        Ok(out)
    }
}

fn identity(a: &IMFActivate) -> (String, String) {
    let name = string(a, &MF_DEVSOURCE_ATTRIBUTE_FRIENDLY_NAME).unwrap_or_default();
    let id = string(a, &MF_DEVSOURCE_ATTRIBUTE_SOURCE_TYPE_VIDCAP_SYMBOLIC_LINK).unwrap_or_else(|| name.clone());
    (id, name)
}

/// The formats a source offers, from its first video stream.
fn native_types(source: &IMFMediaSource) -> Result<(IMFMediaTypeHandler, Vec<(IMFMediaType, VideoFormat)>), CaptureError> {
    unsafe {
        let pd = source.CreatePresentationDescriptor().map_err(|e| err("CreatePresentationDescriptor", e))?;
        let mut selected = Default::default();
        let mut sd = None;
        pd.GetStreamDescriptorByIndex(0, &mut selected, &mut sd).map_err(|e| err("GetStreamDescriptorByIndex", e))?;
        let handler = sd.unwrap().GetMediaTypeHandler().map_err(|e| err("GetMediaTypeHandler", e))?;
        let count = handler.GetMediaTypeCount().map_err(|e| err("GetMediaTypeCount", e))?;
        let mut types = Vec::new();
        for i in 0..count {
            if let Ok(t) = handler.GetMediaTypeByIndex(i) {
                if let Some(f) = video_format(&t) {
                    types.push((t, f));
                }
            }
        }
        Ok((handler, types))
    }
}

pub fn devices() -> Result<Vec<VideoDevice>, CaptureError> {
    init()?;
    let mut out = Vec::new();
    for a in activates()? {
        let (id, name) = identity(&a);
        // Listing formats needs the source; a card in use by another app may
        // still list them (it fails only when streaming starts).
        let formats = match unsafe { a.ActivateObject::<IMFMediaSource>() } {
            Ok(source) => {
                let mut formats: Vec<VideoFormat> =
                    native_types(&source).map(|(_, t)| t.into_iter().map(|(_, f)| f).collect()).unwrap_or_default();
                formats.dedup();
                unsafe {
                    let _ = source.Shutdown();
                    let _ = a.ShutdownObject();
                }
                formats
            }
            Err(_) => Vec::new(),
        };
        out.push(VideoDevice { id, name, formats });
    }
    Ok(out)
}

type FrameResult = Result<VideoFrame, CaptureError>;

/// What the reader's callback shares with `Reader`.
struct CallbackState {
    tx: SyncSender<FrameResult>,
    stop: AtomicBool,
    /// Taken out when closing, so the callback stops asking for frames.
    reader: Mutex<Option<IMFSourceReader>>,
    clock: Mutex<DeviceClock>,
    output: Output,
    flushed: (Mutex<bool>, Condvar),
}

/// Media Foundation calls this on its own threads with each frame (async
/// mode). Unlike a blocking ReadSample, nothing waits forever when a card
/// stops sending: closing is a Flush.
#[implement(IMFSourceReaderCallback)]
struct Callback(Arc<CallbackState>);

impl IMFSourceReaderCallback_Impl for Callback_Impl {
    fn OnReadSample(&self, hrstatus: HRESULT, _stream: u32, flags: u32, timestamp: i64, sample: Ref<IMFSample>) -> windows::core::Result<()> {
        let st = &self.0;
        if st.stop.load(Ordering::Relaxed) {
            return Ok(());
        }
        let reader = st.reader.lock().unwrap().clone();
        let Some(reader) = reader else { return Ok(()) };
        let result = if hrstatus.is_err() {
            Err(err("ReadSample", windows::core::Error::from(hrstatus)))
        } else if flags & MF_SOURCE_READERF_ERROR.0 as u32 != 0 {
            Err(CaptureError::Lost("stream error".into()))
        } else if flags & MF_SOURCE_READERF_ENDOFSTREAM.0 as u32 != 0 {
            Err(CaptureError::Lost("end of stream".into()))
        } else {
            match sample.as_ref() {
                Some(sample) => unsafe { frame_from(&reader, sample, timestamp, st.output, &mut st.clock.lock().unwrap()) }.map(Some),
                None => Ok(None),
            }
        };
        match result {
            Ok(Some(frame)) => {
                // The consumer wants the newest frame; if it's behind, drop.
                let _ = st.tx.try_send(Ok(frame));
            }
            Ok(None) => {}
            Err(e) => {
                let _ = st.tx.try_send(Err(e));
                return Ok(());
            }
        }
        unsafe {
            let _ = reader.ReadSample(VIDEO_STREAM, 0, None, None, None, None);
        }
        Ok(())
    }

    fn OnFlush(&self, _stream: u32) -> windows::core::Result<()> {
        let (lock, cv) = &self.0.flushed;
        *lock.lock().unwrap() = true;
        cv.notify_all();
        Ok(())
    }

    fn OnEvent(&self, _stream: u32, _event: Ref<IMFMediaEvent>) -> windows::core::Result<()> {
        Ok(())
    }
}

pub struct Reader {
    rx: Receiver<FrameResult>,
    state: Arc<CallbackState>,
    source: IMFMediaSource,
}

impl Reader {
    pub fn open(id: &str, wanted: Option<VideoFormat>, output: Output) -> Result<(Self, VideoFormat), CaptureError> {
        // A few frames of slack: the consumer only ever wants the newest.
        let (tx, rx) = mpsc::sync_channel(3);
        let state = Arc::new(CallbackState {
            tx,
            stop: AtomicBool::new(false),
            reader: Mutex::new(None),
            clock: Mutex::new(DeviceClock::default()),
            output,
            flushed: (Mutex::new(false), Condvar::new()),
        });
        let callback: IMFSourceReaderCallback = Callback(state.clone()).into();
        let (reader, source, format) = setup(id, wanted, output, &callback)?;
        *state.reader.lock().unwrap() = Some(reader.clone());
        unsafe {
            reader.ReadSample(VIDEO_STREAM, 0, None, None, None, None).map_err(|e| err("ReadSample", e))?;
        }
        Ok((Self { rx, state, source }, format))
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
        let _ = init();
        self.state.stop.store(true, Ordering::Relaxed);
        let reader = self.state.reader.lock().unwrap().take();
        if let Some(reader) = reader {
            unsafe {
                // Cancels the pending read; OnFlush follows.
                if reader.Flush(MF_SOURCE_READER_ALL_STREAMS.0 as u32).is_ok() {
                    let (lock, cv) = &self.state.flushed;
                    let done = lock.lock().unwrap();
                    let _ = cv.wait_timeout_while(done, Duration::from_secs(2), |d| !*d);
                }
            }
            drop(reader);
        }
        unsafe {
            let _ = self.source.Shutdown();
        }
    }
}

fn setup(
    id: &str,
    wanted: Option<VideoFormat>,
    output: Output,
    callback: &IMFSourceReaderCallback,
) -> Result<(IMFSourceReader, IMFMediaSource, VideoFormat), CaptureError> {
    init()?;
    let all = activates()?;
    let activate = all
        .iter()
        .find(|a| identity(a).0 == id)
        .or_else(|| all.iter().find(|a| identity(a).1 == id))
        .ok_or_else(|| CaptureError::NotFound(id.to_string()))?;
    unsafe {
        let source: IMFMediaSource = activate.ActivateObject().map_err(|e| err("ActivateObject", e))?;
        let (handler, types) = native_types(&source)?;
        let formats: Vec<VideoFormat> = types.iter().map(|(_, f)| *f).collect();
        let format = match wanted {
            Some(w) => formats
                .iter()
                .copied()
                .find(|f| f.pixel == w.pixel && f.width == w.width && f.height == w.height && (f.fps - w.fps).abs() < 0.5)
                .ok_or_else(|| CaptureError::Format(format!("{w} not offered")))?,
            None => best_format(&formats).ok_or_else(|| CaptureError::Format("no usable formats".into()))?,
        };
        let (native, _) = types.iter().find(|(_, f)| *f == format).unwrap();
        handler.SetCurrentMediaType(native).map_err(|e| err("set capture format", e))?;

        let mut attrs = None;
        MFCreateAttributes(&mut attrs, 3).map_err(|e| err("MFCreateAttributes", e))?;
        let attrs = attrs.unwrap();
        attrs.SetUINT32(&MF_LOW_LATENCY, 1).ok();
        attrs.SetUnknown(&MF_SOURCE_READER_ASYNC_CALLBACK, callback).map_err(|e| err("async callback", e))?;
        if output == Output::Bgra && format.pixel != PixelFormat::Bgra {
            // Lets the reader insert the MJPEG decoder and colour converter.
            attrs.SetUINT32(&MF_SOURCE_READER_ENABLE_ADVANCED_VIDEO_PROCESSING, 1).ok();
        }
        let reader = MFCreateSourceReaderFromMediaSource(&source, &attrs).map_err(|e| err("create reader", e))?;
        let out_type = MFCreateMediaType().map_err(|e| err("MFCreateMediaType", e))?;
        out_type.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video).ok();
        let out_pixel = if output == Output::Bgra { PixelFormat::Bgra } else { format.pixel };
        out_type.SetGUID(&MF_MT_SUBTYPE, &subtype_of(out_pixel)).ok();
        out_type
            .SetUINT64(&MF_MT_FRAME_SIZE, ((format.width as u64) << 32) | format.height as u64)
            .ok();
        reader
            .SetCurrentMediaType(VIDEO_STREAM, None, &out_type)
            .map_err(|e| err(&format!("output {}", out_pixel.name()), e))?;
        reader.SetStreamSelection(VIDEO_STREAM, true).ok();
        Ok((reader, source, format))
    }
}

unsafe fn frame_from(
    reader: &IMFSourceReader,
    sample: &IMFSample,
    ts: i64,
    output: Output,
    clock: &mut DeviceClock,
) -> Result<VideoFrame, CaptureError> {
    let current = reader.GetCurrentMediaType(VIDEO_STREAM).map_err(|e| err("GetCurrentMediaType", e))?;
    let format = video_format(&current).ok_or_else(|| CaptureError::Format("unknown output type".into()))?;
    let buffer = sample.ConvertToContiguousBuffer().map_err(|e| err("ConvertToContiguousBuffer", e))?;
    let timestamp = clock.wall(ts);

    let (data, stride) = if output == Output::Bgra || !format.pixel.is_compressed() {
        copy_2d(&buffer, &format)?
    } else {
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut len)).map_err(|e| err("Lock", e))?;
        let data = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        let _ = buffer.Unlock();
        (data, 0)
    };
    Ok(VideoFrame { format: format.pixel, width: format.width, height: format.height, stride, data, timestamp })
}

/// Copy an uncompressed picture top row first, whatever the buffer's pitch
/// (RGB32 from the video processor can be bottom-up).
unsafe fn copy_2d(buffer: &IMFMediaBuffer, format: &VideoFormat) -> Result<(Vec<u8>, u32), CaptureError> {
    let row = match format.pixel {
        PixelFormat::Bgra => format.width * 4,
        PixelFormat::Yuy2 => format.width * 2,
        PixelFormat::Nv12 => format.width,
        PixelFormat::Mjpeg => return Err(CaptureError::Format("can't copy MJPEG as pixels".into())),
    } as usize;
    let rows = match format.pixel {
        PixelFormat::Nv12 => format.height as usize * 3 / 2,
        _ => format.height as usize,
    };
    let mut out = vec![0u8; row * rows];
    if let Ok(b2) = buffer.cast::<IMF2DBuffer>() {
        let mut scan0 = std::ptr::null_mut();
        let mut pitch = 0i32;
        b2.Lock2D(&mut scan0, &mut pitch).map_err(|e| err("Lock2D", e))?;
        for r in 0..rows {
            let src = scan0.offset(r as isize * pitch as isize);
            std::ptr::copy_nonoverlapping(src, out[r * row..].as_mut_ptr(), row);
        }
        let _ = b2.Unlock2D();
    } else {
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut len)).map_err(|e| err("Lock", e))?;
        if (len as usize) < row * rows {
            let _ = buffer.Unlock();
            return Err(CaptureError::Format(format!("short frame ({len} bytes)")));
        }
        std::ptr::copy_nonoverlapping(ptr, out.as_mut_ptr(), row * rows);
        let _ = buffer.Unlock();
    }
    Ok((out, row as u32))
}

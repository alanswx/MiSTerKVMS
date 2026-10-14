//! macOS: AVFoundation. Capture cards are "external" video devices
//! (macOS 14+). Frames come from an AVCaptureVideoDataOutput, always as
//! BGRA (macOS converts and decodes MJPEG in hardware), through a delegate
//! called on a serial dispatch queue.
//!
//! Needs the Camera permission: a signed app bundle with
//! NSCameraUsageDescription; the first open asks the user.

#![allow(non_snake_case)] // Objective-C method names.

use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender};
use std::time::Duration;

use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::{AnyObject, Bool, ProtocolObject};
use objc2::{define_class, msg_send, AllocAnyThread, DefinedClass};
use objc2_av_foundation::*;
use objc2_core_media::{CMFormatDescription, CMSampleBuffer, CMTime, CMVideoFormatDescriptionGetDimensions};
use objc2_core_video::{
    kCVPixelBufferPixelFormatTypeKey, kCVPixelFormatType_32BGRA, CVPixelBufferGetBaseAddress, CVPixelBufferGetBytesPerRow,
    CVPixelBufferGetHeight, CVPixelBufferGetWidth, CVPixelBufferLockBaseAddress, CVPixelBufferLockFlags, CVPixelBufferUnlockBaseAddress,
};
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};

use crate::clock::now_100ns;
use crate::{best_format, CaptureError, Output, PixelFormat, VideoDevice, VideoFormat, VideoFrame};

fn fourcc(code: u32) -> String {
    code.to_be_bytes().iter().map(|&b| if b.is_ascii_graphic() { b as char } else { '?' }).collect()
}

/// How the card itself sends pictures (what we receive is always BGRA).
fn pixel_format(subtype: u32) -> PixelFormat {
    match fourcc(subtype).as_str() {
        "jpeg" | "dmb1" | "mjpa" | "mjpb" => PixelFormat::Mjpeg,
        "420v" | "420f" => PixelFormat::Nv12,
        "yuvs" | "2vuy" | "yuv2" => PixelFormat::Yuy2,
        _ => PixelFormat::Bgra,
    }
}

unsafe fn video_devices() -> Vec<Retained<AVCaptureDevice>> {
    let mut types = vec![AVCaptureDeviceTypeExternal, AVCaptureDeviceTypeBuiltInWideAngleCamera];
    types.dedup();
    let types = NSArray::from_slice(&types);
    let session = AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
        &types,
        AVMediaTypeVideo,
        AVCaptureDevicePosition::Unspecified,
    );
    session.devices().to_vec()
}

/// Each format with its fastest rate (and 60 when it's in range, MiSTer's).
unsafe fn formats(device: &AVCaptureDevice) -> Vec<(VideoFormat, Retained<AVCaptureDeviceFormat>, f64)> {
    let mut out = Vec::new();
    for f in device.formats().to_vec() {
        let desc: Retained<CMFormatDescription> = f.formatDescription();
        let dims = CMVideoFormatDescriptionGetDimensions(&desc);
        let pixel = pixel_format(desc.media_sub_type());
        for range in f.videoSupportedFrameRateRanges().to_vec() {
            let (min, max) = (range.minFrameRate(), range.maxFrameRate());
            let mut rates = vec![max];
            if (min..max).contains(&60.0) {
                rates.push(60.0);
            }
            for fps in rates {
                let vf = VideoFormat { pixel, width: dims.width as u32, height: dims.height as u32, fps: (fps * 100.0).round() / 100.0 };
                if !out.iter().any(|(o, _, _): &(VideoFormat, _, _)| *o == vf) {
                    out.push((vf, f.clone(), fps));
                }
            }
        }
    }
    out
}

pub fn devices() -> Result<Vec<VideoDevice>, CaptureError> {
    unsafe {
        Ok(video_devices()
            .iter()
            .map(|d| VideoDevice {
                id: d.uniqueID().to_string(),
                name: d.localizedName().to_string(),
                formats: formats(d).into_iter().map(|(f, _, _)| f).collect(),
            })
            .collect())
    }
}

/// Ask for the Camera permission if it hasn't been decided, and wait.
unsafe fn ensure_access() -> Result<(), CaptureError> {
    let media = AVMediaTypeVideo.ok_or_else(|| CaptureError::Other("AVFoundation unavailable".into()))?;
    let status = AVCaptureDevice::authorizationStatusForMediaType(media);
    if status == AVAuthorizationStatus::Authorized {
        return Ok(());
    }
    if status == AVAuthorizationStatus::NotDetermined {
        let (tx, rx) = mpsc::channel();
        let block = RcBlock::new(move |granted: Bool| {
            let _ = tx.send(granted.as_bool());
        });
        AVCaptureDevice::requestAccessForMediaType_completionHandler(media, &block);
        if rx.recv_timeout(Duration::from_secs(120)).unwrap_or(false) {
            return Ok(());
        }
    }
    Err(CaptureError::Other(
        "no permission to use capture cards: allow it in System Settings > Privacy & Security > Camera".into(),
    ))
}

type FrameResult = Result<VideoFrame, CaptureError>;

pub struct DelegateIvars {
    tx: SyncSender<FrameResult>,
}

define_class!(
    // SAFETY: NSObject has no subclassing requirements; the delegate has no
    // Drop-sensitive state beyond its ivars.
    #[unsafe(super(NSObject))]
    #[name = "MiSTerKVMSCaptureDelegate"]
    #[ivars = DelegateIvars]
    struct FrameDelegate;

    unsafe impl NSObjectProtocol for FrameDelegate {}

    unsafe impl AVCaptureVideoDataOutputSampleBufferDelegate for FrameDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn captureOutput_didOutputSampleBuffer_fromConnection(
            &self,
            _output: &AVCaptureOutput,
            sample: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            if let Some(frame) = unsafe { copy_frame(sample) } {
                let _ = self.ivars().tx.try_send(Ok(frame));
            }
        }
    }
);

impl FrameDelegate {
    fn new(tx: SyncSender<FrameResult>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars { tx });
        unsafe { msg_send![super(this), init] }
    }
}

/// Copy a BGRA pixel buffer out, top row first, tightly packed.
unsafe fn copy_frame(sample: &CMSampleBuffer) -> Option<VideoFrame> {
    let image = sample.image_buffer()?;
    if CVPixelBufferLockBaseAddress(&image, CVPixelBufferLockFlags::ReadOnly) != 0 {
        return None;
    }
    let (w, h) = (CVPixelBufferGetWidth(&image), CVPixelBufferGetHeight(&image));
    let pitch = CVPixelBufferGetBytesPerRow(&image);
    let base = CVPixelBufferGetBaseAddress(&image) as *const u8;
    let row = w * 4;
    let frame = (!base.is_null() && pitch >= row).then(|| {
        let mut data = Vec::with_capacity(row * h);
        for y in 0..h {
            data.extend_from_slice(std::slice::from_raw_parts(base.add(y * pitch), row));
        }
        VideoFrame { format: PixelFormat::Bgra, width: w as u32, height: h as u32, stride: row as u32, data, timestamp: now_100ns() }
    });
    CVPixelBufferUnlockBaseAddress(&image, CVPixelBufferLockFlags::ReadOnly);
    frame
}

pub struct Reader {
    rx: Receiver<FrameResult>,
    session: Retained<AVCaptureSession>,
    output: Retained<AVCaptureVideoDataOutput>,
    _delegate: Retained<FrameDelegate>,
}

// SAFETY: AVCaptureSession and its outputs may be used from any thread;
// start/stop are documented as blocking calls meant for background threads.
unsafe impl Send for Reader {}

impl Reader {
    pub fn open(id: &str, wanted: Option<VideoFormat>, _output: Output) -> Result<(Self, VideoFormat), CaptureError> {
        unsafe {
            ensure_access()?;
            let device = video_devices()
                .into_iter()
                .find(|d| d.uniqueID().to_string() == id)
                .or_else(|| video_devices().into_iter().find(|d| d.localizedName().to_string() == id))
                .ok_or_else(|| CaptureError::NotFound(id.to_string()))?;
            let offered = formats(&device);
            let list: Vec<VideoFormat> = offered.iter().map(|(f, _, _)| *f).collect();
            let format = match wanted {
                Some(w) => list
                    .iter()
                    .copied()
                    .find(|f| f.pixel == w.pixel && f.width == w.width && f.height == w.height && (f.fps - w.fps).abs() < 0.5)
                    .ok_or_else(|| CaptureError::Format(format!("{w} not offered")))?,
                None => best_format(&list).ok_or_else(|| CaptureError::Format("no usable formats".into()))?,
            };
            let (_, native, fps) = offered.into_iter().find(|(f, _, _)| *f == format).unwrap();

            let input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device).map_err(|e| {
                let text = e.localizedDescription().to_string();
                // Another app holding the card is the usual reason.
                CaptureError::InUse(text)
            })?;
            let session = AVCaptureSession::new();
            session.beginConfiguration();
            if !session.canAddInput(&input) {
                session.commitConfiguration();
                return Err(CaptureError::InUse("the capture card can't be added (in use?)".into()));
            }
            session.addInput(&input);

            let output = AVCaptureVideoDataOutput::new();
            let key: &NSString = &*(kCVPixelBufferPixelFormatTypeKey as *const _ as *const NSString);
            let value = NSNumber::new_u32(kCVPixelFormatType_32BGRA);
            let settings = NSDictionary::<NSString, AnyObject>::from_slices(&[key], &[value.as_ref() as &AnyObject]);
            output.setVideoSettings(Some(&settings));
            output.setAlwaysDiscardsLateVideoFrames(true);
            let (tx, rx) = mpsc::sync_channel(3);
            let delegate = FrameDelegate::new(tx);
            let queue = DispatchQueue::new("misterkvms.capture", None);
            output.setSampleBufferDelegate_queue(Some(ProtocolObject::from_ref(&*delegate)), Some(&queue));
            if !session.canAddOutput(&output) {
                session.commitConfiguration();
                return Err(CaptureError::Other("can't add video output".into()));
            }
            session.addOutput(&output);

            // The chosen format and rate (InputPriority: the session keeps it).
            if device.lockForConfiguration().is_ok() {
                device.setActiveFormat(&native);
                let frame_time = CMTime { value: 1000, timescale: (fps * 1000.0).round() as i32, flags: objc2_core_media::CMTimeFlags::Valid, epoch: 0 };
                device.setActiveVideoMinFrameDuration(frame_time);
                device.setActiveVideoMaxFrameDuration(frame_time);
                device.unlockForConfiguration();
            }
            session.commitConfiguration();
            session.startRunning();
            Ok((Self { rx, session, output, _delegate: delegate }, format))
        }
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
        unsafe {
            self.output.setSampleBufferDelegate_queue(None, None);
            self.session.stopRunning();
        }
    }
}

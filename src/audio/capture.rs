//! AVFoundation microphone capture (macOS, objc2).
//!
//! Unlike the Go/ObjC version (which inspected the device format and hand-
//! rolled downmix + linear resampling), we ask AVFoundation for the target
//! format directly via `audioSettings` (macOS-only API): PCM16 LE, mono, at
//! the requested sample rate. The delegate then just forwards raw bytes.

#![allow(non_snake_case)]

use std::ptr::NonNull;
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{self, SyncSender};

use anyhow::{Result, anyhow, bail};
use block2::RcBlock;
use dispatch2::DispatchQueue;
use objc2::rc::Retained;
use objc2::runtime::ProtocolObject;
use objc2::{AllocAnyThread, DefinedClass, define_class, msg_send};
use objc2_av_foundation::{
    AVAuthorizationStatus, AVCaptureAudioDataOutput, AVCaptureAudioDataOutputSampleBufferDelegate,
    AVCaptureConnection, AVCaptureDevice, AVCaptureDeviceDiscoverySession, AVCaptureDeviceInput,
    AVCaptureDevicePosition, AVCaptureDeviceTypeMicrophone, AVCaptureOutput, AVCaptureSession,
    AVMediaTypeAudio,
};
use objc2_core_media::CMSampleBuffer;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSObjectProtocol, NSString};

use super::Device;

const K_AUDIO_FORMAT_LINEAR_PCM: u32 = u32::from_be_bytes(*b"lpcm");

struct DelegateIvars {
    tx: SyncSender<Vec<u8>>,
    paused: AtomicBool,
}

define_class!(
    #[unsafe(super(NSObject))]
    #[name = "YogurtAudioDelegate"]
    #[ivars = DelegateIvars]
    struct AudioDelegate;

    unsafe impl NSObjectProtocol for AudioDelegate {}

    unsafe impl AVCaptureAudioDataOutputSampleBufferDelegate for AudioDelegate {
        #[unsafe(method(captureOutput:didOutputSampleBuffer:fromConnection:))]
        fn capture_output(
            &self,
            _output: &AVCaptureOutput,
            sample_buffer: &CMSampleBuffer,
            _connection: &AVCaptureConnection,
        ) {
            // Unwinding across the ObjC callback boundary is UB.
            let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                self.handle_sample_buffer(sample_buffer);
            }));
        }
    }
);

impl AudioDelegate {
    fn new(tx: SyncSender<Vec<u8>>) -> Retained<Self> {
        let this = Self::alloc().set_ivars(DelegateIvars {
            tx,
            paused: AtomicBool::new(false),
        });
        unsafe { msg_send![super(this), init] }
    }

    fn handle_sample_buffer(&self, sample_buffer: &CMSampleBuffer) {
        if self.ivars().paused.load(Ordering::Relaxed) {
            return;
        }
        let Some(block) = (unsafe { sample_buffer.data_buffer() }) else {
            return;
        };
        let len = unsafe { block.data_length() };
        if len == 0 {
            return;
        }
        let mut buf = vec![0u8; len];
        let status = unsafe {
            block.copy_data_bytes(0, len, NonNull::new(buf.as_mut_ptr().cast()).unwrap())
        };
        if status != 0 {
            return;
        }
        // Drop on a slow consumer rather than blocking the capture queue.
        let _ = self.ivars().tx.try_send(buf);
    }
}

/// Owns a running AVCaptureSession. `!Send` Retained pointers are held behind
/// a Send wrapper: AVCaptureSession start/stop/configuration are documented
/// thread-safe, and we only touch them via these methods.
pub struct Capture {
    session: Retained<AVCaptureSession>,
    delegate: Retained<AudioDelegate>,
    _queue: dispatch2::DispatchRetained<DispatchQueue>,
}

unsafe impl Send for Capture {}

impl Capture {
    pub fn start(device_index: i32, sample_rate: u32, tx: SyncSender<Vec<u8>>) -> Result<Capture> {
        unsafe {
            let device = pick_device(device_index)
                .ok_or_else(|| anyhow!("no audio input device found (index {device_index})"))?;

            let input = AVCaptureDeviceInput::deviceInputWithDevice_error(&device)
                .map_err(|e| anyhow!("open audio device: {e}"))?;

            let session = AVCaptureSession::new();
            if !session.canAddInput(&input) {
                bail!("cannot add audio input to capture session");
            }
            session.addInput(&input);

            let output = AVCaptureAudioDataOutput::new();

            // Request our exact target format; AVFoundation converts for us.
            let keys = [
                NSString::from_str("AVFormatIDKey"),
                NSString::from_str("AVSampleRateKey"),
                NSString::from_str("AVNumberOfChannelsKey"),
                NSString::from_str("AVLinearPCMBitDepthKey"),
                NSString::from_str("AVLinearPCMIsFloatKey"),
                NSString::from_str("AVLinearPCMIsBigEndianKey"),
                NSString::from_str("AVLinearPCMIsNonInterleaved"),
            ];
            let values: [Retained<NSNumber>; 7] = [
                NSNumber::new_u32(K_AUDIO_FORMAT_LINEAR_PCM),
                NSNumber::new_f64(sample_rate as f64),
                NSNumber::new_u32(1),
                NSNumber::new_u32(16),
                NSNumber::new_bool(false),
                NSNumber::new_bool(false),
                NSNumber::new_bool(false),
            ];
            let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
            let value_refs: Vec<&objc2::runtime::AnyObject> = values
                .iter()
                .map(|v| {
                    let o: &objc2::runtime::AnyObject = v;
                    o
                })
                .collect();
            let settings: Retained<NSDictionary<NSString, objc2::runtime::AnyObject>> =
                NSDictionary::from_slices(&key_refs, &value_refs);
            output.setAudioSettings(Some(&settings));

            let delegate = AudioDelegate::new(tx);
            let queue = DispatchQueue::new("com.yogurt.audio", None);
            output.setSampleBufferDelegate_queue(
                Some(ProtocolObject::from_ref(&*delegate)),
                Some(&queue),
            );

            if !session.canAddOutput(&output) {
                bail!("cannot add audio output to capture session");
            }
            session.addOutput(&output);
            session.startRunning();

            Ok(Capture {
                session,
                delegate,
                _queue: queue,
            })
        }
    }

    pub fn pause(&self) {
        self.delegate.ivars().paused.store(true, Ordering::Relaxed);
    }

    pub fn resume(&self) {
        self.delegate.ivars().paused.store(false, Ordering::Relaxed);
    }
}

impl Drop for Capture {
    fn drop(&mut self) {
        unsafe {
            self.session.stopRunning();
            self.session.beginConfiguration();
            for input in self.session.inputs().iter() {
                self.session.removeInput(&input);
            }
            for output in self.session.outputs().iter() {
                self.session.removeOutput(&output);
            }
            self.session.commitConfiguration();
        }
    }
}

fn discover_devices() -> Vec<Retained<AVCaptureDevice>> {
    unsafe {
        let types = NSArray::from_slice(&[AVCaptureDeviceTypeMicrophone]);
        let session = AVCaptureDeviceDiscoverySession::discoverySessionWithDeviceTypes_mediaType_position(
            &types,
            AVMediaTypeAudio,
            AVCaptureDevicePosition::Unspecified,
        );
        session.devices().to_vec()
    }
}

fn pick_device(index: i32) -> Option<Retained<AVCaptureDevice>> {
    unsafe {
        if index >= 0 {
            let devices = discover_devices();
            if let Some(d) = devices.get(index as usize) {
                return Some(d.clone());
            }
        }
        AVCaptureDevice::defaultDeviceWithMediaType(AVMediaTypeAudio.unwrap())
    }
}

pub fn list_devices() -> Vec<Device> {
    discover_devices()
        .iter()
        .enumerate()
        .map(|(i, d)| Device {
            index: i as i32,
            name: unsafe { d.localizedName() }.to_string(),
        })
        .collect()
}

/// 3 = authorized, 2 = denied, 1 = restricted, 0 = not determined.
pub fn authorization_status() -> i32 {
    unsafe {
        let status: AVAuthorizationStatus =
            AVCaptureDevice::authorizationStatusForMediaType(AVMediaTypeAudio.unwrap());
        status.0 as i32
    }
}

/// Blocks until the user answers the permission dialog.
pub fn request_permission() -> Result<()> {
    let (tx, rx) = mpsc::channel::<bool>();
    unsafe {
        let block = RcBlock::new(move |granted: objc2::runtime::Bool| {
            let _ = tx.send(granted.as_bool());
        });
        AVCaptureDevice::requestAccessForMediaType_completionHandler(AVMediaTypeAudio.unwrap(), &block);
    }
    match rx.recv() {
        Ok(true) => Ok(()),
        _ => bail!(
            "microphone access denied — enable in System Settings → Privacy & Security → Microphone"
        ),
    }
}

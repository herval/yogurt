//! System-audio capture via a Core Audio process tap (macOS 14.2+).
//!
//! A global mono-mixdown tap captures everything the system plays (meeting
//! apps included), wrapped in a private aggregate device whose IOProc streams
//! the audio to us. Output: PCM16 LE mono at the requested rate, resampled
//! from the tap's native format. Requires the "System Audio Recording"
//! permission (NSAudioCaptureUsageDescription).

#![allow(non_snake_case)]

use std::ptr::NonNull;
use std::sync::Mutex;
use std::sync::mpsc::SyncSender;

use anyhow::{Result, bail};
use block2::RcBlock;
use dispatch2::{DispatchQueue, DispatchRetained};
use objc2::AllocAnyThread;
use objc2::rc::Retained;
use objc2_core_audio::{
    AudioDeviceCreateIOProcIDWithBlock, AudioDeviceDestroyIOProcID, AudioDeviceIOProcID,
    AudioDeviceStart, AudioDeviceStop, AudioHardwareCreateAggregateDevice,
    AudioHardwareCreateProcessTap, AudioHardwareDestroyAggregateDevice,
    AudioHardwareDestroyProcessTap, AudioObjectGetPropertyData, AudioObjectID,
    AudioObjectPropertyAddress, CATapDescription, kAudioObjectPropertyElementMain,
    kAudioObjectPropertyScopeGlobal, kAudioTapPropertyFormat,
};
use objc2_core_audio_types::{AudioBufferList, AudioStreamBasicDescription, AudioTimeStamp};
use objc2_core_foundation::CFDictionary;
use objc2_foundation::{NSArray, NSDictionary, NSNumber, NSObject, NSString};

type IoBlock = RcBlock<
    dyn Fn(
        NonNull<AudioTimeStamp>,
        NonNull<AudioBufferList>,
        NonNull<AudioTimeStamp>,
        NonNull<AudioBufferList>,
        NonNull<AudioTimeStamp>,
    ),
>;

pub struct SystemTap {
    tap_id: AudioObjectID,
    agg_id: AudioObjectID,
    proc_id: AudioDeviceIOProcID,
    _block: IoBlock,
    _queue: DispatchRetained<DispatchQueue>,
}

// The Core Audio object IDs are plain handles; start/stop/destroy are
// documented thread-safe HAL calls.
unsafe impl Send for SystemTap {}

struct ResampleState {
    /// Fractional read position into the source stream, carried across callbacks.
    pos: f64,
    /// Last sample of the previous callback (for interpolation continuity).
    prev: f32,
}

impl SystemTap {
    pub fn start(target_rate: u32, tx: SyncSender<Vec<u8>>) -> Result<SystemTap> {
        unsafe {
            // Global mono mixdown of all system audio, excluding nothing.
            let desc = CATapDescription::initMonoGlobalTapButExcludeProcesses(
                CATapDescription::alloc(),
                &NSArray::new(),
            );
            desc.setName(&NSString::from_str("yogurt system audio tap"));
            desc.setPrivate(true);

            let mut tap_id: AudioObjectID = 0;
            let status =
                AudioHardwareCreateProcessTap(Some(&desc), &mut tap_id);
            if status != 0 {
                bail!(
                    "create system audio tap failed (OSStatus {status}) — grant \
                     \u{201c}System Audio Recording\u{201d} to yogurt in System Settings → \
                     Privacy & Security"
                );
            }

            let cleanup_tap = |tap_id: AudioObjectID| {
                AudioHardwareDestroyProcessTap(tap_id);
            };

            // The tap's native format tells us what the IOProc will deliver.
            let mut asbd: AudioStreamBasicDescription = std::mem::zeroed();
            let mut size = size_of::<AudioStreamBasicDescription>() as u32;
            let addr = AudioObjectPropertyAddress {
                mSelector: kAudioTapPropertyFormat,
                mScope: kAudioObjectPropertyScopeGlobal,
                mElement: kAudioObjectPropertyElementMain,
            };
            let status = AudioObjectGetPropertyData(
                tap_id,
                NonNull::from(&addr),
                0,
                std::ptr::null(),
                NonNull::from(&mut size),
                NonNull::from(&mut asbd).cast(),
            );
            if status != 0 {
                cleanup_tap(tap_id);
                bail!("query tap format failed (OSStatus {status})");
            }
            let src_rate = asbd.mSampleRate;
            let src_channels = asbd.mChannelsPerFrame.max(1) as usize;
            log::info!("system tap format: {src_rate} Hz, {src_channels} ch");

            // Private aggregate device hosting the tap.
            let tap_uuid = desc.UUID().UUIDString();
            let agg_uid = NSString::from_str(&format!("dev.herval.yogurt.tap-agg"));
            let sub_tap: Retained<NSDictionary<NSString, NSObject>> = {
                let keys = [
                    NSString::from_str("uid"), // kAudioSubTapUIDKey
                    NSString::from_str("drift"), // kAudioSubTapDriftCompensationKey
                ];
                let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
                let uuid_obj: &NSObject = &tap_uuid;
                let drift = NSNumber::new_bool(true);
                let drift_obj: &NSObject = &drift;
                NSDictionary::from_slices(&key_refs, &[uuid_obj, drift_obj])
            };
            let agg_desc: Retained<NSDictionary<NSString, NSObject>> = {
                let keys = [
                    NSString::from_str("uid"),     // kAudioAggregateDeviceUIDKey
                    NSString::from_str("private"), // kAudioAggregateDeviceIsPrivateKey
                    NSString::from_str("taps"),    // kAudioAggregateDeviceTapListKey
                ];
                let key_refs: Vec<&NSString> = keys.iter().map(|k| &**k).collect();
                let uid_obj: &NSObject = &agg_uid;
                let private = NSNumber::new_bool(true);
                let private_obj: &NSObject = &private;
                let taps = NSArray::from_retained_slice(&[sub_tap]);
                let taps_obj: &NSObject = &taps;
                NSDictionary::from_slices(&key_refs, &[uid_obj, private_obj, taps_obj])
            };

            let mut agg_id: AudioObjectID = 0;
            // NSDictionary is toll-free bridged to CFDictionary.
            let cf_desc: &CFDictionary =
                &*(Retained::as_ptr(&agg_desc) as *const CFDictionary);
            let status = AudioHardwareCreateAggregateDevice(cf_desc, NonNull::from(&mut agg_id));
            if status != 0 {
                cleanup_tap(tap_id);
                bail!("create aggregate device failed (OSStatus {status})");
            }

            // IOProc: source-format frames in → PCM16 mono at target_rate out.
            let ratio = src_rate / target_rate as f64;
            let state = Mutex::new(ResampleState { pos: 0.0, prev: 0.0 });
            let block: IoBlock = RcBlock::new(
                move |_now: NonNull<AudioTimeStamp>,
                      in_data: NonNull<AudioBufferList>,
                      _in_time: NonNull<AudioTimeStamp>,
                      _out_data: NonNull<AudioBufferList>,
                      _out_time: NonNull<AudioTimeStamp>| {
                    let _ = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                        let mono = abl_to_mono_f32(in_data.as_ref(), src_channels);
                        if mono.is_empty() {
                            return;
                        }
                        let mut st = state.lock().unwrap();
                        let out = resample_to_i16(&mono, ratio, &mut st);
                        if !out.is_empty() {
                            let _ = tx.try_send(out);
                        }
                    }));
                },
            );

            let queue = DispatchQueue::new("com.yogurt.systemtap", None);
            let mut proc_id: AudioDeviceIOProcID = None;
            let status = AudioDeviceCreateIOProcIDWithBlock(
                NonNull::from(&mut proc_id),
                agg_id,
                Some(&queue),
                &*block as *const _ as *mut _,
            );
            if status != 0 {
                AudioHardwareDestroyAggregateDevice(agg_id);
                cleanup_tap(tap_id);
                bail!("create tap IOProc failed (OSStatus {status})");
            }

            let status = AudioDeviceStart(agg_id, proc_id);
            if status != 0 {
                AudioDeviceDestroyIOProcID(agg_id, proc_id);
                AudioHardwareDestroyAggregateDevice(agg_id);
                cleanup_tap(tap_id);
                bail!("start system tap failed (OSStatus {status})");
            }

            Ok(SystemTap {
                tap_id,
                agg_id,
                proc_id,
                _block: block,
                _queue: queue,
            })
        }
    }
}

impl Drop for SystemTap {
    fn drop(&mut self) {
        unsafe {
            AudioDeviceStop(self.agg_id, self.proc_id);
            AudioDeviceDestroyIOProcID(self.agg_id, self.proc_id);
            AudioHardwareDestroyAggregateDevice(self.agg_id);
            AudioHardwareDestroyProcessTap(self.tap_id);
        }
    }
}

/// Downmix an AudioBufferList of Float32 samples to mono.
/// Planar (one buffer per channel) and interleaved layouts both handled.
unsafe fn abl_to_mono_f32(abl: &AudioBufferList, channels: usize) -> Vec<f32> {
    let n_buffers = abl.mNumberBuffers as usize;
    if n_buffers == 0 {
        return Vec::new();
    }
    let buffers = unsafe {
        std::slice::from_raw_parts(abl.mBuffers.as_ptr(), n_buffers)
    };

    if n_buffers > 1 {
        // Planar: average across buffers frame by frame.
        let frames = (buffers[0].mDataByteSize as usize / 4).min(usize::MAX);
        let mut out = vec![0.0f32; frames];
        let mut used = 0usize;
        for b in buffers {
            if b.mData.is_null() {
                continue;
            }
            let data = unsafe {
                std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4)
            };
            for (i, s) in data.iter().take(frames).enumerate() {
                out[i] += s;
            }
            used += 1;
        }
        if used > 1 {
            let inv = 1.0 / used as f32;
            for s in &mut out {
                *s *= inv;
            }
        }
        out
    } else {
        let b = &buffers[0];
        if b.mData.is_null() {
            return Vec::new();
        }
        let data = unsafe {
            std::slice::from_raw_parts(b.mData as *const f32, b.mDataByteSize as usize / 4)
        };
        let ch = b.mNumberChannels.max(1) as usize;
        let ch = if ch == 1 { channels.max(1) } else { ch };
        if ch == 1 {
            data.to_vec()
        } else {
            data.chunks_exact(ch)
                .map(|frame| frame.iter().sum::<f32>() / ch as f32)
                .collect()
        }
    }
}

/// Linear-interpolation resample by `ratio` source frames per output frame,
/// converting to i16 LE bytes. Carries fractional position across calls.
fn resample_to_i16(src: &[f32], ratio: f64, st: &mut ResampleState) -> Vec<u8> {
    let mut out = Vec::new();
    let mut pos = st.pos;
    while pos < src.len() as f64 {
        let idx = pos as usize;
        let frac = (pos - idx as f64) as f32;
        let a = if idx == 0 && frac < 1.0 && pos < 1.0 {
            st.prev
        } else {
            src[idx.min(src.len() - 1)]
        };
        let b = src[(idx + 1).min(src.len() - 1)];
        let sample = a + (b - a) * frac;
        let clamped = (sample.clamp(-1.0, 1.0) * 32767.0) as i16;
        out.extend_from_slice(&clamped.to_le_bytes());
        pos += ratio;
    }
    st.pos = pos - src.len() as f64;
    st.prev = *src.last().unwrap_or(&0.0);
    out
}

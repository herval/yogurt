//! AVFoundation microphone capture (macOS). M2: objc2 implementation.

use std::sync::mpsc::SyncSender;

use anyhow::{Result, bail};

use super::Device;

pub struct Capture {}

impl Capture {
    pub fn start(_device_index: i32, _sample_rate: u32, _tx: SyncSender<Vec<u8>>) -> Result<Capture> {
        bail!("audio capture not yet implemented (M2)")
    }

    pub fn pause(&self) {}
    pub fn resume(&self) {}
}

pub fn list_devices() -> Vec<Device> {
    Vec::new()
}

/// 3 = authorized, 2 = denied, 1 = restricted, 0 = not determined.
pub fn authorization_status() -> i32 {
    0
}

pub fn request_permission() -> Result<()> {
    bail!("permission request not yet implemented (M2)")
}

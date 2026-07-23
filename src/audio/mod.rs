pub mod capture;
pub mod files;
pub mod mixer;
pub mod opus_enc;
pub mod system_tap;
pub mod wav;

#[derive(Debug, Clone)]
pub struct Device {
    pub index: i32,
    pub name: String,
}

/// Peak-based level meter: max |sample| / 32768, boosted 2x, clamped to 1.0.
pub fn calc_level(pcm: &[u8]) -> f64 {
    let mut peak: i32 = 0;
    for chunk in pcm.chunks_exact(2) {
        let s = i16::from_le_bytes([chunk[0], chunk[1]]) as i32;
        peak = peak.max(s.abs());
    }
    (peak as f64 / 32768.0 * 2.0).min(1.0)
}

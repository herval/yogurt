use anyhow::{Result, bail};
use std::io::Write;
use std::path::Path;

/// Write PCM16 LE data as a canonical 44-byte-header WAV file.
pub fn write_wav(path: &Path, pcm: &[u8], sample_rate: u32, channels: u16) -> Result<()> {
    let mut f = std::fs::File::create(path)?;
    f.write_all(&wav_header(pcm.len() as u32, sample_rate, channels))?;
    f.write_all(pcm)?;
    Ok(())
}

pub fn wav_header(data_len: u32, sample_rate: u32, channels: u16) -> Vec<u8> {
    let block_align = channels as u32 * 2;
    let byte_rate = sample_rate * block_align;
    let mut header = Vec::with_capacity(44);
    header.extend_from_slice(b"RIFF");
    header.extend_from_slice(&(36 + data_len).to_le_bytes());
    header.extend_from_slice(b"WAVE");
    header.extend_from_slice(b"fmt ");
    header.extend_from_slice(&16u32.to_le_bytes()); // fmt chunk size
    header.extend_from_slice(&1u16.to_le_bytes()); // PCM
    header.extend_from_slice(&channels.to_le_bytes());
    header.extend_from_slice(&sample_rate.to_le_bytes());
    header.extend_from_slice(&byte_rate.to_le_bytes());
    header.extend_from_slice(&(block_align as u16).to_le_bytes());
    header.extend_from_slice(&16u16.to_le_bytes()); // bits per sample
    header.extend_from_slice(b"data");
    header.extend_from_slice(&data_len.to_le_bytes());
    header
}

/// Parse a WAV file into (interleaved PCM16 LE, sample_rate, channels).
/// Tolerant chunk scan; 16-bit PCM only.
pub fn read_wav(data: &[u8]) -> Result<(Vec<u8>, u32, u16)> {
    if data.len() < 12 || &data[0..4] != b"RIFF" || &data[8..12] != b"WAVE" {
        bail!("not a WAV file (missing RIFF/WAVE header)");
    }

    let mut pos = 12usize;
    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut bits_per_sample = 0u16;
    let mut pcm: Option<Vec<u8>> = None;

    while pos + 8 <= data.len() {
        let id = &data[pos..pos + 4];
        let size = u32::from_le_bytes(data[pos + 4..pos + 8].try_into().unwrap()) as usize;
        pos += 8;
        let end = (pos + size).min(data.len());
        match id {
            b"fmt " => {
                if size < 16 {
                    bail!("WAV fmt chunk too small");
                }
                let c = &data[pos..pos + 16];
                channels = u16::from_le_bytes([c[2], c[3]]);
                sample_rate = u32::from_le_bytes([c[4], c[5], c[6], c[7]]);
                bits_per_sample = u16::from_le_bytes([c[14], c[15]]);
            }
            b"data" => {
                pcm = Some(data[pos..end].to_vec());
            }
            _ => {}
        }
        // Chunks are word-aligned; sizes may be odd.
        pos = end + (size & 1);
    }

    if sample_rate == 0 {
        bail!("WAV fmt chunk not found");
    }
    let Some(pcm) = pcm else {
        bail!("WAV data chunk not found");
    };
    if bits_per_sample != 16 {
        bail!("only 16-bit WAV supported (got {}-bit)", bits_per_sample);
    }
    Ok((pcm, sample_rate, channels))
}

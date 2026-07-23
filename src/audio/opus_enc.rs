//! Ogg-Opus encoding for STT uploads: ~16x smaller than WAV for speech,
//! turning an hour of stereo audio from ~230MB into ~15MB.

use anyhow::{Context, Result, bail};
use ogg::writing::{PacketWriteEndInfo, PacketWriter};

const FRAME_MS: usize = 20;
const OGG_SERIAL: u32 = 0x79677274; // "ygrt"

/// Sample rates libopus accepts as input.
pub fn opus_supports(sample_rate: u32) -> bool {
    matches!(sample_rate, 8000 | 12000 | 16000 | 24000 | 48000)
}

/// Encode interleaved PCM16 LE into an Ogg-Opus file (RFC 7845).
pub fn encode_ogg_opus(pcm: &[u8], sample_rate: u32, channels: u16) -> Result<Vec<u8>> {
    if !opus_supports(sample_rate) {
        bail!("opus does not support {sample_rate}Hz input");
    }
    let chans = match channels {
        1 => opus::Channels::Mono,
        2 => opus::Channels::Stereo,
        n => bail!("unsupported channel count {n} for opus"),
    };

    let mut enc = opus::Encoder::new(sample_rate, chans, opus::Application::Voip)
        .context("create opus encoder")?;
    let bitrate = if channels == 1 { 24_000 } else { 40_000 };
    enc.set_bitrate(opus::Bitrate::Bits(bitrate))
        .context("set opus bitrate")?;
    // Pre-skip is expressed in 48kHz samples.
    let pre_skip: u16 = (enc.get_lookahead().unwrap_or(0) as u64 * 48000 / sample_rate as u64) as u16;

    let mut out = Vec::with_capacity(pcm.len() / 12);
    let mut writer = PacketWriter::new(&mut out);

    // OpusHead
    let mut head = Vec::with_capacity(19);
    head.extend_from_slice(b"OpusHead");
    head.push(1); // version
    head.push(channels as u8);
    head.extend_from_slice(&pre_skip.to_le_bytes());
    head.extend_from_slice(&sample_rate.to_le_bytes());
    head.extend_from_slice(&0i16.to_le_bytes()); // output gain
    head.push(0); // mapping family
    writer
        .write_packet(head, OGG_SERIAL, PacketWriteEndInfo::EndPage, 0)
        .context("write OpusHead")?;

    // OpusTags
    let vendor = b"yogurt";
    let mut tags = Vec::new();
    tags.extend_from_slice(b"OpusTags");
    tags.extend_from_slice(&(vendor.len() as u32).to_le_bytes());
    tags.extend_from_slice(vendor);
    tags.extend_from_slice(&0u32.to_le_bytes()); // no user comments
    writer
        .write_packet(tags, OGG_SERIAL, PacketWriteEndInfo::EndPage, 0)
        .context("write OpusTags")?;

    // Audio packets: fixed 20ms frames, final frame zero-padded.
    let samples_per_frame = sample_rate as usize / 1000 * FRAME_MS;
    let frame_values = samples_per_frame * channels as usize;
    let mut samples: Vec<i16> = pcm
        .chunks_exact(2)
        .map(|c| i16::from_le_bytes([c[0], c[1]]))
        .collect();
    let rem = samples.len() % frame_values;
    if rem != 0 {
        samples.resize(samples.len() + (frame_values - rem), 0);
    }

    let n_frames = samples.len() / frame_values;
    let granule_per_frame = 48000 / 1000 * FRAME_MS as u64; // 48kHz samples per frame
    let mut packet_buf = vec![0u8; 4000];
    for (i, frame) in samples.chunks_exact(frame_values).enumerate() {
        let len = enc.encode(frame, &mut packet_buf).context("opus encode")?;
        let granule = pre_skip as u64 + (i as u64 + 1) * granule_per_frame;
        let end = if i + 1 == n_frames {
            PacketWriteEndInfo::EndStream
        } else {
            PacketWriteEndInfo::NormalPacket
        };
        writer
            .write_packet(packet_buf[..len].to_vec(), OGG_SERIAL, end, granule)
            .context("write opus packet")?;
    }
    drop(writer);
    Ok(out)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn tone(ms: usize, rate: u32, channels: u16) -> Vec<u8> {
        let frames = rate as usize / 1000 * ms;
        let mut out = Vec::new();
        for i in 0..frames {
            let v = ((i as f32 * 0.1).sin() * 8000.0) as i16;
            for _ in 0..channels {
                out.extend_from_slice(&v.to_le_bytes());
            }
        }
        out
    }

    #[test]
    fn encodes_valid_ogg_structure() {
        for channels in [1u16, 2] {
            let pcm = tone(1500, 16000, channels);
            let ogg = encode_ogg_opus(&pcm, 16000, channels).unwrap();
            assert_eq!(&ogg[0..4], b"OggS", "ogg capture pattern");
            assert!(
                ogg.windows(8).any(|w| w == b"OpusHead"),
                "OpusHead present"
            );
            assert!(ogg.len() < pcm.len() / 4, "compressed: {} vs {}", ogg.len(), pcm.len());
        }
    }

    /// Manual helper: OPUS_IN_WAV=<in.wav> OPUS_OUT=<out.ogg> cargo test -- --ignored encode_wav
    #[test]
    #[ignore]
    fn encode_wav_file_to_ogg() {
        let src = std::env::var("OPUS_IN_WAV").unwrap();
        let dst = std::env::var("OPUS_OUT").unwrap();
        let data = std::fs::read(&src).unwrap();
        let (pcm, rate, channels) = crate::audio::wav::read_wav(&data).unwrap();
        let ogg = encode_ogg_opus(&pcm, rate, channels).unwrap();
        std::fs::write(&dst, &ogg).unwrap();
        eprintln!("{} bytes wav -> {} bytes ogg", data.len(), ogg.len());
    }

    #[test]
    fn rejects_unsupported_rate() {
        assert!(encode_ogg_opus(&[0u8; 4000], 44100, 1).is_err());
    }
}

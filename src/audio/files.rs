use anyhow::{Context, Result, bail};
use std::path::Path;

use super::wav::read_wav;

/// Read an audio file (.wav or .mp3) into mono PCM16 LE + sample rate.
pub fn read_audio_file(path: &Path) -> Result<(Vec<u8>, u32)> {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    match ext.as_str() {
        "wav" => {
            let data = std::fs::read(path)
                .with_context(|| format!("open {}", path.display()))?;
            let (pcm, rate, channels) = read_wav(&data)?;
            Ok((to_mono(&pcm, channels)?, rate))
        }
        "mp3" => read_mp3(path),
        _ => bail!("unsupported audio format {ext:?} (supported: wav, mp3)"),
    }
}

fn read_mp3(path: &Path) -> Result<(Vec<u8>, u32)> {
    use symphonia::core::audio::SampleBuffer;
    use symphonia::core::codecs::DecoderOptions;
    use symphonia::core::formats::FormatOptions;
    use symphonia::core::io::MediaSourceStream;
    use symphonia::core::meta::MetadataOptions;
    use symphonia::core::probe::Hint;

    let file = std::fs::File::open(path)
        .with_context(|| format!("open {}", path.display()))?;
    let mss = MediaSourceStream::new(Box::new(file), Default::default());
    let mut hint = Hint::new();
    hint.with_extension("mp3");

    let probed = symphonia::default::get_probe()
        .format(&hint, mss, &FormatOptions::default(), &MetadataOptions::default())
        .context("probe mp3")?;
    let mut format = probed.format;
    let track = format
        .default_track()
        .context("no audio track in mp3")?;
    let track_id = track.id;
    let mut decoder = symphonia::default::get_codecs()
        .make(&track.codec_params, &DecoderOptions::default())
        .context("create mp3 decoder")?;

    let mut sample_rate = 0u32;
    let mut channels = 0u16;
    let mut pcm: Vec<u8> = Vec::new();
    let mut sample_buf: Option<SampleBuffer<i16>> = None;

    loop {
        let packet = match format.next_packet() {
            Ok(p) => p,
            Err(symphonia::core::errors::Error::IoError(e))
                if e.kind() == std::io::ErrorKind::UnexpectedEof =>
            {
                break;
            }
            Err(e) => return Err(e).context("read mp3 packet"),
        };
        if packet.track_id() != track_id {
            continue;
        }
        let decoded = match decoder.decode(&packet) {
            Ok(d) => d,
            Err(symphonia::core::errors::Error::DecodeError(_)) => continue, // skip bad frame
            Err(e) => return Err(e).context("decode mp3"),
        };
        let spec = *decoded.spec();
        sample_rate = spec.rate;
        channels = spec.channels.count() as u16;
        let buf = sample_buf.get_or_insert_with(|| {
            SampleBuffer::<i16>::new(decoded.capacity() as u64, spec)
        });
        buf.copy_interleaved_ref(decoded);
        for s in buf.samples() {
            pcm.extend_from_slice(&s.to_le_bytes());
        }
    }

    if sample_rate == 0 {
        bail!("no decodable audio in mp3");
    }
    Ok((to_mono(&pcm, channels)?, sample_rate))
}

/// Downmix interleaved PCM16 LE to mono by averaging channels.
pub fn to_mono(pcm: &[u8], channels: u16) -> Result<Vec<u8>> {
    match channels {
        1 => Ok(pcm.to_vec()),
        0 => bail!("invalid channel count 0"),
        n => {
            let n = n as usize;
            let frame_bytes = n * 2;
            let frames = pcm.len() / frame_bytes;
            let mut out = Vec::with_capacity(frames * 2);
            for f in 0..frames {
                let mut acc: i32 = 0;
                for c in 0..n {
                    let off = f * frame_bytes + c * 2;
                    acc += i16::from_le_bytes([pcm[off], pcm[off + 1]]) as i32;
                }
                out.extend_from_slice(&((acc / n as i32) as i16).to_le_bytes());
            }
            Ok(out)
        }
    }
}

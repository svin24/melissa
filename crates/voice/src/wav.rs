//! WAV in, 16 kHz mono 16-bit PCM out (what whisper.cpp reads), in safe Rust.
//!
//! The browser records with MediaRecorder (webm/opus, mp4/aac), decodes the clip itself
//! (`decodeAudioData`), renders it at 16 kHz mono and sends a plain WAV, so no Opus or AAC
//! decoder is needed here. This module still accepts any common PCM WAV (8-48 kHz, 1-8
//! channels, 8/16/24/32-bit integer or 32-bit float), downmixes and resamples it, and writes
//! a fresh canonical file: whisper.cpp never parses bytes that came from the network.

/// The sample rate whisper.cpp expects.
pub const SAMPLE_RATE: u32 = 16_000;
/// The longest clip Melissa transcribes.
pub const MAX_SECONDS: u32 = 120;
/// Shorter than this is "nothing recorded".
const MIN_MILLIS: u64 = 200;

const FORMAT_PCM: u16 = 1;
const FORMAT_FLOAT: u16 = 3;
const FORMAT_EXTENSIBLE: u16 = 0xFFFE;

/// Why a clip was refused. The messages are short and may be shown in the UI.
#[derive(Debug, Clone, PartialEq, Eq, thiserror::Error)]
pub enum AudioError {
    #[error("That isn't a WAV recording.")]
    NotWav,
    #[error("That recording format isn't supported ({0}).")]
    Unsupported(String),
    #[error("The recording is cut off.")]
    Truncated,
    #[error("Nothing was recorded.")]
    Empty,
    #[error("Recordings can be up to {max} seconds; this one is {seconds}.", max = MAX_SECONDS)]
    TooLong { seconds: u64 },
}

/// A clip at [`SAMPLE_RATE`], mono, signed 16-bit.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Clip {
    samples: Vec<i16>,
}

impl Clip {
    pub fn from_samples(samples: Vec<i16>) -> Self {
        Self { samples }
    }

    pub fn samples(&self) -> &[i16] {
        &self.samples
    }

    pub fn millis(&self) -> u64 {
        self.samples.len() as u64 * 1000 / u64::from(SAMPLE_RATE)
    }

    /// A canonical WAV file (RIFF, PCM, 16 kHz, mono, 16-bit little-endian).
    pub fn to_wav(&self) -> Vec<u8> {
        let data_len = (self.samples.len() * 2) as u32;
        let mut out = Vec::with_capacity(44 + self.samples.len() * 2);
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data_len).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&FORMAT_PCM.to_le_bytes());
        out.extend_from_slice(&1u16.to_le_bytes());
        out.extend_from_slice(&SAMPLE_RATE.to_le_bytes());
        out.extend_from_slice(&(SAMPLE_RATE * 2).to_le_bytes());
        out.extend_from_slice(&2u16.to_le_bytes());
        out.extend_from_slice(&16u16.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&data_len.to_le_bytes());
        for s in &self.samples {
            out.extend_from_slice(&s.to_le_bytes());
        }
        out
    }
}

#[derive(Debug, Clone, Copy)]
struct Format {
    float: bool,
    channels: u16,
    rate: u32,
    bits: u16,
}

impl Format {
    fn frame_bytes(&self) -> usize {
        usize::from(self.channels) * usize::from(self.bits / 8)
    }
}

/// Read a WAV file and convert it to a 16 kHz mono [`Clip`], refusing clips longer than
/// [`MAX_SECONDS`] before converting anything.
pub fn decode(bytes: &[u8]) -> Result<Clip, AudioError> {
    if bytes.len() < 12 || &bytes[0..4] != b"RIFF" || &bytes[8..12] != b"WAVE" {
        return Err(AudioError::NotWav);
    }
    let mut format = None;
    let mut data = None;
    let mut at = 12;
    while at + 8 <= bytes.len() {
        let id = &bytes[at..at + 4];
        let len = u32_at(bytes, at + 4) as usize;
        let body = at + 8;
        // Browsers and some tools write 0 or 0xFFFFFFFF for a data chunk of unknown length:
        // take the rest of the file.
        let end = if id == b"data" && (len == 0 || len == u32::MAX as usize) {
            bytes.len()
        } else {
            body.checked_add(len).ok_or(AudioError::Truncated)?
        };
        if id == b"fmt " {
            if end > bytes.len() {
                return Err(AudioError::Truncated);
            }
            format = Some(parse_format(&bytes[body..end])?);
        } else if id == b"data" {
            // A data chunk cut short (a recording stopped mid-write) is still usable.
            data = Some(&bytes[body..end.min(bytes.len())]);
            break;
        }
        // Chunks are padded to an even length.
        at = end + (len & 1);
    }
    let format = format.ok_or(AudioError::NotWav)?;
    let data = data.ok_or(AudioError::NotWav)?;
    let frame = format.frame_bytes();
    let frames = data.len() / frame;
    let millis = frames as u64 * 1000 / u64::from(format.rate);
    if millis > u64::from(MAX_SECONDS) * 1000 {
        return Err(AudioError::TooLong {
            seconds: millis.div_ceil(1000),
        });
    }
    if millis < MIN_MILLIS {
        return Err(AudioError::Empty);
    }
    let mono = downmix(&data[..frames * frame], format);
    let resampled = resample(&mono, format.rate, SAMPLE_RATE);
    let samples = resampled
        .into_iter()
        .map(|x| (x.clamp(-1.0, 1.0) * 32767.0).round() as i16)
        .collect();
    Ok(Clip { samples })
}

fn parse_format(fmt: &[u8]) -> Result<Format, AudioError> {
    if fmt.len() < 16 {
        return Err(AudioError::Truncated);
    }
    let mut tag = u16_at(fmt, 0);
    let channels = u16_at(fmt, 2);
    let rate = u32_at(fmt, 4);
    let bits = u16_at(fmt, 14);
    if tag == FORMAT_EXTENSIBLE {
        // WAVEFORMATEXTENSIBLE: the sub-format GUID starts with the real format tag.
        if fmt.len() < 26 {
            return Err(AudioError::Truncated);
        }
        tag = u16_at(fmt, 24);
    }
    let float = match (tag, bits) {
        (FORMAT_PCM, 8 | 16 | 24 | 32) => false,
        (FORMAT_FLOAT, 32) => true,
        _ => return Err(AudioError::Unsupported(format!("format {tag}, {bits}-bit"))),
    };
    if !(1..=8).contains(&channels) {
        return Err(AudioError::Unsupported(format!("{channels} channels")));
    }
    if !(8_000..=48_000).contains(&rate) {
        return Err(AudioError::Unsupported(format!("{rate} Hz")));
    }
    Ok(Format {
        float,
        channels,
        rate,
        bits,
    })
}

/// One sample as -1.0..1.0.
fn sample(b: &[u8], f: Format) -> f32 {
    match (f.float, f.bits) {
        (true, _) => f32::from_le_bytes([b[0], b[1], b[2], b[3]]),
        // 8-bit WAV is unsigned.
        (false, 8) => (f32::from(b[0]) - 128.0) / 128.0,
        (false, 16) => f32::from(i16::from_le_bytes([b[0], b[1]])) / 32768.0,
        (false, 24) => (i32::from_le_bytes([0, b[0], b[1], b[2]]) >> 8) as f32 / 8_388_608.0,
        (false, _) => i32::from_le_bytes([b[0], b[1], b[2], b[3]]) as f32 / 2_147_483_648.0,
    }
}

fn downmix(data: &[u8], f: Format) -> Vec<f32> {
    let width = usize::from(f.bits / 8);
    let channels = usize::from(f.channels);
    data.chunks_exact(f.frame_bytes())
        .map(|frame| {
            let sum: f32 = frame.chunks_exact(width).map(|s| sample(s, f)).sum();
            let x = sum / channels as f32;
            // NaN or infinite floats from a broken file become silence.
            if x.is_finite() {
                x
            } else {
                0.0
            }
        })
        .collect()
}

/// Linear-interpolation resampling. Going down, each output sample first averages the
/// input samples it covers (a box low-pass), which keeps aliasing low enough for speech.
fn resample(input: &[f32], from: u32, to: u32) -> Vec<f32> {
    if from == to || input.is_empty() {
        return input.to_vec();
    }
    let ratio = f64::from(from) / f64::from(to);
    let out_len = ((input.len() as f64) / ratio).floor() as usize;
    let last = input.len() - 1;
    (0..out_len)
        .map(|i| {
            let pos = i as f64 * ratio;
            if ratio > 1.0 {
                let start = pos.floor() as usize;
                let end = ((pos + ratio).ceil() as usize)
                    .min(input.len())
                    .max(start + 1);
                let window = &input[start.min(last)..end];
                window.iter().sum::<f32>() / window.len() as f32
            } else {
                let j = pos.floor() as usize;
                let frac = (pos - j as f64) as f32;
                let a = input[j.min(last)];
                let b = input[(j + 1).min(last)];
                a + (b - a) * frac
            }
        })
        .collect()
}

fn u16_at(b: &[u8], at: usize) -> u16 {
    u16::from_le_bytes([b[at], b[at + 1]])
}

fn u32_at(b: &[u8], at: usize) -> u32 {
    u32::from_le_bytes([b[at], b[at + 1], b[at + 2], b[at + 3]])
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A WAV file with the given format and raw sample bytes.
    fn wav(tag: u16, channels: u16, rate: u32, bits: u16, data: &[u8]) -> Vec<u8> {
        let mut out = Vec::new();
        out.extend_from_slice(b"RIFF");
        out.extend_from_slice(&(36 + data.len() as u32).to_le_bytes());
        out.extend_from_slice(b"WAVEfmt ");
        out.extend_from_slice(&16u32.to_le_bytes());
        out.extend_from_slice(&tag.to_le_bytes());
        out.extend_from_slice(&channels.to_le_bytes());
        out.extend_from_slice(&rate.to_le_bytes());
        let block = channels * bits / 8;
        out.extend_from_slice(&(rate * u32::from(block)).to_le_bytes());
        out.extend_from_slice(&block.to_le_bytes());
        out.extend_from_slice(&bits.to_le_bytes());
        out.extend_from_slice(b"data");
        out.extend_from_slice(&(data.len() as u32).to_le_bytes());
        out.extend_from_slice(data);
        out
    }

    /// `seconds` of a 440 Hz sine at `rate`, stereo 16-bit, left at full and right at half
    /// amplitude.
    fn stereo_sine(rate: u32, seconds: f32) -> Vec<u8> {
        let n = (rate as f32 * seconds) as usize;
        let mut data = Vec::with_capacity(n * 4);
        for i in 0..n {
            let x = (2.0 * std::f32::consts::PI * 440.0 * i as f32 / rate as f32).sin();
            data.extend_from_slice(&((x * 20000.0) as i16).to_le_bytes());
            data.extend_from_slice(&((x * 10000.0) as i16).to_le_bytes());
        }
        wav(FORMAT_PCM, 2, rate, 16, &data)
    }

    /// Stereo 48 kHz becomes 16 kHz mono with the same length and level; float 8 kHz is
    /// upsampled; canonical 16 kHz mono round-trips unchanged.
    #[test]
    fn clips_are_converted_to_16k_mono_and_back() {
        let clip = decode(&stereo_sine(48_000, 1.5)).unwrap();
        assert_eq!(clip.samples().len(), 24_000);
        assert_eq!(clip.millis(), 1500);
        // Downmixed: (20000 + 10000) / 2 = 15000 peak; the box filter costs a little.
        let peak = clip
            .samples()
            .iter()
            .map(|s| s.unsigned_abs())
            .max()
            .unwrap();
        assert!((13_500..=15_100).contains(&peak), "peak {peak}");

        let samples: Vec<i16> = (0..8000)
            .map(|i| ((i % 200) * 100 - 10_000) as i16)
            .collect();
        let clip = Clip::from_samples(samples.clone());
        let back = decode(&clip.to_wav()).unwrap();
        assert_eq!(back.samples(), &samples[..]);
        // The canonical header: PCM, mono, 16 kHz, 16-bit.
        let w = clip.to_wav();
        assert_eq!((u16_at(&w, 20), u16_at(&w, 22)), (1, 1));
        assert_eq!((u32_at(&w, 24), u16_at(&w, 34)), (16_000, 16));
        assert_eq!(u32_at(&w, 40) as usize, samples.len() * 2);

        let data: Vec<u8> = (0..4000)
            .flat_map(|i| (if i % 2 == 0 { 0.5f32 } else { -0.5 }).to_le_bytes())
            .collect();
        let clip = decode(&wav(FORMAT_FLOAT, 1, 8_000, 32, &data)).unwrap();
        assert_eq!(clip.samples().len(), 8000);
        assert!(clip.samples().iter().all(|s| s.unsigned_abs() <= 16_384));
    }

    #[test]
    fn other_chunks_odd_padding_and_unknown_data_length_are_handled() {
        let mut w = stereo_sine(16_000, 0.5);
        // Insert an odd-length LIST chunk (padded) before "data".
        let data_at = w.windows(4).position(|x| x == b"data").unwrap();
        let mut list = b"LIST".to_vec();
        list.extend_from_slice(&3u32.to_le_bytes());
        list.extend_from_slice(b"abc\0");
        w.splice(data_at..data_at, list);
        // Streaming writers leave the data length as 0xFFFFFFFF.
        let len_at = w.windows(4).position(|x| x == b"data").unwrap() + 4;
        w[len_at..len_at + 4].copy_from_slice(&u32::MAX.to_le_bytes());
        assert_eq!(decode(&w).unwrap().samples().len(), 8000);
    }

    #[test]
    fn bad_input_is_refused() {
        assert_eq!(decode(b"OggS\0\0\0\0"), Err(AudioError::NotWav));
        assert_eq!(decode(b"RIFF\0\0\0\0WAVE"), Err(AudioError::NotWav));
        let tiny = wav(FORMAT_PCM, 1, 16_000, 16, &[0; 200]);
        assert_eq!(decode(&tiny), Err(AudioError::Empty));
        let alaw = wav(6, 1, 8_000, 8, &[0; 16_000]);
        assert!(matches!(decode(&alaw), Err(AudioError::Unsupported(_))));
        let fast = wav(FORMAT_PCM, 1, 96_000, 16, &[0; 96_000]);
        assert!(matches!(decode(&fast), Err(AudioError::Unsupported(_))));
        let mut cut = stereo_sine(16_000, 0.5);
        cut.truncate(30);
        assert_eq!(decode(&cut), Err(AudioError::Truncated));
    }

    #[test]
    fn clips_over_two_minutes_are_refused_before_converting() {
        // 121 s of 8 kHz 8-bit mono: small file, long clip.
        let long = wav(FORMAT_PCM, 1, 8_000, 8, &vec![128; 8_000 * 121]);
        assert_eq!(decode(&long), Err(AudioError::TooLong { seconds: 121 }));
        let ok = wav(FORMAT_PCM, 1, 8_000, 8, &vec![128; 8_000 * 120]);
        assert_eq!(decode(&ok).unwrap().millis(), 120_000);
    }
}

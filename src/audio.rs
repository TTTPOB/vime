//! Audio normalization shared by WAV input and the WASAPI capture callback.
use anyhow::{bail, Context, Result};
use std::path::Path;

pub const SAMPLE_RATE: u32 = 24_000;

/// Linear streaming resampler. Keeps interpolation state across callback boundaries.
pub struct Resampler {
    step: u64,
    position: u64,
    pending: Vec<f32>,
}
impl Resampler {
    pub fn new(input_rate: u32) -> Result<Self> {
        if input_rate == 0 {
            bail!("audio sample rate must be positive");
        }
        Ok(Self {
            step: input_rate as u64,
            position: 0,
            pending: Vec::new(),
        })
    }
    pub fn push(&mut self, mono: &[f32]) -> Vec<i16> {
        self.pending.extend_from_slice(mono);
        let mut out = Vec::new();
        while self.position / SAMPLE_RATE as u64 + 1 < self.pending.len() as u64 {
            let index = (self.position / SAMPLE_RATE as u64) as usize;
            let fraction = (self.position % SAMPLE_RATE as u64) as f32 / SAMPLE_RATE as f32;
            out.push(to_pcm(
                self.pending[index] * (1.0 - fraction) + self.pending[index + 1] * fraction,
            ));
            self.position += self.step;
        }
        let consumed = ((self.position / SAMPLE_RATE as u64) as usize).min(self.pending.len());
        self.pending.drain(..consumed);
        self.position -= consumed as u64 * SAMPLE_RATE as u64;
        out
    }
    pub fn finish(&mut self) -> Vec<i16> {
        let mut out = Vec::new();
        while self.position < self.pending.len() as u64 * SAMPLE_RATE as u64 {
            let index = (self.position / SAMPLE_RATE as u64) as usize;
            out.push(to_pcm(self.pending[index]));
            self.position += self.step;
        }
        self.pending.clear();
        self.position = 0;
        out
    }
}
fn to_pcm(value: f32) -> i16 {
    if !value.is_finite() {
        return 0;
    }
    (value.clamp(-1.0, 1.0) * 32768.0)
        .round()
        .clamp(-32768.0, 32767.0) as i16
}
pub fn downmix(samples: &[f32], channels: usize) -> Result<Vec<f32>> {
    if channels == 0 || !samples.len().is_multiple_of(channels) {
        bail!("invalid interleaved audio channels");
    }
    Ok(samples
        .chunks_exact(channels)
        .map(|frame| frame.iter().sum::<f32>() / channels as f32)
        .collect())
}
pub fn load_wav(path: impl AsRef<Path>) -> Result<Vec<i16>> {
    let mut reader = hound::WavReader::open(path).context("open WAV input")?;
    let spec = reader.spec();
    if spec.channels == 0 || spec.sample_rate == 0 {
        bail!("invalid WAV sample format");
    }
    let samples: Vec<f32> = match spec.sample_format {
        hound::SampleFormat::Float => reader
            .samples::<f32>()
            .collect::<std::result::Result<_, _>>()?,
        hound::SampleFormat::Int => {
            if spec.bits_per_sample == 0 || spec.bits_per_sample > 32 {
                bail!("unsupported WAV bit depth");
            }
            let scale = 2_f64.powi(spec.bits_per_sample as i32 - 1);
            reader
                .samples::<i32>()
                .map(|s| s.map(|v| (v as f64 / scale) as f32))
                .collect::<std::result::Result<_, _>>()?
        }
    };
    let mono = downmix(&samples, spec.channels as usize)?;
    let mut resampler = Resampler::new(spec.sample_rate)?;
    let mut result = resampler.push(&mono);
    result.extend(resampler.finish());
    Ok(result)
}
#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn streaming_boundaries_preserve_samples() {
        for rate in [8000, 16000, 24000, 44100, 48000, 96000] {
            let samples: Vec<f32> = (0..1000).map(|n| (n as f32 * 0.03).sin()).collect();
            let mut whole = Resampler::new(rate).unwrap();
            let mut expected = whole.push(&samples);
            expected.extend(whole.finish());
            let mut streaming = Resampler::new(rate).unwrap();
            let mut actual = Vec::new();
            for chunk in samples.chunks(7) {
                actual.extend(streaming.push(chunk));
            }
            actual.extend(streaming.finish());
            assert_eq!(actual, expected, "sample rate {rate}");
        }
    }
    #[test]
    fn wav_stereo_normalizes_to_mono_24khz() {
        let temp = tempfile::NamedTempFile::new().unwrap();
        let spec = hound::WavSpec {
            channels: 2,
            sample_rate: 48000,
            bits_per_sample: 16,
            sample_format: hound::SampleFormat::Int,
        };
        let mut writer = hound::WavWriter::create(temp.path(), spec).unwrap();
        for _ in 0..480 {
            writer.write_sample(16384_i16).unwrap();
            writer.write_sample(0_i16).unwrap();
        }
        writer.finalize().unwrap();
        let result = load_wav(temp.path()).unwrap();
        assert_eq!(result.len(), 240);
        assert!(result.iter().all(|&s| s == 8192));
    }
    #[test]
    fn saturation_and_non_finite_are_safe() {
        assert_eq!(to_pcm(-1.0), i16::MIN);
        assert_eq!(to_pcm(1.0), i16::MAX);
        assert_eq!(to_pcm(f32::NAN), 0);
    }
}

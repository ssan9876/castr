use anyhow::Context;
use castr_media::audio::{AudioDecoder, CHANNELS, SAMPLE_RATE};
use sdl2::audio::{AudioQueue, AudioSpecDesired};

pub fn resample_linear(input: &[i16], ratio: f64) -> Vec<i16> {
    if (ratio - 1.0).abs() < 1e-9 || input.len() < 4 {
        return input.to_vec();
    }
    let frames_in = input.len() / CHANNELS;
    let frames_out = ((frames_in as f64) / ratio).round().max(1.0) as usize;
    let mut out = Vec::with_capacity(frames_out * CHANNELS);
    for i in 0..frames_out {
        let pos = i as f64 * ratio;
        let idx = (pos.floor() as usize).min(frames_in - 1);
        let next = (idx + 1).min(frames_in - 1);
        let t = pos - idx as f64;
        for ch in 0..CHANNELS {
            let a = input[idx * CHANNELS + ch] as f64;
            let b = input[next * CHANNELS + ch] as f64;
            out.push((a + (b - a) * t).round() as i16);
        }
    }
    out
}

pub struct AudioOut {
    queue: AudioQueue<i16>,
    decoder: AudioDecoder,
    pub target_us: u64,
}

impl AudioOut {
    pub fn new(audio: &sdl2::AudioSubsystem, target_us: u64) -> anyhow::Result<Self> {
        let spec = AudioSpecDesired {
            freq: Some(SAMPLE_RATE as i32),
            channels: Some(CHANNELS as u8),
            samples: Some(480),
        };
        let queue = audio
            .open_queue::<i16, _>(None, &spec)
            .map_err(|e| anyhow::anyhow!(e))
            .context("open audio queue")?;
        queue.resume();
        Ok(Self {
            queue,
            decoder: AudioDecoder::new()?,
            target_us,
        })
    }

    /// Microseconds of audio queued but not yet played.
    pub fn buffered_us(&self) -> u64 {
        let samples = self.queue.size() as u64 / (2 * CHANNELS as u64);
        samples * 1_000_000 / SAMPLE_RATE as u64
    }

    /// Decode and queue one Opus packet. Returns Ok(true) if queued, Ok(false) if dropped for being far ahead.
    pub fn push_packet(&mut self, packet: &[u8], drift_ratio: f64) -> anyhow::Result<bool> {
        if self.buffered_us() > self.target_us * 4 {
            return Ok(false);
        }
        let pcm = self.decoder.decode(Some(packet))?;
        let pcm = resample_linear(&pcm, drift_ratio);
        self.queue
            .queue_audio(&pcm)
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(true)
    }

    /// Queue samples that arrived uncompressed, as Miracast's LPCM does.
    /// Same drift correction as the Opus path, no decoder in the way.
    pub fn push_pcm(&mut self, pcm: &[i16], drift_ratio: f64) -> anyhow::Result<bool> {
        if self.buffered_us() > self.target_us * 4 {
            return Ok(false);
        }
        let pcm = resample_linear(pcm, drift_ratio);
        self.queue
            .queue_audio(&pcm)
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(true)
    }

    pub fn conceal_one(&mut self) -> anyhow::Result<()> {
        let pcm = self.decoder.decode(None)?;
        self.queue
            .queue_audio(&pcm)
            .map_err(|e| anyhow::anyhow!(e))?;
        Ok(())
    }

    pub fn clear(&mut self) {
        self.queue.clear();
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn ratio_one_is_identity() {
        let input: Vec<i16> = (0..200).collect();
        assert_eq!(resample_linear(&input, 1.0), input);
    }

    #[test]
    fn ratio_above_one_shortens_and_below_lengthens() {
        let input: Vec<i16> = (0..2000).collect();
        let fast = resample_linear(&input, 1.005);
        let slow = resample_linear(&input, 0.995);
        assert!(fast.len() < input.len() && fast.len().is_multiple_of(2));
        assert!(slow.len() > input.len() && slow.len().is_multiple_of(2));
        assert!((fast.len() as f64 - 2000.0 / 1.005).abs() <= 2.0);
    }

    #[test]
    fn keeps_channels_interleaved() {
        let input: Vec<i16> = (0..1000).flat_map(|_| [1000i16, -1000i16]).collect();
        for s in resample_linear(&input, 1.003).chunks(2) {
            assert!(s[0] > 900 && s[1] < -900);
        }
    }
}

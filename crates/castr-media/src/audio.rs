use anyhow::Context;
use audiopus::{coder, Application, Bitrate, Channels, SampleRate};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const FRAME_SAMPLES: usize = 480;
pub const FRAME_INTERLEAVED: usize = FRAME_SAMPLES * CHANNELS;
const MAX_PACKET: usize = 1500;

pub struct AudioEncoder {
    inner: coder::Encoder,
}

impl AudioEncoder {
    pub fn new() -> anyhow::Result<Self> {
        let mut inner =
            coder::Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::LowDelay)
                .context("opus encoder")?;
        inner
            .set_bitrate(Bitrate::BitsPerSecond(128_000))
            .context("opus bitrate")?;
        Ok(Self { inner })
    }

    pub fn encode(&mut self, pcm: &[i16]) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(
            pcm.len() == FRAME_INTERLEAVED,
            "expected {FRAME_INTERLEAVED} samples, got {}",
            pcm.len()
        );
        let mut out = vec![0u8; MAX_PACKET];
        let n = self.inner.encode(pcm, &mut out).context("opus encode")?;
        out.truncate(n);
        Ok(out)
    }
}

pub struct AudioDecoder {
    inner: coder::Decoder,
}

impl AudioDecoder {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            inner: coder::Decoder::new(SampleRate::Hz48000, Channels::Stereo)
                .context("opus decoder")?,
        })
    }

    pub fn decode(&mut self, packet: Option<&[u8]>) -> anyhow::Result<Vec<i16>> {
        let mut out = vec![0i16; FRAME_INTERLEAVED];
        let n = self
            .inner
            .decode(packet, &mut out[..], false)
            .context("opus decode")?;
        out.truncate(n * CHANNELS);
        Ok(out)
    }
}

#[derive(Default)]
pub struct FrameChunker {
    buf: Vec<i16>,
}

impl FrameChunker {
    pub fn new() -> Self {
        Self::default()
    }
    pub fn push(&mut self, samples: &[i16]) {
        self.buf.extend_from_slice(samples);
    }
    pub fn next_frame(&mut self) -> Option<Vec<i16>> {
        if self.buf.len() < FRAME_INTERLEAVED {
            return None;
        }
        let rest = self.buf.split_off(FRAME_INTERLEAVED);
        Some(std::mem::replace(&mut self.buf, rest))
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frame_idx: usize) -> Vec<i16> {
        (0..FRAME_SAMPLES)
            .flat_map(|i| {
                let t = (frame_idx * FRAME_SAMPLES + i) as f32 / SAMPLE_RATE as f32;
                let s = ((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16;
                [s, s]
            })
            .collect()
    }

    #[test]
    fn encode_decode_preserves_frame_size_and_is_small() {
        let mut enc = AudioEncoder::new().unwrap();
        let mut dec = AudioDecoder::new().unwrap();
        for i in 0..20 {
            let pkt = enc.encode(&sine(i)).unwrap();
            assert!(
                pkt.len() < 400,
                "packet {} bytes, expected < 400 at 128 kbps/10 ms",
                pkt.len()
            );
            let out = dec.decode(Some(&pkt)).unwrap();
            assert_eq!(out.len(), FRAME_INTERLEAVED);
        }
    }

    #[test]
    fn decoded_signal_resembles_input_after_warmup() {
        let mut enc = AudioEncoder::new().unwrap();
        let mut dec = AudioDecoder::new().unwrap();
        let mut last_in = Vec::new();
        let mut last_out = Vec::new();
        for i in 0..30 {
            let pcm = sine(i);
            let out = dec.decode(Some(&enc.encode(&pcm).unwrap())).unwrap();
            last_in = pcm;
            last_out = out;
        }
        let energy_in: f64 = last_in.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
        let energy_out: f64 = last_out.iter().map(|s| (*s as f64).powi(2)).sum::<f64>();
        let ratio = energy_out / energy_in;
        assert!((0.5..2.0).contains(&ratio), "energy ratio {ratio}");
    }

    #[test]
    fn plc_on_lost_packet_returns_a_full_frame() {
        let mut enc = AudioEncoder::new().unwrap();
        let mut dec = AudioDecoder::new().unwrap();
        dec.decode(Some(&enc.encode(&sine(0)).unwrap())).unwrap();
        assert_eq!(dec.decode(None).unwrap().len(), FRAME_INTERLEAVED);
    }

    #[test]
    fn encoder_rejects_wrong_length() {
        let mut enc = AudioEncoder::new().unwrap();
        assert!(enc.encode(&[0i16; 100]).is_err());
    }

    #[test]
    fn chunker_yields_exact_frames() {
        let mut c = FrameChunker::new();
        c.push(&[1i16; 700]);
        assert!(c.next_frame().is_none());
        c.push(&[2i16; 700]);
        let f = c.next_frame().unwrap();
        assert_eq!(f.len(), FRAME_INTERLEAVED);
        assert_eq!(f[699], 1);
        assert_eq!(f[700], 2);
        assert!(c.next_frame().is_none());
        c.push(&[3i16; 520]);
        assert_eq!(c.next_frame().unwrap().len(), FRAME_INTERLEAVED);
    }
}

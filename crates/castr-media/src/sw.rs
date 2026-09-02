use crate::codec::*;
use anyhow::Context;
use openh264::decoder::Decoder;
use openh264::encoder::{Encoder, EncoderConfig as OhConfig, RateControlMode, UsageType};
use openh264::formats::YUVSource;
use openh264::OpenH264API;

struct I420Source<'a> {
    w: usize,
    h: usize,
    data: &'a [u8],
}

impl YUVSource for I420Source<'_> {
    fn dimensions(&self) -> (usize, usize) {
        (self.w, self.h)
    }
    fn strides(&self) -> (usize, usize, usize) {
        (self.w, self.w / 2, self.w / 2)
    }
    fn y(&self) -> &[u8] {
        &self.data[..self.w * self.h]
    }
    fn u(&self) -> &[u8] {
        let n = self.w * self.h;
        &self.data[n..n + n / 4]
    }
    fn v(&self) -> &[u8] {
        let n = self.w * self.h;
        &self.data[n + n / 4..n + n / 2]
    }
}

pub struct SwEncoder {
    cfg: EncoderConfig,
    inner: Encoder,
    force_key: bool,
}

impl SwEncoder {
    pub fn new(cfg: EncoderConfig) -> anyhow::Result<Self> {
        Ok(Self {
            inner: Self::build(&cfg)?,
            cfg,
            force_key: true,
        })
    }

    // Note on openh264 0.6.6 (this workspace's resolved version): the plan's
    // `BitRate`/`FrameRate`/`IntraFramePeriod`/`Profile` types and the
    // `.bitrate()`/`.profile()`/`.intra_frame_period()` builder methods do not
    // exist in this version's `openh264::encoder::EncoderConfig`. The
    // equivalents actually available are `.set_bitrate_bps(u32)` and
    // `.max_frame_rate(f32)`; there is no periodic-intraframe/profile knob at
    // all in 0.6.6, so those settings are dropped. Keyframes are instead
    // driven entirely by explicit `force_intra_frame()` calls (first frame,
    // `request_keyframe`, and after `set_bitrate`/`set_mode`), which preserves
    // the behavior the tests exercise.
    fn build(cfg: &EncoderConfig) -> anyhow::Result<Encoder> {
        let oh = OhConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .rate_control_mode(RateControlMode::Bitrate)
            .set_bitrate_bps(cfg.bitrate_bps)
            .max_frame_rate(cfg.fps as f32)
            .enable_skip_frame(false);
        Encoder::with_api_config(OpenH264API::from_source(), oh).context("openh264 encoder init")
    }
}

impl VideoEncoder for SwEncoder {
    fn encode(&mut self, frame: &RawFrame) -> anyhow::Result<Option<EncodedFrame>> {
        anyhow::ensure!(frame.format == PixelFormat::I420, "SwEncoder expects I420");
        anyhow::ensure!(
            frame.width == self.cfg.width && frame.height == self.cfg.height,
            "frame size mismatch"
        );
        if self.force_key {
            self.inner.force_intra_frame();
            self.force_key = false;
        }
        let src = I420Source {
            w: frame.width as usize,
            h: frame.height as usize,
            data: &frame.data,
        };
        let bs = self.inner.encode(&src).context("openh264 encode")?;
        let data = bs.to_vec();
        if data.is_empty() {
            return Ok(None);
        }
        let keyframe = matches!(
            bs.frame_type(),
            openh264::encoder::FrameType::IDR | openh264::encoder::FrameType::I
        );
        Ok(Some(EncodedFrame {
            data,
            keyframe,
            timestamp_us: frame.timestamp_us,
        }))
    }

    fn request_keyframe(&mut self) {
        self.force_key = true;
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> anyhow::Result<()> {
        self.cfg.bitrate_bps = bitrate_bps;
        self.inner = Self::build(&self.cfg)?;
        self.force_key = true;
        Ok(())
    }

    fn set_mode(&mut self, mode: Mode) -> anyhow::Result<()> {
        self.cfg.mode = mode;
        self.inner = Self::build(&self.cfg)?;
        self.force_key = true;
        Ok(())
    }

    fn input_format(&self) -> PixelFormat {
        PixelFormat::I420
    }
    fn name(&self) -> &'static str {
        "openh264"
    }
}

pub struct SwDecoder {
    inner: Decoder,
}

impl SwDecoder {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self {
            inner: Decoder::new().context("openh264 decoder init")?,
        })
    }
}

impl VideoDecoder for SwDecoder {
    fn decode(&mut self, data: &[u8], timestamp_us: u64) -> anyhow::Result<Option<RawFrame>> {
        let Some(yuv) = self.inner.decode(data).context("openh264 decode")? else {
            return Ok(None);
        };
        let (w, h) = yuv.dimensions();
        let (sy, su, sv) = yuv.strides();
        let mut out = Vec::with_capacity(w * h * 3 / 2);
        for row in 0..h {
            out.extend_from_slice(&yuv.y()[row * sy..row * sy + w]);
        }
        for row in 0..h / 2 {
            out.extend_from_slice(&yuv.u()[row * su..row * su + w / 2]);
        }
        for row in 0..h / 2 {
            out.extend_from_slice(&yuv.v()[row * sv..row * sv + w / 2]);
        }
        Ok(Some(RawFrame {
            format: PixelFormat::I420,
            width: w as u32,
            height: h as u32,
            stride: w as u32,
            data: out,
            timestamp_us,
        }))
    }
    fn name(&self) -> &'static str {
        "openh264"
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn cfg() -> EncoderConfig {
        EncoderConfig {
            width: 320,
            height: 240,
            fps: 30,
            bitrate_bps: 800_000,
            mode: Mode::Game,
        }
    }

    fn gradient_frame(i: u32) -> RawFrame {
        let (w, h) = (320u32, 240u32);
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[
                    ((x + i * 3) % 256) as u8,
                    (y % 256) as u8,
                    ((x + y) % 256) as u8,
                    255,
                ]);
            }
        }
        crate::convert::convert(
            &RawFrame {
                format: PixelFormat::Bgra,
                width: w,
                height: h,
                stride: w * 4,
                data,
                timestamp_us: i as u64 * 33_333,
            },
            PixelFormat::I420,
        )
    }

    #[test]
    fn first_frame_is_keyframe_and_round_trips() {
        let mut enc = SwEncoder::new(cfg()).unwrap();
        let mut dec = SwDecoder::new().unwrap();
        assert_eq!(enc.input_format(), PixelFormat::I420);
        let f0 = gradient_frame(0);
        let e0 = enc
            .encode(&f0)
            .unwrap()
            .expect("encoder must emit first frame");
        assert!(e0.keyframe);
        assert_eq!(e0.timestamp_us, 0);
        assert!(
            e0.data.starts_with(&[0, 0, 0, 1]) || e0.data.starts_with(&[0, 0, 1]),
            "Annex B start code"
        );
        let d0 = dec
            .decode(&e0.data, e0.timestamp_us)
            .unwrap()
            .expect("decoder must output first frame");
        assert_eq!(
            (d0.width, d0.height, d0.format),
            (320, 240, PixelFormat::I420)
        );
        assert_eq!(d0.data.len(), 320 * 240 * 3 / 2);
        assert_eq!(d0.timestamp_us, 0);
        let mean_diff: f64 = d0.data[..320 * 240]
            .iter()
            .zip(&f0.data[..320 * 240])
            .map(|(a, b)| (*a as f64 - *b as f64).abs())
            .sum::<f64>()
            / (320.0 * 240.0);
        assert!(mean_diff < 8.0, "luma mean abs diff {mean_diff}");
    }

    #[test]
    fn delta_frames_follow_and_keyframe_request_is_honored() {
        let mut enc = SwEncoder::new(cfg()).unwrap();
        let mut dec = SwDecoder::new().unwrap();
        let mut keyframes = 0;
        for i in 0..10 {
            if i == 5 {
                enc.request_keyframe();
            }
            let e = enc.encode(&gradient_frame(i)).unwrap().unwrap();
            if e.keyframe {
                keyframes += 1;
            }
            assert!(dec.decode(&e.data, e.timestamp_us).unwrap().is_some());
        }
        assert!(
            keyframes >= 2,
            "expected initial + requested keyframe, got {keyframes}"
        );
    }

    #[test]
    fn set_bitrate_forces_keyframe() {
        let mut enc = SwEncoder::new(cfg()).unwrap();
        enc.encode(&gradient_frame(0)).unwrap();
        let e1 = enc.encode(&gradient_frame(1)).unwrap().unwrap();
        assert!(!e1.keyframe);
        enc.set_bitrate(400_000).unwrap();
        let e2 = enc.encode(&gradient_frame(2)).unwrap().unwrap();
        assert!(e2.keyframe);
    }

    #[test]
    fn decoder_returns_none_on_garbage_without_panicking() {
        let mut dec = SwDecoder::new().unwrap();
        let r = dec.decode(&[0, 0, 0, 1, 0x65, 1, 2, 3], 0);
        assert!(r.is_err() || r.unwrap().is_none());
    }
}

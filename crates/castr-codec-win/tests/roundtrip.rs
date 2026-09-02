use castr_codec_win::*;
use castr_media::sw::SwDecoder;
use castr_media::*;

fn cfg() -> EncoderConfig {
    EncoderConfig {
        width: 640,
        height: 360,
        fps: 30,
        bitrate_bps: 2_000_000,
        mode: Mode::Game,
    }
}

fn frame(i: u32) -> RawFrame {
    let (w, h) = (640u32, 360u32);
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&[
                ((x + i * 5) % 256) as u8,
                (y % 256) as u8,
                ((x ^ y) % 256) as u8,
                255,
            ]);
        }
    }
    convert::convert(
        &RawFrame {
            format: PixelFormat::Bgra,
            width: w,
            height: h,
            stride: w * 4,
            data,
            timestamp_us: i as u64 * 33_333,
        },
        PixelFormat::Nv12,
    )
}

#[test]
fn mf_encoder_output_decodes_with_openh264() {
    let mut enc = MfEncoder::new(cfg()).unwrap();
    assert_eq!(enc.input_format(), PixelFormat::Nv12);
    let mut dec = SwDecoder::new().unwrap();
    let mut outputs = 0;
    let mut first_key = None;
    for i in 0..30 {
        if let Some(e) = enc.encode(&frame(i)).unwrap() {
            if first_key.is_none() {
                first_key = Some(e.keyframe);
            }
            outputs += 1;
            assert!(
                e.data.starts_with(&[0, 0, 0, 1]) || e.data.starts_with(&[0, 0, 1]),
                "Annex B"
            );
            if let Some(d) = dec.decode(&e.data, e.timestamp_us).unwrap() {
                assert_eq!((d.width, d.height), (640, 360));
            }
        }
    }
    assert!(
        outputs >= 25,
        "expected most inputs to produce output, got {outputs}"
    );
    assert_eq!(first_key, Some(true), "first output must be a keyframe");
}

#[test]
fn mf_encoder_honors_keyframe_request_and_live_bitrate() {
    let mut enc = MfEncoder::new(cfg()).unwrap();
    let mut keys = 0;
    for i in 0..20 {
        if i == 10 {
            enc.request_keyframe();
        }
        if i == 12 {
            enc.set_bitrate(800_000).unwrap();
        }
        if let Some(e) = enc.encode(&frame(i)).unwrap() {
            if e.keyframe {
                keys += 1;
            }
        }
    }
    assert!(
        keys >= 2,
        "expected initial keyframe plus a requested one, got {keys}"
    );
}

#[test]
fn mf_decoder_decodes_mf_encoder_output() {
    let mut enc = MfEncoder::new(cfg()).unwrap();
    let mut dec = MfDecoder::new().unwrap();
    let mut decoded = 0;
    for i in 0..30 {
        if let Some(e) = enc.encode(&frame(i)).unwrap() {
            if let Some(d) = dec.decode(&e.data, e.timestamp_us).unwrap() {
                assert_eq!((d.width, d.height, d.format), (640, 360, PixelFormat::Nv12));
                assert_eq!(d.data.len(), 640 * 360 * 3 / 2);
                decoded += 1;
            }
        }
    }
    assert!(decoded >= 20, "decoded {decoded}");
}

#[test]
fn mf_decoder_decodes_openh264_output() {
    let mut enc = castr_media::sw::SwEncoder::new(cfg()).unwrap();
    let mut dec = MfDecoder::new().unwrap();
    let mut decoded = 0;
    for i in 0..30 {
        let f = convert::convert(&frame(i), PixelFormat::I420);
        if let Some(e) = enc.encode(&f).unwrap() {
            if dec.decode(&e.data, e.timestamp_us).unwrap().is_some() {
                decoded += 1;
            }
        }
    }
    assert!(decoded >= 20, "decoded {decoded}");
}

//! Run on a Raspberry Pi with `--ignored`. See scripts/pi/run-hw-tests.sh.
#![cfg(target_os = "linux")]

use castr_codec_v4l2::V4l2Decoder;
use castr_media::sw::SwEncoder;
use castr_media::*;
use std::time::{Duration, Instant};

fn frame(w: u32, h: u32, i: u32, fmt: PixelFormat) -> RawFrame {
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
        fmt,
    )
}

/// Encode `n` frames at `w`x`h` with the software encoder; returns (data, timestamp) per access unit.
fn clip(w: u32, h: u32, n: u32, first_ts: u64) -> Vec<(Vec<u8>, u64)> {
    let mut enc = SwEncoder::new(EncoderConfig {
        width: w,
        height: h,
        fps: 30,
        bitrate_bps: 4_000_000,
        mode: Mode::Game,
    })
    .unwrap();
    let fmt = enc.input_format();
    let mut out = Vec::new();
    for i in 0..n {
        let mut f = frame(w, h, i, fmt);
        f.timestamp_us = first_ts + i as u64 * 33_333;
        if let Some(e) = enc.encode(&f).unwrap() {
            out.push((e.data, e.timestamp_us));
        }
    }
    assert!(
        out.len() >= n as usize - 2,
        "encoder produced {} of {n}",
        out.len()
    );
    out
}

/// Feed every access unit, then pull out the pictures still inside the
/// decoder. The V4L2 decoder is pipelined (a picture comes out a few access
/// units after the one that produced it) and the driver batches heavily when
/// it is fed faster than real time, so the tail has to be collected
/// explicitly. `poll_frame` does that without feeding anything, which is also
/// what the receiver does when its jitter buffer runs dry.
fn drain(dec: &mut V4l2Decoder, aus: &[(Vec<u8>, u64)]) -> Vec<RawFrame> {
    let mut frames = Vec::new();
    for (au, ts) in aus {
        if let Some(f) = dec.decode(au, *ts).unwrap() {
            frames.push(f);
        }
    }
    frames.extend(tail(dec, aus.len() - frames.len()));
    frames
}

/// Pull up to `want` more pictures out of the decoder. It can legitimately go
/// quiet for a moment - while it waits for the driver's first SOURCE_CHANGE,
/// or while it reallocates buffers across a resolution change - so give up
/// only after several empty polls in a row.
fn tail(dec: &mut V4l2Decoder, want: usize) -> Vec<RawFrame> {
    let mut out = Vec::new();
    let mut quiet = 0;
    let deadline = Instant::now() + Duration::from_secs(5);
    while out.len() < want && quiet < 5 && Instant::now() < deadline {
        match dec.poll_frame().unwrap() {
            Some(f) => {
                quiet = 0;
                out.push(f);
            }
            None => quiet += 1,
        }
    }
    out
}

#[test]
#[ignore]
fn decodes_a_synthetic_clip() {
    let aus = clip(640, 360, 30, 0);
    let mut dec = V4l2Decoder::open().unwrap();
    let frames = drain(&mut dec, &aus);
    assert!(frames.len() >= 25, "decoded only {} frames", frames.len());
    assert_eq!(dec.frame_size(), Some((640, 360)));
    for f in &frames {
        assert_eq!((f.width, f.height, f.format), (640, 360, PixelFormat::Nv12));
        assert_eq!(f.data.len(), 640 * 360 * 3 / 2);
    }
    let ts: Vec<u64> = frames.iter().map(|f| f.timestamp_us).collect();
    assert!(
        ts.windows(2).all(|w| w[0] <= w[1]),
        "timestamps not monotonic: {ts:?}"
    );
    let mid = &frames[frames.len() / 2];
    let y = &mid.data[..640 * 360];
    let distinct = y.iter().collect::<std::collections::HashSet<_>>().len();
    assert!(
        distinct > 16,
        "middle frame looks flat ({distinct} distinct luma values)"
    );
}

#[test]
#[ignore]
fn follows_a_resolution_change() {
    let mut aus = clip(640, 360, 30, 0);
    aus.extend(clip(1280, 720, 30, 30 * 33_333));
    let mut dec = V4l2Decoder::open().unwrap();
    let frames = drain(&mut dec, &aus);
    let small = frames.iter().filter(|f| f.width == 640).count();
    let large = frames.iter().filter(|f| f.width == 1280).count();
    assert!(small >= 25 && large >= 25, "small={small} large={large}");
    assert_eq!(dec.frame_size(), Some((1280, 720)));
}

#[test]
#[ignore]
fn decodes_1080p_in_real_time() {
    // Software-encoding 300 frames of 1080p on a Pi 3 takes minutes, so encode
    // one 60-frame GOP (starts with SPS/PPS/IDR) and replay it five times with
    // advancing timestamps; the decoder sees 300 valid access units.
    let base = clip(1920, 1080, 60, 0);
    let aus: Vec<(Vec<u8>, u64)> = (0..5u64)
        .flat_map(|r| {
            base.iter()
                .map(move |(d, ts)| (d.clone(), ts + r * 60 * 33_333))
        })
        .collect();
    let mut dec = V4l2Decoder::open().unwrap();
    let start = Instant::now();
    let mut worst = Duration::ZERO;
    let mut bring_up = Duration::ZERO;
    let mut running = 0usize;
    let mut n = 0;
    for (au, ts) in &aus {
        let t = Instant::now();
        if dec.decode(au, *ts).unwrap().is_some() {
            n += 1;
        }
        let call = t.elapsed();
        // The decoder cannot size its CAPTURE buffers until the stream's SPS
        // has been parsed, so a few access units in it allocates 6 x 3 MB of
        // CMA and restarts the VPU's output port. That one-off cost (paid per
        // resolution change, not per frame) is measured separately from the
        // per-frame budget the latency target is about.
        if dec.frame_size().is_none() || running < 3 {
            running += usize::from(dec.frame_size().is_some());
            bring_up = bring_up.max(call);
        } else {
            worst = worst.max(call);
        }
    }
    let total = start.elapsed();
    // Pictures still inside the decoder count too: fed this much faster than
    // real time the driver batches, so the tail comes out after the feed.
    let tail = tail(&mut dec, aus.len() - n).len();
    n += tail;
    eprintln!(
        "1080p: {n} frames ({tail} drained) in {total:?}, worst steady call {worst:?}, capture bring-up {bring_up:?}"
    );
    assert!(total < Duration::from_secs(10), "too slow: {total:?}");
    assert!(
        worst < Duration::from_millis(40),
        "worst steady decode call {worst:?}"
    );
    assert!(
        bring_up < Duration::from_millis(150),
        "capture bring-up {bring_up:?}"
    );
    assert!(n >= 290, "only {n} frames");
}

#[test]
#[ignore]
fn open_fails_cleanly_on_a_non_device() {
    let e = V4l2Decoder::open_path("/dev/null").unwrap_err();
    let s = format!("{e:#}");
    assert!(s.contains("/dev/null"), "{s}");
}

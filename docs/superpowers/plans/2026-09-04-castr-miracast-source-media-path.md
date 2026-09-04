# Miracast Source Media Path Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the source half of the Wi-Fi Display media path so `castr-sender` can stream a desktop to a Miracast sink over IP, negotiated through RTSP M1-M7.

**Architecture:** Five pure modules under a new `source` namespace inside `castr-miracast`, mirroring the sink modules that already exist beside them, plus one impure driver that owns sockets, encoder and capture. No radio in this sub-project: everything is exercised against our own Pi sink over Ethernet, which is also the Miracast-over-Infrastructure transport.

**Tech Stack:** Rust (workspace edition 2021), `castr-codec-win` (`MfEncoder`, hardware H.264), `castr-capture-win` (`DesktopCapture`, `LoopbackCapture`), existing `castr-miracast` modules `rtsp`, `wfd`, `ts`, `rtp`.

**Spec:** `docs/superpowers/specs/2026-09-04-castr-miracast-source-media-path-design.md`

## Global Constraints

- **Pure modules own no I/O.** `ts_mux`, `rtp_pack`, `lpcm`, `caps` and `source::session` take bytes and return bytes or actions. Sockets, clocks and threads belong to the driver. This is what makes the sink testable today and it is not negotiable for the source.
- **Pure modules build on every platform.** They are declared unconditionally in `lib.rs` so their tests run in the Windows workspace suite; only the driver is `cfg(windows)`.
- **Transport packet length is 188 bytes** (`ts::PACKET_LEN`). RTP payload is seven packets, 1316 bytes. RTP payload type is 33.
- **Timestamps are 90 kHz.** One monotonic origin per session, shared by audio and video.
- **Audio is LPCM 48 kHz 16-bit stereo, big-endian**, the only format the sink offers (`AudioCodecs::lpcm_48k_stereo`, bit 1).
- **Unknown RTSP parameters are ignored, never fatal.**
- **Every failure names its stage**: connect, negotiation, session, teardown.
- Windows suite: `cargo test -q --workspace`. Linux suite plus `clippy -D warnings`: `bash scripts/pi/test-linux.sh`. Both must pass before any commit.
- Commit messages follow the repository's style: lowercase `type(scope): summary`, then why it matters. Trailer: `Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>`.

## File Structure

| File | Responsibility |
|---|---|
| `crates/castr-miracast/src/source/mod.rs` | declares the namespace, nothing else |
| `crates/castr-miracast/src/source/lpcm.rs` | Wi-Fi Display LPCM framing |
| `crates/castr-miracast/src/source/ts_mux.rs` | PES packetization, PAT/PMT, PCR, continuity counters |
| `crates/castr-miracast/src/source/rtp_pack.rs` | RTP packetization |
| `crates/castr-miracast/src/source/caps.rs` | parse a sink's capabilities, choose one mode |
| `crates/castr-miracast/src/source/session.rs` | the M1-M7 source state machine |
| `crates/castr-sender/src/miracast_cast.rs` | the driver: sockets, encoder, capture, clock |
| `crates/castr-miracast/src/ts.rs` | modified: strip the LPCM header on the sink side (Task 1) |
| `crates/castr-miracast/src/lib.rs` | modified: declare `source`, rewrite the crate doc comment |

---

### Task 1: LPCM framing, and the sink bug it exposes

The sink treats a whole audio PES payload as raw big-endian samples
(`pipeline.rs:1235`). Wi-Fi Display LPCM is not raw: it carries a four-byte
audio data header ahead of the samples. If that is right, the sink has a latent
bug that a real Windows source would have hit, and it must be fixed here —
otherwise our source and our sink would agree with each other while both being
wrong, and every round-trip test in this plan would pass for the wrong reason.

**Read the specification before writing code.** Take the header layout from the
Wi-Fi Display specification's LPCM section, not from memory. The structure
below is what the tests assume; if the specification disagrees, the
specification wins and the tests change with it.

**Files:**
- Create: `crates/castr-miracast/src/source/mod.rs`
- Create: `crates/castr-miracast/src/source/lpcm.rs`
- Modify: `crates/castr-miracast/src/lib.rs`
- Modify: `crates/castr-receiver/src/pipeline.rs` (strip the header)

**Interfaces:**
- Produces: `source::lpcm::HEADER_LEN: usize`, `source::lpcm::frame(samples: &[i16]) -> Vec<u8>`, `source::lpcm::payload(frame: &[u8]) -> &[u8]`

- [ ] **Step 1: Create the namespace**

Create `crates/castr-miracast/src/source/mod.rs`:

```rust
//! The Wi-Fi Display *source* role: what we send when castr is the thing
//! being cast from, rather than the thing being cast to.
//!
//! Each module here mirrors a sink module beside it - `ts_mux` against `ts`,
//! `rtp_pack` against `rtp` - and follows the same rule: bytes in, bytes or
//! actions out, no sockets, so a whole session replays in a test.

pub mod lpcm;
```

Add to `crates/castr-miracast/src/lib.rs`, beside the other `pub mod` lines:

```rust
pub mod source;
```

The crate no longer describes only a sink, so replace the opening two lines of
its doc comment:

```rust
//! Miracast (Wi-Fi Display), both roles: a sink that receives a cast, and the
//! source half that sends one. The sink owns a Wi-Fi Direct group, an RTSP
//! session and MPEG-TS over RTP, decoded by the same pipeline castr's own
//! protocol uses; the `source` modules mirror it in the other direction.
//! Linux only in its radio layer; on other targets the pure layers still build
//! so the workspace compiles everywhere.
```

- [ ] **Step 2: Write the failing test**

In `crates/castr-miracast/src/source/lpcm.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_frame_is_a_header_then_big_endian_samples() {
        // Big-endian is not the machine's order, and getting it wrong yields
        // loud static rather than silence - so it is pinned to literal bytes.
        let out = frame(&[0x0102, -2]);
        assert_eq!(out.len(), HEADER_LEN + 4);
        assert_eq!(&out[HEADER_LEN..], &[0x01, 0x02, 0xff, 0xfe]);
    }

    #[test]
    fn the_header_declares_the_payload_size() {
        let out = frame(&[0; 480]);
        let declared = u16::from_be_bytes([out[0], out[1]]) as usize;
        assert_eq!(declared, 960, "480 stereo-interleaved samples are 960 bytes");
    }

    #[test]
    fn payload_skips_the_header() {
        let out = frame(&[0x1234]);
        assert_eq!(payload(&out), &[0x12, 0x34]);
    }

    #[test]
    fn a_runt_frame_yields_nothing_rather_than_panicking() {
        // A damaged stream must not take the process down.
        assert!(payload(&[0, 0]).is_empty());
    }
}
```

- [ ] **Step 3: Run it and watch it fail**

Run: `cargo test -q -p castr-miracast lpcm`
Expected: FAIL, `cannot find function frame in this scope`.

- [ ] **Step 4: Implement**

At the top of `crates/castr-miracast/src/source/lpcm.rs`:

```rust
//! Wi-Fi Display LPCM framing.
//!
//! The audio a sink expects is not plain PCM: each PES payload carries a short
//! header declaring its size and format, and the samples that follow are
//! big-endian. Getting either wrong produces static, not silence, so both are
//! pinned by tests against literal bytes.

/// Bytes of audio data header ahead of the samples.
pub const HEADER_LEN: usize = 4;

/// 48 kHz, 16-bit, two channels - the only mode the sink offers.
const FORMAT_BYTE: u8 = 0x11;

/// Wraps interleaved stereo samples as one LPCM audio frame.
pub fn frame(samples: &[i16]) -> Vec<u8> {
    let payload_len = samples.len() * 2;
    let mut out = Vec::with_capacity(HEADER_LEN + payload_len);
    out.extend_from_slice(&(payload_len as u16).to_be_bytes());
    out.push(FORMAT_BYTE);
    out.push(0);
    for s in samples {
        out.extend_from_slice(&s.to_be_bytes());
    }
    out
}

/// The samples of a frame, without the header.
pub fn payload(frame: &[u8]) -> &[u8] {
    frame.get(HEADER_LEN..).unwrap_or(&[])
}
```

- [ ] **Step 5: Run it and watch it pass**

Run: `cargo test -q -p castr-miracast lpcm`
Expected: PASS, 4 tests.

- [ ] **Step 6: Fix the sink to strip the header**

In `crates/castr-receiver/src/pipeline.rs`, replace the body of the
`SinkOut::Audio` arm:

```rust
                    SinkOut::Audio { data, .. } => {
                        // LPCM, 16-bit big-endian stereo at 48 kHz, behind the
                        // audio data header every Wi-Fi Display source sends.
                        // Treating the header as samples put a click at the
                        // head of every packet.
                        let samples: Vec<i16> = castr_miracast::source::lpcm::payload(&data)
                            .chunks_exact(2)
                            .map(|b| i16::from_be_bytes([b[0], b[1]]))
                            .collect();
                        let _ = ui.blocking_send(UiEvent::AudioPcm(samples));
                    }
```

- [ ] **Step 7: Run both suites**

Run: `cargo test -q --workspace`
Run: `bash scripts/pi/test-linux.sh`
Expected: both PASS.

- [ ] **Step 8: Commit**

```bash
git add crates/castr-miracast/src/source crates/castr-miracast/src/lib.rs crates/castr-receiver/src/pipeline.rs
git commit -m "feat(miracast): Wi-Fi Display LPCM framing, and strip it on the sink

The sink read a whole audio payload as samples, header included, which put
a click at the head of every packet. Writing the source half made the
asymmetry visible: had both sides kept the same wrong assumption, every
round-trip test in this plan would have passed for the wrong reason.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 2: The transport stream multiplexer

**Files:**
- Create: `crates/castr-miracast/src/source/ts_mux.rs`
- Modify: `crates/castr-miracast/src/source/mod.rs`

**Interfaces:**
- Consumes: `source::lpcm::frame`
- Produces: `source::ts_mux::Muxer`, `Muxer::new() -> Muxer`, `Muxer::push_video(&mut self, au: &[u8], pts_us: u64) -> Vec<u8>`, `Muxer::push_audio(&mut self, samples: &[i16], pts_us: u64) -> Vec<u8>`, `source::ts_mux::VIDEO_PID: u16`, `AUDIO_PID: u16`, `PMT_PID: u16`

Returned buffers are a whole number of 188-byte packets, concatenated.

**Take the PCR interval and the PAT/PMT repetition rate from the specification
text.** They are constants that break interop quietly when wrong.

- [ ] **Step 1: Write the failing round-trip test**

In `crates/castr-miracast/src/source/ts_mux.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ts::{Demux, Unit, PACKET_LEN};

    /// Feeds a muxed buffer through the sink's demuxer, as the wire would.
    fn round_trip(buf: &[u8]) -> Vec<Unit> {
        let mut d = Demux::new();
        let mut units = Vec::new();
        for p in buf.chunks(PACKET_LEN) {
            units.extend(d.push(p));
        }
        units
    }

    #[test]
    fn every_emitted_buffer_is_whole_packets() {
        let mut m = Muxer::new();
        let out = m.push_video(&[0, 0, 0, 1, 0x65, 0xaa], 0);
        assert!(!out.is_empty());
        assert_eq!(out.len() % PACKET_LEN, 0, "a partial packet is unsendable");
        assert!(out.chunks(PACKET_LEN).all(|p| p[0] == 0x47), "lost sync byte");
    }

    #[test]
    fn an_access_unit_survives_our_own_demuxer() {
        // The strongest cheap check we have: the sink already knows how to
        // read a real source's stream, so anything it reads back correctly is
        // at least self-consistent.
        let au: Vec<u8> = [0, 0, 0, 1, 0x65].iter().copied().chain(0..200u8).collect();
        let mut m = Muxer::new();
        let mut buf = m.push_video(&au, 1_000);
        buf.extend(m.push_video(&au, 34_000));
        let units = round_trip(&buf);
        assert!(
            units.iter().any(|u| matches!(u, Unit::Video { data, .. } if *data == au)),
            "the access unit did not come back intact"
        );
    }

    #[test]
    fn audio_survives_with_its_header_intact() {
        let mut m = Muxer::new();
        let buf = m.push_audio(&[0x0102; 480], 1_000);
        let units = round_trip(&buf);
        let audio = units.iter().find_map(|u| match u {
            Unit::Audio { data, .. } => Some(data.clone()),
            _ => None,
        });
        let audio = audio.expect("no audio unit came back");
        assert_eq!(crate::source::lpcm::payload(&audio).len(), 960);
    }

    #[test]
    fn continuity_counters_do_not_break() {
        let mut m = Muxer::new();
        let mut buf = Vec::new();
        for i in 0..40u64 {
            buf.extend(m.push_video(&[0, 0, 0, 1, 0x41, i as u8], i * 33_000));
        }
        let mut d = Demux::new();
        for p in buf.chunks(PACKET_LEN) {
            d.push(p);
        }
        assert_eq!(
            d.stats().continuity_errors,
            0,
            "the demuxer saw a gap in a stream that was never lossy"
        );
    }

    #[test]
    fn the_program_tables_are_repeated_not_sent_once() {
        // A sink that tunes in late, or drops a packet, must still learn the
        // program before it gives up.
        let mut m = Muxer::new();
        let mut buf = Vec::new();
        for i in 0..60u64 {
            buf.extend(m.push_video(&[0, 0, 0, 1, 0x41, 1], i * 33_000));
        }
        let pat = buf
            .chunks(PACKET_LEN)
            .filter(|p| ((p[1] as u16 & 0x1f) << 8 | p[2] as u16) == 0)
            .count();
        assert!(pat >= 2, "the program association table was sent {pat} times");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -q -p castr-miracast ts_mux`
Expected: FAIL, `cannot find type Muxer`.

- [ ] **Step 3: Implement the multiplexer**

Write this above the test module in `crates/castr-miracast/src/source/ts_mux.rs`.
Consult `ts.rs` as you go: it is the reader for this writer, and every field it
inspects is a field this must set.

```rust
//! MPEG-TS multiplexing for the source role: access units in, 188-byte
//! transport packets out.
//!
//! The mirror of `ts.rs`, and tested through it. Everything is driven by whole
//! access units the caller supplies, so it has no I/O and no clock of its own.

use crate::ts::PACKET_LEN;
use std::collections::HashMap;

pub const PMT_PID: u16 = 0x1000;
pub const VIDEO_PID: u16 = 0x1011;
pub const AUDIO_PID: u16 = 0x1100;
const PROGRAM_NUMBER: u16 = 1;
const STREAM_TYPE_H264: u8 = 0x1b;
const STREAM_TYPE_LPCM: u8 = 0x83;
const STREAM_ID_VIDEO: u8 = 0xe0;
const STREAM_ID_PRIVATE_1: u8 = 0xbd;
/// Program tables are repeated this often. A sink that tunes in late, or drops
/// the one copy, must still learn the program rather than give up. Take the
/// exact figure from the specification and cite it here.
const TABLE_INTERVAL_PACKETS: u32 = 40;

/// A 33-bit presentation time in 90 kHz units, spread over five bytes with the
/// marker bits the format requires.
fn pts_bytes(prefix: u8, pts_90k: u64) -> [u8; 5] {
    [
        (prefix << 4) | (((pts_90k >> 30) as u8) & 0x07) << 1 | 1,
        ((pts_90k >> 22) & 0xff) as u8,
        ((((pts_90k >> 15) & 0x7f) as u8) << 1) | 1,
        ((pts_90k >> 7) & 0xff) as u8,
        ((((pts_90k) & 0x7f) as u8) << 1) | 1,
    ]
}

/// One PES packet. Video declares length zero - it is unbounded, and ends at
/// the next PES start - while audio declares its true length, because the
/// demuxer uses it to strip the padding at the tail of the last packet.
fn pes(stream_id: u8, payload: &[u8], pts_us: u64, bounded: bool) -> Vec<u8> {
    let header = pts_bytes(0b0010, pts_us * 9 / 100);
    let body_len = payload.len() + header.len() + 3;
    let declared = if bounded { body_len as u16 } else { 0 };
    let mut v = Vec::with_capacity(6 + body_len);
    v.extend_from_slice(&[0x00, 0x00, 0x01, stream_id]);
    v.extend_from_slice(&declared.to_be_bytes());
    v.push(0x80); // '10' marker, no scrambling, no priority
    v.push(0x80); // PTS present, no DTS
    v.push(header.len() as u8);
    v.extend_from_slice(&header);
    v.extend_from_slice(payload);
    v
}

/// Wraps a section in a transport packet, padded to 188 with 0xff.
fn section_packet(pid: u16, section: &[u8], cc: u8) -> Vec<u8> {
    let mut p = Vec::with_capacity(PACKET_LEN);
    p.push(0x47);
    p.push(0x40 | ((pid >> 8) as u8 & 0x1f)); // payload unit start
    p.push((pid & 0xff) as u8);
    p.push(0x10 | (cc & 0x0f)); // payload only
    p.push(0x00); // pointer field
    p.extend_from_slice(section);
    p.resize(PACKET_LEN, 0xff);
    p
}

/// A section's four-byte CRC, as the format defines it.
fn crc32(data: &[u8]) -> u32 {
    let mut crc: u32 = 0xffff_ffff;
    for byte in data {
        crc ^= (*byte as u32) << 24;
        for _ in 0..8 {
            crc = if crc & 0x8000_0000 != 0 {
                (crc << 1) ^ 0x04c1_1db7
            } else {
                crc << 1
            };
        }
    }
    crc
}

/// Builds a section: table id, syntax indicator and length, then the body and
/// its CRC.
fn section(table_id: u8, body: &[u8]) -> Vec<u8> {
    let mut s = vec![table_id];
    let len = body.len() as u16 + 4; // body plus CRC
    s.extend_from_slice(&(0xb000 | len).to_be_bytes());
    s.extend_from_slice(body);
    let crc = crc32(&s);
    s.extend_from_slice(&crc.to_be_bytes());
    s
}

fn pat_section() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&PROGRAM_NUMBER.to_be_bytes()); // transport stream id
    body.push(0xc1); // version 0, current
    body.push(0x00); // section number
    body.push(0x00); // last section number
    body.extend_from_slice(&PROGRAM_NUMBER.to_be_bytes());
    body.extend_from_slice(&(0xe000 | PMT_PID).to_be_bytes());
    section(0x00, &body)
}

fn pmt_section() -> Vec<u8> {
    let mut body = Vec::new();
    body.extend_from_slice(&PROGRAM_NUMBER.to_be_bytes());
    body.push(0xc1);
    body.push(0x00);
    body.push(0x00);
    body.extend_from_slice(&(0xe000 | VIDEO_PID).to_be_bytes()); // PCR PID
    body.extend_from_slice(&0xf000u16.to_be_bytes()); // no program info
    for (stream_type, pid) in [(STREAM_TYPE_H264, VIDEO_PID), (STREAM_TYPE_LPCM, AUDIO_PID)] {
        body.push(stream_type);
        body.extend_from_slice(&(0xe000 | pid).to_be_bytes());
        body.extend_from_slice(&0xf000u16.to_be_bytes()); // no descriptors
    }
    section(0x02, &body)
}

#[derive(Default)]
pub struct Muxer {
    cc: HashMap<u16, u8>,
    since_tables: u32,
}

impl Muxer {
    pub fn new() -> Self {
        Self::default()
    }

    /// Next continuity counter for a PID. The demuxer counts a gap here as
    /// loss, so it must advance by exactly one per packet carrying payload.
    fn next_cc(&mut self, pid: u16) -> u8 {
        let c = self.cc.entry(pid).or_insert(0x0f);
        *c = (*c + 1) & 0x0f;
        *c
    }

    fn tables_if_due(&mut self) -> Vec<u8> {
        if self.since_tables != 0 && self.since_tables < TABLE_INTERVAL_PACKETS {
            return Vec::new();
        }
        self.since_tables = 0;
        let pat_cc = self.next_cc(0);
        let pmt_cc = self.next_cc(PMT_PID);
        let mut out = section_packet(0, &pat_section(), pat_cc);
        out.extend(section_packet(PMT_PID, &pmt_section(), pmt_cc));
        out
    }

    /// Splits one PES across transport packets. The first carries the
    /// payload-unit-start indicator, and the last is padded with an adaptation
    /// field rather than stuffing inside the payload, which the demuxer would
    /// read as data.
    fn packetize(&mut self, pid: u16, pes: &[u8], pcr_us: Option<u64>) -> Vec<u8> {
        let mut out = Vec::new();
        let mut offset = 0;
        let mut first = true;
        while offset < pes.len() {
            let mut adaptation: Vec<u8> = Vec::new();
            if first {
                if let Some(us) = pcr_us {
                    let pcr = us * 27 / 300; // 90 kHz base
                    adaptation.push(0x10); // PCR present
                    adaptation.extend_from_slice(&[
                        ((pcr >> 25) & 0xff) as u8,
                        ((pcr >> 17) & 0xff) as u8,
                        ((pcr >> 9) & 0xff) as u8,
                        ((pcr >> 1) & 0xff) as u8,
                        (((pcr & 1) as u8) << 7) | 0x7e,
                        0x00,
                    ]);
                }
            }
            let remaining = pes.len() - offset;
            let mut room = PACKET_LEN - 4 - if adaptation.is_empty() { 0 } else { adaptation.len() + 1 };
            if remaining < room {
                // Pad by growing the adaptation field, never the payload.
                let pad = room - remaining;
                if adaptation.is_empty() {
                    adaptation.push(0x00);
                }
                adaptation.resize(adaptation.len() + pad.saturating_sub(1), 0xff);
                room = remaining;
            }
            let take = room.min(remaining);
            let cc = self.next_cc(pid);
            let mut p = Vec::with_capacity(PACKET_LEN);
            p.push(0x47);
            p.push(if first { 0x40 } else { 0x00 } | ((pid >> 8) as u8 & 0x1f));
            p.push((pid & 0xff) as u8);
            p.push(if adaptation.is_empty() { 0x10 } else { 0x30 } | (cc & 0x0f));
            if !adaptation.is_empty() {
                p.push(adaptation.len() as u8);
                p.extend_from_slice(&adaptation);
            }
            p.extend_from_slice(&pes[offset..offset + take]);
            p.resize(PACKET_LEN, 0xff);
            out.extend(p);
            offset += take;
            first = false;
            self.since_tables += 1;
        }
        out
    }

    pub fn push_video(&mut self, au: &[u8], pts_us: u64) -> Vec<u8> {
        let mut out = self.tables_if_due();
        let pes = pes(STREAM_ID_VIDEO, au, pts_us, false);
        out.extend(self.packetize(VIDEO_PID, &pes, Some(pts_us)));
        out
    }

    pub fn push_audio(&mut self, samples: &[i16], pts_us: u64) -> Vec<u8> {
        let mut out = self.tables_if_due();
        let frame = crate::source::lpcm::frame(samples);
        let pes = pes(STREAM_ID_PRIVATE_1, &frame, pts_us, true);
        out.extend(self.packetize(AUDIO_PID, &pes, None));
        out
    }
}
```

Expect to iterate here: the padding arithmetic and the adaptation-field flags
are the two places this most often goes subtly wrong, and the round-trip test
is what will tell you.

- [ ] **Step 4: Run the tests until they pass**

Run: `cargo test -q -p castr-miracast ts_mux`
Expected: PASS, 5 tests.

- [ ] **Step 5: Declare the module**

In `crates/castr-miracast/src/source/mod.rs` add `pub mod ts_mux;`.

- [ ] **Step 6: Run both suites and commit**

Run: `cargo test -q --workspace` and `bash scripts/pi/test-linux.sh`

```bash
git add crates/castr-miracast/src/source
git commit -m "feat(miracast): multiplex a transport stream for the source role

Mirrors the demuxer the sink has always had, and is tested through it: an
access unit that does not come back intact from our own reader would not
have survived a television either.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 3: The RTP packetizer

**Files:**
- Create: `crates/castr-miracast/src/source/rtp_pack.rs`
- Modify: `crates/castr-miracast/src/source/mod.rs`

**Interfaces:**
- Produces: `source::rtp_pack::Packetizer`, `Packetizer::new(ssrc: u32) -> Packetizer`, `Packetizer::push(&mut self, ts_packets: &[u8], timestamp_90k: u32) -> Vec<Vec<u8>>`, `source::rtp_pack::PAYLOAD_TYPE: u8`, `PACKETS_PER_DATAGRAM: usize`

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtp;
    use crate::ts::PACKET_LEN;

    fn ts_packets(n: usize) -> Vec<u8> {
        let mut v = Vec::new();
        for i in 0..n {
            let mut p = [0u8; PACKET_LEN];
            p[0] = 0x47;
            p[4] = i as u8;
            v.extend_from_slice(&p);
        }
        v
    }

    #[test]
    fn seven_transport_packets_make_one_datagram() {
        let mut p = Packetizer::new(1);
        let out = p.push(&ts_packets(7), 0);
        assert_eq!(out.len(), 1);
        assert_eq!(out[0].len(), 12 + 7 * PACKET_LEN, "header plus 1316 bytes");
    }

    #[test]
    fn our_own_parser_reads_what_we_wrote() {
        let mut p = Packetizer::new(0xdead_beef);
        let out = p.push(&ts_packets(14), 900);
        assert_eq!(out.len(), 2);
        let first = rtp::parse(&out[0]).expect("unparseable datagram");
        assert_eq!(first.payload_type, PAYLOAD_TYPE);
        assert_eq!(first.timestamp, 900);
        assert_eq!(first.payload.len(), 7 * PACKET_LEN);
        let second = rtp::parse(&out[1]).expect("unparseable datagram");
        assert_eq!(
            second.sequence,
            first.sequence.wrapping_add(1),
            "sequence numbers must advance by one"
        );
    }

    #[test]
    fn a_short_tail_is_still_sent() {
        // Dropping the remainder would lose the end of every frame.
        let mut p = Packetizer::new(1);
        let out = p.push(&ts_packets(9), 0);
        assert_eq!(out.len(), 2);
        assert_eq!(out[1].len(), 12 + 2 * PACKET_LEN);
    }

    #[test]
    fn sequence_numbers_wrap_rather_than_overflow() {
        let mut p = Packetizer::new(1);
        p.set_sequence_for_test(u16::MAX);
        let out = p.push(&ts_packets(14), 0);
        let a = rtp::parse(&out[0]).unwrap().sequence;
        let b = rtp::parse(&out[1]).unwrap().sequence;
        assert_eq!(a, u16::MAX);
        assert_eq!(b, 0);
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -q -p castr-miracast rtp_pack`
Expected: FAIL, `cannot find type Packetizer`.

- [ ] **Step 3: Implement**

```rust
//! RTP packetization for the source role.
//!
//! Wi-Fi Display carries MPEG-TS in payload type 33, seven transport packets
//! to a datagram: 1316 bytes of payload, which with headers stays inside an
//! ordinary 1500-byte path without fragmenting.
//!
//! The mirror of `rtp.rs`, and tested through it.

use crate::ts::PACKET_LEN;

pub const PAYLOAD_TYPE: u8 = 33;
pub const PACKETS_PER_DATAGRAM: usize = 7;

pub struct Packetizer {
    ssrc: u32,
    sequence: u16,
}

impl Packetizer {
    pub fn new(ssrc: u32) -> Self {
        Self { ssrc, sequence: 0 }
    }

    #[cfg(test)]
    fn set_sequence_for_test(&mut self, seq: u16) {
        self.sequence = seq;
    }

    /// Splits whole transport packets into datagrams, all stamped with the
    /// same presentation time: they belong to one access unit.
    pub fn push(&mut self, ts_packets: &[u8], timestamp_90k: u32) -> Vec<Vec<u8>> {
        let mut out = Vec::new();
        for group in ts_packets.chunks(PACKETS_PER_DATAGRAM * PACKET_LEN) {
            let mut d = Vec::with_capacity(12 + group.len());
            d.push(0x80); // version 2, no padding, no extension, no CSRC
            d.push(PAYLOAD_TYPE);
            d.extend_from_slice(&self.sequence.to_be_bytes());
            d.extend_from_slice(&timestamp_90k.to_be_bytes());
            d.extend_from_slice(&self.ssrc.to_be_bytes());
            d.extend_from_slice(group);
            self.sequence = self.sequence.wrapping_add(1);
            out.push(d);
        }
        out
    }
}
```

- [ ] **Step 4: Run it and watch it pass**

Run: `cargo test -q -p castr-miracast rtp_pack`
Expected: PASS, 4 tests.

- [ ] **Step 5: Declare, verify, commit**

Add `pub mod rtp_pack;` to `source/mod.rs`. Run `cargo test -q --workspace` and
`bash scripts/pi/test-linux.sh`.

```bash
git add crates/castr-miracast/src/source
git commit -m "feat(miracast): packetize a transport stream into RTP

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 4: Capability intersection

**Files:**
- Create: `crates/castr-miracast/src/source/caps.rs`
- Modify: `crates/castr-miracast/src/source/mod.rs`

**Interfaces:**
- Consumes: `wfd::parse_parameter_body`, `rtsp::VideoMode`
- Produces: `source::caps::SinkCaps { cea: u32, vesa: u32, hh: u32, profile: u8, level: u8, lpcm_modes: u32, rtp_port: u16, max_bitrate_kbps: Option<u32>, content_protection: Option<String> }`, `source::caps::parse(body: &str) -> Result<SinkCaps, CapsError>`, `source::caps::choose(sink: &SinkCaps, ours: &[rtsp::VideoMode]) -> Result<rtsp::VideoMode, NoCommonFormat>`, `source::caps::NoCommonFormat` (implements `Display`)

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtsp::VideoMode;
    use crate::wfd::{capabilities_body, AudioCodecs, Capabilities, ClientPorts, VideoFormats};

    /// Exactly what our own sink sends, so the two halves are tested together.
    fn our_sink_body() -> String {
        capabilities_body(&Capabilities {
            video: VideoFormats::only_720p30(),
            audio: AudioCodecs::lpcm_48k_stereo(),
            ports: ClientPorts { rtp_port: 5000 },
            max_bitrate_kbps: 20_000,
            latency_management: true,
            format_change: true,
        })
    }

    #[test]
    fn our_own_sink_capabilities_parse() {
        let c = parse(&our_sink_body()).expect("our own sink must parse");
        assert_eq!(c.cea, 0x0000_0020, "bit 5 is 1280x720p30");
        assert_eq!(c.profile, 0x02);
        assert_eq!(c.level, 0x04);
        assert_eq!(c.rtp_port, 5000);
        assert_eq!(c.lpcm_modes, 0x0000_0002);
        assert_eq!(c.max_bitrate_kbps, Some(20_000));
    }

    #[test]
    fn the_only_common_mode_is_chosen() {
        let c = parse(&our_sink_body()).unwrap();
        let ours = [
            VideoMode { width: 1920, height: 1080, fps: 60 },
            VideoMode { width: 1280, height: 720, fps: 30 },
        ];
        assert_eq!(
            choose(&c, &ours).unwrap(),
            VideoMode { width: 1280, height: 720, fps: 30 }
        );
    }

    #[test]
    fn an_unknown_parameter_is_ignored_not_fatal() {
        // A real television sends vendor parameters we have never seen, and
        // refusing them would refuse the television.
        let body = format!("{}some_vendor_extension: whatever\r\n", our_sink_body());
        assert!(parse(&body).is_ok());
    }

    #[test]
    fn no_common_format_says_what_each_side_offered() {
        // The stage most likely to fail against an unfamiliar display is the
        // stage that must explain itself best.
        let c = parse(&our_sink_body()).unwrap();
        let ours = [VideoMode { width: 1920, height: 1080, fps: 60 }];
        let err = choose(&c, &ours).unwrap_err();
        let text = err.to_string();
        assert!(text.contains("1280x720"), "must name what the sink offered: {text}");
        assert!(text.contains("1920x1080"), "must name what we offered: {text}");
    }

    #[test]
    fn a_body_with_no_video_formats_is_an_error_not_a_guess() {
        assert!(parse("wfd_audio_codecs: LPCM 00000002 00\r\n").is_err());
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -q -p castr-miracast caps`
Expected: FAIL, `cannot find function parse`.

- [ ] **Step 3: Implement**

```rust
//! What a sink says it can accept, and what we choose from it.
//!
//! This is where interoperability is won or lost, and it is pure decision
//! logic over parsed text - so once a real display's reply is captured, it
//! becomes a fixture and the decision is testable forever after.

use crate::rtsp::VideoMode;
use crate::wfd::parse_parameter_body;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SinkCaps {
    pub cea: u32,
    pub vesa: u32,
    pub hh: u32,
    pub profile: u8,
    pub level: u8,
    pub lpcm_modes: u32,
    pub rtp_port: u16,
    pub max_bitrate_kbps: Option<u32>,
    pub content_protection: Option<String>,
}

#[derive(Debug, PartialEq, Eq)]
pub enum CapsError {
    NoVideoFormats,
    MalformedVideoFormats(String),
}

impl std::fmt::Display for CapsError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            CapsError::NoVideoFormats => write!(f, "the sink advertised no video formats"),
            CapsError::MalformedVideoFormats(s) => {
                write!(f, "could not read wfd_video_formats: {s:?}")
            }
        }
    }
}

impl std::error::Error for CapsError {}

/// CEA table index to resolution. `rtsp::cea_mode` covers only the one mode our
/// own sink offers; a source meets displays that offer many, so the fuller
/// table lives here. Unify the two when the sink learns more modes.
pub fn cea_mode(bit: u32) -> Option<VideoMode> {
    let (width, height, fps) = match bit {
        0 => (640, 480, 60),
        1 => (720, 480, 60),
        2 => (720, 480, 60),
        3 => (720, 576, 50),
        4 => (720, 576, 50),
        5 => (1280, 720, 30),
        6 => (1280, 720, 60),
        7 => (1920, 1080, 30),
        8 => (1920, 1080, 60),
        9 => (1920, 1080, 30),
        10 => (1280, 720, 25),
        11 => (1280, 720, 50),
        12 => (1920, 1080, 25),
        13 => (1920, 1080, 50),
        14 => (1920, 1080, 25),
        15 => (1280, 720, 24),
        16 => (1920, 1080, 24),
        _ => return None,
    };
    Some(VideoMode { width, height, fps })
}

pub fn parse(body: &str) -> Result<SinkCaps, CapsError> {
    let params = parse_parameter_body(body);
    let get = |name: &str| {
        params
            .iter()
            .find(|(k, _)| k == name)
            .map(|(_, v)| v.as_str())
    };

    let video = get("wfd_video_formats").ok_or(CapsError::NoVideoFormats)?;
    let f: Vec<&str> = video.split_whitespace().collect();
    // native, preferred-display-mode, profile, level, cea, vesa, hh, ...
    if f.len() < 7 {
        return Err(CapsError::MalformedVideoFormats(video.to_string()));
    }
    let hex8 = |s: &str| u32::from_str_radix(s, 16).ok();
    let hex2 = |s: &str| u8::from_str_radix(s, 16).ok();
    let bad = || CapsError::MalformedVideoFormats(video.to_string());

    Ok(SinkCaps {
        profile: hex2(f[2]).ok_or_else(bad)?,
        level: hex2(f[3]).ok_or_else(bad)?,
        cea: hex8(f[4]).ok_or_else(bad)?,
        vesa: hex8(f[5]).ok_or_else(bad)?,
        hh: hex8(f[6]).ok_or_else(bad)?,
        lpcm_modes: get("wfd_audio_codecs")
            .and_then(|v| v.split_whitespace().nth(1).and_then(hex8))
            .unwrap_or(0),
        rtp_port: get("wfd_client_rtp_ports")
            .and_then(|v| v.split_whitespace().nth(1))
            .and_then(|p| p.parse().ok())
            .unwrap_or(5000),
        max_bitrate_kbps: get("microsoft_max_bitrate").and_then(|v| v.trim().parse().ok()),
        content_protection: get("wfd_content_protection")
            .map(str::to_string)
            .filter(|v| v.trim() != "none"),
    })
}

#[derive(Debug)]
pub struct NoCommonFormat {
    pub sink_offered: Vec<VideoMode>,
    pub we_offered: Vec<VideoMode>,
}

impl std::fmt::Display for NoCommonFormat {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let show = |ms: &[VideoMode]| {
            ms.iter()
                .map(|m| format!("{}x{}p{}", m.width, m.height, m.fps))
                .collect::<Vec<_>>()
                .join(", ")
        };
        write!(
            f,
            "no format in common: the display offered {}; we offered {}",
            show(&self.sink_offered),
            show(&self.we_offered)
        )
    }
}

impl std::error::Error for NoCommonFormat {}

/// Every mode a sink's bitmaps advertise, in table order.
pub fn sink_modes(c: &SinkCaps) -> Vec<VideoMode> {
    (0..32).filter(|b| c.cea & (1 << b) != 0).filter_map(cea_mode).collect()
}

/// Our modes in preference order; the first the sink also offers wins.
pub fn choose(sink: &SinkCaps, ours: &[VideoMode]) -> Result<VideoMode, NoCommonFormat> {
    let offered = sink_modes(sink);
    ours.iter()
        .find(|m| offered.contains(m))
        .copied()
        .ok_or_else(|| NoCommonFormat {
            sink_offered: offered,
            we_offered: ours.to_vec(),
        })
}
```

Check `wfd::parse_parameter_body`'s exact return type before writing this; the
code above assumes `Vec<(String, String)>`, which is what `wfd.rs` declares
today. If a display's `wfd_video_formats` carries per-mode entries after the
seventh field, ignore them for now and record it for part 4.

- [ ] **Step 4: Run until green, then both suites, then commit**

Run: `cargo test -q -p castr-miracast caps`, then `cargo test -q --workspace`
and `bash scripts/pi/test-linux.sh`.

```bash
git add crates/castr-miracast/src/source
git commit -m "feat(miracast): read a sink's capabilities and choose one mode

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 5: The source session state machine

**Files:**
- Create: `crates/castr-miracast/src/source/session.rs`
- Modify: `crates/castr-miracast/src/source/mod.rs`

**Interfaces:**
- Consumes: `rtsp::{Message, Action, parse, request, response}`, `source::caps`
- Produces: `source::session::SourceSession`, `SourceSession::new(cfg: SourceConfig) -> SourceSession`, `SourceSession::start(&mut self) -> Vec<Action>`, `SourceSession::on_message(&mut self, m: &Message) -> Vec<Action>`, `SourceSession::tick(&mut self, now: Instant) -> Vec<Action>`, `SourceSession::chosen(&self) -> Option<rtsp::VideoMode>`, `SourceSession::state(&self) -> SourceState`, `source::session::SourceConfig { modes: Vec<rtsp::VideoMode>, rtp_port: u16 }`, `source::session::SourceState { Init, AwaitingCaps, Configuring, AwaitingSetup, Playing, Done }`

`rtsp::Action` is reused rather than duplicated: `Send(Message)`, `Play`,
`Teardown(&'static str)` already say everything a source needs.

- [ ] **Step 1: Write the failing test**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::rtsp::{self, Action, StartLine, VideoMode};

    fn cfg() -> SourceConfig {
        SourceConfig {
            modes: vec![VideoMode { width: 1280, height: 720, fps: 30 }],
            rtp_port: 5000,
        }
    }

    fn sent(actions: &[Action]) -> Vec<rtsp::Message> {
        actions
            .iter()
            .filter_map(|a| match a {
                Action::Send(m) => Some(m.clone()),
                _ => None,
            })
            .collect()
    }

    #[test]
    fn a_session_opens_with_m1_options() {
        let mut s = SourceSession::new(cfg());
        let msgs = sent(&s.start());
        assert_eq!(msgs.len(), 1);
        match &msgs[0].start {
            StartLine::Request { method, .. } => assert_eq!(method, "OPTIONS"),
            other => panic!("M1 must be a request, got {other:?}"),
        }
    }

    #[test]
    fn the_sinks_options_request_is_answered() {
        // M2 runs the other way down the same connection; a source that only
        // spoke as a client would hang here.
        let mut s = SourceSession::new(cfg());
        s.start();
        let m2 = rtsp::request("OPTIONS", "*", 1, "");
        let msgs = sent(&s.on_message(&m2));
        assert!(
            msgs.iter().any(|m| matches!(m.start, StartLine::Response { status: 200, .. })),
            "the sink's OPTIONS went unanswered"
        );
    }

    #[test]
    fn an_unknown_parameter_in_m3_does_not_end_the_session() {
        let mut s = SourceSession::new(cfg());
        s.start();
        let body = "wfd_video_formats: 40 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none\r\n\
                    wfd_audio_codecs: LPCM 00000002 00\r\n\
                    wfd_client_rtp_ports: RTP/AVP/UDP;unicast 5000 0 mode=play\r\n\
                    some_vendor_extension: whatever\r\n";
        let actions = s.on_message(&rtsp::response(200, 2, body));
        assert!(
            !actions.iter().any(|a| matches!(a, Action::Teardown(_))),
            "a vendor parameter must not end the session"
        );
        assert_eq!(s.chosen(), Some(VideoMode { width: 1280, height: 720, fps: 30 }));
    }

    #[test]
    fn no_common_format_tears_down_rather_than_streaming_blindly() {
        let mut s = SourceSession::new(SourceConfig {
            modes: vec![VideoMode { width: 1920, height: 1080, fps: 60 }],
            rtp_port: 5000,
        });
        s.start();
        let body = "wfd_video_formats: 40 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none\r\n";
        let actions = s.on_message(&rtsp::response(200, 2, body));
        assert!(actions.iter().any(|a| matches!(a, Action::Teardown(_))));
    }

    #[test]
    fn our_source_and_our_sink_negotiate_each_other_to_playing() {
        // Two state machines, no sockets. Neither proves the specification,
        // but a disagreement here is a bug in one of them for certain.
        use crate::rtsp::{Negotiation, NegState};
        use crate::wfd::{AudioCodecs, Capabilities, ClientPorts, VideoFormats};

        let mut source = SourceSession::new(cfg());
        let mut sink = Negotiation::new(
            Capabilities {
                video: VideoFormats::only_720p30(),
                audio: AudioCodecs::lpcm_48k_stereo(),
                ports: ClientPorts { rtp_port: 5000 },
                max_bitrate_kbps: 20_000,
                latency_management: true,
                format_change: true,
            },
            "1234".to_string(),
        );

        let mut pending = source.start();
        let mut source_playing = false;
        for _ in 0..24 {
            let mut next = Vec::new();
            for action in pending.drain(..) {
                match action {
                    Action::Send(m) => next.extend(sink.on_message(&m)),
                    Action::Play => source_playing = true,
                    Action::Teardown(why) => panic!("torn down: {why}"),
                }
            }
            for action in next.drain(..) {
                match action {
                    Action::Send(m) => pending.extend(source.on_message(&m)),
                    Action::Play => {}
                    Action::Teardown(why) => panic!("sink tore down: {why}"),
                }
            }
            if pending.is_empty() {
                break;
            }
        }
        assert!(source_playing, "the source never reached Play");
        assert_eq!(sink.state(), NegState::Playing, "the sink never reached Playing");
    }
}
```

- [ ] **Step 2: Run it and watch it fail**

Run: `cargo test -q -p castr-miracast source::session`
Expected: FAIL, `cannot find type SourceSession`.

- [ ] **Step 3: Implement**

Read `rtsp::Negotiation::on_message_at` while writing this: it is the peer this
must satisfy, and it already shows how the repository formats each message.

```rust
//! The source half of the Wi-Fi Display negotiation.
//!
//! Bytes in, actions out, exactly as `rtsp::Negotiation` does for the sink, so
//! a whole M1-M7 exchange replays in a test with no socket - and the two can be
//! driven against each other directly.

use crate::rtsp::{self, Action, Message, StartLine, VideoMode};
use crate::source::caps;
use std::time::{Duration, Instant};

/// Matches the sink's tolerance in `rtsp.rs`: two missed keep-alives, not one.
const KEEPALIVE_EVERY: Duration = Duration::from_secs(5);
const KEEPALIVE_TIMEOUT: Duration = Duration::from_secs(10);

#[derive(Debug, Clone)]
pub struct SourceConfig {
    /// Our modes in preference order.
    pub modes: Vec<VideoMode>,
    pub rtp_port: u16,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SourceState {
    Init,
    AwaitingCaps,
    Configuring,
    AwaitingSetup,
    Playing,
    Done,
}

pub struct SourceSession {
    cfg: SourceConfig,
    state: SourceState,
    next_cseq: u32,
    chosen: Option<VideoMode>,
    sink: Option<caps::SinkCaps>,
    session_id: Option<String>,
    last_heard: Option<Instant>,
    last_keepalive: Option<Instant>,
}

impl SourceSession {
    pub fn new(cfg: SourceConfig) -> Self {
        Self {
            cfg,
            state: SourceState::Init,
            next_cseq: 1,
            chosen: None,
            sink: None,
            session_id: None,
            last_heard: None,
            last_keepalive: None,
        }
    }

    pub fn state(&self) -> SourceState {
        self.state
    }

    pub fn chosen(&self) -> Option<VideoMode> {
        self.chosen
    }

    /// The negotiated RTP port, once the sink has told us one.
    pub fn sink_rtp_port(&self) -> Option<u16> {
        self.sink.as_ref().map(|c| c.rtp_port)
    }

    pub fn max_bitrate_kbps(&self) -> Option<u32> {
        self.sink.as_ref().and_then(|c| c.max_bitrate_kbps)
    }

    fn cseq(&mut self) -> u32 {
        let c = self.next_cseq;
        self.next_cseq += 1;
        c
    }

    /// M1: the first thing on the connection.
    pub fn start(&mut self) -> Vec<Action> {
        let cseq = self.cseq();
        self.state = SourceState::AwaitingCaps;
        vec![Action::Send(rtsp::request("OPTIONS", "*", cseq, ""))]
    }

    pub fn on_message(&mut self, m: &Message) -> Vec<Action> {
        self.on_message_at(m, Instant::now())
    }

    /// The clock is injected so the keep-alive rule is testable without waiting.
    pub fn on_message_at(&mut self, m: &Message, now: Instant) -> Vec<Action> {
        self.last_heard = Some(now);
        match &m.start {
            // The sink talks to us as a client too - M2, then SETUP and PLAY.
            StartLine::Request { method, .. } => self.on_request(m, method.clone()),
            StartLine::Response { status, .. } => {
                if *status != 200 {
                    return vec![Action::Teardown("the sink refused a request")];
                }
                self.on_response(m)
            }
        }
    }

    fn on_request(&mut self, m: &Message, method: String) -> Vec<Action> {
        let cseq = m.cseq().unwrap_or(0);
        match method.as_str() {
            "OPTIONS" => vec![Action::Send(rtsp::response(200, cseq, ""))],
            "SETUP" => {
                self.session_id = Some("castr".to_string());
                self.state = SourceState::AwaitingSetup;
                vec![Action::Send(rtsp::response(200, cseq, ""))]
            }
            "PLAY" => {
                self.state = SourceState::Playing;
                vec![Action::Send(rtsp::response(200, cseq, "")), Action::Play]
            }
            "TEARDOWN" => {
                self.state = SourceState::Done;
                vec![
                    Action::Send(rtsp::response(200, cseq, "")),
                    Action::Teardown("the sink ended the session"),
                ]
            }
            // Never fatal: a display may ask us things we have never seen.
            _ => vec![Action::Send(rtsp::response(200, cseq, ""))],
        }
    }

    fn on_response(&mut self, m: &Message) -> Vec<Action> {
        match self.state {
            // M1 answered: ask what it can take.
            SourceState::AwaitingCaps if self.sink.is_none() => {
                let cseq = self.cseq();
                let body = "wfd_video_formats\r\nwfd_audio_codecs\r\n\
                            wfd_content_protection\r\nwfd_client_rtp_ports\r\n";
                vec![Action::Send(rtsp::request("GET_PARAMETER", "rtsp://localhost/wfd1.0", cseq, body))]
            }
            _ => self.on_capabilities(m),
        }
    }

    fn on_capabilities(&mut self, m: &Message) -> Vec<Action> {
        if self.sink.is_some() {
            return Vec::new(); // a keep-alive answer, nothing to do
        }
        let sink = match caps::parse(&m.body) {
            Ok(c) => c,
            Err(_) => return vec![Action::Teardown("the sink sent capabilities we could not read")],
        };
        let chosen = match caps::choose(&sink, &self.cfg.modes) {
            Ok(mode) => mode,
            Err(_) => return vec![Action::Teardown("no video format in common")],
        };
        self.chosen = Some(chosen);
        self.sink = Some(sink);
        self.state = SourceState::Configuring;

        // M4 then M5, back to back: set the chosen mode, then ask the sink to
        // take over as client and send us SETUP.
        let m4 = self.cseq();
        let m5 = self.cseq();
        let set = format!(
            "wfd_video_formats: 00 00 02 04 {:08X} 00000000 00000000 00 0000 0000 00 none none\r\n\
             wfd_audio_codecs: LPCM 00000002 00\r\n\
             wfd_presentation_URL: rtsp://localhost/wfd1.0/streamid=0 none\r\n\
             wfd_client_rtp_ports: RTP/AVP/UDP;unicast {} 0 mode=play\r\n",
            mode_bit(chosen),
            self.cfg.rtp_port
        );
        vec![
            Action::Send(rtsp::request("SET_PARAMETER", "rtsp://localhost/wfd1.0", m4, &set)),
            Action::Send(rtsp::request(
                "SET_PARAMETER",
                "rtsp://localhost/wfd1.0",
                m5,
                "wfd_trigger_method: SETUP\r\n",
            )),
        ]
    }

    pub fn tick(&mut self, now: Instant) -> Vec<Action> {
        if self.state != SourceState::Playing {
            return Vec::new();
        }
        if let Some(last) = self.last_heard {
            if now.duration_since(last) > KEEPALIVE_TIMEOUT {
                self.state = SourceState::Done;
                return vec![Action::Teardown("the sink stopped answering")];
            }
        }
        let due = self
            .last_keepalive
            .is_none_or(|t| now.duration_since(t) >= KEEPALIVE_EVERY);
        if !due {
            return Vec::new();
        }
        self.last_keepalive = Some(now);
        let cseq = self.cseq();
        vec![Action::Send(rtsp::request(
            "GET_PARAMETER",
            "rtsp://localhost/wfd1.0",
            cseq,
            "",
        ))]
    }
}

/// The CEA bit for a mode we chose, for the M4 body.
fn mode_bit(mode: VideoMode) -> u32 {
    (0..32)
        .find(|b| caps::cea_mode(*b) == Some(mode))
        .map(|b| 1u32 << b)
        .unwrap_or(0)
}
```

The `AwaitingCaps` arm above distinguishes the M1 and M3 responses by whether
`self.sink` is set, which is the smallest thing that works; if the sequence
grows, give the state enum its own variant rather than adding a second flag.

- [ ] **Step 4: Run until green, then both suites, then commit**

```bash
git add crates/castr-miracast/src/source
git commit -m "feat(miracast): the source half of the M1-M7 negotiation

Tested against our own sink's state machine, in process and without a
socket: two implementations of one specification that disagree mean at
least one of them is wrong.

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 6: The driver

**Files:**
- Create: `crates/castr-sender/src/miracast_cast.rs`
- Modify: `crates/castr-sender/src/main.rs`

**Interfaces:**
- Consumes: everything from tasks 1-5, `castr_capture_win::{DesktopCapture, LoopbackCapture}`, `castr_codec_win::MfEncoder`
- Produces: `miracast_cast::cast_to(addr: SocketAddr, opts: MiracastOptions) -> anyhow::Result<()>`, `miracast_cast::MiracastOptions { duration: Option<Duration>, output: u32 }`

This is the only impure module, and it is `cfg(windows)`. It has no unit tests;
it is proved by Task 7.

- [ ] **Step 1: Write the driver**

Structure, following the threading already used in `castr-sender/src/cast.rs`:

1. Connect TCP to `addr` (port 7236). On failure, report the stage by name:
   `"connect: {addr} did not answer within {timeout:?}"`.
2. Run `SourceSession`, reading with `rtsp::parse` over a growing buffer and
   writing every `Action::Send`.
3. On `Action::Play`, start capture and encoding at the chosen mode, open the
   UDP socket to the sink's advertised RTP port, and run the media loop: frame
   to `MfEncoder`, access unit to `Muxer::push_video`, datagrams from
   `Packetizer::push`, sent on the socket. Audio drains from `LoopbackCapture`
   into `Muxer::push_audio` on the same clock origin.
4. Timestamps: one `Instant` taken at `Play`; `pts_us` is elapsed microseconds;
   the RTP timestamp is `pts_us * 9 / 100`.
5. On `Action::Teardown(why)`, or `Ctrl-C`, or the duration expiring, send
   `TEARDOWN`, close both sockets, and log `"teardown: {why}"`.
6. Bound the encoder bitrate by `SourceSession::max_bitrate_kbps` when the sink
   sent one, and configure `MfEncoder` to the profile and level the sink
   advertised. Media Foundation does not necessarily honour every constraint
   asked of it, so log what was requested and what the encoder reports back —
   a display that rejects our stream for exceeding its level will otherwise
   look like an unexplained failure.

- [ ] **Step 2: Add the CLI entry**

In `crates/castr-sender/src/main.rs`, add a `miracast-cast <addr>` subcommand
taking `--duration SECS` and honouring `CASTR_OUTPUT` as the existing cast path
does.

- [ ] **Step 3: Verify it builds and both suites still pass**

Run: `cargo build --release` and `cargo test -q --workspace` and
`bash scripts/pi/test-linux.sh`.

- [ ] **Step 4: Commit**

```bash
git add crates/castr-sender/src
git commit -m "feat(sender): cast to a Miracast sink over IP

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

### Task 7: End to end against the Pi, and the write-up

**Files:**
- Create: `docs/superpowers/verification/2026-09-04-castr-miracast-source-e2e.md`
- Modify: `README.md`

**The Pi sink listens for RTSP on the Wi-Fi Direct group address only.** Before
this task, make it also listen on the LAN address, or run the receiver with the
group interface absent so it binds the ordinary interface. Whichever is chosen,
record it: this is the seam that becomes MS-MICE.

- [ ] **Step 1: Cast to the Pi over Ethernet**

```bash
cargo build --release
./target/release/castr-sender.exe miracast-cast 192.168.88.157:7236 --duration 60
```

Watch the Pi: `ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver -f'`

Expected: the negotiation completes, picture and sound appear, and the sink
logs no continuity errors.

- [ ] **Step 2: Sustain it for ten minutes**

Re-run with `--duration 600`. Expected: no stall, no teardown, and
`DemuxStats::continuity_errors` still zero at the end.

- [ ] **Step 3: Provoke each failure this part owns**

- **connect** - point at a port nothing listens on; expect the address and
  timeout named.
- **negotiation** - build with a source mode list of 1080p60 only; expect a
  teardown naming both offers.
- **session** - stop the receiver mid-cast; expect the keepalive to notice
  within `KEEPALIVE_TIMEOUT` and say so.
- **teardown** - end a cast normally, then immediately start another; expect
  the second to negotiate without a restart.

- [ ] **Step 4: Write the verification document**

Follow `docs/superpowers/verification/2026-09-03-castr-cast-quality-e2e.md`:
one row per claim, each PASS, INCONCLUSIVE or NOT RUN, with the evidence
quoted. Anything not actually observed is not a PASS.

- [ ] **Step 5: Update the README**

Document `castr-sender miracast-cast`, and add to "Known gaps" anything the
verification could not settle.

- [ ] **Step 6: Commit**

```bash
git add docs README.md
git commit -m "docs: Miracast source end-to-end verification

Co-Authored-By: Claude Opus 5 (1M context) <noreply@anthropic.com>"
```

---

## Notes for the executor

- **Round-tripping against ourselves proves consistency, not correctness.** Both
  halves can share one wrong assumption — Task 1 exists because they already
  did. Where a format is at stake, check the specification text, and treat a
  real display in part 4 as the only real authority.
- **The sink is the reference implementation for every writer here.** `ts.rs`
  reads what `ts_mux` writes; `rtp.rs` reads what `rtp_pack` writes;
  `rtsp::Negotiation` is the peer `SourceSession` must satisfy. Read the reader
  before writing the writer.
- **If a task turns out bigger than it looks, stop and say so** rather than
  pushing through. The multiplexer in Task 2 is the most likely candidate.

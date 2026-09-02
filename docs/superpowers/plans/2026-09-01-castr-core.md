# castr Core Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Build the castr protocol, a portable Windows screen sender, and a desktop receiver so a Windows PC can cast its screen and audio to another PC (and, with software decode, a Linux box) over the LAN with pairing, adaptive bitrate, and fast reconnect.

**Architecture:** A Cargo workspace. Three platform-free crates hold the wire format (`castr-proto`), media logic (`castr-media`), and QUIC transport, discovery, and pairing (`castr-net`). Two Windows-only crates wrap Desktop Duplication + WASAPI capture (`castr-capture-win`) and Media Foundation H.264 codecs (`castr-codec-win`). Two binaries wire them together. Video and audio travel as QUIC datagrams with a 20-byte header; control messages travel on a reliable QUIC stream.

**Tech Stack:** Rust stable, `quinn` 0.11 / `rustls` 0.23, `rcgen`, `mdns-sd`, `spake2`, `postcard`, `openh264`, `audiopus`, `sdl2` (bundled), `windows` 0.58, `eframe`, `tokio`, `clap`, `tracing`.

**Spec:** `docs/superpowers/specs/2026-09-01-castr-core-design.md`

## Global Constraints

- H.264 is the mandatory codec. No HEVC.
- No FFmpeg on any platform. Codecs are Media Foundation on Windows and `openh264` elsewhere.
- `castr-proto`, `castr-net`, and `castr-media` must compile on any target with no platform-specific code.
- `castr-sender` on Windows is a single portable exe: no installer, no admin, no external DLLs.
- Datagram header is exactly 20 bytes, little-endian, layout per spec section 6.2.
- Control messages are `postcard`-encoded with a 4-byte little-endian length prefix.
- Bitrate controller constants: floor 1 Mbps; loss > 2% multiplies by 0.7 at most once per 500 ms; decode queue > 3 multiplies by 0.85; 1 s clean (loss < 0.5%, queue <= 1) adds 5% of ceiling.
- Default max bitrate: 10 Mbps on ARM Linux, 40 Mbps elsewhere.
- Audio: Opus 48 kHz stereo 128 kbps 10 ms frames, one frame per datagram, fragment count 1.
- Reconnect: backoff starts 200 ms, doubles, caps at 5 s, gives up after 30 s. Session token valid 60 s.
- QUIC idle timeout 3 s. Receiver requests keyframe after 1 s without video.
- Sender resends a NACKed fragment only if the frame is a keyframe or younger than one frame interval; ring buffer holds 500 ms.
- Unchanged desktop: re-send last frame every 500 ms.
- Commit after every task with the message shown. Run `cargo fmt` and `cargo clippy --workspace` before each commit; clippy warnings must be fixed, not allowed.

## File Structure

```
Cargo.toml                          workspace root, shared dependency versions
rust-toolchain.toml                 pin stable
.gitignore
crates/castr-proto/src/lib.rs       re-exports
crates/castr-proto/src/header.rs    DatagramHeader: 20-byte encode/decode
crates/castr-proto/src/packetize.rs Packetizer: encoded frame -> datagram payloads
crates/castr-proto/src/reassemble.rs Reassembler: datagrams -> complete frames, NACK lists, timeouts
crates/castr-proto/src/control.rs   ControlMessage enum, read/write framing
crates/castr-proto/src/session.rs   Session state machine incl. token resume
crates/castr-media/src/lib.rs       re-exports
crates/castr-media/src/codec.rs     VideoEncoder / VideoDecoder traits and frame types
crates/castr-media/src/convert.rs   BGRA -> I420 and NV12 conversion
crates/castr-media/src/sw.rs        openh264 software backend
crates/castr-media/src/audio.rs     Opus encode/decode wrappers
crates/castr-media/src/jitter.rs    JitterBuffer with game/quality rules
crates/castr-media/src/clock.rs     AvClock: audio-master presentation timing
crates/castr-media/src/bitrate.rs   BitrateController
crates/castr-net/src/lib.rs         re-exports
crates/castr-net/src/identity.rs    cert + key generation, fingerprint, paired.toml store
crates/castr-net/src/tls.rs         rustls verifiers that pin fingerprints or accept-any for pairing
crates/castr-net/src/transport.rs   quinn endpoint, Link wrapper (control, datagrams, nack stream)
crates/castr-net/src/pairing.rs     SPAKE2 + HMAC exchange over a Link
crates/castr-net/src/discovery.rs   mDNS advertise/browse and UDP broadcast fallback
crates/castr-net/src/retransmit.rs  sender ring buffer for NACK replies
crates/castr-capture-win/src/lib.rs
crates/castr-capture-win/src/dxgi.rs   Desktop Duplication capture
crates/castr-capture-win/src/wasapi.rs WASAPI loopback capture
crates/castr-codec-win/src/lib.rs
crates/castr-codec-win/src/mf.rs       MF startup, MFT helpers, sample helpers
crates/castr-codec-win/src/encoder.rs  MF H.264 encoder implementing VideoEncoder
crates/castr-codec-win/src/decoder.rs  MF H.264 decoder implementing VideoDecoder
crates/castr-receiver/src/main.rs      clap CLI
crates/castr-receiver/src/pipeline.rs  Link -> Reassembler -> JitterBuffer -> decoder -> render
crates/castr-receiver/src/render.rs    SDL2 window, YUV texture, overlay text
crates/castr-receiver/src/audio_out.rs SDL2 audio queue playback
crates/castr-sender/src/main.rs        clap CLI, launches GUI when no args
crates/castr-sender/src/cast.rs        capture -> encode -> packetize -> Link, stats, reconnect
crates/castr-sender/src/gui.rs         eframe window
```

---

### Task 0: Toolchain and workspace scaffold

**Files:**
- Create: `Cargo.toml`, `rust-toolchain.toml`, `.gitignore`
- Create: `crates/castr-proto/Cargo.toml`, `crates/castr-proto/src/lib.rs`
- Create: `crates/castr-media/Cargo.toml`, `crates/castr-media/src/lib.rs`
- Create: `crates/castr-net/Cargo.toml`, `crates/castr-net/src/lib.rs`

**Interfaces:**
- Produces: a building workspace with three empty library crates. Later tasks add the Windows crates and binaries.

- [ ] **Step 1: Install the toolchain (PowerShell, run once)**

```powershell
winget install --id Rustlang.Rustup -e --accept-package-agreements --accept-source-agreements
winget install --id Kitware.CMake -e --accept-package-agreements --accept-source-agreements
winget install --id Microsoft.VisualStudio.2022.BuildTools -e --accept-package-agreements --accept-source-agreements --override "--quiet --wait --add Microsoft.VisualStudio.Workload.VCTools --includeRecommended"
```

Close and reopen the terminal so PATH updates. Then:

```powershell
rustup default stable
rustup component add clippy rustfmt
cargo --version; cmake --version
```

Expected: `cargo 1.8x.x` and `cmake version 3.x` or `4.x` print.

- [ ] **Step 2: Write the workspace files**

`rust-toolchain.toml`:
```toml
[toolchain]
channel = "stable"
components = ["clippy", "rustfmt"]
```

`.gitignore`:
```
/target
*.log
```

`Cargo.toml`:
```toml
[workspace]
resolver = "2"
members = ["crates/*"]

[workspace.package]
version = "0.1.0"
edition = "2021"
license = "MIT"

[workspace.dependencies]
anyhow = "1"
thiserror = "1"
bytes = "1"
serde = { version = "1", features = ["derive"] }
postcard = { version = "1", features = ["alloc"] }
tokio = { version = "1", features = ["rt-multi-thread", "macros", "sync", "time", "net", "io-util"] }
tracing = "0.1"
tracing-subscriber = { version = "0.3", features = ["env-filter"] }
clap = { version = "4", features = ["derive"] }
quinn = "0.11"
rustls = { version = "0.23", default-features = false, features = ["ring", "std"] }
rcgen = "0.13"
sha2 = "0.10"
hmac = "0.12"
spake2 = "0.4"
rand = "0.8"
hex = "0.4"
toml = "0.8"
dirs = "5"
mdns-sd = "0.11"
openh264 = { version = "0.6", features = ["source"] }
audiopus = "0.2"
sdl2 = { version = "0.36", features = ["bundled", "static-link"] }
eframe = "0.28"
windows = "0.58"

[profile.release]
opt-level = 3
lto = "thin"
codegen-units = 1
strip = true
```

`crates/castr-proto/Cargo.toml`:
```toml
[package]
name = "castr-proto"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
bytes.workspace = true
serde.workspace = true
postcard.workspace = true
thiserror.workspace = true
```

`crates/castr-media/Cargo.toml`:
```toml
[package]
name = "castr-media"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
thiserror.workspace = true
bytes.workspace = true
tracing.workspace = true
openh264.workspace = true
audiopus.workspace = true
castr-proto = { path = "../castr-proto" }
```

`crates/castr-net/Cargo.toml`:
```toml
[package]
name = "castr-net"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
thiserror.workspace = true
bytes.workspace = true
serde.workspace = true
postcard.workspace = true
tokio.workspace = true
tracing.workspace = true
quinn.workspace = true
rustls.workspace = true
rcgen.workspace = true
sha2.workspace = true
hmac.workspace = true
spake2.workspace = true
rand.workspace = true
hex.workspace = true
toml.workspace = true
dirs.workspace = true
mdns-sd.workspace = true
castr-proto = { path = "../castr-proto" }
```

Each `src/lib.rs` for now:
```rust
//! See docs/superpowers/specs/2026-09-01-castr-core-design.md
```

- [ ] **Step 3: Build**

Run: `cargo build --workspace`
Expected: compiles. The first build downloads and compiles openh264 and opus from source with cmake; several minutes.

- [ ] **Step 4: Commit**

```bash
git add -A
git commit -m "chore: workspace scaffold with proto, media, net crates"
```

---

### Task 1: Datagram header

**Files:**
- Create: `crates/castr-proto/src/header.rs`
- Modify: `crates/castr-proto/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub const HEADER_LEN: usize = 20;
  pub const STREAM_VIDEO: u8 = 0;
  pub const STREAM_AUDIO: u8 = 1;
  pub const FLAG_KEYFRAME: u8 = 0b01;
  pub const FLAG_END_OF_FRAME: u8 = 0b10;
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct DatagramHeader { pub stream: u8, pub flags: u8, pub fragment_index: u16, pub fragment_count: u16, pub frame_number: u32, pub timestamp_us: u64 }
  impl DatagramHeader {
      pub fn encode(&self, out: &mut [u8; HEADER_LEN]);
      pub fn decode(buf: &[u8]) -> Result<(DatagramHeader, &[u8]), HeaderError>; // returns payload slice
      pub fn is_keyframe(&self) -> bool; pub fn is_end(&self) -> bool;
  }
  #[derive(Debug, thiserror::Error, PartialEq)] pub enum HeaderError { #[error("datagram shorter than header")] TooShort, #[error("unknown stream id {0}")] UnknownStream(u8) }
  ```

- [ ] **Step 1: Write the failing tests**

`crates/castr-proto/src/header.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_preserves_all_fields() {
        let h = DatagramHeader {
            stream: STREAM_VIDEO,
            flags: FLAG_KEYFRAME | FLAG_END_OF_FRAME,
            fragment_index: 3,
            fragment_count: 7,
            frame_number: 0xDEADBEEF,
            timestamp_us: 1_234_567_890_123,
        };
        let mut buf = [0u8; HEADER_LEN];
        h.encode(&mut buf);
        let (decoded, payload) = DatagramHeader::decode(&buf).unwrap();
        assert_eq!(decoded, h);
        assert!(payload.is_empty());
    }

    #[test]
    fn layout_matches_spec_little_endian() {
        let h = DatagramHeader {
            stream: STREAM_AUDIO, flags: 0, fragment_index: 0x0102, fragment_count: 0x0304,
            frame_number: 0x05060708, timestamp_us: 0x090A0B0C0D0E0F10,
        };
        let mut buf = [0u8; HEADER_LEN];
        h.encode(&mut buf);
        assert_eq!(buf[0], 1);
        assert_eq!(buf[1], 0);
        assert_eq!(&buf[2..4], &[0x02, 0x01]);
        assert_eq!(&buf[4..6], &[0x04, 0x03]);
        assert_eq!(&buf[6..8], &[0, 0]);
        assert_eq!(&buf[8..12], &[0x08, 0x07, 0x06, 0x05]);
        assert_eq!(&buf[12..20], &[0x10, 0x0F, 0x0E, 0x0D, 0x0C, 0x0B, 0x0A, 0x09]);
    }

    #[test]
    fn decode_returns_payload_after_header() {
        let mut buf = vec![0u8; HEADER_LEN];
        buf.extend_from_slice(b"abc");
        let (_, payload) = DatagramHeader::decode(&buf).unwrap();
        assert_eq!(payload, b"abc");
    }

    #[test]
    fn decode_rejects_short_and_unknown_stream() {
        assert_eq!(DatagramHeader::decode(&[0u8; 19]).unwrap_err(), HeaderError::TooShort);
        let mut buf = [0u8; HEADER_LEN];
        buf[0] = 9;
        assert_eq!(DatagramHeader::decode(&buf).unwrap_err(), HeaderError::UnknownStream(9));
    }
}
```

`crates/castr-proto/src/lib.rs`:
```rust
//! See docs/superpowers/specs/2026-09-01-castr-core-design.md
pub mod header;
pub use header::*;
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-proto`
Expected: compile error, `DatagramHeader` not found.

- [ ] **Step 3: Implement**

Top of `crates/castr-proto/src/header.rs` (above the tests module):
```rust
pub const HEADER_LEN: usize = 20;
pub const STREAM_VIDEO: u8 = 0;
pub const STREAM_AUDIO: u8 = 1;
pub const FLAG_KEYFRAME: u8 = 0b01;
pub const FLAG_END_OF_FRAME: u8 = 0b10;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct DatagramHeader {
    pub stream: u8,
    pub flags: u8,
    pub fragment_index: u16,
    pub fragment_count: u16,
    pub frame_number: u32,
    pub timestamp_us: u64,
}

#[derive(Debug, thiserror::Error, PartialEq, Eq)]
pub enum HeaderError {
    #[error("datagram shorter than header")]
    TooShort,
    #[error("unknown stream id {0}")]
    UnknownStream(u8),
}

impl DatagramHeader {
    pub fn encode(&self, out: &mut [u8; HEADER_LEN]) {
        out[0] = self.stream;
        out[1] = self.flags;
        out[2..4].copy_from_slice(&self.fragment_index.to_le_bytes());
        out[4..6].copy_from_slice(&self.fragment_count.to_le_bytes());
        out[6..8].copy_from_slice(&[0, 0]);
        out[8..12].copy_from_slice(&self.frame_number.to_le_bytes());
        out[12..20].copy_from_slice(&self.timestamp_us.to_le_bytes());
    }

    pub fn decode(buf: &[u8]) -> Result<(DatagramHeader, &[u8]), HeaderError> {
        if buf.len() < HEADER_LEN {
            return Err(HeaderError::TooShort);
        }
        let stream = buf[0];
        if stream != STREAM_VIDEO && stream != STREAM_AUDIO {
            return Err(HeaderError::UnknownStream(stream));
        }
        let h = DatagramHeader {
            stream,
            flags: buf[1],
            fragment_index: u16::from_le_bytes([buf[2], buf[3]]),
            fragment_count: u16::from_le_bytes([buf[4], buf[5]]),
            frame_number: u32::from_le_bytes([buf[8], buf[9], buf[10], buf[11]]),
            timestamp_us: u64::from_le_bytes(buf[12..20].try_into().unwrap()),
        };
        Ok((h, &buf[HEADER_LEN..]))
    }

    pub fn is_keyframe(&self) -> bool { self.flags & FLAG_KEYFRAME != 0 }
    pub fn is_end(&self) -> bool { self.flags & FLAG_END_OF_FRAME != 0 }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-proto`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(proto): 20-byte datagram header encode/decode"
```

---

### Task 2: Packetizer

**Files:**
- Create: `crates/castr-proto/src/packetize.rs`
- Modify: `crates/castr-proto/src/lib.rs`

**Interfaces:**
- Consumes: `DatagramHeader`, constants from Task 1.
- Produces:
  ```rust
  pub struct Packetizer { next_frame: u32 }
  impl Packetizer {
      pub fn new() -> Self;
      /// Splits one encoded frame into datagrams, each <= max_datagram. Returns Vec of complete datagram bytes (header + payload).
      pub fn packetize(&mut self, stream: u8, keyframe: bool, timestamp_us: u64, data: &[u8], max_datagram: usize) -> Vec<bytes::Bytes>;
      pub fn last_frame_number(&self) -> u32;
  }
  ```

- [ ] **Step 1: Write the failing tests**

`crates/castr-proto/src/packetize.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::*;

    #[test]
    fn small_frame_is_one_datagram_with_both_flags() {
        let mut p = Packetizer::new();
        let out = p.packetize(STREAM_VIDEO, true, 42, b"hello", 1200);
        assert_eq!(out.len(), 1);
        let (h, payload) = DatagramHeader::decode(&out[0]).unwrap();
        assert_eq!(payload, b"hello");
        assert_eq!(h.fragment_index, 0);
        assert_eq!(h.fragment_count, 1);
        assert_eq!(h.frame_number, 0);
        assert_eq!(h.timestamp_us, 42);
        assert!(h.is_keyframe());
        assert!(h.is_end());
    }

    #[test]
    fn large_frame_splits_at_payload_budget() {
        let mut p = Packetizer::new();
        let data = vec![7u8; 2500];
        let out = p.packetize(STREAM_VIDEO, false, 0, &data, HEADER_LEN + 1000);
        assert_eq!(out.len(), 3);
        for (i, d) in out.iter().enumerate() {
            let (h, payload) = DatagramHeader::decode(d).unwrap();
            assert!(d.len() <= HEADER_LEN + 1000);
            assert_eq!(h.fragment_index as usize, i);
            assert_eq!(h.fragment_count, 3);
            assert!(!h.is_keyframe());
            assert_eq!(h.is_end(), i == 2);
            let expected_len = if i == 2 { 500 } else { 1000 };
            assert_eq!(payload.len(), expected_len);
        }
    }

    #[test]
    fn frame_number_increments_per_call_and_wraps() {
        let mut p = Packetizer { next_frame: u32::MAX };
        let a = p.packetize(STREAM_VIDEO, false, 0, b"a", 100);
        let b = p.packetize(STREAM_VIDEO, false, 0, b"b", 100);
        assert_eq!(DatagramHeader::decode(&a[0]).unwrap().0.frame_number, u32::MAX);
        assert_eq!(DatagramHeader::decode(&b[0]).unwrap().0.frame_number, 0);
        assert_eq!(p.last_frame_number(), 0);
    }

    #[test]
    fn empty_frame_produces_nothing() {
        let mut p = Packetizer::new();
        assert!(p.packetize(STREAM_VIDEO, false, 0, b"", 1200).is_empty());
    }
}
```

Add `pub mod packetize; pub use packetize::*;` to `lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-proto packetize`
Expected: compile error, `Packetizer` not found.

- [ ] **Step 3: Implement**

Top of `crates/castr-proto/src/packetize.rs`:
```rust
use crate::header::*;
use bytes::{Bytes, BytesMut};

#[derive(Debug, Default)]
pub struct Packetizer {
    next_frame: u32,
}

impl Packetizer {
    pub fn new() -> Self { Self { next_frame: 0 } }

    pub fn last_frame_number(&self) -> u32 { self.next_frame.wrapping_sub(1) }

    pub fn packetize(
        &mut self, stream: u8, keyframe: bool, timestamp_us: u64, data: &[u8], max_datagram: usize,
    ) -> Vec<Bytes> {
        if data.is_empty() {
            return Vec::new();
        }
        let budget = max_datagram.saturating_sub(HEADER_LEN).max(1);
        let frame_number = self.next_frame;
        self.next_frame = self.next_frame.wrapping_add(1);
        let chunks: Vec<&[u8]> = data.chunks(budget).collect();
        let count = chunks.len();
        assert!(count <= u16::MAX as usize, "frame too large for u16 fragment count");
        let mut out = Vec::with_capacity(count);
        for (i, chunk) in chunks.into_iter().enumerate() {
            let mut flags = 0;
            if keyframe { flags |= FLAG_KEYFRAME; }
            if i + 1 == count { flags |= FLAG_END_OF_FRAME; }
            let h = DatagramHeader {
                stream, flags,
                fragment_index: i as u16,
                fragment_count: count as u16,
                frame_number, timestamp_us,
            };
            let mut hdr = [0u8; HEADER_LEN];
            h.encode(&mut hdr);
            let mut buf = BytesMut::with_capacity(HEADER_LEN + chunk.len());
            buf.extend_from_slice(&hdr);
            buf.extend_from_slice(chunk);
            out.push(buf.freeze());
        }
        out
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-proto`
Expected: 8 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(proto): packetizer splits frames into MTU-sized datagrams"
```

---

### Task 3: Reassembler

**Files:**
- Create: `crates/castr-proto/src/reassemble.rs`
- Modify: `crates/castr-proto/src/lib.rs`

**Interfaces:**
- Consumes: `DatagramHeader`.
- Produces:
  ```rust
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct CompleteFrame { pub stream: u8, pub frame_number: u32, pub timestamp_us: u64, pub keyframe: bool, pub data: Vec<u8> }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct Nack { pub frame_number: u32, pub missing: Vec<u16> }
  pub struct Reassembler { /* private */ }
  impl Reassembler {
      /// max_age_us: incomplete frames older than this (by arrival clock) are discarded.
      pub fn new(max_age_us: u64) -> Self;
      /// Feed one datagram. now_us is the receiver's monotonic clock. Returns a frame if this datagram completed it.
      pub fn push(&mut self, datagram: &[u8], now_us: u64) -> Result<Option<CompleteFrame>, HeaderError>;
      /// Discard expired incomplete frames. Returns NACKs for incomplete keyframes still within max_age.
      pub fn tick(&mut self, now_us: u64) -> Vec<Nack>;
      pub fn pending(&self) -> usize;
      pub fn fragments_lost(&mut self) -> u64; // counter of fragments discarded as expired, resets on read
  }
  ```
  Frame ordering uses wrapping comparison: `a` is newer than `b` if `a.wrapping_sub(b) < u32::MAX / 2`.

- [ ] **Step 1: Write the failing tests**

`crates/castr-proto/src/reassemble.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::header::*;
    use crate::packetize::Packetizer;

    fn frames(p: &mut Packetizer, keyframe: bool, data: &[u8]) -> Vec<bytes::Bytes> {
        p.packetize(STREAM_VIDEO, keyframe, 5, data, HEADER_LEN + 100)
    }

    #[test]
    fn in_order_fragments_complete_a_frame() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let data: Vec<u8> = (0..250).map(|i| i as u8).collect();
        let dgs = frames(&mut p, true, &data);
        assert_eq!(dgs.len(), 3);
        assert_eq!(r.push(&dgs[0], 0).unwrap(), None);
        assert_eq!(r.push(&dgs[1], 0).unwrap(), None);
        let f = r.push(&dgs[2], 0).unwrap().unwrap();
        assert_eq!(f, CompleteFrame { stream: STREAM_VIDEO, frame_number: 0, timestamp_us: 5, keyframe: true, data });
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn out_of_order_and_duplicate_fragments_still_complete() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let data: Vec<u8> = (0..250).map(|i| i as u8).collect();
        let dgs = frames(&mut p, false, &data);
        assert_eq!(r.push(&dgs[2], 0).unwrap(), None);
        assert_eq!(r.push(&dgs[2], 0).unwrap(), None);
        assert_eq!(r.push(&dgs[0], 0).unwrap(), None);
        let f = r.push(&dgs[1], 0).unwrap().unwrap();
        assert_eq!(f.data, data);
    }

    #[test]
    fn tick_nacks_incomplete_keyframe_and_expires_old_frames() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let data = vec![1u8; 350];
        let dgs = frames(&mut p, true, &data);
        r.push(&dgs[0], 0).unwrap();
        r.push(&dgs[3], 0).unwrap();
        let nacks = r.tick(100_000);
        assert_eq!(nacks, vec![Nack { frame_number: 0, missing: vec![1, 2] }]);
        assert_eq!(r.pending(), 1);
        assert!(r.tick(600_001).is_empty());
        assert_eq!(r.pending(), 0);
        assert_eq!(r.fragments_lost(), 2);
        assert_eq!(r.fragments_lost(), 0);
    }

    #[test]
    fn tick_does_not_nack_delta_frames() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let dgs = frames(&mut p, false, &[2u8; 350]);
        r.push(&dgs[0], 0).unwrap();
        assert!(r.tick(100_000).is_empty());
    }

    #[test]
    fn late_fragment_for_already_completed_frame_is_ignored() {
        let mut p = Packetizer::new();
        let mut r = Reassembler::new(500_000);
        let dgs = frames(&mut p, false, &[3u8; 150]);
        r.push(&dgs[0], 0).unwrap();
        assert!(r.push(&dgs[1], 0).unwrap().is_some());
        assert_eq!(r.push(&dgs[1], 0).unwrap(), None);
        assert_eq!(r.pending(), 0);
    }

    #[test]
    fn frame_numbers_wrap_without_confusing_age() {
        let mut p = Packetizer { next_frame: u32::MAX };
        let mut r = Reassembler::new(500_000);
        let a = frames(&mut p, false, b"a");
        let b = frames(&mut p, false, b"b");
        assert_eq!(r.push(&a[0], 0).unwrap().unwrap().frame_number, u32::MAX);
        assert_eq!(r.push(&b[0], 0).unwrap().unwrap().frame_number, 0);
    }

    #[test]
    fn rejects_bad_header() {
        let mut r = Reassembler::new(1);
        assert_eq!(r.push(&[0u8; 3], 0).unwrap_err(), HeaderError::TooShort);
    }
}
```

Add `pub mod reassemble; pub use reassemble::*;` to `lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-proto reassemble`
Expected: compile error, `Reassembler` not found.

- [ ] **Step 3: Implement**

Top of `crates/castr-proto/src/reassemble.rs`:
```rust
use crate::header::*;
use std::collections::BTreeMap;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CompleteFrame {
    pub stream: u8,
    pub frame_number: u32,
    pub timestamp_us: u64,
    pub keyframe: bool,
    pub data: Vec<u8>,
}

#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct Nack {
    pub frame_number: u32,
    pub missing: Vec<u16>,
}

struct Partial {
    stream: u8,
    timestamp_us: u64,
    keyframe: bool,
    first_seen_us: u64,
    parts: Vec<Option<Vec<u8>>>,
    received: usize,
}

pub struct Reassembler {
    max_age_us: u64,
    partial: BTreeMap<u32, Partial>,
    /// Frame numbers completed recently, so late duplicates are dropped.
    completed: std::collections::VecDeque<u32>,
    lost: u64,
}

/// True if `a` is newer than or equal to `b` under wrapping arithmetic.
pub fn frame_newer_or_eq(a: u32, b: u32) -> bool {
    a.wrapping_sub(b) < u32::MAX / 2
}

impl Reassembler {
    pub fn new(max_age_us: u64) -> Self {
        Self { max_age_us, partial: BTreeMap::new(), completed: std::collections::VecDeque::new(), lost: 0 }
    }

    pub fn pending(&self) -> usize { self.partial.len() }

    pub fn fragments_lost(&mut self) -> u64 { std::mem::take(&mut self.lost) }

    pub fn push(&mut self, datagram: &[u8], now_us: u64) -> Result<Option<CompleteFrame>, HeaderError> {
        let (h, payload) = DatagramHeader::decode(datagram)?;
        if self.completed.contains(&h.frame_number) {
            return Ok(None);
        }
        let count = h.fragment_count.max(1) as usize;
        let entry = self.partial.entry(h.frame_number).or_insert_with(|| Partial {
            stream: h.stream,
            timestamp_us: h.timestamp_us,
            keyframe: h.is_keyframe(),
            first_seen_us: now_us,
            parts: vec![None; count],
            received: 0,
        });
        let idx = h.fragment_index as usize;
        if idx >= entry.parts.len() || entry.parts[idx].is_some() {
            return Ok(None);
        }
        entry.parts[idx] = Some(payload.to_vec());
        entry.received += 1;
        if entry.received < entry.parts.len() {
            return Ok(None);
        }
        let done = self.partial.remove(&h.frame_number).unwrap();
        self.remember_completed(h.frame_number);
        let mut data = Vec::new();
        for p in done.parts.into_iter() {
            data.extend_from_slice(&p.unwrap());
        }
        Ok(Some(CompleteFrame {
            stream: done.stream,
            frame_number: h.frame_number,
            timestamp_us: done.timestamp_us,
            keyframe: done.keyframe,
            data,
        }))
    }

    fn remember_completed(&mut self, n: u32) {
        self.completed.push_back(n);
        while self.completed.len() > 64 {
            self.completed.pop_front();
        }
    }

    pub fn tick(&mut self, now_us: u64) -> Vec<Nack> {
        let mut nacks = Vec::new();
        let mut expired = Vec::new();
        for (&fnum, p) in self.partial.iter() {
            let age = now_us.saturating_sub(p.first_seen_us);
            if age > self.max_age_us {
                expired.push(fnum);
                self.lost += (p.parts.len() - p.received) as u64;
            } else if p.keyframe {
                let missing: Vec<u16> = p.parts.iter().enumerate()
                    .filter(|(_, x)| x.is_none()).map(|(i, _)| i as u16).collect();
                nacks.push(Nack { frame_number: fnum, missing });
            }
        }
        for f in expired {
            self.partial.remove(&f);
        }
        nacks
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-proto`
Expected: 15 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(proto): reassembler with NACK generation and expiry"
```

---

### Task 4: Control messages and framing

**Files:**
- Create: `crates/castr-proto/src/control.rs`
- Modify: `crates/castr-proto/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub const PROTOCOL_VERSION: u16 = 1;
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] pub enum Mode { Game, Quality }
  #[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)] pub enum Codec { H264 }
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct Capabilities { pub max_width: u32, pub max_height: u32, pub max_fps: u32, pub max_bitrate_bps: u32, pub codecs: Vec<Codec>, pub audio: bool }
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct StreamParams { pub codec: Codec, pub width: u32, pub height: u32, pub fps: u32, pub mode: Mode, pub bitrate_bps: u32 }
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct Stats { pub frames_received: u32, pub frames_dropped: u32, pub fragments_lost: u32, pub fragments_received: u32, pub decode_queue_depth: u32, pub interval_ms: u32 }
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub enum ControlMessage {
      Hello { version: u16, name: String, resume_token: Option<[u8; 16]> },
      HelloAck { name: String, caps: Capabilities },
      StartStream(StreamParams),
      SessionToken([u8; 16]),
      SetMode(Mode),
      RequestKeyframe,
      Stats(Stats),
      PairInit(Vec<u8>), PairResp(Vec<u8>), PairProof([u8; 32]), PairOk,
      Error { code: u16, message: String },
      Goodbye { reason: String },
  }
  pub fn encode_len_prefixed<T: Serialize>(v: &T) -> Vec<u8>;       // 4-byte LE length + postcard body
  pub fn decode_len_prefixed<T: DeserializeOwned>(buf: &[u8]) -> Result<Option<(T, usize)>, ControlError>; // None if incomplete; usize = bytes consumed
  pub fn encode_frame(msg: &ControlMessage) -> Vec<u8>;             // = encode_len_prefixed
  pub fn decode_frame(buf: &[u8]) -> Result<Option<(ControlMessage, usize)>, ControlError>;
  pub const MAX_CONTROL_FRAME: usize = 64 * 1024;
  #[derive(Debug, thiserror::Error)] pub enum ControlError { #[error("frame exceeds {MAX_CONTROL_FRAME} bytes")] TooLarge, #[error("decode: {0}")] Postcard(#[from] postcard::Error) }
  ```
  The `Pair*` variants carry the SPAKE2 exchange in Task 14 and are defined here so the enum is closed.

- [ ] **Step 1: Write the failing tests**

`crates/castr-proto/src/control.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn round_trip_hello_with_token() {
        let m = ControlMessage::Hello { version: PROTOCOL_VERSION, name: "pc".into(), resume_token: Some([9u8; 16]) };
        let bytes = encode_frame(&m);
        let len = u32::from_le_bytes(bytes[0..4].try_into().unwrap()) as usize;
        assert_eq!(len + 4, bytes.len());
        let (decoded, used) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(decoded, m);
        assert_eq!(used, bytes.len());
    }

    #[test]
    fn partial_buffer_returns_none() {
        let bytes = encode_frame(&ControlMessage::RequestKeyframe);
        assert!(decode_frame(&bytes[..2]).unwrap().is_none());
        assert!(decode_frame(&bytes[..bytes.len() - 1]).unwrap().is_none());
    }

    #[test]
    fn two_frames_back_to_back_decode_sequentially() {
        let mut bytes = encode_frame(&ControlMessage::SetMode(Mode::Game));
        bytes.extend(encode_frame(&ControlMessage::Goodbye { reason: "bye".into() }));
        let (a, used) = decode_frame(&bytes).unwrap().unwrap();
        assert_eq!(a, ControlMessage::SetMode(Mode::Game));
        let (b, used2) = decode_frame(&bytes[used..]).unwrap().unwrap();
        assert_eq!(b, ControlMessage::Goodbye { reason: "bye".into() });
        assert_eq!(used + used2, bytes.len());
    }

    #[test]
    fn oversized_length_is_rejected() {
        let mut bytes = ((MAX_CONTROL_FRAME + 1) as u32).to_le_bytes().to_vec();
        bytes.push(0);
        assert!(matches!(decode_frame(&bytes), Err(ControlError::TooLarge)));
    }

    #[test]
    fn stats_and_caps_round_trip() {
        let m = ControlMessage::HelloAck {
            name: "pi".into(),
            caps: Capabilities { max_width: 1920, max_height: 1080, max_fps: 30, max_bitrate_bps: 10_000_000, codecs: vec![Codec::H264], audio: true },
        };
        let (d, _) = decode_frame(&encode_frame(&m)).unwrap().unwrap();
        assert_eq!(d, m);
    }
}
```

Add `pub mod control; pub use control::*;` to `lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-proto control`
Expected: compile error.

- [ ] **Step 3: Implement**

Top of `crates/castr-proto/src/control.rs`:
```rust
use serde::{Deserialize, Serialize};

pub const PROTOCOL_VERSION: u16 = 1;
pub const MAX_CONTROL_FRAME: usize = 64 * 1024;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Mode { Game, Quality }

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum Codec { H264 }

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capabilities {
    pub max_width: u32,
    pub max_height: u32,
    pub max_fps: u32,
    pub max_bitrate_bps: u32,
    pub codecs: Vec<Codec>,
    pub audio: bool,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct StreamParams {
    pub codec: Codec,
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub mode: Mode,
    pub bitrate_bps: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize, Default)]
pub struct Stats {
    pub frames_received: u32,
    pub frames_dropped: u32,
    pub fragments_lost: u32,
    pub fragments_received: u32,
    pub decode_queue_depth: u32,
    pub interval_ms: u32,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub enum ControlMessage {
    Hello { version: u16, name: String, resume_token: Option<[u8; 16]> },
    HelloAck { name: String, caps: Capabilities },
    StartStream(StreamParams),
    SessionToken([u8; 16]),
    SetMode(Mode),
    RequestKeyframe,
    Stats(Stats),
    PairInit(Vec<u8>),
    PairResp(Vec<u8>),
    PairProof([u8; 32]),
    PairOk,
    Error { code: u16, message: String },
    Goodbye { reason: String },
}

#[derive(Debug, thiserror::Error)]
pub enum ControlError {
    #[error("frame exceeds {MAX_CONTROL_FRAME} bytes")]
    TooLarge,
    #[error("decode: {0}")]
    Postcard(#[from] postcard::Error),
}

/// Generic 4-byte LE length prefix + postcard body. Used for control messages and NACKs.
pub fn encode_len_prefixed<T: Serialize>(v: &T) -> Vec<u8> {
    let body = postcard::to_allocvec(v).expect("postcard encode cannot fail for these types");
    let mut out = Vec::with_capacity(4 + body.len());
    out.extend_from_slice(&(body.len() as u32).to_le_bytes());
    out.extend_from_slice(&body);
    out
}

pub fn decode_len_prefixed<T: serde::de::DeserializeOwned>(buf: &[u8]) -> Result<Option<(T, usize)>, ControlError> {
    if buf.len() < 4 {
        return Ok(None);
    }
    let len = u32::from_le_bytes(buf[0..4].try_into().unwrap()) as usize;
    if len > MAX_CONTROL_FRAME {
        return Err(ControlError::TooLarge);
    }
    if buf.len() < 4 + len {
        return Ok(None);
    }
    let v: T = postcard::from_bytes(&buf[4..4 + len])?;
    Ok(Some((v, 4 + len)))
}

pub fn encode_frame(msg: &ControlMessage) -> Vec<u8> { encode_len_prefixed(msg) }

pub fn decode_frame(buf: &[u8]) -> Result<Option<(ControlMessage, usize)>, ControlError> { decode_len_prefixed(buf) }
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-proto`
Expected: 20 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(proto): control message enum with length-prefixed postcard framing"
```

---

### Task 5: Session state machine

**Files:**
- Create: `crates/castr-proto/src/session.rs`
- Modify: `crates/castr-proto/src/lib.rs`

**Interfaces:**
- Consumes: `ControlMessage`, `Capabilities`, `StreamParams`, `PROTOCOL_VERSION`.
- Produces (receiver side; the sender side is simple enough to live in the sender binary):
  ```rust
  pub const TOKEN_TTL_US: u64 = 60_000_000;
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum ReceiverState { AwaitingHello, Streaming { params: Option<StreamParams> }, Disconnected { since_us: u64 }, Closed }
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub enum Action { Send(ControlMessage), Resumed, Fail(String) }
  pub struct ReceiverSession { /* private */ }
  impl ReceiverSession {
      pub fn new(name: String, caps: Capabilities, token: [u8; 16]) -> Self;
      pub fn state(&self) -> &ReceiverState;
      pub fn token(&self) -> [u8; 16];
      pub fn params(&self) -> Option<&StreamParams>;
      /// Drive the machine with an inbound control message. Returns actions to perform in order.
      pub fn on_message(&mut self, msg: ControlMessage, now_us: u64) -> Vec<Action>;
      /// The QUIC connection dropped.
      pub fn on_disconnect(&mut self, now_us: u64);
  }
  ```
  Rules: `Hello` with no token while AwaitingHello replies `HelloAck` then `SessionToken`. `Hello` with a matching token while Disconnected and within TTL replies `HelloAck` (so the sender learns caps again cheaply), emits `Resumed`, and moves to Streaming keeping previous params. `Hello` with wrong or expired token yields `Fail` and `Send(Error{code:1})`. Wrong version yields `Send(Error{code:2})` and `Fail`. `StartStream` while Streaming stores params. `Goodbye` moves to Closed.

- [ ] **Step 1: Write the failing tests**

`crates/castr-proto/src/session.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::control::*;

    fn caps() -> Capabilities {
        Capabilities { max_width: 1920, max_height: 1080, max_fps: 60, max_bitrate_bps: 40_000_000, codecs: vec![Codec::H264], audio: true }
    }
    fn params() -> StreamParams {
        StreamParams { codec: Codec::H264, width: 1280, height: 720, fps: 30, mode: Mode::Game, bitrate_bps: 5_000_000 }
    }
    fn hello(token: Option<[u8; 16]>) -> ControlMessage {
        ControlMessage::Hello { version: PROTOCOL_VERSION, name: "pc".into(), resume_token: token }
    }

    #[test]
    fn fresh_hello_gets_ack_and_token() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [1u8; 16]);
        let actions = s.on_message(hello(None), 0);
        assert_eq!(actions, vec![
            Action::Send(ControlMessage::HelloAck { name: "pi".into(), caps: caps() }),
            Action::Send(ControlMessage::SessionToken([1u8; 16])),
        ]);
        assert_eq!(s.state(), &ReceiverState::Streaming { params: None });
        assert!(s.on_message(ControlMessage::StartStream(params()), 0).is_empty());
        assert_eq!(s.params(), Some(&params()));
    }

    #[test]
    fn wrong_version_is_rejected() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [1u8; 16]);
        let actions = s.on_message(ControlMessage::Hello { version: 99, name: "pc".into(), resume_token: None }, 0);
        assert!(matches!(actions[0], Action::Send(ControlMessage::Error { code: 2, .. })));
        assert!(matches!(actions[1], Action::Fail(_)));
    }

    #[test]
    fn resume_with_valid_token_keeps_params() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [7u8; 16]);
        s.on_message(hello(None), 0);
        s.on_message(ControlMessage::StartStream(params()), 0);
        s.on_disconnect(10_000_000);
        assert_eq!(s.state(), &ReceiverState::Disconnected { since_us: 10_000_000 });
        let actions = s.on_message(hello(Some([7u8; 16])), 20_000_000);
        assert_eq!(actions, vec![
            Action::Send(ControlMessage::HelloAck { name: "pi".into(), caps: caps() }),
            Action::Resumed,
        ]);
        assert_eq!(s.params(), Some(&params()));
        assert!(matches!(s.state(), ReceiverState::Streaming { .. }));
    }

    #[test]
    fn resume_with_expired_token_fails() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [7u8; 16]);
        s.on_message(hello(None), 0);
        s.on_disconnect(10_000_000);
        let actions = s.on_message(hello(Some([7u8; 16])), 10_000_000 + TOKEN_TTL_US + 1);
        assert!(matches!(actions[0], Action::Send(ControlMessage::Error { code: 1, .. })));
        assert!(matches!(actions[1], Action::Fail(_)));
    }

    #[test]
    fn resume_with_wrong_token_fails() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [7u8; 16]);
        s.on_message(hello(None), 0);
        s.on_disconnect(0);
        let actions = s.on_message(hello(Some([8u8; 16])), 1);
        assert!(matches!(actions[1], Action::Fail(_)));
    }

    #[test]
    fn hello_with_token_while_awaiting_is_treated_as_fresh() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [7u8; 16]);
        let actions = s.on_message(hello(Some([9u8; 16])), 0);
        assert_eq!(actions.len(), 2);
        assert!(matches!(actions[1], Action::Send(ControlMessage::SessionToken(_))));
    }

    #[test]
    fn goodbye_closes() {
        let mut s = ReceiverSession::new("pi".into(), caps(), [7u8; 16]);
        s.on_message(hello(None), 0);
        s.on_message(ControlMessage::Goodbye { reason: "done".into() }, 0);
        assert_eq!(s.state(), &ReceiverState::Closed);
    }
}
```

Add `pub mod session; pub use session::*;` to `lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-proto session`
Expected: compile error.

- [ ] **Step 3: Implement**

Top of `crates/castr-proto/src/session.rs`:
```rust
use crate::control::*;

pub const TOKEN_TTL_US: u64 = 60_000_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ReceiverState {
    AwaitingHello,
    Streaming { params: Option<StreamParams> },
    Disconnected { since_us: u64 },
    Closed,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Action {
    Send(ControlMessage),
    Resumed,
    Fail(String),
}

pub struct ReceiverSession {
    name: String,
    caps: Capabilities,
    token: [u8; 16],
    state: ReceiverState,
    params: Option<StreamParams>,
}

impl ReceiverSession {
    pub fn new(name: String, caps: Capabilities, token: [u8; 16]) -> Self {
        Self { name, caps, token, state: ReceiverState::AwaitingHello, params: None }
    }

    pub fn state(&self) -> &ReceiverState { &self.state }
    pub fn token(&self) -> [u8; 16] { self.token }
    pub fn params(&self) -> Option<&StreamParams> { self.params.as_ref() }

    fn ack(&self) -> ControlMessage {
        ControlMessage::HelloAck { name: self.name.clone(), caps: self.caps.clone() }
    }

    pub fn on_disconnect(&mut self, now_us: u64) {
        if !matches!(self.state, ReceiverState::Closed) {
            self.state = ReceiverState::Disconnected { since_us: now_us };
        }
    }

    pub fn on_message(&mut self, msg: ControlMessage, now_us: u64) -> Vec<Action> {
        match (&self.state, msg) {
            (_, ControlMessage::Hello { version, .. }) if version != PROTOCOL_VERSION => {
                self.state = ReceiverState::Closed;
                vec![
                    Action::Send(ControlMessage::Error { code: 2, message: format!("unsupported protocol version {version}") }),
                    Action::Fail("version mismatch".into()),
                ]
            }
            (ReceiverState::AwaitingHello, ControlMessage::Hello { .. }) => {
                self.state = ReceiverState::Streaming { params: None };
                vec![Action::Send(self.ack()), Action::Send(ControlMessage::SessionToken(self.token))]
            }
            (ReceiverState::Disconnected { since_us }, ControlMessage::Hello { resume_token, .. }) => {
                let since = *since_us;
                let fresh = now_us.saturating_sub(since) <= TOKEN_TTL_US;
                if resume_token == Some(self.token) && fresh {
                    self.state = ReceiverState::Streaming { params: self.params.clone() };
                    vec![Action::Send(self.ack()), Action::Resumed]
                } else {
                    self.state = ReceiverState::Closed;
                    vec![
                        Action::Send(ControlMessage::Error { code: 1, message: "invalid or expired session token".into() }),
                        Action::Fail("bad resume token".into()),
                    ]
                }
            }
            (ReceiverState::Streaming { .. }, ControlMessage::StartStream(p)) => {
                self.params = Some(p.clone());
                self.state = ReceiverState::Streaming { params: Some(p) };
                vec![]
            }
            (ReceiverState::Streaming { .. }, ControlMessage::SetMode(m)) => {
                if let Some(p) = self.params.as_mut() {
                    p.mode = m;
                }
                vec![]
            }
            (_, ControlMessage::Goodbye { .. }) => {
                self.state = ReceiverState::Closed;
                vec![]
            }
            _ => vec![],
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-proto`
Expected: 27 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(proto): receiver session state machine with token resume"
```

---

### Task 6: Codec traits and pixel conversion

**Files:**
- Create: `crates/castr-media/src/codec.rs`, `crates/castr-media/src/convert.rs`
- Modify: `crates/castr-media/src/lib.rs`

**Interfaces:**
- Consumes: `castr_proto::Mode`.
- Produces:
  ```rust
  // codec.rs
  #[derive(Debug, Clone, Copy, PartialEq, Eq)] pub enum PixelFormat { Bgra, I420, Nv12 }
  /// A raw picture. `data` holds all planes contiguously. For I420: Y (w*h), U (w/2*h/2), V. For NV12: Y, then interleaved UV. For BGRA: `stride` bytes per row.
  #[derive(Debug, Clone)] pub struct RawFrame { pub format: PixelFormat, pub width: u32, pub height: u32, pub stride: u32, pub data: Vec<u8>, pub timestamp_us: u64 }
  #[derive(Debug, Clone)] pub struct EncodedFrame { pub data: Vec<u8>, pub keyframe: bool, pub timestamp_us: u64 }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct EncoderConfig { pub width: u32, pub height: u32, pub fps: u32, pub bitrate_bps: u32, pub mode: Mode }
  pub trait VideoEncoder: Send {
      fn encode(&mut self, frame: &RawFrame) -> anyhow::Result<Option<EncodedFrame>>;
      fn request_keyframe(&mut self);
      fn set_bitrate(&mut self, bitrate_bps: u32) -> anyhow::Result<()>;
      fn set_mode(&mut self, mode: Mode) -> anyhow::Result<()>;
      fn input_format(&self) -> PixelFormat;   // what `encode` expects
      fn name(&self) -> &'static str;
  }
  pub trait VideoDecoder: Send {
      /// Feed one complete access unit (Annex B). May return zero or one frame.
      fn decode(&mut self, data: &[u8], timestamp_us: u64) -> anyhow::Result<Option<RawFrame>>;
      fn name(&self) -> &'static str;
  }
  // convert.rs
  pub fn bgra_to_i420(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8>;
  pub fn bgra_to_nv12(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8>;
  pub fn nv12_to_i420(src: &[u8], width: u32, height: u32) -> Vec<u8>;
  pub fn convert(frame: &RawFrame, to: PixelFormat) -> RawFrame; // identity if same; panics on unsupported pair
  ```
  Widths and heights must be even; `convert` asserts this. BT.601 limited range: Y = 16 + (65.738R + 129.057G + 25.064B)/256; U = 128 + (-37.945R - 74.494G + 112.439B)/256; V = 128 + (112.439R - 94.154G - 18.285B)/256. Chroma is averaged over each 2x2 block.

- [ ] **Step 1: Write the failing tests**

`crates/castr-media/src/convert.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::*;

    fn solid_bgra(w: u32, h: u32, b: u8, g: u8, r: u8) -> Vec<u8> {
        let mut v = Vec::with_capacity((w * h * 4) as usize);
        for _ in 0..w * h { v.extend_from_slice(&[b, g, r, 255]); }
        v
    }

    #[test]
    fn white_maps_to_limited_range_white() {
        let out = bgra_to_i420(&solid_bgra(4, 2, 255, 255, 255), 4, 2, 16);
        assert_eq!(out.len(), 4 * 2 + 2 * 2);
        assert!(out[..8].iter().all(|&y| (234..=236).contains(&y)), "Y={:?}", &out[..8]);
        assert!(out[8..].iter().all(|&c| (127..=129).contains(&c)));
    }

    #[test]
    fn pure_red_has_high_v_low_u() {
        let out = bgra_to_i420(&solid_bgra(2, 2, 0, 0, 255), 2, 2, 8);
        let y = out[0]; let u = out[4]; let v = out[5];
        assert!((78..=84).contains(&y), "y={y}");
        assert!(u < 100, "u={u}");
        assert!(v > 220, "v={v}");
    }

    #[test]
    fn stride_larger_than_width_is_respected() {
        let mut src = solid_bgra(2, 2, 255, 255, 255);
        src.splice(8..8, vec![0u8; 8]);
        src.extend_from_slice(&[0u8; 8]);
        let out = bgra_to_i420(&src, 2, 2, 16);
        assert!(out[..4].iter().all(|&y| y > 230));
    }

    #[test]
    fn nv12_and_i420_agree() {
        let src = solid_bgra(4, 4, 30, 200, 90);
        let i420 = bgra_to_i420(&src, 4, 4, 16);
        let nv12 = bgra_to_nv12(&src, 4, 4, 16);
        assert_eq!(nv12.len(), i420.len());
        assert_eq!(&nv12[..16], &i420[..16]);
        assert_eq!(nv12_to_i420(&nv12, 4, 4), i420);
    }

    #[test]
    fn convert_identity_and_bgra_to_targets() {
        let f = RawFrame { format: PixelFormat::Bgra, width: 2, height: 2, stride: 8, data: solid_bgra(2, 2, 1, 2, 3), timestamp_us: 9 };
        let same = convert(&f, PixelFormat::Bgra);
        assert_eq!(same.data, f.data);
        let i420 = convert(&f, PixelFormat::I420);
        assert_eq!(i420.format, PixelFormat::I420);
        assert_eq!(i420.stride, 2);
        assert_eq!(i420.timestamp_us, 9);
        assert_eq!(i420.data.len(), 6);
        let nv12 = convert(&f, PixelFormat::Nv12);
        assert_eq!(nv12.data.len(), 6);
    }
}
```

`crates/castr-media/src/lib.rs`:
```rust
//! See docs/superpowers/specs/2026-09-01-castr-core-design.md
pub mod codec;
pub mod convert;
pub use codec::*;
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-media`
Expected: compile error.

- [ ] **Step 3: Implement**

`crates/castr-media/src/codec.rs`:
```rust
pub use castr_proto::Mode;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PixelFormat { Bgra, I420, Nv12 }

#[derive(Debug, Clone)]
pub struct RawFrame {
    pub format: PixelFormat,
    pub width: u32,
    pub height: u32,
    pub stride: u32,
    pub data: Vec<u8>,
    pub timestamp_us: u64,
}

#[derive(Debug, Clone)]
pub struct EncodedFrame {
    pub data: Vec<u8>,
    pub keyframe: bool,
    pub timestamp_us: u64,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct EncoderConfig {
    pub width: u32,
    pub height: u32,
    pub fps: u32,
    pub bitrate_bps: u32,
    pub mode: Mode,
}

pub trait VideoEncoder: Send {
    fn encode(&mut self, frame: &RawFrame) -> anyhow::Result<Option<EncodedFrame>>;
    fn request_keyframe(&mut self);
    fn set_bitrate(&mut self, bitrate_bps: u32) -> anyhow::Result<()>;
    fn set_mode(&mut self, mode: Mode) -> anyhow::Result<()>;
    fn input_format(&self) -> PixelFormat;
    fn name(&self) -> &'static str;
}

pub trait VideoDecoder: Send {
    fn decode(&mut self, data: &[u8], timestamp_us: u64) -> anyhow::Result<Option<RawFrame>>;
    fn name(&self) -> &'static str;
}
```

Top of `crates/castr-media/src/convert.rs`:
```rust
use crate::codec::{PixelFormat, RawFrame};

#[inline]
fn rgb_to_y(r: i32, g: i32, b: i32) -> u8 {
    (16 + ((66 * r + 129 * g + 25 * b + 128) >> 8)).clamp(16, 235) as u8
}
#[inline]
fn rgb_to_u(r: i32, g: i32, b: i32) -> u8 {
    (128 + ((-38 * r - 74 * g + 112 * b + 128) >> 8)).clamp(16, 240) as u8
}
#[inline]
fn rgb_to_v(r: i32, g: i32, b: i32) -> u8 {
    (128 + ((112 * r - 94 * g - 18 * b + 128) >> 8)).clamp(16, 240) as u8
}

/// Returns (Y plane, U plane, V plane) at 4:2:0.
fn bgra_planes(src: &[u8], width: u32, height: u32, stride: u32) -> (Vec<u8>, Vec<u8>, Vec<u8>) {
    assert!(width % 2 == 0 && height % 2 == 0, "dimensions must be even");
    let (w, h, s) = (width as usize, height as usize, stride as usize);
    let mut y = vec![0u8; w * h];
    let mut u = vec![0u8; w * h / 4];
    let mut v = vec![0u8; w * h / 4];
    for row in 0..h {
        for col in 0..w {
            let p = row * s + col * 4;
            let (b, g, r) = (src[p] as i32, src[p + 1] as i32, src[p + 2] as i32);
            y[row * w + col] = rgb_to_y(r, g, b);
        }
    }
    for row in (0..h).step_by(2) {
        for col in (0..w).step_by(2) {
            let (mut rs, mut gs, mut bs) = (0, 0, 0);
            for (dr, dc) in [(0, 0), (0, 1), (1, 0), (1, 1)] {
                let p = (row + dr) * s + (col + dc) * 4;
                bs += src[p] as i32;
                gs += src[p + 1] as i32;
                rs += src[p + 2] as i32;
            }
            let (r, g, b) = (rs / 4, gs / 4, bs / 4);
            let ci = (row / 2) * (w / 2) + col / 2;
            u[ci] = rgb_to_u(r, g, b);
            v[ci] = rgb_to_v(r, g, b);
        }
    }
    (y, u, v)
}

pub fn bgra_to_i420(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let (mut y, u, v) = bgra_planes(src, width, height, stride);
    y.extend_from_slice(&u);
    y.extend_from_slice(&v);
    y
}

pub fn bgra_to_nv12(src: &[u8], width: u32, height: u32, stride: u32) -> Vec<u8> {
    let (mut y, u, v) = bgra_planes(src, width, height, stride);
    for (a, b) in u.iter().zip(v.iter()) {
        y.push(*a);
        y.push(*b);
    }
    y
}

pub fn nv12_to_i420(src: &[u8], width: u32, height: u32) -> Vec<u8> {
    let (w, h) = (width as usize, height as usize);
    let mut out = Vec::with_capacity(w * h * 3 / 2);
    out.extend_from_slice(&src[..w * h]);
    let uv = &src[w * h..];
    out.extend(uv.iter().step_by(2));
    out.extend(uv.iter().skip(1).step_by(2));
    out
}

pub fn convert(frame: &RawFrame, to: PixelFormat) -> RawFrame {
    if frame.format == to {
        return frame.clone();
    }
    let data = match (frame.format, to) {
        (PixelFormat::Bgra, PixelFormat::I420) => bgra_to_i420(&frame.data, frame.width, frame.height, frame.stride),
        (PixelFormat::Bgra, PixelFormat::Nv12) => bgra_to_nv12(&frame.data, frame.width, frame.height, frame.stride),
        (PixelFormat::Nv12, PixelFormat::I420) => nv12_to_i420(&frame.data, frame.width, frame.height),
        (from, to) => panic!("unsupported conversion {from:?} -> {to:?}"),
    };
    RawFrame { format: to, width: frame.width, height: frame.height, stride: frame.width, data, timestamp_us: frame.timestamp_us }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-media`
Expected: 5 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(media): codec traits and BGRA to I420/NV12 conversion"
```

---

### Task 7: openh264 software backend

**Files:**
- Create: `crates/castr-media/src/sw.rs`
- Modify: `crates/castr-media/src/lib.rs`

**Interfaces:**
- Consumes: traits from Task 6.
- Produces:
  ```rust
  pub struct SwEncoder { /* private */ }
  impl SwEncoder { pub fn new(cfg: EncoderConfig) -> anyhow::Result<Self>; }
  impl VideoEncoder for SwEncoder { /* input_format = I420, name = "openh264" */ }
  pub struct SwDecoder { /* private */ }
  impl SwDecoder { pub fn new() -> anyhow::Result<Self>; }
  impl VideoDecoder for SwDecoder { /* outputs I420 RawFrame, name = "openh264" */ }
  ```
  `set_bitrate` and `set_mode` recreate the encoder with the new config and force a keyframe on the next `encode`. This is acceptable for the fallback backend.
  Note on the `openh264` crate: this plan targets 0.6. If a builder method name differs in the resolved version, check `https://docs.rs/openh264/0.6` for the equivalent and keep behavior the same.

- [ ] **Step 1: Write the failing tests**

`crates/castr-media/src/sw.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::codec::*;

    fn cfg() -> EncoderConfig {
        EncoderConfig { width: 320, height: 240, fps: 30, bitrate_bps: 800_000, mode: Mode::Game }
    }

    fn gradient_frame(i: u32) -> RawFrame {
        let (w, h) = (320u32, 240u32);
        let mut data = Vec::with_capacity((w * h * 4) as usize);
        for y in 0..h {
            for x in 0..w {
                data.extend_from_slice(&[((x + i * 3) % 256) as u8, (y % 256) as u8, ((x + y) % 256) as u8, 255]);
            }
        }
        crate::convert::convert(&RawFrame { format: PixelFormat::Bgra, width: w, height: h, stride: w * 4, data, timestamp_us: i as u64 * 33_333 }, PixelFormat::I420)
    }

    #[test]
    fn first_frame_is_keyframe_and_round_trips() {
        let mut enc = SwEncoder::new(cfg()).unwrap();
        let mut dec = SwDecoder::new().unwrap();
        assert_eq!(enc.input_format(), PixelFormat::I420);
        let f0 = gradient_frame(0);
        let e0 = enc.encode(&f0).unwrap().expect("encoder must emit first frame");
        assert!(e0.keyframe);
        assert_eq!(e0.timestamp_us, 0);
        assert!(e0.data.starts_with(&[0, 0, 0, 1]) || e0.data.starts_with(&[0, 0, 1]), "Annex B start code");
        let d0 = dec.decode(&e0.data, e0.timestamp_us).unwrap().expect("decoder must output first frame");
        assert_eq!((d0.width, d0.height, d0.format), (320, 240, PixelFormat::I420));
        assert_eq!(d0.data.len(), 320 * 240 * 3 / 2);
        assert_eq!(d0.timestamp_us, 0);
        let mean_diff: f64 = d0.data[..320 * 240].iter().zip(&f0.data[..320 * 240])
            .map(|(a, b)| (*a as f64 - *b as f64).abs()).sum::<f64>() / (320.0 * 240.0);
        assert!(mean_diff < 8.0, "luma mean abs diff {mean_diff}");
    }

    #[test]
    fn delta_frames_follow_and_keyframe_request_is_honored() {
        let mut enc = SwEncoder::new(cfg()).unwrap();
        let mut dec = SwDecoder::new().unwrap();
        let mut keyframes = 0;
        for i in 0..10 {
            if i == 5 { enc.request_keyframe(); }
            let e = enc.encode(&gradient_frame(i)).unwrap().unwrap();
            if e.keyframe { keyframes += 1; }
            assert!(dec.decode(&e.data, e.timestamp_us).unwrap().is_some());
        }
        assert!(keyframes >= 2, "expected initial + requested keyframe, got {keyframes}");
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
```

Add `pub mod sw;` to `lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-media sw`
Expected: compile error.

- [ ] **Step 3: Implement**

Top of `crates/castr-media/src/sw.rs`:
```rust
use crate::codec::*;
use anyhow::Context;
use openh264::decoder::Decoder;
use openh264::encoder::{BitRate, Encoder, EncoderConfig as OhConfig, FrameRate, IntraFramePeriod, Profile, RateControlMode, UsageType};
use openh264::formats::YUVSource;
use openh264::OpenH264API;

struct I420Source<'a> { w: usize, h: usize, data: &'a [u8] }

impl YUVSource for I420Source<'_> {
    fn dimensions(&self) -> (usize, usize) { (self.w, self.h) }
    fn strides(&self) -> (usize, usize, usize) { (self.w, self.w / 2, self.w / 2) }
    fn y(&self) -> &[u8] { &self.data[..self.w * self.h] }
    fn u(&self) -> &[u8] { let n = self.w * self.h; &self.data[n..n + n / 4] }
    fn v(&self) -> &[u8] { let n = self.w * self.h; &self.data[n + n / 4..n + n / 2] }
}

pub struct SwEncoder {
    cfg: EncoderConfig,
    inner: Encoder,
    force_key: bool,
}

impl SwEncoder {
    pub fn new(cfg: EncoderConfig) -> anyhow::Result<Self> {
        Ok(Self { inner: Self::build(&cfg)?, cfg, force_key: true })
    }

    fn build(cfg: &EncoderConfig) -> anyhow::Result<Encoder> {
        let gop = match cfg.mode { Mode::Game => cfg.fps * 10, Mode::Quality => cfg.fps * 2 };
        let oh = OhConfig::new()
            .usage_type(UsageType::ScreenContentRealTime)
            .profile(Profile::Baseline)
            .rate_control_mode(RateControlMode::Bitrate)
            .bitrate(BitRate::from_bps(cfg.bitrate_bps))
            .max_frame_rate(FrameRate::from_hz(cfg.fps as f32))
            .intra_frame_period(IntraFramePeriod::from_num_frames(gop))
            .enable_skip_frame(false);
        Encoder::with_api_config(OpenH264API::from_source(), oh).context("openh264 encoder init")
    }
}

impl VideoEncoder for SwEncoder {
    fn encode(&mut self, frame: &RawFrame) -> anyhow::Result<Option<EncodedFrame>> {
        anyhow::ensure!(frame.format == PixelFormat::I420, "SwEncoder expects I420");
        anyhow::ensure!(frame.width == self.cfg.width && frame.height == self.cfg.height, "frame size mismatch");
        if self.force_key {
            self.inner.force_intra_frame();
            self.force_key = false;
        }
        let src = I420Source { w: frame.width as usize, h: frame.height as usize, data: &frame.data };
        let bs = self.inner.encode(&src).context("openh264 encode")?;
        let data = bs.to_vec();
        if data.is_empty() {
            return Ok(None);
        }
        let keyframe = matches!(bs.frame_type(), openh264::encoder::FrameType::IDR | openh264::encoder::FrameType::I);
        Ok(Some(EncodedFrame { data, keyframe, timestamp_us: frame.timestamp_us }))
    }

    fn request_keyframe(&mut self) { self.force_key = true; }

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

    fn input_format(&self) -> PixelFormat { PixelFormat::I420 }
    fn name(&self) -> &'static str { "openh264" }
}

pub struct SwDecoder { inner: Decoder }

impl SwDecoder {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { inner: Decoder::new().context("openh264 decoder init")? })
    }
}

impl VideoDecoder for SwDecoder {
    fn decode(&mut self, data: &[u8], timestamp_us: u64) -> anyhow::Result<Option<RawFrame>> {
        let Some(yuv) = self.inner.decode(data).context("openh264 decode")? else { return Ok(None); };
        let (w, h) = yuv.dimensions();
        let (sy, su, sv) = yuv.strides();
        let mut out = Vec::with_capacity(w * h * 3 / 2);
        for row in 0..h { out.extend_from_slice(&yuv.y()[row * sy..row * sy + w]); }
        for row in 0..h / 2 { out.extend_from_slice(&yuv.u()[row * su..row * su + w / 2]); }
        for row in 0..h / 2 { out.extend_from_slice(&yuv.v()[row * sv..row * sv + w / 2]); }
        Ok(Some(RawFrame { format: PixelFormat::I420, width: w as u32, height: h as u32, stride: w as u32, data: out, timestamp_us }))
    }
    fn name(&self) -> &'static str { "openh264" }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-media`
Expected: 9 passed. If `first_frame_is_keyframe_and_round_trips` fails because the decoder returns `None` on the first call, openh264 is holding the frame for reordering; call `self.inner.flush_remaining()` after `decode` returns `None` and return the first frame it yields. Keep the test unchanged.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(media): openh264 software encoder and decoder backend"
```

---

### Task 8: Opus audio wrappers

**Files:**
- Create: `crates/castr-media/src/audio.rs`
- Modify: `crates/castr-media/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub const SAMPLE_RATE: u32 = 48_000;
  pub const CHANNELS: usize = 2;
  pub const FRAME_SAMPLES: usize = 480;              // 10 ms per channel
  pub const FRAME_INTERLEAVED: usize = FRAME_SAMPLES * CHANNELS;
  pub struct AudioEncoder { /* private */ }
  impl AudioEncoder {
      pub fn new() -> anyhow::Result<Self>;             // 128 kbps, LowDelay
      /// pcm must be exactly FRAME_INTERLEAVED i16 samples. Returns one Opus packet.
      pub fn encode(&mut self, pcm: &[i16]) -> anyhow::Result<Vec<u8>>;
  }
  pub struct AudioDecoder { /* private */ }
  impl AudioDecoder {
      pub fn new() -> anyhow::Result<Self>;
      /// Returns FRAME_INTERLEAVED i16 samples. `None` packet performs packet-loss concealment.
      pub fn decode(&mut self, packet: Option<&[u8]>) -> anyhow::Result<Vec<i16>>;
  }
  /// Accumulates arbitrary-length interleaved i16 input and yields exact 10 ms frames.
  pub struct FrameChunker { buf: Vec<i16> }
  impl FrameChunker { pub fn new() -> Self; pub fn push(&mut self, samples: &[i16]); pub fn next_frame(&mut self) -> Option<Vec<i16>>; }
  ```

- [ ] **Step 1: Write the failing tests**

`crates/castr-media/src/audio.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    fn sine(frame_idx: usize) -> Vec<i16> {
        (0..FRAME_SAMPLES).flat_map(|i| {
            let t = (frame_idx * FRAME_SAMPLES + i) as f32 / SAMPLE_RATE as f32;
            let s = ((t * 440.0 * std::f32::consts::TAU).sin() * 8000.0) as i16;
            [s, s]
        }).collect()
    }

    #[test]
    fn encode_decode_preserves_frame_size_and_is_small() {
        let mut enc = AudioEncoder::new().unwrap();
        let mut dec = AudioDecoder::new().unwrap();
        for i in 0..20 {
            let pkt = enc.encode(&sine(i)).unwrap();
            assert!(pkt.len() < 400, "packet {} bytes, expected < 400 at 128 kbps/10 ms", pkt.len());
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
            last_in = pcm; last_out = out;
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
```

Add `pub mod audio;` to `lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-media audio`
Expected: compile error.

- [ ] **Step 3: Implement**

Top of `crates/castr-media/src/audio.rs`:
```rust
use anyhow::Context;
use audiopus::{coder, Application, Bitrate, Channels, SampleRate};

pub const SAMPLE_RATE: u32 = 48_000;
pub const CHANNELS: usize = 2;
pub const FRAME_SAMPLES: usize = 480;
pub const FRAME_INTERLEAVED: usize = FRAME_SAMPLES * CHANNELS;
const MAX_PACKET: usize = 1500;

pub struct AudioEncoder { inner: coder::Encoder }

impl AudioEncoder {
    pub fn new() -> anyhow::Result<Self> {
        let mut inner = coder::Encoder::new(SampleRate::Hz48000, Channels::Stereo, Application::LowDelay)
            .context("opus encoder")?;
        inner.set_bitrate(Bitrate::BitsPerSecond(128_000)).context("opus bitrate")?;
        Ok(Self { inner })
    }

    pub fn encode(&mut self, pcm: &[i16]) -> anyhow::Result<Vec<u8>> {
        anyhow::ensure!(pcm.len() == FRAME_INTERLEAVED, "expected {FRAME_INTERLEAVED} samples, got {}", pcm.len());
        let mut out = vec![0u8; MAX_PACKET];
        let n = self.inner.encode(pcm, &mut out).context("opus encode")?;
        out.truncate(n);
        Ok(out)
    }
}

pub struct AudioDecoder { inner: coder::Decoder }

impl AudioDecoder {
    pub fn new() -> anyhow::Result<Self> {
        Ok(Self { inner: coder::Decoder::new(SampleRate::Hz48000, Channels::Stereo).context("opus decoder")? })
    }

    pub fn decode(&mut self, packet: Option<&[u8]>) -> anyhow::Result<Vec<i16>> {
        let mut out = vec![0i16; FRAME_INTERLEAVED];
        let n = self.inner.decode(packet, &mut out, false).context("opus decode")?;
        out.truncate(n * CHANNELS);
        Ok(out)
    }
}

#[derive(Default)]
pub struct FrameChunker { buf: Vec<i16> }

impl FrameChunker {
    pub fn new() -> Self { Self::default() }
    pub fn push(&mut self, samples: &[i16]) { self.buf.extend_from_slice(samples); }
    pub fn next_frame(&mut self) -> Option<Vec<i16>> {
        if self.buf.len() < FRAME_INTERLEAVED {
            return None;
        }
        let rest = self.buf.split_off(FRAME_INTERLEAVED);
        Some(std::mem::replace(&mut self.buf, rest))
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-media`
Expected: 14 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(media): Opus encoder/decoder wrappers and 10 ms chunker"
```

---

### Task 9: Jitter buffer

**Files:**
- Create: `crates/castr-media/src/jitter.rs`
- Modify: `crates/castr-media/src/lib.rs`

**Interfaces:**
- Consumes: `castr_proto::{CompleteFrame, Mode}`.
- Produces:
  ```rust
  pub struct JitterBuffer { /* private */ }
  impl JitterBuffer {
      pub fn new(mode: Mode, frame_interval_us: u64) -> Self;
      pub fn set_mode(&mut self, mode: Mode);      // flushes
      /// `now_us` is receiver monotonic time at arrival.
      pub fn push(&mut self, frame: CompleteFrame, now_us: u64);
      /// Returns the next frame to decode, or None.
      pub fn pop(&mut self, now_us: u64) -> Option<CompleteFrame>;
      pub fn depth(&self) -> usize;
      pub fn dropped(&mut self) -> u32;             // frames discarded since last read
      pub fn flush(&mut self);
  }
  ```
  Rules from spec 7.2 and 8.2:
  - Frames are ordered by frame number (wrapping). Frames older than or equal to the last popped frame are discarded on push.
  - Keyframe rule (both modes): when skipping ahead over several candidate frames, if any candidate is a keyframe, return the newest keyframe among them instead of the newest frame, and keep the frames after it in the buffer. Skipping past an undecoded keyframe would break the decoder's reference chain.
  - Game: `pop` returns the newest buffered frame immediately (subject to the keyframe rule), discarding the older ones it skipped.
  - Quality: the first push sets `base = arrival_now - timestamp + 150ms`. A frame is due when `now >= timestamp + base`, so playback starts 150 ms after the first frame arrives. `pop` returns the oldest due frame. If the oldest frame is more than one frame interval past due and a newer frame is also due, the buffer skips to the newest due frame (subject to the keyframe rule), dropping what it skipped.

- [ ] **Step 1: Write the failing tests**

`crates/castr-media/src/jitter.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use castr_proto::STREAM_VIDEO;

    fn f(n: u32, ts: u64, key: bool) -> CompleteFrame {
        CompleteFrame { stream: STREAM_VIDEO, frame_number: n, timestamp_us: ts, keyframe: key, data: vec![n as u8] }
    }
    const IV: u64 = 33_333;

    #[test]
    fn game_returns_newest_and_drops_older_deltas() {
        let mut j = JitterBuffer::new(Mode::Game, IV);
        j.push(f(1, 0, false), 0);
        j.push(f(2, IV, false), 0);
        j.push(f(3, 2 * IV, false), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 3);
        assert_eq!(j.depth(), 0);
        assert_eq!(j.dropped(), 2);
        assert!(j.pop(0).is_none());
    }

    #[test]
    fn game_returns_keyframe_first_then_newest_delta() {
        let mut j = JitterBuffer::new(Mode::Game, IV);
        j.push(f(1, 0, false), 0);
        j.push(f(2, IV, true), 0);
        j.push(f(3, 2 * IV, false), 0);
        j.push(f(4, 3 * IV, false), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 2);
        assert_eq!(j.dropped(), 1);
        assert_eq!(j.depth(), 2);
        assert_eq!(j.pop(0).unwrap().frame_number, 4);
        assert_eq!(j.dropped(), 1);
        assert!(j.pop(0).is_none());
    }

    #[test]
    fn game_discards_frames_older_than_last_popped() {
        let mut j = JitterBuffer::new(Mode::Game, IV);
        j.push(f(5, 0, true), 0);
        j.pop(0);
        j.push(f(4, 0, false), 0);
        j.push(f(5, 0, false), 0);
        assert_eq!(j.depth(), 0);
        assert_eq!(j.dropped(), 2);
    }

    #[test]
    fn quality_holds_frames_until_150ms_after_first_arrival() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 1_000_000, true), 0);
        j.push(f(2, 1_000_000 + IV, false), 0);
        assert!(j.pop(149_999).is_none());
        assert_eq!(j.pop(150_000).unwrap().frame_number, 1);
        assert!(j.pop(150_000 + IV - 1).is_none());
        assert_eq!(j.pop(150_000 + IV).unwrap().frame_number, 2);
    }

    #[test]
    fn quality_skips_frame_more_than_one_interval_late_when_newer_is_due() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 0, true), 0);
        assert_eq!(j.pop(150_000).unwrap().frame_number, 1);
        j.push(f(2, IV, false), 150_000 + IV);
        j.push(f(3, 2 * IV, false), 150_000 + 2 * IV);
        let r = j.pop(150_000 + 3 * IV + 1).unwrap();
        assert_eq!(r.frame_number, 3);
        assert_eq!(j.dropped(), 1);
    }

    #[test]
    fn quality_keeps_late_keyframe() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 0, true), 0);
        j.pop(150_000);
        j.push(f(2, IV, true), 150_000 + IV);
        j.push(f(3, 2 * IV, false), 150_000 + 2 * IV);
        let now = 150_000 + 3 * IV + 1;
        assert_eq!(j.pop(now).unwrap().frame_number, 2);
        assert_eq!(j.dropped(), 0);
        assert_eq!(j.pop(now).unwrap().frame_number, 3);
    }

    #[test]
    fn quality_one_interval_late_is_not_skipped() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 0, true), 0);
        j.pop(150_000);
        j.push(f(2, IV, false), 150_000 + IV);
        j.push(f(3, 2 * IV, false), 150_000 + 2 * IV);
        assert_eq!(j.pop(150_000 + 2 * IV).unwrap().frame_number, 2);
        assert_eq!(j.dropped(), 0);
    }

    #[test]
    fn set_mode_flushes_and_resets_base() {
        let mut j = JitterBuffer::new(Mode::Quality, IV);
        j.push(f(1, 0, true), 0);
        j.set_mode(Mode::Game);
        assert_eq!(j.depth(), 0);
        j.push(f(2, 0, true), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 2);
    }

    #[test]
    fn wrapping_order_is_respected() {
        let mut j = JitterBuffer::new(Mode::Game, IV);
        j.push(f(u32::MAX, 0, false), 0);
        j.push(f(0, IV, false), 0);
        assert_eq!(j.pop(0).unwrap().frame_number, 0);
        assert_eq!(j.dropped(), 1);
    }
}
```

Add `pub mod jitter;` to `lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-media jitter`
Expected: compile error.

- [ ] **Step 3: Implement**

Top of `crates/castr-media/src/jitter.rs`:
```rust
use castr_proto::{frame_newer_or_eq, CompleteFrame, Mode};
use std::collections::VecDeque;

const QUALITY_DELAY_US: u64 = 150_000;

pub struct JitterBuffer {
    mode: Mode,
    interval_us: u64,
    frames: VecDeque<CompleteFrame>,
    last_popped: Option<u32>,
    base_us: Option<i64>,
    dropped: u32,
}

impl JitterBuffer {
    pub fn new(mode: Mode, frame_interval_us: u64) -> Self {
        Self { mode, interval_us: frame_interval_us, frames: VecDeque::new(), last_popped: None, base_us: None, dropped: 0 }
    }

    pub fn set_mode(&mut self, mode: Mode) { self.mode = mode; self.flush(); }
    pub fn flush(&mut self) { self.frames.clear(); self.base_us = None; }
    pub fn depth(&self) -> usize { self.frames.len() }
    pub fn dropped(&mut self) -> u32 { std::mem::take(&mut self.dropped) }

    pub fn push(&mut self, frame: CompleteFrame, now_us: u64) {
        if let Some(last) = self.last_popped {
            if !frame_newer_or_eq(frame.frame_number, last) || frame.frame_number == last {
                self.dropped += 1;
                return;
            }
        }
        if self.base_us.is_none() {
            self.base_us = Some(now_us as i64 - frame.timestamp_us as i64 + QUALITY_DELAY_US as i64);
        }
        let pos = self.frames.iter().position(|f| frame_newer_or_eq(f.frame_number, frame.frame_number));
        match pos {
            Some(i) if self.frames[i].frame_number == frame.frame_number => {}
            Some(i) => self.frames.insert(i, frame),
            None => self.frames.push_back(frame),
        }
    }

    /// frames[0..=idx] are candidates. Return the newest keyframe among them if any, else idx.
    fn choose(&self, idx: usize) -> usize {
        (0..=idx).rev().find(|&i| self.frames[i].keyframe).unwrap_or(idx)
    }

    /// Pop frames[idx], dropping everything before it.
    fn take(&mut self, idx: usize) -> CompleteFrame {
        for _ in 0..idx {
            self.frames.pop_front();
            self.dropped += 1;
        }
        let f = self.frames.pop_front().unwrap();
        self.last_popped = Some(f.frame_number);
        f
    }

    pub fn pop(&mut self, now_us: u64) -> Option<CompleteFrame> {
        if self.frames.is_empty() {
            return None;
        }
        match self.mode {
            Mode::Game => {
                let idx = self.choose(self.frames.len() - 1);
                Some(self.take(idx))
            }
            Mode::Quality => {
                let base = self.base_us.expect("base set on push");
                let due_at = |f: &CompleteFrame| f.timestamp_us as i64 + base;
                if due_at(&self.frames[0]) > now_us as i64 {
                    return None;
                }
                let mut last_due = 0;
                while last_due + 1 < self.frames.len() && due_at(&self.frames[last_due + 1]) <= now_us as i64 {
                    last_due += 1;
                }
                let lateness = now_us as i64 - due_at(&self.frames[0]);
                let idx = if last_due > 0 && lateness > self.interval_us as i64 { self.choose(last_due) } else { 0 };
                Some(self.take(idx))
            }
        }
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-media`
Expected: 23 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(media): jitter buffer with game and quality mode rules"
```

---

### Task 10: A/V clock

**Files:**
- Create: `crates/castr-media/src/clock.rs`
- Modify: `crates/castr-media/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  /// Audio-master presentation clock. The receiver reports how much audio has been played; video asks whether a frame is due.
  pub struct AvClock { /* private */ }
  impl AvClock {
      pub fn new() -> Self;
      /// Called when audio for sender timestamp `ts_us` begins playing at receiver time `now_us`.
      pub fn audio_played(&mut self, ts_us: u64, now_us: u64);
      /// True when no audio update has arrived for 200 ms (receiver time).
      pub fn audio_stale(&self, now_us: u64) -> bool;
      /// Estimated sender timestamp that is being presented right now. None until the first audio_played or video fallback.
      pub fn presented_ts(&self, now_us: u64) -> Option<u64>;
      /// With audio: due when presented_ts >= frame ts. Without audio (stale or none): due when now >= ts + offset, offset learned from the first call.
      pub fn video_due(&mut self, frame_ts_us: u64, now_us: u64) -> bool;
      /// Ratio to resample audio playback by, in [0.995, 1.005]. >1 means play faster (buffer growing).
      pub fn drift_ratio(&self, buffered_audio_us: u64, target_us: u64) -> f64;
  }
  ```

- [ ] **Step 1: Write the failing tests**

`crates/castr-media/src/clock.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn video_follows_audio_when_audio_flowing() {
        let mut c = AvClock::new();
        c.audio_played(1_000_000, 50_000);
        assert_eq!(c.presented_ts(50_000), Some(1_000_000));
        assert_eq!(c.presented_ts(60_000), Some(1_010_000));
        assert!(!c.video_due(1_020_000, 60_000));
        assert!(c.video_due(1_020_000, 70_000));
    }

    #[test]
    fn audio_stale_after_200ms() {
        let mut c = AvClock::new();
        c.audio_played(0, 0);
        assert!(!c.audio_stale(199_999));
        assert!(c.audio_stale(200_000));
        assert!(AvClock::new().audio_stale(0));
    }

    #[test]
    fn video_without_audio_uses_learned_offset() {
        let mut c = AvClock::new();
        assert!(c.video_due(5_000_000, 100));
        assert!(!c.video_due(5_010_000, 5_000));
        assert!(c.video_due(5_010_000, 10_100));
    }

    #[test]
    fn stale_audio_falls_back_to_last_known_delta() {
        let mut c = AvClock::new();
        c.audio_played(1_000_000, 50_000);
        assert!(c.video_due(1_250_000, 300_000));
        assert!(!c.video_due(1_300_000, 300_000));
        assert!(c.video_due(1_300_000, 350_000));
    }

    #[test]
    fn drift_ratio_is_bounded_and_directional() {
        let c = AvClock::new();
        assert_eq!(c.drift_ratio(40_000, 40_000), 1.0);
        let fast = c.drift_ratio(200_000, 40_000);
        let slow = c.drift_ratio(0, 40_000);
        assert!(fast > 1.0 && fast <= 1.005);
        assert!(slow < 1.0 && slow >= 0.995);
    }
}
```

Add `pub mod clock;` to `lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-media clock`
Expected: compile error.

- [ ] **Step 3: Implement**

Top of `crates/castr-media/src/clock.rs`:
```rust
const STALE_US: u64 = 200_000;

#[derive(Default)]
pub struct AvClock {
    /// (sender ts, receiver now) at the last audio update.
    audio_anchor: Option<(u64, u64)>,
    /// receiver_now - sender_ts learned when video runs without audio.
    video_offset: Option<i64>,
}

impl AvClock {
    pub fn new() -> Self { Self::default() }

    pub fn audio_played(&mut self, ts_us: u64, now_us: u64) {
        self.audio_anchor = Some((ts_us, now_us));
        self.video_offset = Some(now_us as i64 - ts_us as i64);
    }

    pub fn audio_stale(&self, now_us: u64) -> bool {
        match self.audio_anchor {
            Some((_, at)) => now_us.saturating_sub(at) >= STALE_US,
            None => true,
        }
    }

    pub fn presented_ts(&self, now_us: u64) -> Option<u64> {
        match (self.audio_anchor, self.video_offset) {
            (Some((ts, at)), _) if !self.audio_stale(now_us) => Some(ts + now_us.saturating_sub(at)),
            (_, Some(off)) => Some((now_us as i64 - off).max(0) as u64),
            _ => None,
        }
    }

    pub fn video_due(&mut self, frame_ts_us: u64, now_us: u64) -> bool {
        if self.video_offset.is_none() {
            self.video_offset = Some(now_us as i64 - frame_ts_us as i64);
            return true;
        }
        self.presented_ts(now_us).map(|p| p >= frame_ts_us).unwrap_or(true)
    }

    pub fn drift_ratio(&self, buffered_audio_us: u64, target_us: u64) -> f64 {
        let err = buffered_audio_us as f64 - target_us as f64;
        (1.0 + err / 40_000_000.0).clamp(0.995, 1.005)
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-media`
Expected: 28 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(media): audio-master A/V clock"
```

---

### Task 11: Bitrate controller

**Files:**
- Create: `crates/castr-media/src/bitrate.rs`
- Modify: `crates/castr-media/src/lib.rs`

**Interfaces:**
- Consumes: `castr_proto::{Stats, Mode}`.
- Produces:
  ```rust
  pub const MIN_BITRATE: u32 = 1_000_000;
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct Resolution { pub width: u32, pub height: u32 }
  #[derive(Debug, Clone, Copy, PartialEq, Eq)]
  pub struct Decision { pub bitrate_bps: u32, pub resolution: Resolution }
  pub struct BitrateController { /* private */ }
  impl BitrateController {
      pub fn new(ceiling_bps: u32, initial_bps: u32, native: Resolution, mode: Mode) -> Self;
      pub fn set_mode(&mut self, mode: Mode);
      pub fn current(&self) -> Decision;
      /// Feed a receiver Stats report at receiver-relative time `now_us`. Returns Some(decision) when anything changed.
      pub fn on_stats(&mut self, stats: &Stats, now_us: u64) -> Option<Decision>;
  }
  ```
  Rules (spec 8.1): loss ratio = fragments_lost / (fragments_lost + fragments_received), 0 if both zero. Loss > 2%: ×0.7, at most once per 500 ms. Queue depth > 3: ×0.85 (same 500 ms guard). Clean interval means loss < 0.5% and queue ≤ 1; after 1 s of consecutive clean intervals, add 5% of ceiling and reset the clean timer. Clamp to [MIN_BITRATE, ceiling]. Resolution ladder: native, 1280×720, 960×540 (skip rungs not smaller than native). Game mode only: after bitrate has sat at MIN_BITRATE for 2 s, step down one rung; after 5 s of consecutive clean intervals step up one rung. Quality mode never changes resolution and resets to native on `set_mode(Quality)`.

- [ ] **Step 1: Write the failing tests**

`crates/castr-media/src/bitrate.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    const NATIVE: Resolution = Resolution { width: 1920, height: 1080 };
    fn st(lost: u32, received: u32, queue: u32) -> Stats {
        Stats { frames_received: 3, frames_dropped: 0, fragments_lost: lost, fragments_received: received, decode_queue_depth: queue, interval_ms: 100 }
    }
    fn ctl() -> BitrateController { BitrateController::new(10_000_000, 5_000_000, NATIVE, Mode::Game) }

    #[test]
    fn loss_over_two_percent_cuts_30_percent_once_per_500ms() {
        let mut c = ctl();
        let d = c.on_stats(&st(3, 97, 0), 0).unwrap();
        assert_eq!(d.bitrate_bps, 3_500_000);
        assert!(c.on_stats(&st(3, 97, 0), 100_000).is_none());
        assert!(c.on_stats(&st(3, 97, 0), 499_999).is_none());
        assert_eq!(c.on_stats(&st(3, 97, 0), 500_000).unwrap().bitrate_bps, 2_450_000);
    }

    #[test]
    fn loss_at_two_percent_is_not_a_cut() {
        let mut c = ctl();
        assert!(c.on_stats(&st(2, 98, 0), 0).is_none());
    }

    #[test]
    fn deep_decode_queue_cuts_15_percent() {
        let mut c = ctl();
        assert_eq!(c.on_stats(&st(0, 100, 4), 0).unwrap().bitrate_bps, 4_250_000);
        assert!(c.on_stats(&st(0, 100, 3), 600_000).is_none());
    }

    #[test]
    fn one_second_clean_adds_5_percent_of_ceiling() {
        let mut c = ctl();
        for i in 0..10 {
            assert!(c.on_stats(&st(0, 100, 0), i * 100_000).is_none(), "tick {i}");
        }
        assert_eq!(c.on_stats(&st(0, 100, 1), 1_000_000).unwrap().bitrate_bps, 5_500_000);
        assert!(c.on_stats(&st(0, 100, 0), 1_100_000).is_none());
    }

    #[test]
    fn dirty_interval_resets_clean_timer() {
        let mut c = ctl();
        for i in 0..9 { c.on_stats(&st(0, 100, 0), i * 100_000); }
        c.on_stats(&st(1, 99, 0), 900_000);
        assert!(c.on_stats(&st(0, 100, 0), 1_000_000).is_none());
    }

    #[test]
    fn clamps_to_floor_and_ceiling() {
        let mut c = BitrateController::new(10_000_000, 1_200_000, NATIVE, Mode::Quality);
        assert_eq!(c.on_stats(&st(50, 50, 0), 0).unwrap().bitrate_bps, MIN_BITRATE);
        let mut c = BitrateController::new(10_000_000, 9_800_000, NATIVE, Mode::Quality);
        let mut t = 0;
        let mut last = None;
        for _ in 0..=10 { last = c.on_stats(&st(0, 100, 0), t).or(last); t += 100_000; }
        assert_eq!(last.unwrap().bitrate_bps, 10_000_000);
    }

    #[test]
    fn game_mode_steps_resolution_down_after_2s_at_floor_and_up_after_5s_clean() {
        let mut c = BitrateController::new(10_000_000, MIN_BITRATE, NATIVE, Mode::Game);
        let mut t = 0;
        let mut last = c.current();
        for _ in 0..21 {
            if let Some(d) = c.on_stats(&st(5, 95, 0), t) { last = d; }
            t += 100_000;
        }
        assert_eq!(last.resolution, Resolution { width: 1280, height: 720 });
        assert_eq!(last.bitrate_bps, MIN_BITRATE);
        for _ in 0..51 {
            if let Some(d) = c.on_stats(&st(0, 100, 0), t) { last = d; }
            t += 100_000;
        }
        assert_eq!(last.resolution, NATIVE);
    }

    #[test]
    fn quality_mode_never_changes_resolution() {
        let mut c = BitrateController::new(10_000_000, MIN_BITRATE, NATIVE, Mode::Quality);
        let mut t = 0;
        for _ in 0..30 { c.on_stats(&st(5, 95, 0), t); t += 100_000; }
        assert_eq!(c.current().resolution, NATIVE);
    }

    #[test]
    fn small_native_skips_larger_rungs() {
        let small = Resolution { width: 1280, height: 720 };
        let mut c = BitrateController::new(10_000_000, MIN_BITRATE, small, Mode::Game);
        let mut t = 0;
        let mut last = c.current();
        for _ in 0..21 { if let Some(d) = c.on_stats(&st(5, 95, 0), t) { last = d; } t += 100_000; }
        assert_eq!(last.resolution, Resolution { width: 960, height: 540 });
    }
}
```

Add `pub mod bitrate;` to `lib.rs`.

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-media bitrate`
Expected: compile error.

- [ ] **Step 3: Implement**

Top of `crates/castr-media/src/bitrate.rs`:
```rust
use castr_proto::{Mode, Stats};

pub const MIN_BITRATE: u32 = 1_000_000;
const CUT_GUARD_US: u64 = 500_000;
const CLEAN_RAISE_US: u64 = 1_000_000;
const FLOOR_STEP_DOWN_US: u64 = 2_000_000;
const CLEAN_STEP_UP_US: u64 = 5_000_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolution { pub width: u32, pub height: u32 }

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Decision { pub bitrate_bps: u32, pub resolution: Resolution }

pub struct BitrateController {
    ceiling: u32,
    bitrate: u32,
    ladder: Vec<Resolution>,
    rung: usize,
    mode: Mode,
    last_cut_us: Option<u64>,
    clean_since_us: Option<u64>,
    at_floor_since_us: Option<u64>,
    last_raise_us: Option<u64>,
    last_step_up_us: Option<u64>,
}

impl BitrateController {
    pub fn new(ceiling_bps: u32, initial_bps: u32, native: Resolution, mode: Mode) -> Self {
        let mut ladder = vec![native];
        for r in [Resolution { width: 1280, height: 720 }, Resolution { width: 960, height: 540 }] {
            if r.width < native.width { ladder.push(r); }
        }
        Self {
            ceiling: ceiling_bps,
            bitrate: initial_bps.clamp(MIN_BITRATE, ceiling_bps),
            ladder, rung: 0, mode,
            last_cut_us: None, clean_since_us: None, at_floor_since_us: None, last_raise_us: None, last_step_up_us: None,
        }
    }

    pub fn set_mode(&mut self, mode: Mode) {
        self.mode = mode;
        if mode == Mode::Quality { self.rung = 0; }
    }

    pub fn current(&self) -> Decision {
        Decision { bitrate_bps: self.bitrate, resolution: self.ladder[self.rung] }
    }

    pub fn on_stats(&mut self, stats: &Stats, now_us: u64) -> Option<Decision> {
        let before = self.current();
        let total = stats.fragments_lost + stats.fragments_received;
        let loss = if total == 0 { 0.0 } else { stats.fragments_lost as f64 / total as f64 };
        let clean = loss < 0.005 && stats.decode_queue_depth <= 1;
        let cut_allowed = self.last_cut_us.map_or(true, |t| now_us.saturating_sub(t) >= CUT_GUARD_US);

        if loss > 0.02 && cut_allowed {
            self.bitrate = ((self.bitrate as f64 * 0.7) as u32).max(MIN_BITRATE);
            self.last_cut_us = Some(now_us);
        } else if stats.decode_queue_depth > 3 && cut_allowed {
            self.bitrate = ((self.bitrate as f64 * 0.85) as u32).max(MIN_BITRATE);
            self.last_cut_us = Some(now_us);
        }

        if clean {
            let since = *self.clean_since_us.get_or_insert(now_us);
            let raise_ref = self.last_raise_us.unwrap_or(since);
            if now_us.saturating_sub(raise_ref) >= CLEAN_RAISE_US && self.bitrate < self.ceiling {
                self.bitrate = (self.bitrate + self.ceiling / 20).min(self.ceiling);
                self.last_raise_us = Some(now_us);
            }
        } else {
            self.clean_since_us = None;
            self.last_raise_us = None;
        }

        if self.mode == Mode::Game {
            if self.bitrate == MIN_BITRATE {
                let since = *self.at_floor_since_us.get_or_insert(now_us);
                if now_us.saturating_sub(since) >= FLOOR_STEP_DOWN_US && self.rung + 1 < self.ladder.len() {
                    self.rung += 1;
                    self.at_floor_since_us = Some(now_us);
                }
            } else {
                self.at_floor_since_us = None;
            }
            if let Some(since) = self.clean_since_us {
                let step_ref = self.last_step_up_us.map_or(since, |t| t.max(since));
                if now_us.saturating_sub(step_ref) >= CLEAN_STEP_UP_US && self.rung > 0 {
                    self.rung -= 1;
                    self.last_step_up_us = Some(now_us);
                }
            }
        }

        let after = self.current();
        (after != before).then_some(after)
    }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-media`
Expected: 37 passed. If `game_mode_steps_resolution_down_after_2s_at_floor_and_up_after_5s_clean` fails on the step-up half, note that the clean run also raises bitrate every second, so `at_floor_since_us` resets, which is intended; the step-up depends only on `clean_since_us`, so check that `clean_since_us` is not being cleared by the bitrate raise.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(media): AIMD bitrate controller with game-mode resolution ladder"
```

---

### Task 12: Identity and paired-peer store

**Files:**
- Create: `crates/castr-net/src/identity.rs`
- Modify: `crates/castr-net/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct Identity { pub cert_der: Vec<u8>, pub key_der: Vec<u8> /* PKCS#8 */, pub fingerprint: [u8; 32] }
  impl Identity {
      pub fn generate() -> anyhow::Result<Self>;
      pub fn load_or_create(dir: &Path) -> anyhow::Result<Self>;   // files: <dir>/identity.crt, <dir>/identity.key (DER)
      pub fn fingerprint_hex(&self) -> String;
  }
  pub fn fingerprint_of(cert_der: &[u8]) -> [u8; 32];             // SHA-256
  pub fn parse_fingerprint(hex_str: &str) -> Option<[u8; 32]>;
  #[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
  pub struct PairedPeer { pub name: String, pub paired_at_unix: u64 }
  pub struct PairedStore { /* private */ }
  impl PairedStore {
      pub fn load(path: PathBuf) -> anyhow::Result<Self>;         // missing file = empty store
      pub fn save(&self) -> anyhow::Result<()>;
      pub fn is_paired(&self, fp: &[u8; 32]) -> bool;
      pub fn add(&mut self, fp: [u8; 32], name: String);
      pub fn remove(&mut self, fp: &[u8; 32]) -> bool;
      pub fn list(&self) -> Vec<([u8; 32], PairedPeer)>;
      pub fn find_by_name(&self, name: &str) -> Option<[u8; 32]>;
  }
  pub fn config_dir() -> PathBuf;                                 // dirs::config_dir()/castr, created if missing
  ```

- [ ] **Step 1: Write the failing tests**

`crates/castr-net/src/identity.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn generate_yields_cert_key_and_stable_fingerprint() {
        let id = Identity::generate().unwrap();
        assert!(!id.cert_der.is_empty() && !id.key_der.is_empty());
        assert_eq!(id.fingerprint, fingerprint_of(&id.cert_der));
        assert_eq!(id.fingerprint_hex().len(), 64);
        assert_eq!(parse_fingerprint(&id.fingerprint_hex()), Some(id.fingerprint));
        assert_eq!(parse_fingerprint("zz"), None);
    }

    #[test]
    fn load_or_create_persists_and_reloads_same_identity() {
        let dir = std::env::temp_dir().join(format!("castr-id-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&dir);
        let a = Identity::load_or_create(&dir).unwrap();
        let b = Identity::load_or_create(&dir).unwrap();
        assert_eq!(a.fingerprint, b.fingerprint);
        assert!(dir.join("identity.crt").exists() && dir.join("identity.key").exists());
        std::fs::remove_dir_all(&dir).unwrap();
    }

    #[test]
    fn paired_store_round_trips_through_toml() {
        let path = std::env::temp_dir().join(format!("castr-paired-{}.toml", std::process::id()));
        let _ = std::fs::remove_file(&path);
        let mut s = PairedStore::load(path.clone()).unwrap();
        assert!(s.list().is_empty());
        let fp = [0xABu8; 32];
        assert!(!s.is_paired(&fp));
        s.add(fp, "living room".into());
        s.save().unwrap();
        let s2 = PairedStore::load(path.clone()).unwrap();
        assert!(s2.is_paired(&fp));
        assert_eq!(s2.find_by_name("living room"), Some(fp));
        assert_eq!(s2.list()[0].1.name, "living room");
        let mut s3 = s2;
        assert!(s3.remove(&fp));
        assert!(!s3.remove(&fp));
        std::fs::remove_file(&path).unwrap();
    }
}
```

`crates/castr-net/src/lib.rs`:
```rust
//! See docs/superpowers/specs/2026-09-01-castr-core-design.md
pub mod identity;
pub use identity::*;
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-net`
Expected: compile error.

- [ ] **Step 3: Implement**

Top of `crates/castr-net/src/identity.rs`:
```rust
use anyhow::Context;
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

pub struct Identity {
    pub cert_der: Vec<u8>,
    pub key_der: Vec<u8>,
    pub fingerprint: [u8; 32],
}

pub fn fingerprint_of(cert_der: &[u8]) -> [u8; 32] {
    Sha256::digest(cert_der).into()
}

pub fn parse_fingerprint(hex_str: &str) -> Option<[u8; 32]> {
    let bytes = hex::decode(hex_str).ok()?;
    bytes.try_into().ok()
}

impl Identity {
    pub fn generate() -> anyhow::Result<Self> {
        let ck = rcgen::generate_simple_self_signed(vec!["castr.local".to_string()]).context("rcgen")?;
        let cert_der = ck.cert.der().to_vec();
        let key_der = ck.key_pair.serialize_der();
        let fingerprint = fingerprint_of(&cert_der);
        Ok(Self { cert_der, key_der, fingerprint })
    }

    pub fn load_or_create(dir: &Path) -> anyhow::Result<Self> {
        std::fs::create_dir_all(dir).with_context(|| format!("create {}", dir.display()))?;
        let cert_path = dir.join("identity.crt");
        let key_path = dir.join("identity.key");
        if cert_path.exists() && key_path.exists() {
            let cert_der = std::fs::read(&cert_path)?;
            let key_der = std::fs::read(&key_path)?;
            let fingerprint = fingerprint_of(&cert_der);
            return Ok(Self { cert_der, key_der, fingerprint });
        }
        let id = Self::generate()?;
        std::fs::write(&cert_path, &id.cert_der)?;
        std::fs::write(&key_path, &id.key_der)?;
        Ok(id)
    }

    pub fn fingerprint_hex(&self) -> String { hex::encode(self.fingerprint) }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PairedPeer {
    pub name: String,
    pub paired_at_unix: u64,
}

#[derive(Default, Serialize, Deserialize)]
struct StoreFile {
    #[serde(default)]
    peers: BTreeMap<String, PairedPeer>,
}

pub struct PairedStore {
    path: PathBuf,
    file: StoreFile,
}

impl PairedStore {
    pub fn load(path: PathBuf) -> anyhow::Result<Self> {
        let file = if path.exists() {
            toml::from_str(&std::fs::read_to_string(&path)?).context("parse paired.toml")?
        } else {
            StoreFile::default()
        };
        Ok(Self { path, file })
    }

    pub fn save(&self) -> anyhow::Result<()> {
        if let Some(parent) = self.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        std::fs::write(&self.path, toml::to_string_pretty(&self.file)?)?;
        Ok(())
    }

    pub fn is_paired(&self, fp: &[u8; 32]) -> bool { self.file.peers.contains_key(&hex::encode(fp)) }

    pub fn add(&mut self, fp: [u8; 32], name: String) {
        let now = std::time::SystemTime::now().duration_since(std::time::UNIX_EPOCH).map(|d| d.as_secs()).unwrap_or(0);
        self.file.peers.insert(hex::encode(fp), PairedPeer { name, paired_at_unix: now });
    }

    pub fn remove(&mut self, fp: &[u8; 32]) -> bool { self.file.peers.remove(&hex::encode(fp)).is_some() }

    pub fn list(&self) -> Vec<([u8; 32], PairedPeer)> {
        self.file.peers.iter().filter_map(|(k, v)| parse_fingerprint(k).map(|fp| (fp, v.clone()))).collect()
    }

    pub fn find_by_name(&self, name: &str) -> Option<[u8; 32]> {
        self.list().into_iter().find(|(_, p)| p.name == name).map(|(fp, _)| fp)
    }
}

pub fn config_dir() -> PathBuf {
    let dir = dirs::config_dir().unwrap_or_else(|| PathBuf::from(".")).join("castr");
    let _ = std::fs::create_dir_all(&dir);
    dir
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-net`
Expected: 3 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(net): self-signed identity and paired peer store"
```

---

### Task 13: TLS verifiers and QUIC transport

**Files:**
- Create: `crates/castr-net/src/tls.rs`, `crates/castr-net/src/transport.rs`
- Create: `crates/castr-net/tests/loopback.rs`
- Modify: `crates/castr-net/src/lib.rs`

**Interfaces:**
- Consumes: `Identity`, `fingerprint_of`; `castr_proto::{ControlMessage, Nack, encode_len_prefixed, decode_len_prefixed}`.
- Produces:
  ```rust
  // tls.rs
  pub type TrustCheck = Arc<dyn Fn(&[u8; 32]) -> bool + Send + Sync>;
  pub fn accept_any() -> TrustCheck;
  pub fn trust_fingerprints(set: Arc<std::sync::RwLock<std::collections::HashSet<[u8; 32]>>>) -> TrustCheck;
  pub fn server_config(id: &Identity, trust: TrustCheck) -> anyhow::Result<quinn::ServerConfig>;
  pub fn client_config(id: &Identity, trust: TrustCheck) -> anyhow::Result<quinn::ClientConfig>;
  // transport.rs
  pub const ALPN: &[u8] = b"castr/1";
  pub type SendFilter = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;   // false = drop datagram (tests)
  pub struct Endpoint { /* private */ }
  impl Endpoint {
      pub fn server(bind: SocketAddr, id: &Identity, trust: TrustCheck) -> anyhow::Result<Self>;
      pub fn client(bind: SocketAddr, id: &Identity, trust: TrustCheck) -> anyhow::Result<Self>;
      pub fn local_addr(&self) -> anyhow::Result<SocketAddr>;
      pub async fn accept(&self) -> anyhow::Result<Link>;               // receiver side; waits for the sender's control stream
      pub async fn connect(&self, addr: SocketAddr) -> anyhow::Result<Link>; // sender side; opens the control stream
      pub fn close(&self);
  }
  pub struct Link { /* private, Clone */ }
  impl Link {
      pub fn peer_fingerprint(&self) -> [u8; 32];
      pub fn remote_addr(&self) -> SocketAddr;
      pub async fn send_control(&self, msg: &ControlMessage) -> anyhow::Result<()>;
      pub async fn recv_control(&self) -> anyhow::Result<ControlMessage>;
      pub fn send_datagram(&self, d: Bytes) -> anyhow::Result<()>;
      pub async fn recv_datagram(&self) -> anyhow::Result<Bytes>;
      pub fn max_datagram_size(&self) -> usize;                        // 1200 if unknown
      pub fn rtt(&self) -> Duration;
      pub fn set_send_filter(&self, f: Option<SendFilter>);
      pub async fn open_nack_stream(&self) -> anyhow::Result<NackSender>;     // receiver opens
      pub async fn accept_nack_stream(&self) -> anyhow::Result<NackReceiver>; // sender accepts
      pub fn close(&self, reason: &str);
      pub async fn closed(&self);                                       // resolves when the connection is gone
  }
  pub struct NackSender(/* quinn::SendStream */);
  impl NackSender { pub async fn send(&mut self, nack: &Nack) -> anyhow::Result<()>; }
  pub struct NackReceiver(/* quinn::RecvStream, buffer */);
  impl NackReceiver { pub async fn recv(&mut self) -> anyhow::Result<Nack>; }
  ```
  Transport config on both sides: idle timeout 3 s, keep-alive 500 ms, datagram send/receive buffers 4 MiB, TLS 1.3 only, ALPN `castr/1`, mutual certificate auth checked by fingerprint.

- [ ] **Step 1: Write the failing integration test**

`crates/castr-net/tests/loopback.rs`:
```rust
use bytes::Bytes;
use castr_net::*;
use castr_proto::*;
use std::collections::HashSet;
use std::net::SocketAddr;
use std::sync::{Arc, RwLock};

fn loopback() -> SocketAddr { "127.0.0.1:0".parse().unwrap() }

fn pair() -> (Identity, Identity, Endpoint, Endpoint) {
    let recv_id = Identity::generate().unwrap();
    let send_id = Identity::generate().unwrap();
    let recv_trust = Arc::new(RwLock::new(HashSet::from([send_id.fingerprint])));
    let send_trust = Arc::new(RwLock::new(HashSet::from([recv_id.fingerprint])));
    let server = Endpoint::server(loopback(), &recv_id, trust_fingerprints(recv_trust)).unwrap();
    let client = Endpoint::client(loopback(), &send_id, trust_fingerprints(send_trust)).unwrap();
    (recv_id, send_id, server, client)
}

#[tokio::test]
async fn control_datagram_and_nack_round_trip() {
    let (recv_id, send_id, server, client) = pair();
    let addr = server.local_addr().unwrap();
    let (r, s) = tokio::join!(server.accept(), client.connect(addr));
    let (r, s) = (r.unwrap(), s.unwrap());
    assert_eq!(r.peer_fingerprint(), send_id.fingerprint);
    assert_eq!(s.peer_fingerprint(), recv_id.fingerprint);

    let hello = ControlMessage::Hello { version: PROTOCOL_VERSION, name: "pc".into(), resume_token: None };
    s.send_control(&hello).await.unwrap();
    assert_eq!(r.recv_control().await.unwrap(), hello);
    r.send_control(&ControlMessage::RequestKeyframe).await.unwrap();
    assert_eq!(s.recv_control().await.unwrap(), ControlMessage::RequestKeyframe);

    assert!(s.max_datagram_size() >= 1000);
    s.send_datagram(Bytes::from_static(b"video")).unwrap();
    assert_eq!(r.recv_datagram().await.unwrap(), Bytes::from_static(b"video"));

    let mut tx = r.open_nack_stream().await.unwrap();
    let nack = Nack { frame_number: 7, missing: vec![1, 3] };
    tx.send(&nack).await.unwrap();
    let mut rx = s.accept_nack_stream().await.unwrap();
    assert_eq!(rx.recv().await.unwrap(), nack);

    s.close("done");
    tokio::time::timeout(std::time::Duration::from_secs(2), r.closed()).await.expect("receiver sees close");
}

#[tokio::test]
async fn unpaired_client_is_rejected() {
    let recv_id = Identity::generate().unwrap();
    let send_id = Identity::generate().unwrap();
    let empty = Arc::new(RwLock::new(HashSet::new()));
    let server = Endpoint::server(loopback(), &recv_id, trust_fingerprints(empty)).unwrap();
    let client = Endpoint::client(loopback(), &send_id, accept_any()).unwrap();
    let addr = server.local_addr().unwrap();
    let accept = tokio::spawn(async move { server.accept().await.is_ok() });
    let connect = tokio::time::timeout(std::time::Duration::from_secs(5), client.connect(addr)).await.unwrap();
    assert!(connect.is_err(), "client must fail the handshake");
    accept.abort();
}

#[tokio::test]
async fn send_filter_drops_datagrams() {
    let (_, _, server, client) = pair();
    let addr = server.local_addr().unwrap();
    let (r, s) = tokio::join!(server.accept(), client.connect(addr));
    let (r, s) = (r.unwrap(), s.unwrap());
    s.set_send_filter(Some(Arc::new(|d: &[u8]| d[0] != b'x')));
    s.send_datagram(Bytes::from_static(b"xdrop")).unwrap();
    s.send_datagram(Bytes::from_static(b"keep")).unwrap();
    assert_eq!(r.recv_datagram().await.unwrap(), Bytes::from_static(b"keep"));
}
```

- [ ] **Step 2: Run test to verify it fails**

Run: `cargo test -p castr-net --test loopback`
Expected: compile error.

- [ ] **Step 3: Implement tls.rs**

`crates/castr-net/src/tls.rs`:
```rust
use crate::identity::{fingerprint_of, Identity};
use anyhow::Context;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer, ServerName, UnixTime};
use rustls::server::danger::{ClientCertVerified, ClientCertVerifier};
use rustls::{DigitallySignedStruct, DistinguishedName, SignatureScheme};
use std::collections::HashSet;
use std::sync::{Arc, RwLock};
use std::time::Duration;

pub type TrustCheck = Arc<dyn Fn(&[u8; 32]) -> bool + Send + Sync>;

pub fn accept_any() -> TrustCheck { Arc::new(|_| true) }

pub fn trust_fingerprints(set: Arc<RwLock<HashSet<[u8; 32]>>>) -> TrustCheck {
    Arc::new(move |fp| set.read().map(|s| s.contains(fp)).unwrap_or(false))
}

struct FpVerifier {
    trust: TrustCheck,
    provider: Arc<CryptoProvider>,
}

impl std::fmt::Debug for FpVerifier {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result { f.write_str("FpVerifier") }
}

impl FpVerifier {
    fn check(&self, cert: &CertificateDer<'_>) -> Result<(), rustls::Error> {
        if (self.trust)(&fingerprint_of(cert.as_ref())) {
            Ok(())
        } else {
            Err(rustls::Error::General("peer certificate is not paired".into()))
        }
    }
}

impl ServerCertVerifier for FpVerifier {
    fn verify_server_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: &ServerName<'_>, _: &[u8], _: UnixTime) -> Result<ServerCertVerified, rustls::Error> {
        self.check(end_entity).map(|_| ServerCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> { self.provider.signature_verification_algorithms.supported_schemes() }
}

impl ClientCertVerifier for FpVerifier {
    fn root_hint_subjects(&self) -> &[DistinguishedName] { &[] }
    fn verify_client_cert(&self, end_entity: &CertificateDer<'_>, _: &[CertificateDer<'_>], _: UnixTime) -> Result<ClientCertVerified, rustls::Error> {
        self.check(end_entity).map(|_| ClientCertVerified::assertion())
    }
    fn verify_tls12_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn verify_tls13_signature(&self, m: &[u8], c: &CertificateDer<'_>, d: &DigitallySignedStruct) -> Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(m, c, d, &self.provider.signature_verification_algorithms)
    }
    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> { self.provider.signature_verification_algorithms.supported_schemes() }
}

fn provider() -> Arc<CryptoProvider> { Arc::new(rustls::crypto::ring::default_provider()) }

fn cert_and_key(id: &Identity) -> (Vec<CertificateDer<'static>>, PrivateKeyDer<'static>) {
    (
        vec![CertificateDer::from(id.cert_der.clone())],
        PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(id.key_der.clone())),
    )
}

pub fn transport_config() -> anyhow::Result<Arc<quinn::TransportConfig>> {
    let mut t = quinn::TransportConfig::default();
    t.max_idle_timeout(Some(Duration::from_secs(3).try_into().context("idle timeout")?));
    t.keep_alive_interval(Some(Duration::from_millis(500)));
    t.datagram_receive_buffer_size(Some(4 << 20));
    t.datagram_send_buffer_size(4 << 20);
    Ok(Arc::new(t))
}

pub fn server_config(id: &Identity, trust: TrustCheck) -> anyhow::Result<quinn::ServerConfig> {
    let provider = provider();
    let verifier = Arc::new(FpVerifier { trust, provider: provider.clone() });
    let (certs, key) = cert_and_key(id);
    let mut tls = rustls::ServerConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .with_client_cert_verifier(verifier)
        .with_single_cert(certs, key)?;
    tls.alpn_protocols = vec![crate::transport::ALPN.to_vec()];
    let quic = quinn::crypto::rustls::QuicServerConfig::try_from(tls)?;
    let mut cfg = quinn::ServerConfig::with_crypto(Arc::new(quic));
    cfg.transport_config(transport_config()?);
    Ok(cfg)
}

pub fn client_config(id: &Identity, trust: TrustCheck) -> anyhow::Result<quinn::ClientConfig> {
    let provider = provider();
    let verifier = Arc::new(FpVerifier { trust, provider: provider.clone() });
    let (certs, key) = cert_and_key(id);
    let mut tls = rustls::ClientConfig::builder_with_provider(provider)
        .with_protocol_versions(&[&rustls::version::TLS13])?
        .dangerous()
        .with_custom_certificate_verifier(verifier)
        .with_client_auth_cert(certs, key)?;
    tls.alpn_protocols = vec![crate::transport::ALPN.to_vec()];
    let quic = quinn::crypto::rustls::QuicClientConfig::try_from(tls)?;
    let mut cfg = quinn::ClientConfig::new(Arc::new(quic));
    cfg.transport_config(transport_config()?);
    Ok(cfg)
}
```

- [ ] **Step 4: Implement transport.rs**

`crates/castr-net/src/transport.rs`:
```rust
use crate::identity::{fingerprint_of, Identity};
use crate::tls::{client_config, server_config, TrustCheck};
use anyhow::{anyhow, Context};
use bytes::Bytes;
use castr_proto::{decode_len_prefixed, encode_len_prefixed, ControlMessage, Nack};
use rustls::pki_types::CertificateDer;
use std::net::SocketAddr;
use std::sync::{Arc, Mutex as StdMutex};
use std::time::Duration;
use tokio::io::AsyncWriteExt;
use tokio::sync::Mutex;

pub const ALPN: &[u8] = b"castr/1";
pub type SendFilter = Arc<dyn Fn(&[u8]) -> bool + Send + Sync>;

pub struct Endpoint {
    inner: quinn::Endpoint,
}

impl Endpoint {
    pub fn server(bind: SocketAddr, id: &Identity, trust: TrustCheck) -> anyhow::Result<Self> {
        let inner = quinn::Endpoint::server(server_config(id, trust)?, bind).context("bind server endpoint")?;
        Ok(Self { inner })
    }

    pub fn client(bind: SocketAddr, id: &Identity, trust: TrustCheck) -> anyhow::Result<Self> {
        let mut inner = quinn::Endpoint::client(bind).context("bind client endpoint")?;
        inner.set_default_client_config(client_config(id, trust)?);
        Ok(Self { inner })
    }

    pub fn local_addr(&self) -> anyhow::Result<SocketAddr> { Ok(self.inner.local_addr()?) }

    pub async fn accept(&self) -> anyhow::Result<Link> {
        loop {
            let incoming = self.inner.accept().await.ok_or_else(|| anyhow!("endpoint closed"))?;
            match incoming.await {
                Ok(conn) => match conn.accept_bi().await {
                    Ok((tx, rx)) => return Link::new(conn, tx, rx),
                    Err(e) => tracing::warn!("control stream not opened: {e}"),
                },
                Err(e) => tracing::warn!("handshake failed: {e}"),
            }
        }
    }

    pub async fn connect(&self, addr: SocketAddr) -> anyhow::Result<Link> {
        let conn = self.inner.connect(addr, "castr.local")?.await.context("QUIC handshake")?;
        let (tx, rx) = conn.open_bi().await.context("open control stream")?;
        Link::new(conn, tx, rx)
    }

    pub fn close(&self) { self.inner.close(0u32.into(), b"bye"); }
}

struct ControlRx {
    stream: quinn::RecvStream,
    buf: Vec<u8>,
}

#[derive(Clone)]
pub struct Link {
    conn: quinn::Connection,
    control_tx: Arc<Mutex<quinn::SendStream>>,
    control_rx: Arc<Mutex<ControlRx>>,
    peer_fp: [u8; 32],
    filter: Arc<StdMutex<Option<SendFilter>>>,
}

impl Link {
    fn new(conn: quinn::Connection, tx: quinn::SendStream, rx: quinn::RecvStream) -> anyhow::Result<Self> {
        let peer_fp = peer_fingerprint(&conn)?;
        Ok(Self {
            conn,
            control_tx: Arc::new(Mutex::new(tx)),
            control_rx: Arc::new(Mutex::new(ControlRx { stream: rx, buf: Vec::new() })),
            peer_fp,
            filter: Arc::new(StdMutex::new(None)),
        })
    }

    pub fn peer_fingerprint(&self) -> [u8; 32] { self.peer_fp }
    pub fn remote_addr(&self) -> SocketAddr { self.conn.remote_address() }
    pub fn rtt(&self) -> Duration { self.conn.rtt() }
    pub fn max_datagram_size(&self) -> usize { self.conn.max_datagram_size().unwrap_or(1200) }
    pub fn set_send_filter(&self, f: Option<SendFilter>) { *self.filter.lock().unwrap() = f; }
    pub fn close(&self, reason: &str) { self.conn.close(0u32.into(), reason.as_bytes()); }
    pub async fn closed(&self) { let _ = self.conn.closed().await; }

    pub async fn send_control(&self, msg: &ControlMessage) -> anyhow::Result<()> {
        let bytes = encode_len_prefixed(msg);
        let mut tx = self.control_tx.lock().await;
        tx.write_all(&bytes).await.context("control write")?;
        Ok(())
    }

    pub async fn recv_control(&self) -> anyhow::Result<ControlMessage> {
        let mut rx = self.control_rx.lock().await;
        loop {
            if let Some((msg, used)) = decode_len_prefixed::<ControlMessage>(&rx.buf)? {
                rx.buf.drain(..used);
                return Ok(msg);
            }
            let mut chunk = [0u8; 4096];
            let n = rx.stream.read(&mut chunk).await.context("control read")?.ok_or_else(|| anyhow!("control stream closed"))?;
            rx.buf.extend_from_slice(&chunk[..n]);
        }
    }

    pub fn send_datagram(&self, d: Bytes) -> anyhow::Result<()> {
        if let Some(f) = self.filter.lock().unwrap().as_ref() {
            if !f(&d) {
                return Ok(());
            }
        }
        self.conn.send_datagram(d).context("send datagram")
    }

    pub async fn recv_datagram(&self) -> anyhow::Result<Bytes> {
        self.conn.read_datagram().await.context("read datagram")
    }

    pub async fn open_nack_stream(&self) -> anyhow::Result<NackSender> {
        Ok(NackSender(self.conn.open_uni().await.context("open nack stream")?))
    }

    pub async fn accept_nack_stream(&self) -> anyhow::Result<NackReceiver> {
        Ok(NackReceiver { stream: self.conn.accept_uni().await.context("accept nack stream")?, buf: Vec::new() })
    }
}

fn peer_fingerprint(conn: &quinn::Connection) -> anyhow::Result<[u8; 32]> {
    let identity = conn.peer_identity().ok_or_else(|| anyhow!("peer presented no certificate"))?;
    let certs = identity.downcast::<Vec<CertificateDer<'static>>>().map_err(|_| anyhow!("unexpected peer identity type"))?;
    let first = certs.first().ok_or_else(|| anyhow!("empty peer certificate chain"))?;
    Ok(fingerprint_of(first.as_ref()))
}

pub struct NackSender(quinn::SendStream);

impl NackSender {
    pub async fn send(&mut self, nack: &Nack) -> anyhow::Result<()> {
        self.0.write_all(&encode_len_prefixed(nack)).await.context("nack write")
    }
}

pub struct NackReceiver {
    stream: quinn::RecvStream,
    buf: Vec<u8>,
}

impl NackReceiver {
    pub async fn recv(&mut self) -> anyhow::Result<Nack> {
        loop {
            if let Some((nack, used)) = decode_len_prefixed::<Nack>(&self.buf)? {
                self.buf.drain(..used);
                return Ok(nack);
            }
            let mut chunk = [0u8; 1024];
            let n = self.stream.read(&mut chunk).await.context("nack read")?.ok_or_else(|| anyhow!("nack stream closed"))?;
            self.buf.extend_from_slice(&chunk[..n]);
        }
    }
}
```

`crates/castr-net/src/lib.rs` becomes:
```rust
//! See docs/superpowers/specs/2026-09-01-castr-core-design.md
pub mod identity;
pub mod tls;
pub mod transport;
pub use identity::*;
pub use tls::*;
pub use transport::*;
```

Add to `crates/castr-net/Cargo.toml` under `[dev-dependencies]`:
```toml
tokio = { workspace = true, features = ["test-util"] }
```

- [ ] **Step 5: Run tests**

Run: `cargo test -p castr-net`
Expected: 3 unit + 3 integration passed. If `unpaired_client_is_rejected` hangs, the server accepted the handshake: confirm `client_auth_mandatory` defaults to true for the `ClientCertVerifier` impl (it does in rustls 0.23; if not, override `fn client_auth_mandatory(&self) -> bool { true }`).

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(net): QUIC transport with fingerprint-pinned mutual TLS"
```

---

### Task 14: PIN pairing over a Link

**Files:**
- Create: `crates/castr-net/src/pairing.rs`
- Modify: `crates/castr-net/src/lib.rs`, `crates/castr-net/tests/loopback.rs`

**Interfaces:**
- Consumes: `Link`, `ControlMessage::{PairInit, PairResp, PairProof, PairOk}`.
- Produces:
  ```rust
  pub fn generate_pin() -> String;                                    // 6 digits, zero-padded
  /// Sender side. Returns Ok(()) when both proofs verified and PairOk exchanged.
  pub async fn pair_as_sender(link: &Link, own_fp: [u8; 32], pin: &str) -> anyhow::Result<()>;
  /// Receiver side.
  pub async fn pair_as_receiver(link: &Link, own_fp: [u8; 32], pin: &str) -> anyhow::Result<()>;
  ```
  Protocol: sender runs SPAKE2 side A with identities `castr-sender` / `castr-receiver`, sends `PairInit(msgA)`. Receiver runs side B, finishes with msgA, sends `PairResp(msgB)`. Sender finishes. Each side computes `proof = HMAC-SHA256(key, role || own_fp)` with role `b"sender"` or `b"receiver"`. Sender sends `PairProof` first; receiver verifies against `link.peer_fingerprint()`, then sends its own `PairProof`; sender verifies, sends `PairOk`; receiver replies `PairOk`. Any mismatch returns an error and sends `Error { code: 3 }`.

- [ ] **Step 1: Write the failing tests**

Append to `crates/castr-net/tests/loopback.rs`:
```rust
async fn connected_any() -> (Link, Link, Identity, Identity) {
    let recv_id = Identity::generate().unwrap();
    let send_id = Identity::generate().unwrap();
    let server = Endpoint::server(loopback(), &recv_id, accept_any()).unwrap();
    let client = Endpoint::client(loopback(), &send_id, accept_any()).unwrap();
    let addr = server.local_addr().unwrap();
    let (r, s) = tokio::join!(server.accept(), client.connect(addr));
    std::mem::forget(server);
    std::mem::forget(client);
    (r.unwrap(), s.unwrap(), recv_id, send_id)
}

#[tokio::test]
async fn pairing_succeeds_with_matching_pin() {
    let (r, s, recv_id, send_id) = connected_any().await;
    let pin = generate_pin();
    assert_eq!(pin.len(), 6);
    let (a, b) = tokio::join!(
        pair_as_receiver(&r, recv_id.fingerprint, &pin),
        pair_as_sender(&s, send_id.fingerprint, &pin),
    );
    a.unwrap();
    b.unwrap();
}

#[tokio::test]
async fn pairing_fails_with_wrong_pin() {
    let (r, s, recv_id, send_id) = connected_any().await;
    let (a, b) = tokio::join!(
        pair_as_receiver(&r, recv_id.fingerprint, "111111"),
        pair_as_sender(&s, send_id.fingerprint, "222222"),
    );
    assert!(a.is_err());
    assert!(b.is_err());
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-net --test loopback pairing`
Expected: compile error.

- [ ] **Step 3: Implement**

`crates/castr-net/src/pairing.rs`:
```rust
use crate::transport::Link;
use anyhow::{anyhow, bail, Context};
use castr_proto::ControlMessage;
use hmac::{Hmac, Mac};
use rand::Rng;
use sha2::Sha256;
use spake2::{Ed25519Group, Identity as SpakeId, Password, Spake2};

type HmacSha256 = Hmac<Sha256>;
const ID_SENDER: &[u8] = b"castr-sender";
const ID_RECEIVER: &[u8] = b"castr-receiver";

pub fn generate_pin() -> String {
    format!("{:06}", rand::thread_rng().gen_range(0..1_000_000u32))
}

fn proof(key: &[u8], role: &[u8], fp: &[u8; 32]) -> [u8; 32] {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(role);
    mac.update(fp);
    mac.finalize().into_bytes().into()
}

fn verify(key: &[u8], role: &[u8], fp: &[u8; 32], got: &[u8; 32]) -> bool {
    let mut mac = HmacSha256::new_from_slice(key).expect("hmac accepts any key length");
    mac.update(role);
    mac.update(fp);
    mac.verify_slice(got).is_ok()
}

async fn fail(link: &Link, why: &str) -> anyhow::Error {
    let _ = link.send_control(&ControlMessage::Error { code: 3, message: why.to_string() }).await;
    anyhow!("pairing failed: {why}")
}

pub async fn pair_as_sender(link: &Link, own_fp: [u8; 32], pin: &str) -> anyhow::Result<()> {
    let (state, msg_a) = Spake2::<Ed25519Group>::start_a(
        &Password::new(pin.as_bytes()), &SpakeId::new(ID_SENDER), &SpakeId::new(ID_RECEIVER));
    link.send_control(&ControlMessage::PairInit(msg_a)).await?;
    let msg_b = match link.recv_control().await? {
        ControlMessage::PairResp(m) => m,
        ControlMessage::Error { message, .. } => bail!("receiver refused pairing: {message}"),
        other => return Err(fail(link, &format!("unexpected {other:?}")).await),
    };
    let key = state.finish(&msg_b).map_err(|e| anyhow!("spake2: {e:?}"))?;
    link.send_control(&ControlMessage::PairProof(proof(&key, b"sender", &own_fp))).await?;
    match link.recv_control().await? {
        ControlMessage::PairProof(p) if verify(&key, b"receiver", &link.peer_fingerprint(), &p) => {}
        ControlMessage::Error { message, .. } => bail!("receiver rejected proof: {message}"),
        _ => return Err(fail(link, "receiver proof mismatch").await),
    }
    link.send_control(&ControlMessage::PairOk).await?;
    match link.recv_control().await? {
        ControlMessage::PairOk => Ok(()),
        other => bail!("expected PairOk, got {other:?}"),
    }
}

pub async fn pair_as_receiver(link: &Link, own_fp: [u8; 32], pin: &str) -> anyhow::Result<()> {
    let msg_a = match link.recv_control().await? {
        ControlMessage::PairInit(m) => m,
        other => return Err(fail(link, &format!("unexpected {other:?}")).await),
    };
    let (state, msg_b) = Spake2::<Ed25519Group>::start_b(
        &Password::new(pin.as_bytes()), &SpakeId::new(ID_SENDER), &SpakeId::new(ID_RECEIVER));
    let key = state.finish(&msg_a).map_err(|e| anyhow!("spake2: {e:?}"))?;
    link.send_control(&ControlMessage::PairResp(msg_b)).await?;
    match link.recv_control().await? {
        ControlMessage::PairProof(p) if verify(&key, b"sender", &link.peer_fingerprint(), &p) => {}
        ControlMessage::Error { message, .. } => bail!("sender aborted: {message}"),
        _ => return Err(fail(link, "wrong PIN").await),
    }
    link.send_control(&ControlMessage::PairProof(proof(&key, b"receiver", &own_fp))).await?;
    match link.recv_control().await.context("waiting for PairOk")? {
        ControlMessage::PairOk => {}
        ControlMessage::Error { message, .. } => bail!("sender rejected proof: {message}"),
        other => bail!("expected PairOk, got {other:?}"),
    }
    link.send_control(&ControlMessage::PairOk).await?;
    Ok(())
}
```

Add `pub mod pairing; pub use pairing::*;` to `lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-net`
Expected: all pass, including both pairing tests. In the wrong-PIN test the receiver fails first on the sender's proof and sends `Error`; the sender then sees `Error` instead of `PairProof` and fails too.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(net): SPAKE2 PIN pairing with fingerprint proofs"
```

---

### Task 15: Discovery (mDNS plus UDP broadcast fallback)

**Files:**
- Create: `crates/castr-net/src/discovery.rs`
- Modify: `crates/castr-net/src/lib.rs`, `crates/castr-net/tests/loopback.rs`

**Interfaces:**
- Produces:
  ```rust
  pub const SERVICE_TYPE: &str = "_castr._udp.local.";
  pub const PROBE_PORT: u16 = 7331;
  pub const PROBE_MAGIC: &[u8] = b"CASTR?";
  #[derive(Debug, Clone, PartialEq, Eq)]
  pub struct ReceiverInfo { pub name: String, pub fingerprint: [u8; 32], pub addr: SocketAddr, pub version: u16 }
  #[derive(Serialize, Deserialize)] pub struct Beacon { pub name: String, pub fp: [u8; 32], pub port: u16, pub version: u16 }
  pub struct Advertiser { /* private; dropping it unregisters mDNS and stops the UDP responder */ }
  impl Advertiser {
      /// `quic_port` is the receiver's QUIC port. `probe_port` is normally PROBE_PORT; tests pass 0 and read it back.
      pub async fn start(name: &str, fp: [u8; 32], quic_port: u16, probe_port: u16) -> anyhow::Result<Self>;
      pub fn probe_port(&self) -> u16;
  }
  /// Runs mDNS browse and a UDP probe (to 255.255.255.255:probe_port and 127.0.0.1:probe_port) in parallel for `timeout`, merged by fingerprint.
  pub async fn browse(timeout: Duration, probe_port: u16) -> anyhow::Result<Vec<ReceiverInfo>>;
  ```

- [ ] **Step 1: Write the failing tests**

Append to `crates/castr-net/tests/loopback.rs`:
```rust
#[tokio::test]
async fn udp_probe_finds_advertiser_on_loopback() {
    let fp = [0x42u8; 32];
    let adv = Advertiser::start("Test Receiver", fp, 5555, 0).await.unwrap();
    let found = browse(std::time::Duration::from_millis(800), adv.probe_port()).await.unwrap();
    let hit = found.iter().find(|r| r.fingerprint == fp).expect("advertiser discovered");
    assert_eq!(hit.name, "Test Receiver");
    assert_eq!(hit.addr.port(), 5555);
    assert_eq!(hit.version, PROTOCOL_VERSION);
}

#[tokio::test]
async fn browse_with_nothing_advertised_returns_empty() {
    let found = browse(std::time::Duration::from_millis(300), 1).await.unwrap();
    assert!(found.iter().all(|r| r.fingerprint != [0x42u8; 32]));
}
```

- [ ] **Step 2: Run tests to verify they fail**

Run: `cargo test -p castr-net --test loopback probe`
Expected: compile error.

- [ ] **Step 3: Implement**

`crates/castr-net/src/discovery.rs`:
```rust
use anyhow::Context;
use castr_proto::PROTOCOL_VERSION;
use mdns_sd::{ServiceDaemon, ServiceEvent, ServiceInfo};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;
use std::net::{IpAddr, Ipv4Addr, SocketAddr};
use std::time::Duration;
use tokio::net::UdpSocket;

pub const SERVICE_TYPE: &str = "_castr._udp.local.";
pub const PROBE_PORT: u16 = 7331;
pub const PROBE_MAGIC: &[u8] = b"CASTR?";

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReceiverInfo {
    pub name: String,
    pub fingerprint: [u8; 32],
    pub addr: SocketAddr,
    pub version: u16,
}

#[derive(Debug, Serialize, Deserialize)]
pub struct Beacon {
    pub name: String,
    pub fp: [u8; 32],
    pub port: u16,
    pub version: u16,
}

pub struct Advertiser {
    mdns: Option<ServiceDaemon>,
    fullname: String,
    probe_port: u16,
    responder: tokio::task::JoinHandle<()>,
}

impl Advertiser {
    pub async fn start(name: &str, fp: [u8; 32], quic_port: u16, probe_port: u16) -> anyhow::Result<Self> {
        let beacon = Beacon { name: name.to_string(), fp, port: quic_port, version: PROTOCOL_VERSION };
        let reply = postcard::to_allocvec(&beacon)?;
        let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, probe_port)).await.context("bind probe port")?;
        sock.set_broadcast(true)?;
        let probe_port = sock.local_addr()?.port();
        let responder = tokio::spawn(async move {
            let mut buf = [0u8; 64];
            loop {
                let Ok((n, from)) = sock.recv_from(&mut buf).await else { break };
                if n == PROBE_MAGIC.len() + 1 && &buf[..PROBE_MAGIC.len()] == PROBE_MAGIC {
                    let _ = sock.send_to(&reply, from).await;
                }
            }
        });

        let (mdns, fullname) = match ServiceDaemon::new() {
            Ok(daemon) => {
                let host = format!("castr-{}.local.", &hex::encode(fp)[..12]);
                let props: HashMap<String, String> = HashMap::from([
                    ("name".to_string(), name.to_string()),
                    ("fp".to_string(), hex::encode(fp)),
                    ("ver".to_string(), PROTOCOL_VERSION.to_string()),
                ]);
                let info = ServiceInfo::new(SERVICE_TYPE, name, &host, "", quic_port, props)
                    .context("mdns service info")?
                    .enable_addr_auto();
                let fullname = info.get_fullname().to_string();
                match daemon.register(info) {
                    Ok(()) => (Some(daemon), fullname),
                    Err(e) => { tracing::warn!("mDNS register failed: {e}"); (None, String::new()) }
                }
            }
            Err(e) => { tracing::warn!("mDNS unavailable: {e}"); (None, String::new()) }
        };
        Ok(Self { mdns, fullname, probe_port, responder })
    }

    pub fn probe_port(&self) -> u16 { self.probe_port }
}

impl Drop for Advertiser {
    fn drop(&mut self) {
        self.responder.abort();
        if let Some(d) = self.mdns.take() {
            let _ = d.unregister(&self.fullname);
            let _ = d.shutdown();
        }
    }
}

async fn probe_udp(timeout: Duration, probe_port: u16, out: &mut HashMap<[u8; 32], ReceiverInfo>) -> anyhow::Result<()> {
    let sock = UdpSocket::bind((Ipv4Addr::UNSPECIFIED, 0)).await?;
    sock.set_broadcast(true)?;
    let mut probe = PROBE_MAGIC.to_vec();
    probe.push(PROTOCOL_VERSION as u8);
    for target in [IpAddr::V4(Ipv4Addr::BROADCAST), IpAddr::V4(Ipv4Addr::LOCALHOST)] {
        let _ = sock.send_to(&probe, (target, probe_port)).await;
    }
    let deadline = tokio::time::Instant::now() + timeout;
    let mut buf = [0u8; 512];
    loop {
        let Ok(res) = tokio::time::timeout_at(deadline, sock.recv_from(&mut buf)).await else { break };
        let Ok((n, from)) = res else { break };
        if let Ok(b) = postcard::from_bytes::<Beacon>(&buf[..n]) {
            out.entry(b.fp).or_insert(ReceiverInfo {
                name: b.name, fingerprint: b.fp, addr: SocketAddr::new(from.ip(), b.port), version: b.version,
            });
        }
    }
    Ok(())
}

async fn probe_mdns(timeout: Duration, out: &mut HashMap<[u8; 32], ReceiverInfo>) {
    let Ok(daemon) = ServiceDaemon::new() else { return };
    let Ok(rx) = daemon.browse(SERVICE_TYPE) else { return };
    let deadline = tokio::time::Instant::now() + timeout;
    loop {
        let Ok(ev) = tokio::time::timeout_at(deadline, rx.recv_async()).await else { break };
        let Ok(ev) = ev else { break };
        if let ServiceEvent::ServiceResolved(info) = ev {
            let fp = info.get_property_val_str("fp").and_then(crate::identity::parse_fingerprint);
            let name = info.get_property_val_str("name").unwrap_or("").to_string();
            let version = info.get_property_val_str("ver").and_then(|v| v.parse().ok()).unwrap_or(0);
            let ip = info.get_addresses().iter().find(|a| a.is_ipv4()).copied();
            if let (Some(fp), Some(ip)) = (fp, ip) {
                out.entry(fp).or_insert(ReceiverInfo { name, fingerprint: fp, addr: SocketAddr::new(ip, info.get_port()), version });
            }
        }
    }
    let _ = daemon.shutdown();
}

pub async fn browse(timeout: Duration, probe_port: u16) -> anyhow::Result<Vec<ReceiverInfo>> {
    let mut udp = HashMap::new();
    let mut mdns = HashMap::new();
    let (u, _) = tokio::join!(probe_udp(timeout, probe_port, &mut udp), probe_mdns(timeout, &mut mdns));
    u?;
    for (fp, info) in mdns {
        udp.entry(fp).or_insert(info);
    }
    let mut v: Vec<_> = udp.into_values().collect();
    v.sort_by(|a, b| a.name.cmp(&b.name));
    Ok(v)
}
```

Add `pub mod discovery; pub use discovery::*;` to `lib.rs`.

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-net`
Expected: all pass. Windows Firewall may prompt on first run for the test binary; allow it on private networks. The mDNS path is exercised only implicitly here; verify it by hand in Task 24.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(net): mDNS advertise/browse with UDP broadcast fallback"
```

---

### Task 16: Retransmit buffer and lossy loopback test

**Files:**
- Create: `crates/castr-net/src/retransmit.rs`
- Modify: `crates/castr-net/src/lib.rs`, `crates/castr-net/tests/loopback.rs`

**Interfaces:**
- Consumes: `castr_proto::{Nack, DatagramHeader}`.
- Produces:
  ```rust
  pub struct RetransmitBuffer { /* private */ }
  impl RetransmitBuffer {
      pub fn new(max_age_us: u64) -> Self;                            // spec: 500_000
      pub fn record(&mut self, frame_number: u32, keyframe: bool, fragments: Vec<Bytes>, sent_at_us: u64);
      /// Fragments to resend for this NACK, or empty if the frame is unknown, expired, or a delta older than one interval.
      pub fn lookup(&mut self, nack: &Nack, now_us: u64, frame_interval_us: u64) -> Vec<Bytes>;
      pub fn len(&self) -> usize; pub fn is_empty(&self) -> bool;
  }
  ```

- [ ] **Step 1: Write the failing unit tests**

`crates/castr-net/src/retransmit.rs`:
```rust
#[cfg(test)]
mod tests {
    use super::*;
    use castr_proto::Nack;

    fn frags(n: usize) -> Vec<Bytes> { (0..n).map(|i| Bytes::from(vec![i as u8; 10])).collect() }

    #[test]
    fn keyframe_fragments_are_resent_within_max_age() {
        let mut b = RetransmitBuffer::new(500_000);
        b.record(10, true, frags(4), 0);
        let out = b.lookup(&Nack { frame_number: 10, missing: vec![1, 3] }, 400_000, 33_333);
        assert_eq!(out.len(), 2);
        assert_eq!(out[0][0], 1);
        assert_eq!(out[1][0], 3);
    }

    #[test]
    fn delta_is_resent_only_within_one_interval() {
        let mut b = RetransmitBuffer::new(500_000);
        b.record(11, false, frags(2), 0);
        assert_eq!(b.lookup(&Nack { frame_number: 11, missing: vec![0] }, 33_333, 33_333).len(), 1);
        b.record(12, false, frags(2), 100_000);
        assert!(b.lookup(&Nack { frame_number: 12, missing: vec![0] }, 133_334, 33_333).is_empty());
    }

    #[test]
    fn expired_frames_are_pruned() {
        let mut b = RetransmitBuffer::new(500_000);
        b.record(1, true, frags(1), 0);
        b.record(2, true, frags(1), 100_000);
        assert!(b.lookup(&Nack { frame_number: 1, missing: vec![0] }, 500_001, 33_333).is_empty());
        assert_eq!(b.len(), 1);
    }

    #[test]
    fn unknown_frame_or_index_yields_nothing() {
        let mut b = RetransmitBuffer::new(500_000);
        b.record(1, true, frags(2), 0);
        assert!(b.lookup(&Nack { frame_number: 9, missing: vec![0] }, 0, 33_333).is_empty());
        assert!(b.lookup(&Nack { frame_number: 1, missing: vec![5] }, 0, 33_333).is_empty());
    }
}
```

- [ ] **Step 2: Write the failing lossy integration test**

Append to `crates/castr-net/tests/loopback.rs`:
```rust
#[tokio::test]
async fn nack_recovers_dropped_keyframe_fragment() {
    let (_, _, server, client) = pair();
    let addr = server.local_addr().unwrap();
    let (r, s) = tokio::join!(server.accept(), client.connect(addr));
    let (r, s) = (r.unwrap(), s.unwrap());

    // Drop fragment index 1 of frame 0 the first time it is sent.
    let dropped = Arc::new(std::sync::atomic::AtomicBool::new(false));
    let d2 = dropped.clone();
    s.set_send_filter(Some(Arc::new(move |d: &[u8]| {
        let (h, _) = DatagramHeader::decode(d).unwrap();
        if h.frame_number == 0 && h.fragment_index == 1 && !d2.swap(true, std::sync::atomic::Ordering::SeqCst) {
            return false;
        }
        true
    })));

    let mut packetizer = Packetizer::new();
    let mut rtx = RetransmitBuffer::new(500_000);
    let data: Vec<u8> = (0..3000).map(|i| (i % 251) as u8).collect();
    let frags = packetizer.packetize(STREAM_VIDEO, true, 0, &data, 1200);
    rtx.record(0, true, frags.clone(), 0);
    for f in &frags { s.send_datagram(f.clone()).unwrap(); }

    let mut reasm = Reassembler::new(500_000);
    let mut nack_tx = r.open_nack_stream().await.unwrap();
    let recv_task = async {
        loop {
            let d = tokio::time::timeout(std::time::Duration::from_millis(200), r.recv_datagram()).await;
            match d {
                Ok(Ok(d)) => { if let Some(f) = reasm.push(&d, 0).unwrap() { return f; } }
                _ => {
                    for n in reasm.tick(100_000) { nack_tx.send(&n).await.unwrap(); }
                }
            }
        }
    };
    let sender_task = async {
        let mut nack_rx = s.accept_nack_stream().await.unwrap();
        let n = nack_rx.recv().await.unwrap();
        assert_eq!(n, Nack { frame_number: 0, missing: vec![1] });
        for f in rtx.lookup(&n, 10_000, 33_333) { s.send_datagram(f).unwrap(); }
    };
    let (frame, _) = tokio::join!(recv_task, sender_task);
    assert_eq!(frame.data, data);
    assert!(dropped.load(std::sync::atomic::Ordering::SeqCst));
}
```

- [ ] **Step 3: Run tests to verify they fail**

Run: `cargo test -p castr-net`
Expected: compile error, `RetransmitBuffer` not found.

- [ ] **Step 4: Implement**

Top of `crates/castr-net/src/retransmit.rs`:
```rust
use bytes::Bytes;
use castr_proto::Nack;
use std::collections::VecDeque;

struct Sent {
    frame_number: u32,
    keyframe: bool,
    sent_at_us: u64,
    fragments: Vec<Bytes>,
}

pub struct RetransmitBuffer {
    max_age_us: u64,
    frames: VecDeque<Sent>,
}

impl RetransmitBuffer {
    pub fn new(max_age_us: u64) -> Self { Self { max_age_us, frames: VecDeque::new() } }
    pub fn len(&self) -> usize { self.frames.len() }
    pub fn is_empty(&self) -> bool { self.frames.is_empty() }

    fn prune(&mut self, now_us: u64) {
        while let Some(front) = self.frames.front() {
            if now_us.saturating_sub(front.sent_at_us) > self.max_age_us {
                self.frames.pop_front();
            } else {
                break;
            }
        }
    }

    pub fn record(&mut self, frame_number: u32, keyframe: bool, fragments: Vec<Bytes>, sent_at_us: u64) {
        self.frames.push_back(Sent { frame_number, keyframe, sent_at_us, fragments });
        self.prune(sent_at_us);
    }

    pub fn lookup(&mut self, nack: &Nack, now_us: u64, frame_interval_us: u64) -> Vec<Bytes> {
        self.prune(now_us);
        let Some(sent) = self.frames.iter().find(|s| s.frame_number == nack.frame_number) else { return Vec::new() };
        let young = now_us.saturating_sub(sent.sent_at_us) <= frame_interval_us;
        if !sent.keyframe && !young {
            return Vec::new();
        }
        nack.missing.iter().filter_map(|&i| sent.fragments.get(i as usize).cloned()).collect()
    }
}
```

Add `pub mod retransmit; pub use retransmit::*;` to `lib.rs`.

- [ ] **Step 5: Run tests**

Run: `cargo test -p castr-net`
Expected: all pass (7 unit, 8 integration).

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(net): retransmit ring buffer with lossy loopback NACK test"
```

---

### Task 17: Desktop Duplication capture (Windows)

**Files:**
- Create: `crates/castr-capture-win/Cargo.toml`, `crates/castr-capture-win/src/lib.rs`, `crates/castr-capture-win/src/dxgi.rs`

**Interfaces:**
- Consumes: `castr_media::{RawFrame, PixelFormat}`.
- Produces:
  ```rust
  pub struct DesktopCapture { /* private */ }
  impl DesktopCapture {
      pub fn new(output_index: u32) -> anyhow::Result<Self>;
      pub fn size(&self) -> (u32, u32);                                 // always even; odd desktop sizes are rounded down and the extra row/col cropped
      /// Waits up to `timeout_ms` for a new desktop frame. Ok(None) means nothing changed.
      /// Err with message containing "access lost" means the caller must call `new` again (resolution change, UAC prompt, lock screen).
      pub fn next_frame(&mut self, timeout_ms: u32, timestamp_us: u64) -> anyhow::Result<Option<RawFrame>>;
  }
  ```
  Output is BGRA with the row stride reported by D3D11 (may exceed width*4).

- [ ] **Step 1: Crate manifest and failing test**

`crates/castr-capture-win/Cargo.toml`:
```toml
[package]
name = "castr-capture-win"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
tracing.workspace = true
castr-media = { path = "../castr-media" }

[target.'cfg(windows)'.dependencies]
windows = { workspace = true, features = [
    "Win32_Foundation",
    "Win32_Graphics_Direct3D",
    "Win32_Graphics_Direct3D11",
    "Win32_Graphics_Dxgi",
    "Win32_Graphics_Dxgi_Common",
    "Win32_Media_Audio",
    "Win32_Media_KernelStreaming",
    "Win32_Media_Multimedia",
    "Win32_System_Com",
    "Win32_System_Com_StructuredStorage",
    "Win32_System_Variant",
] }
```

`crates/castr-capture-win/src/lib.rs`:
```rust
//! Windows-only capture: Desktop Duplication video and WASAPI loopback audio.
#![cfg(windows)]
pub mod dxgi;
pub use dxgi::DesktopCapture;
```

`crates/castr-capture-win/src/dxgi.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Needs an interactive desktop. Run: cargo test -p castr-capture-win -- --ignored
    #[test]
    #[ignore]
    fn captures_a_frame_with_even_dimensions() {
        let mut cap = DesktopCapture::new(0).unwrap();
        let (w, h) = cap.size();
        assert!(w % 2 == 0 && h % 2 == 0 && w >= 640 && h >= 480);
        let mut got = None;
        for _ in 0..50 {
            if let Some(f) = cap.next_frame(100, 1).unwrap() { got = Some(f); break; }
        }
        let f = got.expect("desktop produced a frame within 5 s");
        assert_eq!((f.width, f.height, f.format), (w, h, castr_media::PixelFormat::Bgra));
        assert!(f.stride >= w * 4);
        assert_eq!(f.data.len(), (f.stride * h) as usize);
        assert_eq!(f.timestamp_us, 1);
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p castr-capture-win -- --ignored`
Expected: compile error.

- [ ] **Step 3: Implement**

Top of `crates/castr-capture-win/src/dxgi.rs`:
```rust
use anyhow::{bail, Context};
use castr_media::{PixelFormat, RawFrame};
use windows::core::Interface;
use windows::Win32::Foundation::HMODULE;
use windows::Win32::Graphics::Direct3D::D3D_DRIVER_TYPE_HARDWARE;
use windows::Win32::Graphics::Direct3D11::*;
use windows::Win32::Graphics::Dxgi::Common::*;
use windows::Win32::Graphics::Dxgi::*;

pub struct DesktopCapture {
    _device: ID3D11Device,
    context: ID3D11DeviceContext,
    dup: IDXGIOutputDuplication,
    staging: ID3D11Texture2D,
    width: u32,
    height: u32,
}

impl DesktopCapture {
    pub fn new(output_index: u32) -> anyhow::Result<Self> {
        let mut device = None;
        let mut context = None;
        unsafe {
            D3D11CreateDevice(
                None, D3D_DRIVER_TYPE_HARDWARE, HMODULE::default(), D3D11_CREATE_DEVICE_BGRA_SUPPORT,
                None, D3D11_SDK_VERSION, Some(&mut device), None, Some(&mut context),
            ).context("D3D11CreateDevice")?;
        }
        let device = device.context("no D3D11 device")?;
        let context = context.context("no D3D11 context")?;
        let dxgi_device: IDXGIDevice = device.cast().context("IDXGIDevice")?;
        let adapter = unsafe { dxgi_device.GetAdapter() }.context("GetAdapter")?;
        let output = unsafe { adapter.EnumOutputs(output_index) }.context("EnumOutputs")?;
        let output1: IDXGIOutput1 = output.cast().context("IDXGIOutput1")?;
        let dup = unsafe { output1.DuplicateOutput(&device) }.context("DuplicateOutput (is another app already duplicating?)")?;
        let mut desc = DXGI_OUTDUPL_DESC::default();
        unsafe { dup.GetDesc(&mut desc) };
        let width = desc.ModeDesc.Width & !1;
        let height = desc.ModeDesc.Height & !1;
        let tex_desc = D3D11_TEXTURE2D_DESC {
            Width: width,
            Height: height,
            MipLevels: 1,
            ArraySize: 1,
            Format: DXGI_FORMAT_B8G8R8A8_UNORM,
            SampleDesc: DXGI_SAMPLE_DESC { Count: 1, Quality: 0 },
            Usage: D3D11_USAGE_STAGING,
            BindFlags: 0,
            CPUAccessFlags: D3D11_CPU_ACCESS_READ.0 as u32,
            MiscFlags: 0,
        };
        let mut staging = None;
        unsafe { device.CreateTexture2D(&tex_desc, None, Some(&mut staging)) }.context("CreateTexture2D staging")?;
        Ok(Self { _device: device, context, dup, staging: staging.context("no staging texture")?, width, height })
    }

    pub fn size(&self) -> (u32, u32) { (self.width, self.height) }

    pub fn next_frame(&mut self, timeout_ms: u32, timestamp_us: u64) -> anyhow::Result<Option<RawFrame>> {
        let mut info = DXGI_OUTDUPL_FRAME_INFO::default();
        let mut resource: Option<IDXGIResource> = None;
        if let Err(e) = unsafe { self.dup.AcquireNextFrame(timeout_ms, &mut info, &mut resource) } {
            if e.code() == DXGI_ERROR_WAIT_TIMEOUT {
                return Ok(None);
            }
            if e.code() == DXGI_ERROR_ACCESS_LOST {
                bail!("desktop duplication access lost");
            }
            return Err(e).context("AcquireNextFrame");
        }
        let result = (|| -> anyhow::Result<RawFrame> {
            let tex: ID3D11Texture2D = resource.as_ref().context("no resource")?.cast().context("ID3D11Texture2D")?;
            let src_box = D3D11_BOX { left: 0, top: 0, front: 0, right: self.width, bottom: self.height, back: 1 };
            unsafe { self.context.CopySubresourceRegion(&self.staging, 0, 0, 0, 0, &tex, 0, Some(&src_box)) };
            let mut mapped = D3D11_MAPPED_SUBRESOURCE::default();
            unsafe { self.context.Map(&self.staging, 0, D3D11_MAP_READ, 0, Some(&mut mapped)) }.context("Map staging")?;
            let stride = mapped.RowPitch;
            let len = (stride * self.height) as usize;
            let data = unsafe { std::slice::from_raw_parts(mapped.pData as *const u8, len) }.to_vec();
            unsafe { self.context.Unmap(&self.staging, 0) };
            Ok(RawFrame { format: PixelFormat::Bgra, width: self.width, height: self.height, stride, data, timestamp_us })
        })();
        let _ = unsafe { self.dup.ReleaseFrame() };
        result.map(Some)
    }
}
```

If the compiler reports that `BindFlags`, `CPUAccessFlags`, or `MiscFlags` expect a flag type rather than `u32`, use `D3D11_BIND_FLAG(0)`, `D3D11_CPU_ACCESS_READ`, and `D3D11_RESOURCE_MISC_FLAG(0)` respectively; the field types changed between `windows` releases and the compiler error names the expected type.

- [ ] **Step 4: Run the test**

Run: `cargo test -p castr-capture-win -- --ignored`
Expected: 1 passed. Move the mouse during the test if it times out; the desktop must change at least once.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(capture-win): Desktop Duplication frame capture"
```

---

### Task 18: WASAPI loopback capture (Windows)

**Files:**
- Create: `crates/castr-capture-win/src/wasapi.rs`
- Modify: `crates/castr-capture-win/src/lib.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct LoopbackCapture { /* private */ }
  impl LoopbackCapture {
      /// Opens the default render device in shared loopback mode, requesting 48 kHz stereo 16-bit with Windows' automatic format conversion.
      pub fn new() -> anyhow::Result<Self>;
      /// Drains whatever is available now into `out` as interleaved i16 stereo at 48 kHz. Non-blocking. Silence packets append zeros.
      pub fn drain(&mut self, out: &mut Vec<i16>) -> anyhow::Result<()>;
  }
  ```
  Caller polls `drain` every 5 ms from a dedicated thread. COM is initialized per thread by `new`; call `new` on the thread that will call `drain`.

- [ ] **Step 1: Write the failing test**

`crates/castr-capture-win/src/wasapi.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    /// Needs an audio render device. Run: cargo test -p castr-capture-win -- --ignored
    #[test]
    #[ignore]
    fn drains_audio_or_silence_for_200ms() {
        let mut cap = LoopbackCapture::new().unwrap();
        let mut out = Vec::new();
        for _ in 0..40 {
            cap.drain(&mut out).unwrap();
            std::thread::sleep(std::time::Duration::from_millis(5));
        }
        assert!(out.len() % 2 == 0);
        assert!(out.len() >= 48 * 2 * 150, "expected at least 150 ms of stereo samples, got {}", out.len() / 96);
    }
}
```

Add `pub mod wasapi; pub use wasapi::LoopbackCapture;` to `lib.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p castr-capture-win -- --ignored wasapi`
Expected: compile error.

- [ ] **Step 3: Implement**

Top of `crates/castr-capture-win/src/wasapi.rs`:
```rust
use anyhow::Context;
use windows::Win32::Media::Audio::*;
use windows::Win32::Media::Multimedia::WAVE_FORMAT_PCM;
use windows::Win32::System::Com::*;

const REFTIMES_PER_MS: i64 = 10_000;

pub struct LoopbackCapture {
    _client: IAudioClient,
    capture: IAudioCaptureClient,
}

impl LoopbackCapture {
    pub fn new() -> anyhow::Result<Self> {
        unsafe {
            let hr = CoInitializeEx(None, COINIT_MULTITHREADED);
            if hr.is_err() && hr != windows::Win32::Foundation::RPC_E_CHANGED_MODE {
                return Err(anyhow::anyhow!("CoInitializeEx: {hr:?}"));
            }
        }
        let enumerator: IMMDeviceEnumerator = unsafe { CoCreateInstance(&MMDeviceEnumerator, None, CLSCTX_ALL) }.context("MMDeviceEnumerator")?;
        let device = unsafe { enumerator.GetDefaultAudioEndpoint(eRender, eConsole) }.context("default render endpoint")?;
        let client: IAudioClient = unsafe { device.Activate(CLSCTX_ALL, None) }.context("IAudioClient")?;
        let format = WAVEFORMATEX {
            wFormatTag: WAVE_FORMAT_PCM as u16,
            nChannels: 2,
            nSamplesPerSec: 48_000,
            nAvgBytesPerSec: 48_000 * 4,
            nBlockAlign: 4,
            wBitsPerSample: 16,
            cbSize: 0,
        };
        let flags = AUDCLNT_STREAMFLAGS_LOOPBACK | AUDCLNT_STREAMFLAGS_AUTOCONVERTPCM | AUDCLNT_STREAMFLAGS_SRC_DEFAULT_QUALITY;
        unsafe {
            client.Initialize(AUDCLNT_SHAREMODE_SHARED, flags, 50 * REFTIMES_PER_MS, 0, &format, None)
                .context("IAudioClient::Initialize (48 kHz s16 stereo loopback with auto-convert)")?;
        }
        let capture: IAudioCaptureClient = unsafe { client.GetService() }.context("IAudioCaptureClient")?;
        unsafe { client.Start() }.context("IAudioClient::Start")?;
        Ok(Self { _client: client, capture })
    }

    pub fn drain(&mut self, out: &mut Vec<i16>) -> anyhow::Result<()> {
        loop {
            let packet = unsafe { self.capture.GetNextPacketSize() }.context("GetNextPacketSize")?;
            if packet == 0 {
                return Ok(());
            }
            let mut data: *mut u8 = std::ptr::null_mut();
            let mut frames = 0u32;
            let mut flags = 0u32;
            unsafe { self.capture.GetBuffer(&mut data, &mut frames, &mut flags, None, None) }.context("GetBuffer")?;
            let samples = (frames * 2) as usize;
            if flags & (AUDCLNT_BUFFERFLAGS_SILENT.0 as u32) != 0 || data.is_null() {
                out.extend(std::iter::repeat(0i16).take(samples));
            } else {
                let slice = unsafe { std::slice::from_raw_parts(data as *const i16, samples) };
                out.extend_from_slice(slice);
            }
            unsafe { self.capture.ReleaseBuffer(frames) }.context("ReleaseBuffer")?;
        }
    }
}
```

If `AUDCLNT_STREAMFLAGS_*` constants are `u32` in the resolved `windows` version, the `|` works as written; if they are a newtype, add `.0` to each and pass the `u32`. If `Initialize` fails with `AUDCLNT_E_UNSUPPORTED_FORMAT`, the device refuses auto-conversion; log the mix format from `client.GetMixFormat()` in the error so the next task can add a resampler, and treat audio as unavailable (the sender must still cast video).

- [ ] **Step 4: Run the test**

Run: `cargo test -p castr-capture-win -- --ignored wasapi`
Expected: 1 passed. Loopback produces silence packets when nothing is playing, so the test passes on a quiet machine too.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(capture-win): WASAPI loopback capture at 48 kHz s16"
```

---

### Task 19: Media Foundation H.264 encoder (Windows)

**Files:**
- Create: `crates/castr-codec-win/Cargo.toml`, `crates/castr-codec-win/src/lib.rs`, `crates/castr-codec-win/src/mf.rs`, `crates/castr-codec-win/src/encoder.rs`

**Interfaces:**
- Consumes: `castr_media::{VideoEncoder, EncoderConfig, RawFrame, EncodedFrame, PixelFormat, Mode}`.
- Produces:
  ```rust
  // mf.rs
  pub fn mf_startup() -> anyhow::Result<()>;                          // idempotent
  pub fn make_sample(data: &[u8], time_us: u64, duration_us: u64) -> anyhow::Result<IMFSample>;
  pub fn read_sample(sample: &IMFSample) -> anyhow::Result<Vec<u8>>;   // contiguous copy of all buffers
  pub fn video_type(subtype: &GUID, w: u32, h: u32, fps: u32, bitrate: Option<u32>) -> anyhow::Result<IMFMediaType>;
  pub fn find_transforms(category: GUID, input: &GUID, output: &GUID, hardware: bool) -> anyhow::Result<Vec<IMFActivate>>;
  // encoder.rs
  pub struct MfEncoder { /* private */ }
  impl MfEncoder { pub fn new(cfg: EncoderConfig) -> anyhow::Result<Self>; }
  impl VideoEncoder for MfEncoder { /* input_format = Nv12, name = "mf-hardware" or "mf-software" */ }
  ```
  Encoder selection: hardware MFTs first, then Microsoft's software encoder. Async MFTs (all hardware ones) are driven through `IMFMediaEventGenerator`; the software MFT is synchronous. `encode` returns the first output produced after this input, waiting up to 100 ms for async transforms; `None` if nothing came out yet.

- [ ] **Step 1: Crate manifest, lib, and failing test**

`crates/castr-codec-win/Cargo.toml`:
```toml
[package]
name = "castr-codec-win"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
tracing.workspace = true
castr-media = { path = "../castr-media" }

[target.'cfg(windows)'.dependencies]
windows = { workspace = true, features = [
    "Win32_Foundation",
    "Win32_Media_MediaFoundation",
    "Win32_System_Com",
    "Win32_System_Com_StructuredStorage",
    "Win32_System_Variant",
    "Win32_System_Ole",
] }
```

`crates/castr-codec-win/src/lib.rs`:
```rust
//! Media Foundation H.264 codecs. Windows only.
#![cfg(windows)]
pub mod mf;
pub mod encoder;
pub use encoder::MfEncoder;
```

`crates/castr-codec-win/tests/roundtrip.rs`:
```rust
use castr_codec_win::*;
use castr_media::sw::SwDecoder;
use castr_media::*;

fn cfg() -> EncoderConfig {
    EncoderConfig { width: 640, height: 360, fps: 30, bitrate_bps: 2_000_000, mode: Mode::Game }
}

fn frame(i: u32) -> RawFrame {
    let (w, h) = (640u32, 360u32);
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&[((x + i * 5) % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8, 255]);
        }
    }
    convert::convert(&RawFrame { format: PixelFormat::Bgra, width: w, height: h, stride: w * 4, data, timestamp_us: i as u64 * 33_333 }, PixelFormat::Nv12)
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
            if first_key.is_none() { first_key = Some(e.keyframe); }
            outputs += 1;
            assert!(e.data.starts_with(&[0, 0, 0, 1]) || e.data.starts_with(&[0, 0, 1]), "Annex B");
            if let Some(d) = dec.decode(&e.data, e.timestamp_us).unwrap() {
                assert_eq!((d.width, d.height), (640, 360));
            }
        }
    }
    assert!(outputs >= 25, "expected most inputs to produce output, got {outputs}");
    assert_eq!(first_key, Some(true), "first output must be a keyframe");
}

#[test]
fn mf_encoder_honors_keyframe_request_and_live_bitrate() {
    let mut enc = MfEncoder::new(cfg()).unwrap();
    let mut keys = 0;
    for i in 0..20 {
        if i == 10 { enc.request_keyframe(); }
        if i == 12 { enc.set_bitrate(800_000).unwrap(); }
        if let Some(e) = enc.encode(&frame(i)).unwrap() { if e.keyframe { keys += 1; } }
    }
    assert!(keys >= 2, "expected initial keyframe plus a requested one, got {keys}");
}
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p castr-codec-win`
Expected: compile error.

- [ ] **Step 3: Implement mf.rs**

`crates/castr-codec-win/src/mf.rs`:
```rust
use anyhow::Context;
use std::sync::Once;
use windows::core::GUID;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Com::{CoInitializeEx, CoTaskMemFree, COINIT_MULTITHREADED};

static START: Once = Once::new();

pub fn mf_startup() -> anyhow::Result<()> {
    let mut result = Ok(());
    START.call_once(|| {
        unsafe {
            let _ = CoInitializeEx(None, COINIT_MULTITHREADED);
            result = MFStartup(MF_VERSION, MFSTARTUP_FULL).context("MFStartup");
        }
    });
    result
}

pub fn make_sample(data: &[u8], time_us: u64, duration_us: u64) -> anyhow::Result<IMFSample> {
    unsafe {
        let buffer = MFCreateMemoryBuffer(data.len() as u32).context("MFCreateMemoryBuffer")?;
        let mut ptr = std::ptr::null_mut();
        buffer.Lock(&mut ptr, None, None).context("buffer Lock")?;
        std::ptr::copy_nonoverlapping(data.as_ptr(), ptr, data.len());
        buffer.Unlock().context("buffer Unlock")?;
        buffer.SetCurrentLength(data.len() as u32)?;
        let sample = MFCreateSample().context("MFCreateSample")?;
        sample.AddBuffer(&buffer)?;
        sample.SetSampleTime((time_us * 10) as i64)?;
        sample.SetSampleDuration((duration_us * 10) as i64)?;
        Ok(sample)
    }
}

pub fn read_sample(sample: &IMFSample) -> anyhow::Result<Vec<u8>> {
    unsafe {
        let buffer = sample.ConvertToContiguousBuffer().context("ConvertToContiguousBuffer")?;
        let mut ptr = std::ptr::null_mut();
        let mut len = 0u32;
        buffer.Lock(&mut ptr, None, Some(&mut len)).context("Lock")?;
        let out = std::slice::from_raw_parts(ptr, len as usize).to_vec();
        buffer.Unlock()?;
        Ok(out)
    }
}

pub fn video_type(subtype: &GUID, w: u32, h: u32, fps: u32, bitrate: Option<u32>) -> anyhow::Result<IMFMediaType> {
    unsafe {
        let t = MFCreateMediaType().context("MFCreateMediaType")?;
        t.SetGUID(&MF_MT_MAJOR_TYPE, &MFMediaType_Video)?;
        t.SetGUID(&MF_MT_SUBTYPE, subtype)?;
        t.SetUINT64(&MF_MT_FRAME_SIZE, ((w as u64) << 32) | h as u64)?;
        t.SetUINT64(&MF_MT_FRAME_RATE, ((fps as u64) << 32) | 1)?;
        t.SetUINT64(&MF_MT_PIXEL_ASPECT_RATIO, (1u64 << 32) | 1)?;
        t.SetUINT32(&MF_MT_INTERLACE_MODE, MFVideoInterlace_Progressive.0 as u32)?;
        if let Some(b) = bitrate {
            t.SetUINT32(&MF_MT_AVG_BITRATE, b)?;
        }
        if *subtype == MFVideoFormat_H264 {
            t.SetUINT32(&MF_MT_MPEG2_PROFILE, eAVEncH264VProfile_Main.0 as u32)?;
        }
        Ok(t)
    }
}

pub fn find_transforms(category: GUID, input: &GUID, output: &GUID, hardware: bool) -> anyhow::Result<Vec<IMFActivate>> {
    let input_info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: *input };
    let output_info = MFT_REGISTER_TYPE_INFO { guidMajorType: MFMediaType_Video, guidSubtype: *output };
    let flags = if hardware {
        MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER
    } else {
        MFT_ENUM_FLAG_SYNCMFT | MFT_ENUM_FLAG_ASYNCMFT | MFT_ENUM_FLAG_SORTANDFILTER
    };
    let mut activates: *mut Option<IMFActivate> = std::ptr::null_mut();
    let mut count = 0u32;
    unsafe {
        MFTEnumEx(category, flags, Some(&input_info), Some(&output_info), &mut activates, &mut count).context("MFTEnumEx")?;
        let mut out = Vec::new();
        if !activates.is_null() {
            for i in 0..count as usize {
                if let Some(a) = (*activates.add(i)).take() {
                    out.push(a);
                }
            }
            CoTaskMemFree(Some(activates as *const _));
        }
        Ok(out)
    }
}

pub fn transform_name(activate: &IMFActivate) -> String {
    unsafe {
        let mut ptr = windows::core::PWSTR::null();
        let mut len = 0u32;
        if activate.GetAllocatedString(&MFT_FRIENDLY_NAME_Attribute, &mut ptr, &mut len).is_ok() && !ptr.is_null() {
            let s = ptr.to_string().unwrap_or_default();
            CoTaskMemFree(Some(ptr.0 as *const _));
            s
        } else {
            "unnamed MFT".into()
        }
    }
}
```

- [ ] **Step 4: Implement encoder.rs**

`crates/castr-codec-win/src/encoder.rs`:
```rust
use crate::mf::*;
use anyhow::{anyhow, bail, Context};
use castr_media::*;
use std::time::{Duration, Instant};
use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::*;
use windows::Win32::System::Variant::VARIANT;

pub struct MfEncoder {
    cfg: EncoderConfig,
    mft: IMFTransform,
    events: Option<IMFMediaEventGenerator>,
    codec_api: Option<ICodecAPI>,
    provides_samples: bool,
    output_size: u32,
    need_input_credits: u32,
    frame_index: u64,
    name: &'static str,
}

fn set_codec_u32(api: &ICodecAPI, key: &windows::core::GUID, v: u32) -> anyhow::Result<()> {
    unsafe { api.SetValue(key, &VARIANT::from(v)) }.with_context(|| format!("ICodecAPI::SetValue {key:?}"))
}

fn set_codec_bool(api: &ICodecAPI, key: &windows::core::GUID, v: bool) -> anyhow::Result<()> {
    unsafe { api.SetValue(key, &VARIANT::from(v)) }.with_context(|| format!("ICodecAPI::SetValue {key:?}"))
}

impl MfEncoder {
    pub fn new(cfg: EncoderConfig) -> anyhow::Result<Self> {
        mf_startup()?;
        let mut candidates: Vec<(IMFActivate, &'static str)> = find_transforms(MFT_CATEGORY_VIDEO_ENCODER, &MFVideoFormat_NV12, &MFVideoFormat_H264, true)?
            .into_iter().map(|a| (a, "mf-hardware")).collect();
        candidates.extend(find_transforms(MFT_CATEGORY_VIDEO_ENCODER, &MFVideoFormat_NV12, &MFVideoFormat_H264, false)?
            .into_iter().map(|a| (a, "mf-software")));
        let mut last_err = anyhow!("no H.264 encoder MFT found");
        for (activate, name) in candidates {
            let friendly = transform_name(&activate);
            match Self::open(&activate, &cfg, name) {
                Ok(enc) => {
                    tracing::info!("using encoder {friendly} ({name})");
                    return Ok(enc);
                }
                Err(e) => {
                    tracing::warn!("encoder {friendly} rejected: {e:#}");
                    last_err = e;
                }
            }
        }
        Err(last_err)
    }

    fn open(activate: &IMFActivate, cfg: &EncoderConfig, name: &'static str) -> anyhow::Result<Self> {
        let mft: IMFTransform = unsafe { activate.ActivateObject() }.context("ActivateObject")?;
        let attrs = unsafe { mft.GetAttributes() }.ok();
        let is_async = attrs.as_ref().map(|a| unsafe { a.GetUINT32(&MF_TRANSFORM_ASYNC) }.unwrap_or(0) == 1).unwrap_or(false);
        if is_async {
            unsafe { attrs.as_ref().unwrap().SetUINT32(&MF_TRANSFORM_ASYNC_UNLOCK, 1) }.context("async unlock")?;
        }
        let codec_api: Option<ICodecAPI> = mft.cast().ok();
        if let Some(api) = &codec_api {
            let _ = set_codec_bool(api, &CODECAPI_AVLowLatencyMode, true);
            let _ = set_codec_u32(api, &CODECAPI_AVEncMPVDefaultBPictureCount, 0);
            Self::apply_mode(api, cfg);
        }
        let out_type = video_type(&MFVideoFormat_H264, cfg.width, cfg.height, cfg.fps, Some(cfg.bitrate_bps))?;
        unsafe { mft.SetOutputType(0, &out_type, 0) }.context("SetOutputType H264")?;
        let in_type = video_type(&MFVideoFormat_NV12, cfg.width, cfg.height, cfg.fps, None)?;
        unsafe { mft.SetInputType(0, &in_type, 0) }.context("SetInputType NV12")?;
        let info = unsafe { mft.GetOutputStreamInfo(0) }.context("GetOutputStreamInfo")?;
        let provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32 | MFT_OUTPUT_STREAM_CAN_PROVIDE_SAMPLES.0 as u32) != 0;
        unsafe {
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        let events = if is_async { Some(mft.cast::<IMFMediaEventGenerator>().context("IMFMediaEventGenerator")?) } else { None };
        Ok(Self {
            cfg: *cfg, mft, events, codec_api, provides_samples,
            output_size: info.cbSize.max(1 << 20), need_input_credits: 0, frame_index: 0, name,
        })
    }

    fn apply_mode(api: &ICodecAPI, cfg: &EncoderConfig) {
        let (rc, gop) = match cfg.mode {
            Mode::Game => (eAVEncCommonRateControlMode_CBR.0 as u32, cfg.fps * 10),
            Mode::Quality => (eAVEncCommonRateControlMode_UnconstrainedVBR.0 as u32, cfg.fps * 2),
        };
        let _ = set_codec_u32(api, &CODECAPI_AVEncCommonRateControlMode, rc);
        let _ = set_codec_u32(api, &CODECAPI_AVEncMPVGOPSize, gop);
        let _ = set_codec_u32(api, &CODECAPI_AVEncCommonMeanBitRate, cfg.bitrate_bps);
    }

    /// Async MFTs: pump events, counting NeedInput credits, returning true when HaveOutput is seen.
    fn pump_events(&mut self, wait: Duration) -> anyhow::Result<bool> {
        let Some(gen) = &self.events else { return Ok(true) };
        let deadline = Instant::now() + wait;
        loop {
            match unsafe { gen.GetEvent(MF_EVENT_FLAG_NO_WAIT) } {
                Ok(ev) => {
                    let t = unsafe { ev.GetType() }?;
                    if t == METransformNeedInput.0 as u32 {
                        self.need_input_credits += 1;
                    } else if t == METransformHaveOutput.0 as u32 {
                        return Ok(true);
                    }
                }
                Err(_) => {
                    if Instant::now() >= deadline {
                        return Ok(false);
                    }
                    std::thread::sleep(Duration::from_millis(1));
                }
            }
        }
    }

    fn wait_need_input(&mut self) -> anyhow::Result<()> {
        if self.events.is_none() { return Ok(()); }
        let deadline = Instant::now() + Duration::from_millis(200);
        while self.need_input_credits == 0 {
            if self.pump_events(Duration::from_millis(5))? {
                // Output arrived before we could feed input; drain it so the MFT keeps going.
                let _ = self.take_output()?;
            }
            if Instant::now() >= deadline {
                bail!("encoder never requested input");
            }
        }
        self.need_input_credits -= 1;
        Ok(())
    }

    fn take_output(&mut self) -> anyhow::Result<Option<EncodedFrame>> {
        let mut out = MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: std::mem::ManuallyDrop::new(None), dwStatus: 0, pEvents: std::mem::ManuallyDrop::new(None) };
        if !self.provides_samples {
            let sample = make_sample(&vec![0u8; self.output_size as usize], 0, 0)?;
            unsafe { sample.GetBufferByIndex(0)?.SetCurrentLength(0)? };
            out.pSample = std::mem::ManuallyDrop::new(Some(sample));
        }
        let mut status = 0u32;
        let hr = unsafe { self.mft.ProcessOutput(0, std::slice::from_mut(&mut out), &mut status) };
        let sample = unsafe { std::mem::ManuallyDrop::take(&mut out.pSample) };
        if let Some(ev) = unsafe { std::mem::ManuallyDrop::take(&mut out.pEvents) } { drop(ev); }
        match hr {
            Ok(()) => {
                let sample = sample.ok_or_else(|| anyhow!("ProcessOutput returned no sample"))?;
                let data = read_sample(&sample)?;
                if data.is_empty() { return Ok(None); }
                let keyframe = unsafe { sample.GetUINT32(&MFSampleExtension_CleanPoint) }.unwrap_or(0) == 1;
                let time_us = unsafe { sample.GetSampleTime() }.unwrap_or(0).max(0) as u64 / 10;
                Ok(Some(EncodedFrame { data, keyframe, timestamp_us: time_us }))
            }
            Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => Ok(None),
            Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                let t = unsafe { self.mft.GetOutputAvailableType(0, 0) }?;
                unsafe { self.mft.SetOutputType(0, &t, 0) }?;
                Ok(None)
            }
            Err(e) => Err(e).context("ProcessOutput"),
        }
    }
}

impl VideoEncoder for MfEncoder {
    fn encode(&mut self, frame: &RawFrame) -> anyhow::Result<Option<EncodedFrame>> {
        anyhow::ensure!(frame.format == PixelFormat::Nv12, "MfEncoder expects NV12");
        anyhow::ensure!(frame.width == self.cfg.width && frame.height == self.cfg.height, "frame size mismatch");
        let duration_us = 1_000_000 / self.cfg.fps as u64;
        let sample = make_sample(&frame.data, frame.timestamp_us, duration_us)?;
        self.wait_need_input()?;
        unsafe { self.mft.ProcessInput(0, &sample, 0) }.context("ProcessInput")?;
        self.frame_index += 1;
        if self.events.is_some() {
            if !self.pump_events(Duration::from_millis(100))? {
                return Ok(None);
            }
        }
        self.take_output()
    }

    fn request_keyframe(&mut self) {
        if let Some(api) = &self.codec_api {
            let _ = set_codec_u32(api, &CODECAPI_AVEncVideoForceKeyFrame, 1);
        }
    }

    fn set_bitrate(&mut self, bitrate_bps: u32) -> anyhow::Result<()> {
        self.cfg.bitrate_bps = bitrate_bps;
        let api = self.codec_api.as_ref().ok_or_else(|| anyhow!("encoder exposes no ICodecAPI"))?;
        set_codec_u32(api, &CODECAPI_AVEncCommonMeanBitRate, bitrate_bps)
    }

    fn set_mode(&mut self, mode: Mode) -> anyhow::Result<()> {
        self.cfg.mode = mode;
        if let Some(api) = &self.codec_api {
            Self::apply_mode(api, &self.cfg);
        }
        self.request_keyframe();
        Ok(())
    }

    fn input_format(&self) -> PixelFormat { PixelFormat::Nv12 }
    fn name(&self) -> &'static str { self.name }
}
```

If `VARIANT::from(u32)` or `VARIANT::from(bool)` is not implemented in the resolved `windows` version, construct the variant through `windows::Win32::System::Variant::VariantInit` and set `Anonymous.Anonymous.vt = VT_UI4` / `VT_BOOL` with the value in `Anonymous.Anonymous.Anonymous.ulVal` / `.boolVal` (`VARIANT_TRUE` is `-1`). The `MFT_OUTPUT_DATA_BUFFER` field wrappers (`ManuallyDrop`) also vary by version; the compiler error names the exact field types.

- [ ] **Step 5: Run tests**

Run: `cargo test -p castr-codec-win`
Expected: 2 passed, with a log line naming the encoder MFT chosen. If the hardware MFT is selected and `mf_encoder_output_decodes_with_openh264` reports fewer than 25 outputs, raise the wait in `pump_events` from 100 ms to 200 ms for the first 3 frames only (hardware encoders warm up), and keep the test unchanged.

- [ ] **Step 6: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(codec-win): Media Foundation H.264 encoder with hardware MFT selection"
```

---

### Task 20: Media Foundation H.264 decoder (Windows)

**Files:**
- Create: `crates/castr-codec-win/src/decoder.rs`
- Modify: `crates/castr-codec-win/src/lib.rs`, `crates/castr-codec-win/tests/roundtrip.rs`

**Interfaces:**
- Produces:
  ```rust
  pub struct MfDecoder { /* private */ }
  impl MfDecoder { pub fn new() -> anyhow::Result<Self>; }
  impl VideoDecoder for MfDecoder { /* outputs NV12 RawFrame, name = "mf-h264" */ }
  ```
  Uses the Microsoft H.264 decoder MFT in low-latency mode with CPU-visible NV12 output. (GPU-resident output via a D3D11 device manager is an optimization; SDL uploads from CPU memory either way.)

- [ ] **Step 1: Write the failing test**

Append to `crates/castr-codec-win/tests/roundtrip.rs`:
```rust
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
            if dec.decode(&e.data, e.timestamp_us).unwrap().is_some() { decoded += 1; }
        }
    }
    assert!(decoded >= 20, "decoded {decoded}");
}
```

Add `pub mod decoder; pub use decoder::MfDecoder;` to `lib.rs`.

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p castr-codec-win decoder`
Expected: compile error.

- [ ] **Step 3: Implement**

`crates/castr-codec-win/src/decoder.rs`:
```rust
use crate::mf::*;
use anyhow::{anyhow, Context};
use castr_media::*;
use windows::core::Interface;
use windows::Win32::Media::MediaFoundation::*;

pub struct MfDecoder {
    mft: IMFTransform,
    width: u32,
    height: u32,
    output_size: u32,
    provides_samples: bool,
}

impl MfDecoder {
    pub fn new() -> anyhow::Result<Self> {
        mf_startup()?;
        let activates = find_transforms(MFT_CATEGORY_VIDEO_DECODER, &MFVideoFormat_H264, &MFVideoFormat_NV12, false)?;
        let activate = activates.first().ok_or_else(|| anyhow!("no H.264 decoder MFT"))?;
        tracing::info!("using decoder {}", transform_name(activate));
        let mft: IMFTransform = unsafe { activate.ActivateObject() }.context("ActivateObject")?;
        if let Ok(attrs) = unsafe { mft.GetAttributes() } {
            let _ = unsafe { attrs.SetUINT32(&CODECAPI_AVLowLatencyMode, 1) };
        }
        let in_type = video_type(&MFVideoFormat_H264, 1920, 1080, 30, None)?;
        unsafe { mft.SetInputType(0, &in_type, 0) }.context("SetInputType H264")?;
        let mut dec = Self { mft, width: 0, height: 0, output_size: 0, provides_samples: false };
        dec.negotiate_output()?;
        unsafe {
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_BEGIN_STREAMING, 0)?;
            dec.mft.ProcessMessage(MFT_MESSAGE_NOTIFY_START_OF_STREAM, 0)?;
        }
        Ok(dec)
    }

    fn negotiate_output(&mut self) -> anyhow::Result<()> {
        let mut i = 0;
        loop {
            let t = unsafe { self.mft.GetOutputAvailableType(0, i) }.context("no NV12 output type offered")?;
            let sub = unsafe { t.GetGUID(&MF_MT_SUBTYPE) }?;
            if sub == MFVideoFormat_NV12 {
                unsafe { self.mft.SetOutputType(0, &t, 0) }.context("SetOutputType NV12")?;
                let size = unsafe { t.GetUINT64(&MF_MT_FRAME_SIZE) }?;
                self.width = (size >> 32) as u32;
                self.height = (size & 0xFFFF_FFFF) as u32;
                let info = unsafe { self.mft.GetOutputStreamInfo(0) }?;
                self.output_size = info.cbSize.max(self.width * self.height * 3 / 2);
                self.provides_samples = info.dwFlags & (MFT_OUTPUT_STREAM_PROVIDES_SAMPLES.0 as u32) != 0;
                return Ok(());
            }
            i += 1;
        }
    }

    fn read_nv12(&self, sample: &IMFSample) -> anyhow::Result<Vec<u8>> {
        let buffer = unsafe { sample.GetBufferByIndex(0) }?;
        let (w, h) = (self.width as usize, self.height as usize);
        let mut out = vec![0u8; w * h * 3 / 2];
        if let Ok(b2d) = buffer.cast::<IMF2DBuffer>() {
            let mut scanline0 = std::ptr::null_mut();
            let mut pitch = 0i32;
            unsafe { b2d.Lock2D(&mut scanline0, &mut pitch) }.context("Lock2D")?;
            let pitch = pitch.unsigned_abs() as usize;
            for row in 0..h * 3 / 2 {
                let src = unsafe { std::slice::from_raw_parts(scanline0.add(row * pitch), w) };
                out[row * w..row * w + w].copy_from_slice(src);
            }
            unsafe { b2d.Unlock2D() }?;
        } else {
            let data = read_sample(sample)?;
            let n = out.len().min(data.len());
            out[..n].copy_from_slice(&data[..n]);
        }
        Ok(out)
    }
}

impl VideoDecoder for MfDecoder {
    fn decode(&mut self, data: &[u8], timestamp_us: u64) -> anyhow::Result<Option<RawFrame>> {
        let sample = make_sample(data, timestamp_us, 33_333)?;
        unsafe { self.mft.ProcessInput(0, &sample, 0) }.context("ProcessInput")?;
        loop {
            let mut out = MFT_OUTPUT_DATA_BUFFER { dwStreamID: 0, pSample: std::mem::ManuallyDrop::new(None), dwStatus: 0, pEvents: std::mem::ManuallyDrop::new(None) };
            if !self.provides_samples {
                let s = make_sample(&vec![0u8; self.output_size as usize], 0, 0)?;
                unsafe { s.GetBufferByIndex(0)?.SetCurrentLength(0)? };
                out.pSample = std::mem::ManuallyDrop::new(Some(s));
            }
            let mut status = 0u32;
            let hr = unsafe { self.mft.ProcessOutput(0, std::slice::from_mut(&mut out), &mut status) };
            let sample = unsafe { std::mem::ManuallyDrop::take(&mut out.pSample) };
            if let Some(ev) = unsafe { std::mem::ManuallyDrop::take(&mut out.pEvents) } { drop(ev); }
            match hr {
                Ok(()) => {
                    let sample = sample.ok_or_else(|| anyhow!("no output sample"))?;
                    let ts = unsafe { sample.GetSampleTime() }.map(|t| t.max(0) as u64 / 10).unwrap_or(timestamp_us);
                    let data = self.read_nv12(&sample)?;
                    return Ok(Some(RawFrame { format: PixelFormat::Nv12, width: self.width, height: self.height, stride: self.width, data, timestamp_us: ts }));
                }
                Err(e) if e.code() == MF_E_TRANSFORM_NEED_MORE_INPUT => return Ok(None),
                Err(e) if e.code() == MF_E_TRANSFORM_STREAM_CHANGE => {
                    self.negotiate_output()?;
                    continue;
                }
                Err(e) => return Err(e).context("ProcessOutput"),
            }
        }
    }

    fn name(&self) -> &'static str { "mf-h264" }
}
```

- [ ] **Step 4: Run tests**

Run: `cargo test -p castr-codec-win`
Expected: 4 passed. The decoder holds one or two frames internally even in low-latency mode, which is why the tests accept 20 of 30.

- [ ] **Step 5: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(codec-win): Media Foundation H.264 decoder"
```

---

### Task 21: Receiver binary

**Files:**
- Create: `crates/castr-receiver/Cargo.toml`, `crates/castr-receiver/src/main.rs`, `crates/castr-receiver/src/render.rs`, `crates/castr-receiver/src/audio_out.rs`, `crates/castr-receiver/src/pipeline.rs`

**Interfaces:**
- Consumes: everything above.
- Produces: the `castr-receiver` executable. Application-level handshake (shared with Task 22):
  1. Sender connects (TLS pins the receiver fingerprint learned from discovery; receiver accepts any client certificate at TLS level).
  2. Sender sends `Hello`. If the sender's fingerprint is not in the receiver's paired store, the receiver replies `Error { code: 4, message: "pairing required" }` and then waits for `PairInit` and runs `pair_as_receiver` with a fresh PIN shown in the window title and console. After `PairOk` the sender sends `Hello` again.
  3. Paired `Hello` is fed to `ReceiverSession`; the receiver performs the returned actions. Streaming begins on `StartStream`.
  4. On connection loss the receiver keeps the session for 60 s and shows a "Reconnecting" overlay; a `Hello` carrying the session token resumes.
- `audio_out.rs` also exposes `pub fn resample_linear(input: &[i16], ratio: f64) -> Vec<i16>` (stereo interleaved; ratio > 1 shortens output) which is unit tested.

- [ ] **Step 1: Manifest and failing unit test for the resampler**

`crates/castr-receiver/Cargo.toml`:
```toml
[package]
name = "castr-receiver"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
bytes.workspace = true
clap.workspace = true
hex.workspace = true
rand.workspace = true
tokio.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
sdl2.workspace = true
castr-proto = { path = "../castr-proto" }
castr-net = { path = "../castr-net" }
castr-media = { path = "../castr-media" }

[target.'cfg(windows)'.dependencies]
castr-codec-win = { path = "../castr-codec-win" }
```

`crates/castr-receiver/src/audio_out.rs` tests:
```rust
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
        assert!(fast.len() < input.len() && fast.len() % 2 == 0);
        assert!(slow.len() > input.len() && slow.len() % 2 == 0);
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
```

- [ ] **Step 2: Run to verify it fails**

Run: `cargo test -p castr-receiver`
Expected: compile error (no main.rs yet). Create `src/main.rs` containing only `mod audio_out; fn main() {}` and re-run: `resample_linear` not found.

- [ ] **Step 3: Implement audio_out.rs**

`crates/castr-receiver/src/audio_out.rs`:
```rust
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
        let spec = AudioSpecDesired { freq: Some(SAMPLE_RATE as i32), channels: Some(CHANNELS as u8), samples: Some(480) };
        let queue = audio.open_queue::<i16, _>(None, &spec).map_err(|e| anyhow::anyhow!(e)).context("open audio queue")?;
        queue.resume();
        Ok(Self { queue, decoder: AudioDecoder::new()?, target_us })
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
        self.queue.queue_audio(&pcm).map_err(|e| anyhow::anyhow!(e))?;
        Ok(true)
    }

    pub fn conceal_one(&mut self) -> anyhow::Result<()> {
        let pcm = self.decoder.decode(None)?;
        self.queue.queue_audio(&pcm).map_err(|e| anyhow::anyhow!(e))?;
        Ok(())
    }

    pub fn clear(&mut self) { self.queue.clear(); }
}
```

- [ ] **Step 4: Run the unit tests**

Run: `cargo test -p castr-receiver`
Expected: 3 passed (SDL links statically; first build takes several minutes).

- [ ] **Step 5: Implement render.rs**

`crates/castr-receiver/src/render.rs`:
```rust
use anyhow::{anyhow, Context};
use castr_media::{PixelFormat, RawFrame};
use sdl2::event::Event;
use sdl2::keyboard::Keycode;
use sdl2::pixels::{Color, PixelFormatEnum};
use sdl2::rect::Rect;
use sdl2::render::{Texture, TextureCreator, WindowCanvas};
use sdl2::video::WindowContext;
use sdl2::{EventPump, Sdl};

pub struct Renderer {
    pub sdl: Sdl,
    canvas: WindowCanvas,
    creator: TextureCreator<WindowContext>,
    texture: Option<Texture>,
    tex_desc: (u32, u32, PixelFormat),
    overlay: Option<String>,
    events: EventPump,
    base_title: String,
    pulse: u32,
}

impl Renderer {
    pub fn new(title: &str, fullscreen: bool) -> anyhow::Result<Self> {
        let sdl = sdl2::init().map_err(|e| anyhow!(e))?;
        let video = sdl.video().map_err(|e| anyhow!(e))?;
        let mut builder = video.window(title, 1280, 720);
        builder.position_centered().resizable();
        if fullscreen { builder.fullscreen_desktop(); }
        let window = builder.build().context("create window")?;
        let canvas = window.into_canvas().accelerated().present_vsync().build().context("create canvas")?;
        let creator = canvas.texture_creator();
        let events = sdl.event_pump().map_err(|e| anyhow!(e))?;
        Ok(Self { sdl, canvas, creator, texture: None, tex_desc: (0, 0, PixelFormat::I420), overlay: None, events, base_title: title.to_string(), pulse: 0 })
    }

    pub fn set_overlay(&mut self, text: Option<&str>) {
        self.overlay = text.map(|s| s.to_string());
        let title = match &self.overlay { Some(t) => format!("{} - {}", self.base_title, t), None => self.base_title.clone() };
        let _ = self.canvas.window_mut().set_title(&title);
    }

    /// Pumps SDL events. Returns true when the user asked to quit (window close or Escape).
    pub fn poll_quit(&mut self) -> bool {
        for ev in self.events.poll_iter() {
            match ev {
                Event::Quit { .. } => return true,
                Event::KeyDown { keycode: Some(Keycode::Escape), .. } => return true,
                _ => {}
            }
        }
        false
    }

    fn ensure_texture(&mut self, f: &RawFrame) -> anyhow::Result<()> {
        if self.texture.is_some() && self.tex_desc == (f.width, f.height, f.format) {
            return Ok(());
        }
        let fmt = match f.format {
            PixelFormat::I420 => PixelFormatEnum::IYUV,
            PixelFormat::Nv12 => PixelFormatEnum::NV12,
            PixelFormat::Bgra => PixelFormatEnum::ARGB8888,
        };
        let tex = self.creator.create_texture_streaming(fmt, f.width, f.height).context("create texture")?;
        self.texture = Some(tex);
        self.tex_desc = (f.width, f.height, f.format);
        Ok(())
    }

    pub fn present(&mut self, f: &RawFrame) -> anyhow::Result<()> {
        self.ensure_texture(f)?;
        let (w, h) = (f.width as usize, f.height as usize);
        let tex = self.texture.as_mut().unwrap();
        match f.format {
            PixelFormat::I420 => {
                let y = &f.data[..w * h];
                let u = &f.data[w * h..w * h + w * h / 4];
                let v = &f.data[w * h + w * h / 4..];
                tex.update_yuv(None, y, w, u, w / 2, v, w / 2).map_err(|e| anyhow!(e))?;
            }
            PixelFormat::Nv12 => tex.update(None, &f.data, w).map_err(|e| anyhow!(e))?,
            PixelFormat::Bgra => tex.update(None, &f.data, f.stride as usize).map_err(|e| anyhow!(e))?,
        }
        self.draw()
    }

    pub fn redraw(&mut self) -> anyhow::Result<()> { self.draw() }

    fn draw(&mut self) -> anyhow::Result<()> {
        self.canvas.set_draw_color(Color::RGB(0, 0, 0));
        self.canvas.clear();
        let (ww, wh) = self.canvas.output_size().map_err(|e| anyhow!(e))?;
        if let Some(tex) = &self.texture {
            let (tw, th, _) = self.tex_desc;
            let scale = (ww as f64 / tw as f64).min(wh as f64 / th as f64);
            let dw = (tw as f64 * scale) as u32;
            let dh = (th as f64 * scale) as u32;
            let dst = Rect::new(((ww - dw) / 2) as i32, ((wh - dh) / 2) as i32, dw, dh);
            self.canvas.copy(tex, None, dst).map_err(|e| anyhow!(e))?;
        }
        if self.overlay.is_some() {
            self.pulse = self.pulse.wrapping_add(1);
            self.canvas.set_blend_mode(sdl2::render::BlendMode::Blend);
            self.canvas.set_draw_color(Color::RGBA(0, 0, 0, 140));
            self.canvas.fill_rect(Rect::new(0, 0, ww, wh)).map_err(|e| anyhow!(e))?;
            let bar_w = ww / 3;
            let x = ((self.pulse * 8) % (ww + bar_w)) as i32 - bar_w as i32;
            self.canvas.set_draw_color(Color::RGBA(255, 255, 255, 200));
            self.canvas.fill_rect(Rect::new(x, (wh / 2) as i32 - 4, bar_w, 8)).map_err(|e| anyhow!(e))?;
        }
        self.canvas.present();
        Ok(())
    }
}
```

- [ ] **Step 6: Implement pipeline.rs**

`crates/castr-receiver/src/pipeline.rs`:
```rust
use crate::audio_out::AudioOut;
use crate::render::Renderer;
use anyhow::{anyhow, Context};
use castr_media::clock::AvClock;
use castr_media::jitter::JitterBuffer;
use castr_media::{sw::SwDecoder, RawFrame, VideoDecoder};
use castr_net::*;
use castr_proto::*;
use std::net::SocketAddr;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};
use tokio::sync::mpsc;

#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum DecoderChoice { Auto, Mf, Sw }

pub struct ReceiverConfig {
    pub name: String,
    pub fullscreen: bool,
    pub max_bitrate: u32,
    pub decoder: DecoderChoice,
    pub bind: SocketAddr,
    pub config_dir: PathBuf,
}

/// Messages from the network side to the SDL main thread.
pub enum UiEvent {
    Overlay(Option<String>),
    Frame(RawFrame),
    AudioPacket { ts_us: u64, data: Vec<u8> },
    Mode(Mode),
    Quit,
}

fn now_us(start: Instant) -> u64 { start.elapsed().as_micros() as u64 }

fn open_decoder(choice: DecoderChoice) -> anyhow::Result<Box<dyn VideoDecoder>> {
    #[cfg(windows)]
    {
        if matches!(choice, DecoderChoice::Auto | DecoderChoice::Mf) {
            match castr_codec_win::MfDecoder::new() {
                Ok(d) => return Ok(Box::new(d)),
                Err(e) if choice == DecoderChoice::Mf => return Err(e),
                Err(e) => tracing::warn!("MF decoder unavailable, falling back to openh264: {e:#}"),
            }
        }
    }
    #[cfg(not(windows))]
    {
        if choice == DecoderChoice::Mf { anyhow::bail!("Media Foundation is Windows only"); }
    }
    Ok(Box::new(SwDecoder::new()?))
}

pub fn run(cfg: ReceiverConfig) -> anyhow::Result<()> {
    let start = Instant::now();
    let id = Identity::load_or_create(&cfg.config_dir)?;
    tracing::info!("receiver '{}' fingerprint {}", cfg.name, id.fingerprint_hex());
    let paired = Arc::new(Mutex::new(PairedStore::load(cfg.config_dir.join("paired.toml"))?));
    let caps = Capabilities { max_width: 1920, max_height: 1080, max_fps: 60, max_bitrate_bps: cfg.max_bitrate, codecs: vec![Codec::H264], audio: true };

    let mut renderer = Renderer::new(&format!("castr - {}", cfg.name), cfg.fullscreen)?;
    let audio_sys = renderer.sdl.audio().map_err(|e| anyhow!(e))?;
    let mut audio = AudioOut::new(&audio_sys, 40_000)?;
    let mut clock = AvClock::new();

    let (ui_tx, mut ui_rx) = mpsc::channel::<UiEvent>(64);
    let jitter = Arc::new(Mutex::new(JitterBuffer::new(Mode::Game, 33_333)));
    let stats = Arc::new(Mutex::new(Stats::default()));

    // Decode thread: jitter buffer -> decoder -> UI.
    {
        let jitter = jitter.clone();
        let ui = ui_tx.clone();
        let stats = stats.clone();
        let choice = cfg.decoder;
        std::thread::Builder::new().name("decode".into()).spawn(move || {
            let mut decoder = match open_decoder(choice) {
                Ok(d) => d,
                Err(e) => { tracing::error!("decoder init failed: {e:#}"); let _ = ui.blocking_send(UiEvent::Quit); return; }
            };
            tracing::info!("decoder: {}", decoder.name());
            loop {
                let frame = jitter.lock().unwrap().pop(now_us(start));
                let Some(f) = frame else { std::thread::sleep(Duration::from_millis(2)); continue; };
                stats.lock().unwrap().decode_queue_depth = jitter.lock().unwrap().depth() as u32;
                match decoder.decode(&f.data, f.timestamp_us) {
                    Ok(Some(raw)) => { if ui.blocking_send(UiEvent::Frame(raw)).is_err() { return; } }
                    Ok(None) => {}
                    Err(e) => tracing::warn!("decode error: {e:#}"),
                }
            }
        })?;
    }

    // Network runtime.
    let rt = tokio::runtime::Runtime::new()?;
    let net_cfg = NetConfig { name: cfg.name.clone(), bind: cfg.bind, caps, id, paired, jitter: jitter.clone(), stats: stats.clone(), ui: ui_tx, start };
    rt.spawn(async move {
        if let Err(e) = network_main(net_cfg).await {
            tracing::error!("network: {e:#}");
        }
    });

    // SDL main loop.
    let mut pending: Option<RawFrame> = None;
    let mut last_video = Instant::now();
    loop {
        if renderer.poll_quit() { break; }
        while let Ok(ev) = ui_rx.try_recv() {
            match ev {
                UiEvent::Overlay(t) => renderer.set_overlay(t.as_deref()),
                UiEvent::Frame(f) => pending = Some(f),
                UiEvent::AudioPacket { ts_us, data } if data.is_empty() => {
                    // Lost packet: let Opus conceal it so the clock keeps advancing smoothly.
                    let _ = audio.conceal_one();
                    let _ = ts_us;
                }
                UiEvent::AudioPacket { ts_us, data } => {
                    let ratio = clock.drift_ratio(audio.buffered_us(), audio.target_us);
                    if audio.push_packet(&data, ratio).unwrap_or(false) {
                        let played_ts = ts_us.saturating_sub(audio.buffered_us());
                        clock.audio_played(played_ts, now_us(start));
                    }
                }
                UiEvent::Mode(m) => {
                    audio.target_us = match m { Mode::Game => 40_000, Mode::Quality => 100_000 };
                    audio.clear();
                }
                UiEvent::Quit => return Err(anyhow!("fatal error, see log")),
            }
        }
        if let Some(f) = pending.take() {
            if clock.video_due(f.timestamp_us, now_us(start)) {
                renderer.present(&f)?;
                last_video = Instant::now();
            } else {
                pending = Some(f);
            }
        }
        if last_video.elapsed() > Duration::from_millis(50) {
            renderer.redraw()?;
            last_video = Instant::now();
        }
        std::thread::sleep(Duration::from_millis(1));
    }
    rt.shutdown_background();
    Ok(())
}

struct NetConfig {
    name: String,
    bind: SocketAddr,
    caps: Capabilities,
    id: Identity,
    paired: Arc<Mutex<PairedStore>>,
    jitter: Arc<Mutex<JitterBuffer>>,
    stats: Arc<Mutex<Stats>>,
    ui: mpsc::Sender<UiEvent>,
    start: Instant,
}

async fn network_main(cfg: NetConfig) -> anyhow::Result<()> {
    let endpoint = Endpoint::server(cfg.bind, &cfg.id, accept_any())?;
    let port = endpoint.local_addr()?.port();
    let _adv = Advertiser::start(&cfg.name, cfg.id.fingerprint, port, PROBE_PORT).await?;
    tracing::info!("listening on {} (QUIC), probe port {}", endpoint.local_addr()?, PROBE_PORT);
    let mut session = ReceiverSession::new(cfg.name.clone(), cfg.caps.clone(), rand::random());
    cfg.ui.send(UiEvent::Overlay(Some("Waiting for sender".into()))).await.ok();
    loop {
        let link = endpoint.accept().await?;
        tracing::info!("connection from {} fp {}", link.remote_addr(), hex_short(&link.peer_fingerprint()));
        if matches!(session.state(), ReceiverState::Closed) {
            session = ReceiverSession::new(cfg.name.clone(), cfg.caps.clone(), rand::random());
        }
        match handle_connection(&cfg, &link, &mut session).await {
            Ok(()) => tracing::info!("session ended"),
            Err(e) => tracing::warn!("connection error: {e:#}"),
        }
        session.on_disconnect(now_us(cfg.start));
        cfg.jitter.lock().unwrap().flush();
        let overlay = if matches!(session.state(), ReceiverState::Disconnected { .. }) { "Reconnecting" } else { "Waiting for sender" };
        cfg.ui.send(UiEvent::Overlay(Some(overlay.into()))).await.ok();
    }
}

fn hex_short(fp: &[u8; 32]) -> String { hex::encode(&fp[..6]) }

async fn handle_connection(cfg: &NetConfig, link: &Link, session: &mut ReceiverSession) -> anyhow::Result<()> {
    // Phase 1: Hello / pairing until the session says Streaming.
    loop {
        let msg = link.recv_control().await?;
        let fp = link.peer_fingerprint();
        let is_paired = cfg.paired.lock().unwrap().is_paired(&fp);
        match msg {
            ControlMessage::Hello { .. } if !is_paired => {
                link.send_control(&ControlMessage::Error { code: 4, message: "pairing required".into() }).await?;
                let pin = generate_pin();
                println!("\n=== PAIRING PIN: {pin} ===\n");
                cfg.ui.send(UiEvent::Overlay(Some(format!("PIN {pin}")))).await.ok();
                match pair_as_receiver(link, cfg.id.fingerprint, &pin).await {
                    Ok(()) => {
                        let mut store = cfg.paired.lock().unwrap();
                        store.add(fp, format!("sender-{}", hex_short(&fp)));
                        store.save()?;
                        tracing::info!("paired with {}", hex_short(&fp));
                    }
                    Err(e) => { tracing::warn!("pairing failed: {e:#}"); return Ok(()); }
                }
            }
            hello @ ControlMessage::Hello { .. } => {
                let was_disconnected = matches!(session.state(), ReceiverState::Disconnected { .. });
                for a in session.on_message(hello, now_us(cfg.start)) {
                    match a {
                        Action::Send(m) => link.send_control(&m).await?,
                        Action::Resumed => tracing::info!("session resumed"),
                        Action::Fail(why) => return Err(anyhow!(why)),
                    }
                }
                if matches!(session.state(), ReceiverState::Streaming { .. }) {
                    if was_disconnected { tracing::info!("resuming stream"); }
                    break;
                }
            }
            other => tracing::debug!("ignoring {other:?} before streaming"),
        }
    }
    cfg.ui.send(UiEvent::Overlay(None)).await.ok();
    stream(cfg, link, session).await
}

async fn stream(cfg: &NetConfig, link: &Link, session: &mut ReceiverSession) -> anyhow::Result<()> {
    let mut reasm = Reassembler::new(500_000);
    let mut nack_tx = link.open_nack_stream().await?;
    let mut tick = tokio::time::interval(Duration::from_millis(20));
    let mut stats_tick = tokio::time::interval(Duration::from_millis(100));
    let mut stall_check = tokio::time::interval(Duration::from_millis(250));
    let mut last_video = Instant::now();
    let mut frames_received = 0u32;
    let mut fragments_received = 0u32;
    let mut last_audio_ts: Option<u64> = None;
    if let Some(p) = session.params() {
        cfg.jitter.lock().unwrap().set_mode(p.mode);
        cfg.ui.send(UiEvent::Mode(p.mode)).await.ok();
    }
    loop {
        tokio::select! {
            d = link.recv_datagram() => {
                let d = d?;
                fragments_received += 1;
                if let Some(f) = reasm.push(&d, now_us(cfg.start))? {
                    match f.stream {
                        STREAM_VIDEO => {
                            frames_received += 1;
                            last_video = Instant::now();
                            cfg.jitter.lock().unwrap().push(f, now_us(cfg.start));
                        }
                        _ => {
                            if let Some(prev) = last_audio_ts {
                                let gap = f.timestamp_us.saturating_sub(prev);
                                if (15_000..200_000).contains(&gap) {
                                    // Lost packets: one PLC frame per missing 10 ms, capped.
                                    let missing = ((gap - 10_000) / 10_000).min(5);
                                    for _ in 0..missing { cfg.ui.send(UiEvent::AudioPacket { ts_us: prev, data: Vec::new() }).await.ok(); }
                                }
                            }
                            last_audio_ts = Some(f.timestamp_us);
                            cfg.ui.send(UiEvent::AudioPacket { ts_us: f.timestamp_us, data: f.data }).await.ok();
                        }
                    }
                }
            }
            m = link.recv_control() => {
                let m = m?;
                match &m {
                    ControlMessage::StartStream(p) => {
                        tracing::info!("stream {}x{}@{} {:?} {} bps", p.width, p.height, p.fps, p.mode, p.bitrate_bps);
                        let mut j = cfg.jitter.lock().unwrap();
                        *j = JitterBuffer::new(p.mode, 1_000_000 / p.fps.max(1) as u64);
                        cfg.ui.send(UiEvent::Mode(p.mode)).await.ok();
                    }
                    ControlMessage::SetMode(mode) => {
                        cfg.jitter.lock().unwrap().set_mode(*mode);
                        cfg.ui.send(UiEvent::Mode(*mode)).await.ok();
                    }
                    ControlMessage::Goodbye { reason } => tracing::info!("goodbye: {reason}"),
                    ControlMessage::Error { code, message } => tracing::warn!("peer error {code}: {message}"),
                    _ => {}
                }
                let goodbye = matches!(&m, ControlMessage::Goodbye { .. });
                for a in session.on_message(m, now_us(cfg.start)) {
                    if let Action::Send(r) = a { link.send_control(&r).await?; }
                }
                if goodbye { return Ok(()); }
            }
            _ = tick.tick() => {
                for n in reasm.tick(now_us(cfg.start)) { nack_tx.send(&n).await?; }
            }
            _ = stats_tick.tick() => {
                let dropped = cfg.jitter.lock().unwrap().dropped();
                let depth = cfg.stats.lock().unwrap().decode_queue_depth;
                let s = Stats { frames_received, frames_dropped: dropped, fragments_lost: reasm.fragments_lost() as u32, fragments_received, decode_queue_depth: depth, interval_ms: 100 };
                frames_received = 0; fragments_received = 0;
                link.send_control(&ControlMessage::Stats(s)).await?;
            }
            _ = stall_check.tick() => {
                if last_video.elapsed() > Duration::from_secs(1) {
                    link.send_control(&ControlMessage::RequestKeyframe).await?;
                    last_video = Instant::now();
                }
            }
            _ = link.closed() => return Err(anyhow!("connection lost")),
        }
    }
}
```

- [ ] **Step 7: Implement main.rs**

`crates/castr-receiver/src/main.rs`:
```rust
mod audio_out;
mod pipeline;
mod render;

use clap::Parser;
use pipeline::{DecoderChoice, ReceiverConfig};

#[derive(Parser)]
#[command(name = "castr-receiver", about = "castr screen receiver")]
struct Cli {
    /// Display name advertised on the network
    #[arg(long, default_value_t = default_name())]
    name: String,
    #[arg(long)]
    fullscreen: bool,
    /// Max video bitrate in bits per second
    #[arg(long, default_value_t = default_bitrate())]
    max_bitrate: u32,
    #[arg(long, value_enum, default_value_t = DecoderChoice::Auto)]
    decoder: DecoderChoice,
    /// UDP address to bind for QUIC
    #[arg(long, default_value = "0.0.0.0:7332")]
    bind: std::net::SocketAddr,
}

fn default_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "castr receiver".into())
}

fn default_bitrate() -> u32 {
    if cfg!(all(target_os = "linux", any(target_arch = "arm", target_arch = "aarch64"))) { 10_000_000 } else { 40_000_000 }
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?)).init();
    let cli = Cli::parse();
    pipeline::run(ReceiverConfig {
        name: cli.name,
        fullscreen: cli.fullscreen,
        max_bitrate: cli.max_bitrate,
        decoder: cli.decoder,
        bind: cli.bind,
        config_dir: castr_net::config_dir().join("receiver"),
    })
}
```

- [ ] **Step 8: Build and smoke-run**

Run: `cargo build -p castr-receiver && cargo run -p castr-receiver`
Expected: a window titled "castr - <hostname> - Waiting for sender" with a moving bar, log lines showing the fingerprint and the QUIC port. Allow the firewall prompt. Escape closes it.

- [ ] **Step 9: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(receiver): SDL2 receiver binary with pairing, jitter buffer, A/V sync"
```

---

### Task 22: Sender binary (CLI)

**Files:**
- Create: `crates/castr-sender/Cargo.toml`, `crates/castr-sender/src/main.rs`, `crates/castr-sender/src/cast.rs`

**Interfaces:**
- Consumes: everything above.
- Produces the `castr-sender` executable and, for the GUI in Task 23:
  ```rust
  pub struct CastOptions { pub target: String, pub mode: Mode, pub fps: u32, pub max_bitrate: Option<u32>, pub sender_name: String, pub config_dir: PathBuf }
  #[derive(Debug, Clone)] pub enum CastCommand { SetMode(Mode), Stop }
  #[derive(Debug, Clone, Default)] pub struct CastStatus { pub state: String, pub width: u32, pub height: u32, pub bitrate_bps: u32, pub rtt_ms: u32, pub loss_pct: f32, pub fps: f32 }
  pub async fn discover(timeout: Duration) -> anyhow::Result<Vec<ReceiverInfo>>;
  pub async fn resolve_target(target: &str, timeout: Duration) -> anyhow::Result<ReceiverInfo>;   // exact name, or fingerprint hex prefix >= 6 chars
  pub async fn pair(target: &ReceiverInfo, pin: &str, config_dir: &Path) -> anyhow::Result<()>;
  pub async fn cast(opts: CastOptions, cmds: mpsc::Receiver<CastCommand>, status: watch::Sender<CastStatus>) -> anyhow::Result<()>;
  pub fn resize_bgra_nearest(src: &[u8], w: u32, h: u32, stride: u32, dw: u32, dh: u32) -> Vec<u8>; // unit tested
  ```
  Sender-side session rules: `Hello` (with the stored token on reconnect); `Error { code: 4 }` means unpaired (cast fails with a clear message; `pair` proceeds to SPAKE2); `HelloAck` supplies caps; the sender picks `min(native, caps)` resolution rounded to even, `min(fps, caps.max_fps)`, ceiling `min(--max-bitrate, caps.max_bitrate_bps)`, initial bitrate = ceiling / 2. Reconnect: on link loss keep capturing, retry with backoff 200 ms doubling to 5 s for 30 s total, then fail.

- [ ] **Step 1: Manifest and failing unit test**

`crates/castr-sender/Cargo.toml`:
```toml
[package]
name = "castr-sender"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
bytes.workspace = true
clap.workspace = true
hex.workspace = true
tokio.workspace = true
tracing.workspace = true
tracing-subscriber.workspace = true
eframe.workspace = true
castr-proto = { path = "../castr-proto" }
castr-net = { path = "../castr-net" }
castr-media = { path = "../castr-media" }

[target.'cfg(windows)'.dependencies]
castr-capture-win = { path = "../castr-capture-win" }
castr-codec-win = { path = "../castr-codec-win" }

[target.'cfg(windows)'.build-dependencies]
winres = "0.1"
```

`crates/castr-sender/src/cast.rs` tests:
```rust
#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn resize_halves_a_checkerboard() {
        let (w, h) = (4u32, 4u32);
        let mut src = Vec::new();
        for y in 0..h { for x in 0..w { let v = if (x / 2 + y / 2) % 2 == 0 { 255 } else { 0 }; src.extend_from_slice(&[v, v, v, 255]); } }
        let out = resize_bgra_nearest(&src, w, h, w * 4, 2, 2);
        assert_eq!(out.len(), 2 * 2 * 4);
        assert_eq!(&out[0..4], &[255, 255, 255, 255]);
        assert_eq!(&out[4..8], &[0, 0, 0, 255]);
        assert_eq!(&out[8..12], &[0, 0, 0, 255]);
        assert_eq!(&out[12..16], &[255, 255, 255, 255]);
    }

    #[test]
    fn resize_respects_source_stride() {
        let src = vec![9u8; 2 * 16];
        let out = resize_bgra_nearest(&src, 2, 2, 16, 2, 2);
        assert_eq!(out, vec![9u8; 16]);
    }

    #[test]
    fn choose_params_clamps_to_caps_and_evens() {
        let caps = Capabilities { max_width: 1280, max_height: 720, max_fps: 30, max_bitrate_bps: 10_000_000, codecs: vec![Codec::H264], audio: true };
        let p = choose_params((2560, 1440), 60, Some(40_000_000), Mode::Game, &caps);
        assert_eq!((p.width, p.height, p.fps), (1280, 720, 30));
        assert_eq!(p.bitrate_bps, 5_000_000);
        let p2 = choose_params((1000, 601), 60, None, Mode::Quality, &caps);
        assert_eq!((p2.width, p2.height), (1000, 600));
    }
}
```

- [ ] **Step 2: Run to verify it fails**

Create `src/main.rs` with `mod cast; fn main() {}` and run: `cargo test -p castr-sender`
Expected: `resize_bgra_nearest` / `choose_params` not found.

- [ ] **Step 3: Implement cast.rs**

`crates/castr-sender/src/cast.rs`:
```rust
use anyhow::{anyhow, bail, Context};
use bytes::Bytes;
use castr_media::audio::{AudioEncoder, FrameChunker};
use castr_media::bitrate::{BitrateController, Decision, Resolution};
use castr_media::*;
use castr_net::*;
use castr_proto::*;
use std::collections::HashSet;
use std::path::{Path, PathBuf};
use std::sync::{Arc, RwLock};
use std::time::{Duration, Instant};
use tokio::sync::{mpsc, watch};

pub struct CastOptions {
    pub target: String,
    pub mode: Mode,
    pub fps: u32,
    pub max_bitrate: Option<u32>,
    pub sender_name: String,
    pub config_dir: PathBuf,
}

#[derive(Debug, Clone)]
pub enum CastCommand { SetMode(Mode), Stop }

#[derive(Debug, Clone, Default)]
pub struct CastStatus {
    pub state: String,
    pub width: u32,
    pub height: u32,
    pub bitrate_bps: u32,
    pub rtt_ms: u32,
    pub loss_pct: f32,
    pub fps: f32,
}

pub fn resize_bgra_nearest(src: &[u8], w: u32, h: u32, stride: u32, dw: u32, dh: u32) -> Vec<u8> {
    let mut out = Vec::with_capacity((dw * dh * 4) as usize);
    for y in 0..dh {
        let sy = (y as u64 * h as u64 / dh as u64) as usize;
        for x in 0..dw {
            let sx = (x as u64 * w as u64 / dw as u64) as usize;
            let p = sy * stride as usize + sx * 4;
            out.extend_from_slice(&src[p..p + 4]);
        }
    }
    out
}

pub fn choose_params(native: (u32, u32), fps: u32, max_bitrate: Option<u32>, mode: Mode, caps: &Capabilities) -> StreamParams {
    let scale = (caps.max_width as f64 / native.0 as f64).min(caps.max_height as f64 / native.1 as f64).min(1.0);
    let width = ((native.0 as f64 * scale) as u32) & !1;
    let height = ((native.1 as f64 * scale) as u32) & !1;
    let ceiling = max_bitrate.unwrap_or(u32::MAX).min(caps.max_bitrate_bps);
    StreamParams { codec: Codec::H264, width, height, fps: fps.min(caps.max_fps).max(1), mode, bitrate_bps: ceiling / 2 }
}

pub async fn discover(timeout: Duration) -> anyhow::Result<Vec<ReceiverInfo>> {
    browse(timeout, PROBE_PORT).await
}

pub async fn resolve_target(target: &str, timeout: Duration) -> anyhow::Result<ReceiverInfo> {
    let found = discover(timeout).await?;
    let t = target.to_lowercase();
    found.into_iter()
        .find(|r| r.name.to_lowercase() == t || (t.len() >= 6 && hex::encode(r.fingerprint).starts_with(&t)))
        .ok_or_else(|| anyhow!("receiver '{target}' not found; run `castr-sender list`"))
}

fn client_for(target: &ReceiverInfo, config_dir: &Path) -> anyhow::Result<(Identity, Endpoint)> {
    let id = Identity::load_or_create(config_dir)?;
    let trust = Arc::new(RwLock::new(HashSet::from([target.fingerprint])));
    let ep = Endpoint::client("0.0.0.0:0".parse()?, &id, trust_fingerprints(trust))?;
    Ok((id, ep))
}

pub async fn pair(target: &ReceiverInfo, pin: &str, config_dir: &Path) -> anyhow::Result<()> {
    let (id, ep) = client_for(target, config_dir)?;
    let link = ep.connect(target.addr).await?;
    link.send_control(&ControlMessage::Hello { version: PROTOCOL_VERSION, name: "pairing".into(), resume_token: None }).await?;
    match link.recv_control().await? {
        ControlMessage::Error { code: 4, .. } => {}
        ControlMessage::HelloAck { .. } => {
            link.send_control(&ControlMessage::Goodbye { reason: "already paired".into() }).await?;
            println!("already paired with {}", target.name);
            return Ok(());
        }
        other => bail!("unexpected reply {other:?}"),
    }
    pair_as_sender(&link, id.fingerprint, pin).await?;
    let mut store = PairedStore::load(config_dir.join("paired.toml"))?;
    store.add(target.fingerprint, target.name.clone());
    store.save()?;
    link.send_control(&ControlMessage::Goodbye { reason: "paired".into() }).await?;
    Ok(())
}

/// Commands from the network task to the capture/encode thread.
enum EncCmd { Bitrate(u32), Keyframe, Mode(Mode), Resolution(u32, u32), Stop }

/// One select! branch handles both "waiting for the receiver to open the NACK stream" and "reading NACKs from it".
enum NackEv { Nack(anyhow::Result<Nack>), Stream(anyhow::Result<NackReceiver>) }

struct VideoOut { frame: EncodedFrame }

#[cfg(windows)]
fn spawn_capture(params_tx: std::sync::mpsc::Sender<(u32, u32)>, mut cmd_rx: std::sync::mpsc::Receiver<EncCmd>, out: mpsc::Sender<VideoOut>, fps: u32, mode: Mode, bitrate: u32, start: Instant)
    -> anyhow::Result<std::thread::JoinHandle<()>> {
    use castr_capture_win::DesktopCapture;
    let handle = std::thread::Builder::new().name("capture".into()).spawn(move || {
        let mut cap = match DesktopCapture::new(0) { Ok(c) => c, Err(e) => { tracing::error!("capture init: {e:#}"); return; } };
        let native = cap.size();
        let _ = params_tx.send(native);
        let (mut w, mut h) = native;
        let mut cfg = EncoderConfig { width: w, height: h, fps, bitrate_bps: bitrate, mode };
        let mut enc: Box<dyn VideoEncoder> = match castr_codec_win::MfEncoder::new(cfg) {
            Ok(e) => Box::new(e),
            Err(e) => { tracing::warn!("MF encoder unavailable ({e:#}), using openh264"); match sw::SwEncoder::new(cfg) { Ok(e) => Box::new(e), Err(e) => { tracing::error!("no encoder: {e:#}"); return; } } }
        };
        tracing::info!("encoder: {}", enc.name());
        let interval = Duration::from_micros(1_000_000 / fps as u64);
        let mut last: Option<RawFrame> = None;
        let mut last_sent = Instant::now();
        loop {
            while let Ok(cmd) = cmd_rx.try_recv() {
                match cmd {
                    EncCmd::Bitrate(b) => { let _ = enc.set_bitrate(b); }
                    EncCmd::Keyframe => enc.request_keyframe(),
                    EncCmd::Mode(m) => { let _ = enc.set_mode(m); }
                    EncCmd::Resolution(nw, nh) => {
                        w = nw; h = nh;
                        cfg.width = w; cfg.height = h;
                        enc = match castr_codec_win::MfEncoder::new(cfg) { Ok(e) => Box::new(e), Err(_) => match sw::SwEncoder::new(cfg) { Ok(e) => Box::new(e), Err(e) => { tracing::error!("encoder: {e:#}"); return; } } };
                    }
                    EncCmd::Stop => return,
                }
            }
            let ts = start.elapsed().as_micros() as u64;
            let frame = match cap.next_frame(interval.as_millis() as u32, ts) {
                Ok(Some(f)) => Some(f),
                Ok(None) => if last_sent.elapsed() >= Duration::from_millis(500) { last.clone().map(|mut f| { f.timestamp_us = ts; f }) } else { None },
                Err(e) => {
                    tracing::warn!("capture: {e:#}; reopening");
                    std::thread::sleep(Duration::from_millis(250));
                    match DesktopCapture::new(0) { Ok(c) => cap = c, Err(e) => tracing::warn!("reopen failed: {e:#}") }
                    enc.request_keyframe();
                    None
                }
            };
            let Some(f) = frame else { continue };
            last = Some(f.clone());
            let scaled = if (f.width, f.height) != (w, h) {
                RawFrame { format: PixelFormat::Bgra, width: w, height: h, stride: w * 4, data: resize_bgra_nearest(&f.data, f.width, f.height, f.stride, w, h), timestamp_us: f.timestamp_us }
            } else { f };
            let input = convert::convert(&scaled, enc.input_format());
            match enc.encode(&input) {
                Ok(Some(e)) => { last_sent = Instant::now(); if out.blocking_send(VideoOut { frame: e }).is_err() { return; } }
                Ok(None) => {}
                Err(e) => tracing::warn!("encode: {e:#}"),
            }
        }
    })?;
    Ok(handle)
}

#[cfg(not(windows))]
fn spawn_capture(_: std::sync::mpsc::Sender<(u32, u32)>, _: std::sync::mpsc::Receiver<EncCmd>, _: mpsc::Sender<VideoOut>, _: u32, _: Mode, _: u32, _: Instant) -> anyhow::Result<std::thread::JoinHandle<()>> {
    bail!("screen capture is only implemented on Windows in this version")
}

struct AudioOut { ts_us: u64, packet: Vec<u8> }

#[cfg(windows)]
fn spawn_audio(out: mpsc::Sender<AudioOut>, start: Instant, stop: Arc<std::sync::atomic::AtomicBool>) {
    std::thread::Builder::new().name("audio".into()).spawn(move || {
        let mut cap = match castr_capture_win::LoopbackCapture::new() { Ok(c) => c, Err(e) => { tracing::warn!("audio capture unavailable: {e:#}"); return; } };
        let mut enc = match AudioEncoder::new() { Ok(e) => e, Err(e) => { tracing::warn!("opus: {e:#}"); return; } };
        let mut chunker = FrameChunker::new();
        let mut buf = Vec::new();
        let mut frames_sent: u64 = 0;
        let origin = start.elapsed().as_micros() as u64;
        while !stop.load(std::sync::atomic::Ordering::Relaxed) {
            buf.clear();
            if let Err(e) = cap.drain(&mut buf) { tracing::warn!("audio drain: {e:#}"); break; }
            chunker.push(&buf);
            while let Some(frame) = chunker.next_frame() {
                let ts = origin + frames_sent * 10_000;
                frames_sent += 1;
                match enc.encode(&frame) {
                    Ok(p) => { if out.blocking_send(AudioOut { ts_us: ts, packet: p }).is_err() { return; } }
                    Err(e) => tracing::warn!("opus encode: {e:#}"),
                }
            }
            std::thread::sleep(Duration::from_millis(5));
        }
    }).ok();
}

#[cfg(not(windows))]
fn spawn_audio(_: mpsc::Sender<AudioOut>, _: Instant, _: Arc<std::sync::atomic::AtomicBool>) {}

pub async fn cast(opts: CastOptions, mut cmds: mpsc::Receiver<CastCommand>, status: watch::Sender<CastStatus>) -> anyhow::Result<()> {
    let start = Instant::now();
    let set_state = |s: &str| { status.send_modify(|st| st.state = s.to_string()); };
    set_state("discovering");
    let target = resolve_target(&opts.target, Duration::from_secs(2)).await?;
    let (_id, ep) = client_for(&target, &opts.config_dir)?;

    let (video_tx, mut video_rx) = mpsc::channel::<VideoOut>(4);
    let (audio_tx, mut audio_rx) = mpsc::channel::<AudioOut>(32);
    let (enc_tx, enc_rx) = std::sync::mpsc::channel::<EncCmd>();
    let (native_tx, native_rx) = std::sync::mpsc::channel::<(u32, u32)>();
    let stop_audio = Arc::new(std::sync::atomic::AtomicBool::new(false));

    // Connect first so we know caps before opening the encoder.
    set_state("connecting");
    let mut token: Option<[u8; 16]> = None;
    let mut link = connect_with_retry(&ep, target.addr, &opts.sender_name, &mut token, Duration::from_secs(5)).await?;
    let caps = match link.recv_control().await? {
        ControlMessage::HelloAck { caps, .. } => caps,
        ControlMessage::Error { code: 4, .. } => bail!("not paired with '{}'; run: castr-sender pair \"{}\"", target.name, target.name),
        other => bail!("unexpected {other:?}"),
    };

    let mode = opts.mode;
    let ceiling = opts.max_bitrate.unwrap_or(u32::MAX).min(caps.max_bitrate_bps);
    let _cap_thread = spawn_capture(native_tx, enc_rx, video_tx, opts.fps, mode, ceiling / 2, start)?;
    let native = tokio::task::spawn_blocking(move || native_rx.recv_timeout(Duration::from_secs(5))).await?.context("capture did not start")?;
    let mut params = choose_params(native, opts.fps, opts.max_bitrate, mode, &caps);
    if (params.width, params.height) != native {
        enc_tx.send(EncCmd::Resolution(params.width, params.height)).ok();
    }
    enc_tx.send(EncCmd::Bitrate(params.bitrate_bps)).ok();
    link.send_control(&ControlMessage::StartStream(params.clone())).await?;
    spawn_audio(audio_tx, start, stop_audio.clone());

    let mut ctl = BitrateController::new(ceiling, params.bitrate_bps, Resolution { width: params.width, height: params.height }, mode);
    let mut packetizer = Packetizer::new();
    let mut rtx = RetransmitBuffer::new(500_000);
    let frame_interval_us = 1_000_000 / params.fps as u64;
    let mut sent_frames = 0u32;
    let mut fps_window = Instant::now();
    let mut nack_rx: Option<NackReceiver> = None;
    set_state("casting");
    status.send_modify(|st| { st.width = params.width; st.height = params.height; st.bitrate_bps = params.bitrate_bps; });

    loop {
        let now = start.elapsed().as_micros() as u64;
        tokio::select! {
            Some(v) = video_rx.recv() => {
                let frags = packetizer.packetize(STREAM_VIDEO, v.frame.keyframe, v.frame.timestamp_us, &v.frame.data, link.max_datagram_size());
                rtx.record(packetizer.last_frame_number(), v.frame.keyframe, frags.clone(), now);
                for f in frags { if let Err(e) = link.send_datagram(f) { tracing::debug!("send: {e:#}"); } }
                sent_frames += 1;
                if fps_window.elapsed() >= Duration::from_secs(1) {
                    let fps = sent_frames as f32 / fps_window.elapsed().as_secs_f32();
                    status.send_modify(|st| { st.fps = fps; st.rtt_ms = link.rtt().as_millis() as u32; });
                    sent_frames = 0; fps_window = Instant::now();
                }
            }
            Some(a) = audio_rx.recv() => {
                for f in packetizer.packetize(STREAM_AUDIO, false, a.ts_us, &a.packet, link.max_datagram_size()) {
                    let _ = link.send_datagram(f);
                }
            }
            m = link.recv_control() => {
                match m {
                    Ok(ControlMessage::SessionToken(t)) => token = Some(t),
                    Ok(ControlMessage::RequestKeyframe) => { enc_tx.send(EncCmd::Keyframe).ok(); }
                    Ok(ControlMessage::Stats(s)) => {
                        let total = s.fragments_lost + s.fragments_received;
                        let loss = if total == 0 { 0.0 } else { s.fragments_lost as f32 * 100.0 / total as f32 };
                        status.send_modify(|st| st.loss_pct = loss);
                        if let Some(Decision { bitrate_bps, resolution }) = ctl.on_stats(&s, now) {
                            if bitrate_bps != params.bitrate_bps {
                                params.bitrate_bps = bitrate_bps;
                                enc_tx.send(EncCmd::Bitrate(bitrate_bps)).ok();
                            }
                            if (resolution.width, resolution.height) != (params.width, params.height) {
                                params.width = resolution.width; params.height = resolution.height;
                                enc_tx.send(EncCmd::Resolution(params.width, params.height)).ok();
                                link.send_control(&ControlMessage::StartStream(params.clone())).await?;
                            }
                            status.send_modify(|st| { st.bitrate_bps = params.bitrate_bps; st.width = params.width; st.height = params.height; });
                        }
                    }
                    Ok(ControlMessage::Error { code, message }) => tracing::warn!("receiver error {code}: {message}"),
                    Ok(ControlMessage::Goodbye { reason }) => { tracing::info!("receiver said goodbye: {reason}"); break; }
                    Ok(_) => {}
                    Err(e) => tracing::debug!("control: {e:#}"),
                }
            }
            ev = async {
                match nack_rx.as_mut() {
                    Some(r) => NackEv::Nack(r.recv().await),
                    None => NackEv::Stream(link.accept_nack_stream().await),
                }
            } => {
                match ev {
                    NackEv::Nack(Ok(nack)) => for f in rtx.lookup(&nack, now, frame_interval_us) { let _ = link.send_datagram(f); },
                    NackEv::Nack(Err(_)) => nack_rx = None,
                    NackEv::Stream(Ok(rx)) => nack_rx = Some(rx),
                    NackEv::Stream(Err(e)) => { tracing::debug!("nack stream: {e:#}"); tokio::time::sleep(Duration::from_millis(100)).await; }
                }
            }
            Some(cmd) = cmds.recv() => {
                match cmd {
                    CastCommand::SetMode(m) => {
                        params.mode = m;
                        ctl.set_mode(m);
                        enc_tx.send(EncCmd::Mode(m)).ok();
                        link.send_control(&ControlMessage::SetMode(m)).await?;
                    }
                    CastCommand::Stop => break,
                }
            }
            _ = link.closed() => {
                set_state("reconnecting");
                tracing::warn!("connection lost, reconnecting");
                match connect_with_retry(&ep, target.addr, &opts.sender_name, &mut token, Duration::from_secs(30)).await {
                    Ok(l) => {
                        link = l;
                        match link.recv_control().await? {
                            ControlMessage::HelloAck { .. } => {}
                            other => bail!("resume failed: {other:?}"),
                        }
                        nack_rx = None;
                        link.send_control(&ControlMessage::StartStream(params.clone())).await?;
                        enc_tx.send(EncCmd::Keyframe).ok();
                        set_state("casting");
                    }
                    Err(e) => { set_state("failed"); enc_tx.send(EncCmd::Stop).ok(); stop_audio.store(true, std::sync::atomic::Ordering::Relaxed); return Err(e); }
                }
            }
        }
    }
    let _ = link.send_control(&ControlMessage::Goodbye { reason: "stopped".into() }).await;
    link.close("stopped");
    enc_tx.send(EncCmd::Stop).ok();
    stop_audio.store(true, std::sync::atomic::Ordering::Relaxed);
    set_state("stopped");
    Ok(())
}

async fn connect_with_retry(ep: &Endpoint, addr: std::net::SocketAddr, name: &str, token: &mut Option<[u8; 16]>, total: Duration) -> anyhow::Result<Link> {
    let deadline = Instant::now() + total;
    let mut backoff = Duration::from_millis(200);
    loop {
        match ep.connect(addr).await {
            Ok(link) => {
                link.send_control(&ControlMessage::Hello { version: PROTOCOL_VERSION, name: name.to_string(), resume_token: *token }).await?;
                return Ok(link);
            }
            Err(e) if Instant::now() + backoff < deadline => {
                tracing::debug!("connect failed ({e:#}), retry in {backoff:?}");
                tokio::time::sleep(backoff).await;
                backoff = (backoff * 2).min(Duration::from_secs(5));
            }
            Err(e) => return Err(e).context("could not reach receiver"),
        }
    }
}
```

- [ ] **Step 4: Implement main.rs (CLI; GUI hook added in Task 23)**

`crates/castr-sender/src/main.rs`:
```rust
mod cast;

use cast::*;
use castr_proto::Mode;
use clap::{Parser, Subcommand};
use std::time::Duration;

#[derive(Parser)]
#[command(name = "castr-sender", about = "castr screen sender")]
struct Cli {
    #[command(subcommand)]
    cmd: Option<Cmd>,
}

#[derive(Subcommand)]
enum Cmd {
    /// List receivers on the network
    List,
    /// Pair with a receiver (shows a PIN on the receiver)
    Pair { target: String },
    /// Cast the screen to a receiver
    Cast {
        target: String,
        #[arg(long, value_enum, default_value_t = ModeArg::Game)]
        mode: ModeArg,
        #[arg(long, default_value_t = 30)]
        fps: u32,
        #[arg(long)]
        max_bitrate: Option<u32>,
    },
}

#[derive(Clone, Copy, clap::ValueEnum)]
enum ModeArg { Game, Quality }
impl From<ModeArg> for Mode { fn from(m: ModeArg) -> Self { match m { ModeArg::Game => Mode::Game, ModeArg::Quality => Mode::Quality } } }

fn sender_name() -> String {
    std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")).unwrap_or_else(|_| "castr sender".into())
}

fn main() -> anyhow::Result<()> {
    tracing_subscriber::fmt().with_env_filter(tracing_subscriber::EnvFilter::from_default_env().add_directive("info".parse()?)).init();
    let cli = Cli::parse();
    let config_dir = castr_net::config_dir().join("sender");
    let rt = tokio::runtime::Runtime::new()?;
    match cli.cmd {
        None => {
            println!("no subcommand given; the GUI arrives in the next task. Try `castr-sender list`.");
            Ok(())
        }
        Some(Cmd::List) => rt.block_on(async {
            for r in discover(Duration::from_secs(2)).await? {
                println!("{:<24} {}  {}", r.name, r.addr, hex::encode(r.fingerprint));
            }
            Ok(())
        }),
        Some(Cmd::Pair { target }) => rt.block_on(async {
            let info = resolve_target(&target, Duration::from_secs(2)).await?;
            println!("Enter the PIN shown on '{}':", info.name);
            let mut pin = String::new();
            std::io::stdin().read_line(&mut pin)?;
            pair(&info, pin.trim(), &config_dir).await?;
            println!("paired with {}", info.name);
            Ok(())
        }),
        Some(Cmd::Cast { target, mode, fps, max_bitrate }) => rt.block_on(async {
            let (cmd_tx, cmd_rx) = tokio::sync::mpsc::channel(4);
            let (status_tx, mut status_rx) = tokio::sync::watch::channel(CastStatus::default());
            tokio::spawn(async move {
                let _ = tokio::signal::ctrl_c().await;
                let _ = cmd_tx.send(CastCommand::Stop).await;
            });
            tokio::spawn(async move {
                while status_rx.changed().await.is_ok() {
                    let s = status_rx.borrow().clone();
                    tracing::info!("{} {}x{} {:.1} Mbps rtt {} ms loss {:.1}% {:.0} fps", s.state, s.width, s.height, s.bitrate_bps as f64 / 1e6, s.rtt_ms, s.loss_pct, s.fps);
                }
            });
            cast(CastOptions { target, mode: mode.into(), fps, max_bitrate, sender_name: sender_name(), config_dir }, cmd_rx, status_tx).await
        }),
    }
}
```

Add `signal` to the tokio workspace features in the root `Cargo.toml`: `features = ["rt-multi-thread", "macros", "sync", "time", "net", "io-util", "signal"]`.

- [ ] **Step 5: Run unit tests and build**

Run: `cargo test -p castr-sender && cargo build -p castr-sender`
Expected: 3 passed; binary builds.

- [ ] **Step 6: Manual pair and cast on one machine**

In one terminal: `cargo run -p castr-receiver`
In another: `cargo run -p castr-sender -- list` shows the receiver. Then `cargo run -p castr-sender -- pair <name>`; type the PIN printed by the receiver. Then `cargo run -p castr-sender -- cast <name>`.
Expected: the receiver window shows the desktop (a hall-of-mirrors effect since it is on the same screen), audio playing on the PC is heard through the receiver window with a small delay, and the sender logs status lines every second. Ctrl+C stops cleanly and the receiver returns to "Waiting for sender".

- [ ] **Step 7: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(sender): CLI sender with capture, encode, adaptive bitrate, reconnect"
```

---

### Task 23: Sender GUI window

**Files:**
- Create: `crates/castr-sender/src/gui.rs`, `crates/castr-sender/build.rs`
- Modify: `crates/castr-sender/src/main.rs`

**Interfaces:**
- Consumes: `cast::{discover, pair, cast, CastOptions, CastCommand, CastStatus}`.
- Produces: `pub fn run_gui(config_dir: PathBuf, sender_name: String) -> anyhow::Result<()>`, launched when no subcommand is given. On Windows the release build has no console window.

- [ ] **Step 1: Implement gui.rs**

`crates/castr-sender/src/gui.rs`:
```rust
use crate::cast::*;
use castr_net::ReceiverInfo;
use castr_proto::Mode;
use eframe::egui;
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use std::time::Duration;
use tokio::sync::{mpsc, watch};

struct Shared {
    receivers: Vec<ReceiverInfo>,
    scanning: bool,
    message: String,
}

struct ActiveCast {
    cmd: mpsc::Sender<CastCommand>,
    status: watch::Receiver<CastStatus>,
}

struct App {
    rt: tokio::runtime::Runtime,
    shared: Arc<Mutex<Shared>>,
    config_dir: PathBuf,
    sender_name: String,
    selected: Option<usize>,
    mode: Mode,
    pin: String,
    active: Option<ActiveCast>,
}

impl App {
    fn scan(&self) {
        let shared = self.shared.clone();
        shared.lock().unwrap().scanning = true;
        self.rt.spawn(async move {
            let found = discover(Duration::from_secs(2)).await.unwrap_or_default();
            let mut s = shared.lock().unwrap();
            s.receivers = found;
            s.scanning = false;
        });
    }

    fn do_pair(&mut self, target: ReceiverInfo) {
        let shared = self.shared.clone();
        let pin = self.pin.trim().to_string();
        let dir = self.config_dir.clone();
        self.rt.spawn(async move {
            let msg = match pair(&target, &pin, &dir).await { Ok(()) => format!("Paired with {}", target.name), Err(e) => format!("Pairing failed: {e:#}") };
            shared.lock().unwrap().message = msg;
        });
    }

    fn do_cast(&mut self, target: ReceiverInfo) {
        let (cmd_tx, cmd_rx) = mpsc::channel(4);
        let (status_tx, status_rx) = watch::channel(CastStatus::default());
        let opts = CastOptions { target: target.name.clone(), mode: self.mode, fps: 30, max_bitrate: None, sender_name: self.sender_name.clone(), config_dir: self.config_dir.clone() };
        let shared = self.shared.clone();
        self.rt.spawn(async move {
            if let Err(e) = cast(opts, cmd_rx, status_tx).await {
                shared.lock().unwrap().message = format!("Cast ended: {e:#}");
            }
        });
        self.active = Some(ActiveCast { cmd: cmd_tx, status: status_rx });
    }
}

impl eframe::App for App {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        ctx.request_repaint_after(Duration::from_millis(250));
        let (receivers, scanning, message) = {
            let s = self.shared.lock().unwrap();
            (s.receivers.clone(), s.scanning, s.message.clone())
        };
        if let Some(a) = &self.active {
            if a.status.borrow().state == "stopped" || a.status.borrow().state == "failed" { self.active = None; }
        }
        egui::CentralPanel::default().show(ctx, |ui| {
            ui.heading("castr");
            ui.horizontal(|ui| {
                if ui.add_enabled(!scanning, egui::Button::new("Scan")).clicked() { self.scan(); }
                if scanning { ui.spinner(); }
            });
            ui.separator();
            for (i, r) in receivers.iter().enumerate() {
                ui.selectable_value(&mut self.selected, Some(i), format!("{}  ({})", r.name, r.addr.ip()));
            }
            if receivers.is_empty() && !scanning { ui.label("No receivers found. Is the receiver running on this network?"); }
            ui.separator();
            ui.horizontal(|ui| {
                ui.label("Mode:");
                if ui.selectable_value(&mut self.mode, Mode::Game, "Game").clicked() { if let Some(a) = &self.active { let _ = a.cmd.try_send(CastCommand::SetMode(Mode::Game)); } }
                if ui.selectable_value(&mut self.mode, Mode::Quality, "Quality").clicked() { if let Some(a) = &self.active { let _ = a.cmd.try_send(CastCommand::SetMode(Mode::Quality)); } }
            });
            let target = self.selected.and_then(|i| receivers.get(i).cloned());
            match &self.active {
                None => {
                    ui.horizontal(|ui| {
                        ui.label("PIN:");
                        ui.add(egui::TextEdit::singleline(&mut self.pin).desired_width(80.0));
                        if ui.add_enabled(target.is_some() && self.pin.trim().len() == 6, egui::Button::new("Pair")).clicked() { self.do_pair(target.clone().unwrap()); }
                        if ui.add_enabled(target.is_some(), egui::Button::new("Cast")).clicked() { self.do_cast(target.clone().unwrap()); }
                    });
                }
                Some(a) => {
                    let s = a.status.borrow().clone();
                    ui.label(format!("{}  {}x{}  {:.1} Mbps  rtt {} ms  loss {:.1}%  {:.0} fps", s.state, s.width, s.height, s.bitrate_bps as f64 / 1e6, s.rtt_ms, s.loss_pct, s.fps));
                    if ui.button("Stop").clicked() { let _ = a.cmd.try_send(CastCommand::Stop); }
                }
            }
            if !message.is_empty() { ui.separator(); ui.label(message); }
        });
    }
}

pub fn run_gui(config_dir: PathBuf, sender_name: String) -> anyhow::Result<()> {
    let rt = tokio::runtime::Runtime::new()?;
    let app = App {
        rt, shared: Arc::new(Mutex::new(Shared { receivers: Vec::new(), scanning: false, message: String::new() })),
        config_dir, sender_name, selected: None, mode: Mode::Game, pin: String::new(), active: None,
    };
    app.scan();
    let options = eframe::NativeOptions { viewport: egui::ViewportBuilder::default().with_inner_size([420.0, 320.0]), ..Default::default() };
    eframe::run_native("castr", options, Box::new(|_| Ok(Box::new(app)))).map_err(|e| anyhow::anyhow!("gui: {e}"))
}
```

If the resolved `eframe` version's `run_native` closure returns `Box<dyn App>` rather than `Result`, drop the `Ok(...)` wrapper; the compiler error names the expected return type.

- [ ] **Step 2: Wire main.rs and hide the console in release builds**

In `main.rs`, add at the very top:
```rust
#![cfg_attr(all(windows, not(debug_assertions)), windows_subsystem = "windows")]
mod gui;
```
and replace the `None =>` arm with:
```rust
        None => gui::run_gui(config_dir, sender_name()),
```

`crates/castr-sender/build.rs`:
```rust
fn main() {
    #[cfg(windows)]
    {
        let mut res = winres::WindowsResource::new();
        res.set("ProductName", "castr sender");
        res.set("FileDescription", "castr screen sender");
        let _ = res.compile();
    }
}
```

- [ ] **Step 3: Build and smoke-run**

Run: `cargo run -p castr-sender`
Expected: a small window lists the receiver after the initial scan. Select it, type the PIN from the receiver window title, click Pair, see "Paired with ...", then Cast. Status line updates each second. Switching Game/Quality mid-cast changes the receiver's smoothness within a second. Stop returns to the idle state.

- [ ] **Step 4: Commit**

```bash
cargo fmt && cargo clippy --workspace
git add -A
git commit -m "feat(sender): eframe pairing and cast window"
```

---

### Task 24: End-to-end verification and portable build

**Files:**
- Create: `docs/superpowers/verification/2026-09-01-castr-core-e2e.md` (results log)

- [ ] **Step 1: Release build and dependency check**

```powershell
cargo build --release --workspace
dumpbin /dependents target\release\castr-sender.exe
```
`dumpbin` lives in the Build Tools (`Developer PowerShell for VS 2022` or run `vcvars64.bat` first). Expected: only system DLLs (`KERNEL32`, `USER32`, `GDI32`, `ADVAPI32`, `SHELL32`, `OLE32`, `d3d11`, `dxgi`, `mfplat`, `mf`, `mfreadwrite`, `bcrypt`, `ws2_32`, `ntdll`, etc.). No `vcruntime140.dll`? It will appear unless the CRT is linked statically. To satisfy the single-exe requirement, create `.cargo/config.toml` at the workspace root:
```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```
Rebuild and confirm `vcruntime140.dll` and `msvcp140.dll` are gone from the list. Record the final dependents list in the results log.

- [ ] **Step 2: Same-machine session**

Run `target\release\castr-receiver.exe` and `target\release\castr-sender.exe` (GUI). Pair and cast. Record in the results log:
- Encoder and decoder names from the logs.
- Steady-state bitrate, fps, rtt from the sender status.
- Whether audio is audible and in sync (play a video with lip movement; sync error should not be perceptible, under about 80 ms).

- [ ] **Step 3: Glass-to-glass latency**

Open a browser stopwatch with millisecond display on the sender screen, place the receiver window beside it, photograph both with a phone. Latency is the difference between the two readings. Take 5 samples in Game mode and 5 in Quality mode; record the median of each. Expected on one machine with hardware encode: Game under 50 ms, Quality about 150 to 200 ms.

- [ ] **Step 4: Reconnect**

While casting, disable and re-enable the network adapter (or on a two-machine setup unplug Ethernet for 5 s). Expected: receiver overlay shows "Reconnecting" within 3 s, stream resumes within 2 s of the link returning, no re-pairing. Then close the receiver entirely and restart it within 60 s: the sender reconnects and the session resumes with a keyframe because the receiver kept no state, so confirm it falls back gracefully: the new receiver process has a fresh session, replies `Error { code: 1 }` to the stale token, and the sender reports the failure clearly. Record both outcomes.

- [ ] **Step 5: Capture edge cases**

While casting: lock the screen (Win+L) and unlock; trigger a UAC prompt; change display resolution. Expected: the capture thread logs "access lost" and reopens; the stream continues within 2 s after each event.

- [ ] **Step 6: Loss handling**

Run the sender with `--max-bitrate 60000000` against the receiver on a Wi-Fi laptop if available, or simulate loss with `clumsy` (https://jagt.github.io/clumsy/) set to 3% drop on UDP. Expected: sender status shows loss, bitrate steps down within a second, picture stays clean (no long green/grey corruption) because keyframe fragments are retransmitted and the receiver skips to the next keyframe on delta loss. Record the lowest bitrate reached and time to recover after removing the loss.

- [ ] **Step 7: Linux build check (WSL, if installed)**

In WSL Ubuntu:
```bash
sudo apt install -y build-essential cmake pkg-config libx11-dev libxext-dev libasound2-dev
cargo build -p castr-receiver -p castr-proto -p castr-media -p castr-net
cargo test -p castr-proto -p castr-media -p castr-net
```
Expected: builds and tests pass with the `openh264` backend and no Windows crates in the graph (`cargo tree -p castr-receiver | grep -i windows` prints nothing). With WSLg, `cargo run -p castr-receiver` opens a window; cast to it from Windows by name. Record the result. If WSL is not installed, record that this step was skipped; the Pi build is the first task of sub-project 2.

- [ ] **Step 8: Commit the results log**

```bash
git add -A
git commit -m "docs: end-to-end verification results for castr core"
```

---

## Self-review notes

- Spec 5.1 to 5.2 (discovery, pairing): Tasks 12, 14, 15. Pairing state lives at the application layer (Task 21 handshake), with TLS pinning on the sender side; the receiver checks the paired store before any media flows, which satisfies spec 5.2.
- Spec 6 (transport and framing): Tasks 1 to 4, 13, 16.
- Spec 7 (media pipeline): Tasks 6, 7, 17 to 20, 21, 22. Zero-copy GPU encode is out of scope per spec 13.
- Spec 8 (adaptation, modes): Tasks 9, 11, and the mode plumbing in 21 to 23.
- Spec 9 (audio and sync): Tasks 8, 10, 18, 21 (audio-master clock, drift resampling capped at 0.5%).
- Spec 10 (errors, reconnection): Tasks 5, 21, 22, verified in 24.
- Spec 11 (CLI): Tasks 21, 22, 23.
- Spec 1.1 (portable exe): Task 24 step 1 with static CRT.
- Known simplification: the MF decoder outputs CPU NV12 rather than D3D11 textures (spec 7.2 step 3 says D3D11 hwaccel). The Microsoft decoder still uses DXVA internally when available; GPU-resident output is deferred with zero-copy encode.

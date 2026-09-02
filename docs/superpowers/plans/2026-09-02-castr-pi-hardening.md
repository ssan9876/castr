# castr Pi hardening (sub-project 2) Implementation Plan

> **For agentic workers:** REQUIRED SUB-SKILL: Use superpowers:subagent-driven-development (recommended) or superpowers:executing-plans to implement this plan task-by-task. Steps use checkbox (`- [ ]`) syntax for tracking.

**Goal:** Hardware H.264 decode on the Raspberry Pi 3 through V4L2, a one-shot Pi setup script, and a systemd service, so the Pi shows a 1920x1080 castr stream at 30 fps from power-on with nobody logged in.

**Architecture:** A new Linux-only crate `castr-codec-v4l2` implements the existing `castr_media::VideoDecoder` trait over the Pi's `bcm2835-codec` V4L2 memory-to-memory decoder (`/dev/video10`), talking to the kernel through hand-written `repr(C)` structs and `libc::ioctl` behind a small `Ops` trait so the buffer/state logic is unit-testable with a scripted fake. The receiver gains `--decoder v4l2`, automatic fallback to openh264, a rebuild-then-fallback rule for mid-stream failures, and a periodic performance stats line. `scripts/pi/` gains `setup.sh` (config.txt, module autoload, `castr` user, systemd unit) and `deploy.sh` (cross-build, push, restart).

**Tech Stack:** Rust stable, `libc` 0.2 (ioctl/mmap/poll), existing `castr-media` (trait, `RawFrame`, software encoder for test clips), SDL2 unchanged, Docker aarch64 cross-build from sub-project 1, systemd.

**Spec:** `docs/superpowers/specs/2026-09-02-castr-pi-hardening-design.md`

## Global Constraints

- Everything is receiver-side and Linux-only. The Windows sender, the protocol, the Windows receiver, and `castr-media`/`castr-net`/`castr-proto` internals are untouched (spec section 1).
- `castr-codec-v4l2` must compile to an empty crate on Windows: `cargo test --workspace` on Windows stays green. New dependencies (`libc`) live only in `castr-codec-v4l2`, and the receiver depends on the crate only under `[target.'cfg(target_os = "linux")'.dependencies]` (spec section 10).
- No GStreamer, no FFmpeg, no bindgen at build time (spec sections 3, 4).
- Decoder device: `/dev/video10`, overridable with `CASTR_V4L2_DEVICE`. Input `V4L2_PIX_FMT_H264` (Annex B, one access unit per buffer, 1 MiB buffers, 4 buffers, at most 2 in flight). Output `V4L2_PIX_FMT_NV12`, MMAP plus `VIDIOC_EXPBUF` DMABUF export, 6 buffers requested (spec 4.1).
- Stall: `decode` returns `Err("decoder stalled")` after 60 consecutive queued access units with no decoded frame, or after 2 s of polling with no progress (spec 4.5).
- Receiver rule: three decoder errors within 10 s rebuild the decoder; if the rebuild fails, fall back to software for the rest of the session (spec 3.1).
- Receiver logs a stats line every 5 s while streaming (spec 5).
- Service runs as user `castr`, `XDG_CONFIG_HOME=/var/lib/castr/config`, `SDL_VIDEODRIVER=kmsdrm`, `Restart=always`, binary at `/usr/local/bin/castr-receiver` (spec 6).
- Every commit: `cargo fmt`, `cargo clippy --workspace --tests` clean of new warnings (four pre-existing warnings in `clock.rs`, `reassemble.rs`, `session.rs`, `packetize.rs` are known), `cargo test --workspace` green on Windows, and the Pi cross-build (`bash scripts/pi/build-pi.sh`) succeeds.
- Hardware tests are `#[ignore]` and run on the Pi at 192.168.88.157 (user `dietpi`, key auth, no SFTP: copy with `cat file | ssh host 'cat > dest'`). Never `pkill -f` inside an ssh command string (it matches the remote shell itself); use `pkill -x`.
- Windows dev shell: `export PATH="$PATH:$HOME/.cargo/bin:/c/Program Files/CMake/bin"` before cargo. Cross-build needs Docker Desktop running and `MSYS_NO_PATHCONV=1` (the script sets it).

---

## File structure

```
crates/castr-codec-v4l2/
  Cargo.toml              linux-only deps: libc; dev-dep castr-media (test clips)
  src/lib.rs              cfg(linux) gate; pub use decoder::V4l2Decoder; mod declarations
  src/sys.rs              repr(C) videodev2 structs, constants, ioctl request numbers
  src/ops.rs              trait Ops (typed V4L2 operations), RealOps over libc, Mapping
  src/annexb.rs           access-unit checks
  src/queue.rs            Queue: one V4L2 buffer queue (allocate/queue/dequeue/release)
  src/decoder.rs          V4l2Decoder<O: Ops>: startup, decode, source change, stall, Drop
  src/fake.rs             FakeOps for unit tests (cfg(test))
  tests/hw.rs             #[ignore] hardware tests, run on the Pi
crates/castr-receiver/
  Cargo.toml              + castr-codec-v4l2 under cfg(target_os = "linux")
  src/pipeline.rs         DecoderChoice::V4l2, open_decoder, ErrorWindow, rebuild, PerfStats
  src/main.rs             hostname default reads /etc/hostname on Linux
scripts/pi/
  setup.sh                one-shot Pi setup (root)
  castr-receiver.service  systemd unit
  deploy.sh               build + push + restart from the dev machine
README.md                 Pi section rewritten around setup.sh/deploy.sh
docs/superpowers/verification/2026-09-02-castr-pi-hardening-e2e.md   Task 9 log
```

---

### Task 0: Crate scaffold and receiver wiring

**Files:**
- Create: `crates/castr-codec-v4l2/Cargo.toml`
- Create: `crates/castr-codec-v4l2/src/lib.rs`
- Modify: `crates/castr-receiver/Cargo.toml`

**Interfaces:**
- Produces: crate `castr-codec-v4l2` (empty on non-Linux), depended on by `castr-receiver` on Linux only.

- [ ] **Step 1: Create the crate manifest**

```toml
# crates/castr-codec-v4l2/Cargo.toml
[package]
name = "castr-codec-v4l2"
version.workspace = true
edition.workspace = true
license.workspace = true

[dependencies]
anyhow.workspace = true
tracing.workspace = true
castr-media = { path = "../castr-media" }

[target.'cfg(target_os = "linux")'.dependencies]
libc = "0.2"

[dev-dependencies]
castr-media = { path = "../castr-media" }
```

- [ ] **Step 2: Create the gated lib root**

```rust
// crates/castr-codec-v4l2/src/lib.rs
//! Hardware H.264 decode on Raspberry Pi through the V4L2 memory-to-memory
//! decoder (`bcm2835-codec`, `/dev/video10`). Linux only; on other targets this
//! crate is empty so the workspace still builds everywhere.

// Pure Rust, no OS dependency: their tests run in the Windows workspace suite too.
pub mod annexb;
pub mod sys;

#[cfg(target_os = "linux")]
pub mod decoder;
#[cfg(target_os = "linux")]
pub mod ops;
#[cfg(target_os = "linux")]
pub mod queue;
#[cfg(all(target_os = "linux", test))]
pub(crate) mod fake;

#[cfg(target_os = "linux")]
pub use decoder::V4l2Decoder;
```

For this task only, so the crate builds before the modules exist, create the five module files as empty placeholders with a single line comment each (`// filled in by Task N`); every later task replaces its file. `fake.rs` is created in Task 3.

- [ ] **Step 3: Wire the receiver dependency**

Append to `crates/castr-receiver/Cargo.toml`:

```toml
[target.'cfg(target_os = "linux")'.dependencies]
castr-codec-v4l2 = { path = "../castr-codec-v4l2" }
```

- [ ] **Step 4: Verify both builds**

Run (Windows): `cargo build --workspace && cargo test -q --workspace 2>&1 | grep -E "test result|FAILED" | tail -3`
Expected: builds; all existing suites still pass (114 tests as of `d2603b3`).

Run: `bash scripts/pi/build-pi.sh`
Expected: `built dist/castr-receiver-aarch64`.

- [ ] **Step 5: Commit**

```bash
git add crates/castr-codec-v4l2 crates/castr-receiver/Cargo.toml Cargo.lock
git commit -m "feat(v4l2): scaffold Linux-only castr-codec-v4l2 crate"
```

---

### Task 1: `sys.rs`: videodev2 ABI and ioctl numbers

**Files:**
- Create: `crates/castr-codec-v4l2/src/sys.rs`

**Interfaces:**
- Produces: `repr(C)` structs `v4l2_capability`, `v4l2_fmtdesc`, `v4l2_format` (with `pix_mp()` accessors), `v4l2_requestbuffers`, `v4l2_plane`, `v4l2_buffer`, `v4l2_exportbuffer`, `v4l2_event_subscription`, `v4l2_event`, `v4l2_selection`, `v4l2_rect`; constants listed below; `const` request numbers `VIDIOC_*`; `pub const fn fourcc`.

- [ ] **Step 1: Write the layout tests**

Add at the bottom of the (new) `sys.rs`:

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use std::mem::size_of;

    // Sizes from <linux/videodev2.h> on 64-bit Linux (aarch64 and x86_64 agree).
    #[test]
    fn struct_sizes_match_the_kernel_abi() {
        assert_eq!(size_of::<v4l2_capability>(), 104);
        assert_eq!(size_of::<v4l2_fmtdesc>(), 64);
        assert_eq!(size_of::<v4l2_plane_pix_format>(), 20);
        assert_eq!(size_of::<v4l2_pix_format_mplane>(), 192);
        assert_eq!(size_of::<v4l2_format>(), 208);
        assert_eq!(size_of::<v4l2_requestbuffers>(), 20);
        assert_eq!(size_of::<v4l2_plane>(), 64);
        assert_eq!(size_of::<v4l2_buffer>(), 88);
        assert_eq!(size_of::<v4l2_exportbuffer>(), 64);
        assert_eq!(size_of::<v4l2_event_subscription>(), 32);
        assert_eq!(size_of::<v4l2_event>(), 136);
        assert_eq!(size_of::<v4l2_selection>(), 64);
    }

    #[test]
    fn ioctl_numbers_match_the_kernel() {
        // Values printed by `v4l2-ctl` sources / kernel headers on 64-bit Linux.
        assert_eq!(VIDIOC_QUERYCAP, 0x8068_5600);
        assert_eq!(VIDIOC_ENUM_FMT, 0xC040_5602);
        assert_eq!(VIDIOC_G_FMT, 0xC0D0_5604);
        assert_eq!(VIDIOC_S_FMT, 0xC0D0_5605);
        assert_eq!(VIDIOC_REQBUFS, 0xC014_5608);
        assert_eq!(VIDIOC_QUERYBUF, 0xC058_5609);
        assert_eq!(VIDIOC_QBUF, 0xC058_560F);
        assert_eq!(VIDIOC_EXPBUF, 0xC040_5610);
        assert_eq!(VIDIOC_DQBUF, 0xC058_5611);
        assert_eq!(VIDIOC_STREAMON, 0x4004_5612);
        assert_eq!(VIDIOC_STREAMOFF, 0x4004_5613);
        assert_eq!(VIDIOC_DQEVENT, 0x8088_5659);
        assert_eq!(VIDIOC_SUBSCRIBE_EVENT, 0x4020_565A);
        assert_eq!(VIDIOC_G_SELECTION, 0xC040_565E);
    }

    #[test]
    fn fourcc_matches_v4l2() {
        assert_eq!(V4L2_PIX_FMT_H264, 0x3436_3248);
        assert_eq!(V4L2_PIX_FMT_NV12, 0x3231_564E);
    }

    #[test]
    fn format_pix_mp_view_round_trips() {
        let mut f = v4l2_format::new(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE);
        {
            let mp = f.pix_mp_mut();
            mp.width = 1920;
            mp.height = 1088;
            mp.pixelformat = V4L2_PIX_FMT_NV12;
            mp.num_planes = 1;
            mp.plane_fmt[0].bytesperline = 1920;
            mp.plane_fmt[0].sizeimage = 1920 * 1088 * 3 / 2;
        }
        let mp = f.pix_mp();
        assert_eq!((mp.width, mp.height, mp.num_planes), (1920, 1088, 1));
        assert_eq!(mp.plane_fmt[0].bytesperline, 1920);
        assert_eq!(f.type_, V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE);
    }
}
```

- [ ] **Step 2: Run the tests to see them fail**

Run: `cargo test -p castr-codec-v4l2 sys::` on Windows.
Expected: compile error (module empty). `sys.rs` is not Linux-specific and is declared without a cfg gate in `lib.rs` (Task 0); keep it free of `libc` types so these tests run in the Windows workspace suite.

- [ ] **Step 3: Write `sys.rs`**

```rust
// crates/castr-codec-v4l2/src/sys.rs
//! The subset of <linux/videodev2.h> the decoder needs, written by hand so no
//! bindgen or kernel headers are needed at build time. Layouts are the 64-bit
//! Linux ABI (identical on aarch64 and x86_64) and are pinned by tests.
#![allow(non_camel_case_types, dead_code)]

pub const V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE: u32 = 9;
pub const V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE: u32 = 10;
pub const V4L2_MEMORY_MMAP: u32 = 1;
pub const V4L2_FIELD_NONE: u32 = 1;
pub const V4L2_CAP_VIDEO_M2M_MPLANE: u32 = 0x0000_4000;
pub const V4L2_CAP_STREAMING: u32 = 0x0400_0000;
pub const V4L2_CAP_DEVICE_CAPS: u32 = 0x8000_0000;
pub const V4L2_EVENT_EOS: u32 = 2;
pub const V4L2_EVENT_SOURCE_CHANGE: u32 = 5;
pub const V4L2_EVENT_SRC_CH_RESOLUTION: u32 = 1;
pub const V4L2_SEL_TGT_COMPOSE: u32 = 0x0100;
pub const V4L2_BUF_FLAG_LAST: u32 = 0x0010_0000;
pub const VIDEO_MAX_PLANES: usize = 8;

pub const fn fourcc(a: u8, b: u8, c: u8, d: u8) -> u32 {
    (a as u32) | ((b as u32) << 8) | ((c as u32) << 16) | ((d as u32) << 24)
}
pub const V4L2_PIX_FMT_H264: u32 = fourcc(b'H', b'2', b'6', b'4');
pub const V4L2_PIX_FMT_NV12: u32 = fourcc(b'N', b'V', b'1', b'2');

// _IOC encoding (asm-generic): dir:2 | size:14 | type:8 | nr:8.
const IOC_NONE: u32 = 0;
const IOC_WRITE: u32 = 1;
const IOC_READ: u32 = 2;
const fn ioc(dir: u32, nr: u32, size: usize) -> u64 {
    ((dir << 30) | ((size as u32) << 16) | ((b'V' as u32) << 8) | nr) as u64
}
const fn ior(nr: u32, size: usize) -> u64 {
    ioc(IOC_READ, nr, size)
}
const fn iow(nr: u32, size: usize) -> u64 {
    ioc(IOC_WRITE, nr, size)
}
const fn iowr(nr: u32, size: usize) -> u64 {
    ioc(IOC_READ | IOC_WRITE, nr, size)
}

use std::mem::size_of;
pub const VIDIOC_QUERYCAP: u64 = ior(0, size_of::<v4l2_capability>());
pub const VIDIOC_ENUM_FMT: u64 = iowr(2, size_of::<v4l2_fmtdesc>());
pub const VIDIOC_G_FMT: u64 = iowr(4, size_of::<v4l2_format>());
pub const VIDIOC_S_FMT: u64 = iowr(5, size_of::<v4l2_format>());
pub const VIDIOC_REQBUFS: u64 = iowr(8, size_of::<v4l2_requestbuffers>());
pub const VIDIOC_QUERYBUF: u64 = iowr(9, size_of::<v4l2_buffer>());
pub const VIDIOC_QBUF: u64 = iowr(15, size_of::<v4l2_buffer>());
pub const VIDIOC_EXPBUF: u64 = iowr(16, size_of::<v4l2_exportbuffer>());
pub const VIDIOC_DQBUF: u64 = iowr(17, size_of::<v4l2_buffer>());
pub const VIDIOC_STREAMON: u64 = iow(18, size_of::<i32>());
pub const VIDIOC_STREAMOFF: u64 = iow(19, size_of::<i32>());
pub const VIDIOC_DQEVENT: u64 = ior(89, size_of::<v4l2_event>());
pub const VIDIOC_SUBSCRIBE_EVENT: u64 = iow(90, size_of::<v4l2_event_subscription>());
pub const VIDIOC_G_SELECTION: u64 = iowr(94, size_of::<v4l2_selection>());

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_capability {
    pub driver: [u8; 16],
    pub card: [u8; 32],
    pub bus_info: [u8; 32],
    pub version: u32,
    pub capabilities: u32,
    pub device_caps: u32,
    pub reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_fmtdesc {
    pub index: u32,
    pub type_: u32,
    pub flags: u32,
    pub description: [u8; 32],
    pub pixelformat: u32,
    pub mbus_code: u32,
    pub reserved: [u32; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_plane_pix_format {
    pub sizeimage: u32,
    pub bytesperline: u32,
    pub reserved: [u16; 6],
}

/// `__attribute__((packed))` in the kernel header.
#[repr(C, packed)]
#[derive(Clone, Copy)]
pub struct v4l2_pix_format_mplane {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub field: u32,
    pub colorspace: u32,
    pub plane_fmt: [v4l2_plane_pix_format; VIDEO_MAX_PLANES],
    pub num_planes: u8,
    pub flags: u8,
    pub ycbcr_enc: u8,
    pub quantization: u8,
    pub xfer_func: u8,
    pub reserved: [u8; 7],
}

/// The `fmt` union: 200 bytes, 8-aligned because some members hold pointers.
#[repr(C, align(8))]
#[derive(Clone, Copy)]
pub struct v4l2_format_fmt {
    pub raw_data: [u8; 200],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_format {
    pub type_: u32,
    pub fmt: v4l2_format_fmt,
}

impl v4l2_format {
    pub fn new(type_: u32) -> Self {
        Self {
            type_,
            fmt: v4l2_format_fmt { raw_data: [0; 200] },
        }
    }
    /// Multiplanar view of `fmt`. Packed struct: read fields by value, do not
    /// take references to them.
    pub fn pix_mp(&self) -> v4l2_pix_format_mplane {
        // SAFETY: raw_data is 200 bytes >= 192, both plain-old-data.
        unsafe { std::ptr::read_unaligned(self.fmt.raw_data.as_ptr() as *const v4l2_pix_format_mplane) }
    }
    pub fn pix_mp_mut(&mut self) -> PixMpView<'_> {
        PixMpView { fmt: self, val: self.pix_mp() }
    }
}

/// Write-back guard so callers can mutate `v4l2_pix_format_mplane` fields
/// without dealing with the packed layout themselves.
pub struct PixMpView<'a> {
    fmt: &'a mut v4l2_format,
    val: v4l2_pix_format_mplane,
}
impl std::ops::Deref for PixMpView<'_> {
    type Target = v4l2_pix_format_mplane;
    fn deref(&self) -> &v4l2_pix_format_mplane {
        &self.val
    }
}
impl std::ops::DerefMut for PixMpView<'_> {
    fn deref_mut(&mut self) -> &mut v4l2_pix_format_mplane {
        &mut self.val
    }
}
impl Drop for PixMpView<'_> {
    fn drop(&mut self) {
        // SAFETY: see pix_mp.
        unsafe {
            std::ptr::write_unaligned(
                self.fmt.fmt.raw_data.as_mut_ptr() as *mut v4l2_pix_format_mplane,
                self.val,
            )
        }
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_requestbuffers {
    pub count: u32,
    pub type_: u32,
    pub memory: u32,
    pub capabilities: u32,
    pub flags: u8,
    pub reserved: [u8; 3],
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_timeval {
    pub tv_sec: i64,
    pub tv_usec: i64,
}

#[repr(C)]
#[derive(Clone, Copy, Default)]
pub struct v4l2_timecode {
    pub type_: u32,
    pub flags: u32,
    pub frames: u8,
    pub seconds: u8,
    pub minutes: u8,
    pub hours: u8,
    pub userbits: [u8; 4],
}

/// `union { __u32 mem_offset; unsigned long userptr; __s32 fd; } m;`
#[repr(C)]
#[derive(Clone, Copy)]
pub union v4l2_plane_m {
    pub mem_offset: u32,
    pub userptr: u64,
    pub fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_plane {
    pub bytesused: u32,
    pub length: u32,
    pub m: v4l2_plane_m,
    pub data_offset: u32,
    pub reserved: [u32; 11],
}

impl Default for v4l2_plane {
    fn default() -> Self {
        // SAFETY: all-zero is a valid v4l2_plane.
        unsafe { std::mem::zeroed() }
    }
}

/// `union { __u32 offset; unsigned long userptr; struct v4l2_plane *planes; __s32 fd; } m;`
#[repr(C)]
#[derive(Clone, Copy)]
pub union v4l2_buffer_m {
    pub offset: u32,
    pub userptr: u64,
    pub planes: *mut v4l2_plane,
    pub fd: i32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_buffer {
    pub index: u32,
    pub type_: u32,
    pub bytesused: u32,
    pub flags: u32,
    pub field: u32,
    pub timestamp: v4l2_timeval,
    pub timecode: v4l2_timecode,
    pub sequence: u32,
    pub memory: u32,
    pub m: v4l2_buffer_m,
    pub length: u32,
    pub reserved2: u32,
    pub request_fd: i32,
}

impl v4l2_buffer {
    /// A zeroed multiplanar buffer descriptor pointing at `planes`.
    pub fn mplane(type_: u32, index: u32, planes: &mut [v4l2_plane]) -> Self {
        // SAFETY: all-zero is a valid v4l2_buffer; fields are then set.
        let mut b: v4l2_buffer = unsafe { std::mem::zeroed() };
        b.index = index;
        b.type_ = type_;
        b.memory = V4L2_MEMORY_MMAP;
        b.length = planes.len() as u32;
        b.m = v4l2_buffer_m { planes: planes.as_mut_ptr() };
        b
    }
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_exportbuffer {
    pub type_: u32,
    pub index: u32,
    pub plane: u32,
    pub flags: u32,
    pub fd: i32,
    pub reserved: [u32; 11],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_event_subscription {
    pub type_: u32,
    pub id: u32,
    pub flags: u32,
    pub reserved: [u32; 5],
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_timespec {
    pub tv_sec: i64,
    pub tv_nsec: i64,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_event {
    pub type_: u32,
    /// Union payload; for SOURCE_CHANGE the first u32 is `changes`.
    pub u: [u8; 64],
    pub pending: u32,
    pub sequence: u32,
    pub timestamp: v4l2_timespec,
    pub id: u32,
    pub reserved: [u32; 8],
}

impl v4l2_event {
    pub fn src_changes(&self) -> u32 {
        u32::from_ne_bytes([self.u[0], self.u[1], self.u[2], self.u[3]])
    }
}

#[repr(C)]
#[derive(Clone, Copy, Default, Debug, PartialEq, Eq)]
pub struct v4l2_rect {
    pub left: i32,
    pub top: i32,
    pub width: u32,
    pub height: u32,
}

#[repr(C)]
#[derive(Clone, Copy)]
pub struct v4l2_selection {
    pub type_: u32,
    pub target: u32,
    pub flags: u32,
    pub r: v4l2_rect,
    pub reserved: [u32; 9],
}

pub fn zeroed<T: Copy>() -> T {
    // SAFETY: every struct in this module is plain data for which all-zero is valid.
    unsafe { std::mem::zeroed() }
}
```

Note for the implementer: `v4l2_event` has a 4-byte pad before `timestamp` (offset 80) which Rust inserts automatically because `v4l2_timespec` is 8-aligned; the size test proves it.

- [ ] **Step 4: Run the tests**

Run: `cargo test -p castr-codec-v4l2 sys::`
Expected: 4 passed. If a size assertion fails, fix the struct, never the expected number: the numbers come from the kernel header.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/castr-codec-v4l2
git commit -m "feat(v4l2): hand-written videodev2 ABI with layout tests"
```

---

### Task 2: `annexb.rs`: access-unit checks

**Files:**
- Create: `crates/castr-codec-v4l2/src/annexb.rs`

**Interfaces:**
- Produces: `pub fn starts_with_start_code(data: &[u8]) -> bool`, `pub fn nal_types(data: &[u8]) -> Vec<u8>`, `pub fn has_sps(data: &[u8]) -> bool`, `pub fn is_idr(data: &[u8]) -> bool`, `pub fn check_access_unit(data: &[u8]) -> anyhow::Result<()>`.

This module has no Linux dependency; like `sys.rs`, declare it without the cfg gate in `lib.rs` so its tests run on Windows.

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;

    const SPS: [u8; 5] = [0, 0, 0, 1, 0x67];
    const PPS: [u8; 5] = [0, 0, 0, 1, 0x68];
    const IDR: [u8; 5] = [0, 0, 0, 1, 0x65];
    const P: [u8; 4] = [0, 0, 1, 0x41];

    fn cat(parts: &[&[u8]]) -> Vec<u8> {
        parts.concat()
    }

    #[test]
    fn detects_three_and_four_byte_start_codes() {
        assert!(starts_with_start_code(&SPS));
        assert!(starts_with_start_code(&P));
        assert!(!starts_with_start_code(&[0, 0, 0, 0, 1]));
        assert!(!starts_with_start_code(&[]));
        assert!(!starts_with_start_code(&[0x65, 1, 2]));
    }

    #[test]
    fn lists_nal_types_in_order() {
        let au = cat(&[&SPS, &[1, 2, 3], &PPS, &IDR, &[0, 0, 3, 1]]);
        assert_eq!(nal_types(&au), vec![7, 8, 5]);
    }

    #[test]
    fn keyframe_and_parameter_set_detection() {
        let key = cat(&[&SPS, &PPS, &IDR]);
        assert!(has_sps(&key));
        assert!(is_idr(&key));
        assert!(!has_sps(&P));
        assert!(!is_idr(&P));
    }

    #[test]
    fn check_rejects_non_annex_b_and_empty_input() {
        assert!(check_access_unit(&[]).is_err());
        assert!(check_access_unit(&[0x00, 0x00, 0x02, 0x09]).is_err());
        assert!(check_access_unit(&P).is_ok());
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p castr-codec-v4l2 annexb::`
Expected: compile errors, functions missing.

- [ ] **Step 3: Implement**

```rust
// crates/castr-codec-v4l2/src/annexb.rs
//! Minimal Annex B inspection: enough to reject junk before it reaches the
//! hardware and to recognise keyframes in tests and logs.

use anyhow::bail;

pub fn starts_with_start_code(data: &[u8]) -> bool {
    data.starts_with(&[0, 0, 1]) || data.starts_with(&[0, 0, 0, 1])
}

/// NAL unit types (low 5 bits of the byte after each start code), in order.
pub fn nal_types(data: &[u8]) -> Vec<u8> {
    let mut out = Vec::new();
    let mut i = 0;
    while i + 3 <= data.len() {
        if data[i] == 0 && data[i + 1] == 0 && data[i + 2] == 1 {
            if let Some(&b) = data.get(i + 3) {
                out.push(b & 0x1f);
            }
            i += 3;
        } else {
            i += 1;
        }
    }
    out
}

pub fn has_sps(data: &[u8]) -> bool {
    nal_types(data).contains(&7)
}

pub fn is_idr(data: &[u8]) -> bool {
    nal_types(data).contains(&5)
}

pub fn check_access_unit(data: &[u8]) -> anyhow::Result<()> {
    if data.is_empty() {
        bail!("empty access unit");
    }
    if !starts_with_start_code(data) {
        bail!("access unit is not Annex B (no start code)");
    }
    Ok(())
}
```

- [ ] **Step 4: Run the tests**

Run: `cargo test -p castr-codec-v4l2 annexb::`
Expected: 4 passed.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/castr-codec-v4l2
git commit -m "feat(v4l2): Annex B access-unit checks"
```

---

### Task 3: `ops.rs` and `fake.rs`: typed V4L2 operations, real and fake

**Files:**
- Create: `crates/castr-codec-v4l2/src/ops.rs`
- Create: `crates/castr-codec-v4l2/src/fake.rs`

**Interfaces:**
- Produces:

```rust
pub struct Mapping { .. }                       // mmap'd plane or (fake) heap bytes
impl Mapping { pub fn as_slice(&self) -> &[u8]; pub fn as_mut_slice(&mut self) -> &mut [u8]; pub fn len(&self) -> usize; }
pub struct PlaneInfo { pub length: u32, pub mem_offset: u32 }
pub struct Dequeued { pub index: u32, pub bytesused: u32, pub timestamp_us: u64, pub flags: u32 }
pub struct Event { pub kind: u32, pub changes: u32 }
pub struct PollResult { pub readable: bool, pub writable: bool, pub event: bool }
pub struct FormatInfo { pub width: u32, pub height: u32, pub pixelformat: u32, pub bytesperline: u32, pub sizeimage: u32 }
pub trait Ops {
    fn query_cap(&mut self) -> io::Result<sys::v4l2_capability>;
    fn enum_fmt(&mut self, buf_type: u32, index: u32) -> io::Result<Option<u32>>;
    fn g_fmt(&mut self, buf_type: u32) -> io::Result<FormatInfo>;
    fn s_fmt(&mut self, buf_type: u32, want: &FormatInfo) -> io::Result<FormatInfo>;
    fn reqbufs(&mut self, buf_type: u32, count: u32) -> io::Result<u32>;
    fn querybuf(&mut self, buf_type: u32, index: u32) -> io::Result<PlaneInfo>;
    fn mmap(&mut self, length: usize, offset: u32) -> io::Result<Mapping>;
    fn expbuf(&mut self, buf_type: u32, index: u32) -> io::Result<OwnedFd>;
    fn qbuf(&mut self, buf_type: u32, index: u32, length: u32, bytesused: u32, timestamp_us: u64) -> io::Result<()>;
    fn dqbuf(&mut self, buf_type: u32) -> io::Result<Option<Dequeued>>;
    fn streamon(&mut self, buf_type: u32) -> io::Result<()>;
    fn streamoff(&mut self, buf_type: u32) -> io::Result<()>;
    fn subscribe(&mut self, event_type: u32) -> io::Result<()>;
    fn dqevent(&mut self) -> io::Result<Option<Event>>;
    fn poll(&mut self, timeout_ms: i32) -> io::Result<PollResult>;
    fn g_selection_compose(&mut self, buf_type: u32) -> io::Result<sys::v4l2_rect>;
}
pub struct RealOps { .. }
impl RealOps { pub fn open(path: &str) -> io::Result<RealOps>; }
pub(crate) struct FakeOps { .. }   // test only: scripted, records calls
```

- [ ] **Step 1: Write the FakeOps tests (they double as the fake's spec)**

```rust
// bottom of crates/castr-codec-v4l2/src/fake.rs
#[cfg(test)]
mod tests {
    use super::*;
    use crate::ops::Ops;
    use crate::sys::*;

    #[test]
    fn fake_records_calls_and_serves_scripted_capture_buffers() {
        let mut f = FakeOps::new();
        assert_eq!(f.reqbufs(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 4).unwrap(), 4);
        let m = f.mmap(16, 0).unwrap();
        assert_eq!(m.len(), 16);
        f.qbuf(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, 1, 16, 5, 1000).unwrap();
        assert_eq!(f.calls, vec!["reqbufs(10,4)", "mmap(16,0)", "qbuf(10,1,5,1000)"]);
        // Nothing scripted: dequeue is EAGAIN -> None.
        assert!(f.dqbuf(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE).unwrap().is_none());
        f.push_dequeue(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, Dequeued { index: 1, bytesused: 0, timestamp_us: 1000, flags: 0 });
        let d = f.dqbuf(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE).unwrap().unwrap();
        assert_eq!(d.index, 1);
    }

    #[test]
    fn fake_serves_events_and_formats() {
        let mut f = FakeOps::new();
        assert!(f.dqevent().unwrap().is_none());
        f.push_event(Event { kind: V4L2_EVENT_SOURCE_CHANGE, changes: V4L2_EVENT_SRC_CH_RESOLUTION });
        assert_eq!(f.dqevent().unwrap().unwrap().kind, V4L2_EVENT_SOURCE_CHANGE);
        f.capture_format = FormatInfo { width: 640, height: 368, pixelformat: V4L2_PIX_FMT_NV12, bytesperline: 640, sizeimage: 640 * 368 * 3 / 2 };
        f.compose = v4l2_rect { left: 0, top: 0, width: 640, height: 360 };
        assert_eq!(f.g_fmt(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE).unwrap().height, 368);
        assert_eq!(f.g_selection_compose(V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE).unwrap().height, 360);
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p castr-codec-v4l2 fake::` on the cross-build (see Step 5 for how; the modules are Linux-gated). On Windows these tests are skipped by the gate, so the check happens in Docker.
Expected: compile error.

- [ ] **Step 3: Write `ops.rs`**

```rust
// crates/castr-codec-v4l2/src/ops.rs
//! Typed V4L2 operations. `RealOps` talks to the kernel through libc;
//! `FakeOps` (tests) scripts the same calls so the queue and decoder state
//! machines are testable without hardware.

use crate::sys::*;
use std::fs::{File, OpenOptions};
use std::io;
use std::os::fd::{AsRawFd, FromRawFd, OwnedFd};

pub struct PlaneInfo {
    pub length: u32,
    pub mem_offset: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Dequeued {
    pub index: u32,
    pub bytesused: u32,
    pub timestamp_us: u64,
    pub flags: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub struct Event {
    pub kind: u32,
    pub changes: u32,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct PollResult {
    pub readable: bool,
    pub writable: bool,
    pub event: bool,
}

#[derive(Clone, Copy, Debug, Default, PartialEq, Eq)]
pub struct FormatInfo {
    pub width: u32,
    pub height: u32,
    pub pixelformat: u32,
    pub bytesperline: u32,
    pub sizeimage: u32,
}

/// One mapped plane. Real mappings are unmapped on drop; test mappings own a Vec.
pub struct Mapping {
    ptr: *mut u8,
    len: usize,
    owned: Option<Vec<u8>>,
}

// SAFETY: the mapping is only ever used from the decode thread; `Send` lets the
// decoder move between threads at construction.
unsafe impl Send for Mapping {}

impl Mapping {
    pub(crate) fn owned(len: usize) -> Self {
        let mut v = vec![0u8; len];
        let ptr = v.as_mut_ptr();
        Self { ptr, len, owned: Some(v) }
    }
    pub fn len(&self) -> usize {
        self.len
    }
    pub fn is_empty(&self) -> bool {
        self.len == 0
    }
    pub fn as_slice(&self) -> &[u8] {
        // SAFETY: ptr/len describe a live mapping (or Vec) for the life of self.
        unsafe { std::slice::from_raw_parts(self.ptr, self.len) }
    }
    pub fn as_mut_slice(&mut self) -> &mut [u8] {
        // SAFETY: as above; &mut self guarantees exclusivity.
        unsafe { std::slice::from_raw_parts_mut(self.ptr, self.len) }
    }
}

impl Drop for Mapping {
    fn drop(&mut self) {
        if self.owned.is_none() && !self.ptr.is_null() {
            // SAFETY: ptr/len came from a successful mmap in RealOps::mmap.
            unsafe {
                libc::munmap(self.ptr as *mut libc::c_void, self.len);
            }
        }
    }
}

pub trait Ops {
    fn query_cap(&mut self) -> io::Result<v4l2_capability>;
    /// Pixel format at `index` for the queue type, or None past the end.
    fn enum_fmt(&mut self, buf_type: u32, index: u32) -> io::Result<Option<u32>>;
    fn g_fmt(&mut self, buf_type: u32) -> io::Result<FormatInfo>;
    /// Sets the format; returns what the driver actually accepted.
    fn s_fmt(&mut self, buf_type: u32, want: &FormatInfo) -> io::Result<FormatInfo>;
    /// Returns the count granted (drivers may round up).
    fn reqbufs(&mut self, buf_type: u32, count: u32) -> io::Result<u32>;
    fn querybuf(&mut self, buf_type: u32, index: u32) -> io::Result<PlaneInfo>;
    fn mmap(&mut self, length: usize, offset: u32) -> io::Result<Mapping>;
    fn expbuf(&mut self, buf_type: u32, index: u32) -> io::Result<OwnedFd>;
    fn qbuf(&mut self, buf_type: u32, index: u32, length: u32, bytesused: u32, timestamp_us: u64) -> io::Result<()>;
    /// None when nothing is ready (EAGAIN).
    fn dqbuf(&mut self, buf_type: u32) -> io::Result<Option<Dequeued>>;
    fn streamon(&mut self, buf_type: u32) -> io::Result<()>;
    fn streamoff(&mut self, buf_type: u32) -> io::Result<()>;
    fn subscribe(&mut self, event_type: u32) -> io::Result<()>;
    /// None when no event is pending (ENOENT).
    fn dqevent(&mut self) -> io::Result<Option<Event>>;
    fn poll(&mut self, timeout_ms: i32) -> io::Result<PollResult>;
    fn g_selection_compose(&mut self, buf_type: u32) -> io::Result<v4l2_rect>;
}

pub struct RealOps {
    file: File,
}

impl RealOps {
    pub fn open(path: &str) -> io::Result<Self> {
        let file = OpenOptions::new()
            .read(true)
            .write(true)
            .custom_flags(libc::O_NONBLOCK)
            .open(path)?;
        Ok(Self { file })
    }

    fn fd(&self) -> libc::c_int {
        self.file.as_raw_fd()
    }

    fn ioctl<T>(&self, req: u64, arg: &mut T) -> io::Result<()> {
        // SAFETY: `req` and `T` are paired by the constants in sys.rs, so the
        // kernel reads/writes exactly size_of::<T>() bytes at `arg`.
        let r = unsafe { libc::ioctl(self.fd(), req as _, arg as *mut T) };
        if r < 0 {
            Err(io::Error::last_os_error())
        } else {
            Ok(())
        }
    }
}

use std::os::unix::fs::OpenOptionsExt;

fn split_ts(ts_us: u64) -> v4l2_timeval {
    v4l2_timeval {
        tv_sec: (ts_us / 1_000_000) as i64,
        tv_usec: (ts_us % 1_000_000) as i64,
    }
}

fn join_ts(tv: v4l2_timeval) -> u64 {
    (tv.tv_sec.max(0) as u64) * 1_000_000 + (tv.tv_usec.max(0) as u64)
}

impl Ops for RealOps {
    fn query_cap(&mut self) -> io::Result<v4l2_capability> {
        let mut cap: v4l2_capability = zeroed();
        self.ioctl(VIDIOC_QUERYCAP, &mut cap)?;
        Ok(cap)
    }

    fn enum_fmt(&mut self, buf_type: u32, index: u32) -> io::Result<Option<u32>> {
        let mut d: v4l2_fmtdesc = zeroed();
        d.type_ = buf_type;
        d.index = index;
        match self.ioctl(VIDIOC_ENUM_FMT, &mut d) {
            Ok(()) => Ok(Some(d.pixelformat)),
            Err(e) if e.raw_os_error() == Some(libc::EINVAL) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn g_fmt(&mut self, buf_type: u32) -> io::Result<FormatInfo> {
        let mut f = v4l2_format::new(buf_type);
        self.ioctl(VIDIOC_G_FMT, &mut f)?;
        let mp = f.pix_mp();
        let pf = mp.plane_fmt[0];
        Ok(FormatInfo {
            width: mp.width,
            height: mp.height,
            pixelformat: mp.pixelformat,
            bytesperline: pf.bytesperline,
            sizeimage: pf.sizeimage,
        })
    }

    fn s_fmt(&mut self, buf_type: u32, want: &FormatInfo) -> io::Result<FormatInfo> {
        let mut f = v4l2_format::new(buf_type);
        {
            let mut mp = f.pix_mp_mut();
            mp.width = want.width;
            mp.height = want.height;
            mp.pixelformat = want.pixelformat;
            mp.field = V4L2_FIELD_NONE;
            mp.num_planes = 1;
            mp.plane_fmt[0].sizeimage = want.sizeimage;
            mp.plane_fmt[0].bytesperline = want.bytesperline;
        }
        self.ioctl(VIDIOC_S_FMT, &mut f)?;
        let mp = f.pix_mp();
        let pf = mp.plane_fmt[0];
        Ok(FormatInfo {
            width: mp.width,
            height: mp.height,
            pixelformat: mp.pixelformat,
            bytesperline: pf.bytesperline,
            sizeimage: pf.sizeimage,
        })
    }

    fn reqbufs(&mut self, buf_type: u32, count: u32) -> io::Result<u32> {
        let mut r: v4l2_requestbuffers = zeroed();
        r.count = count;
        r.type_ = buf_type;
        r.memory = V4L2_MEMORY_MMAP;
        self.ioctl(VIDIOC_REQBUFS, &mut r)?;
        Ok(r.count)
    }

    fn querybuf(&mut self, buf_type: u32, index: u32) -> io::Result<PlaneInfo> {
        let mut planes = [v4l2_plane::default(); 1];
        let mut b = v4l2_buffer::mplane(buf_type, index, &mut planes);
        self.ioctl(VIDIOC_QUERYBUF, &mut b)?;
        // SAFETY: MMAP memory, so `mem_offset` is the active union member.
        let off = unsafe { planes[0].m.mem_offset };
        Ok(PlaneInfo { length: planes[0].length, mem_offset: off })
    }

    fn mmap(&mut self, length: usize, offset: u32) -> io::Result<Mapping> {
        // SAFETY: standard shared read/write mapping of a V4L2 buffer.
        let p = unsafe {
            libc::mmap(
                std::ptr::null_mut(),
                length,
                libc::PROT_READ | libc::PROT_WRITE,
                libc::MAP_SHARED,
                self.fd(),
                offset as libc::off_t,
            )
        };
        if p == libc::MAP_FAILED {
            return Err(io::Error::last_os_error());
        }
        Ok(Mapping { ptr: p as *mut u8, len: length, owned: None })
    }

    fn expbuf(&mut self, buf_type: u32, index: u32) -> io::Result<OwnedFd> {
        let mut e: v4l2_exportbuffer = zeroed();
        e.type_ = buf_type;
        e.index = index;
        e.flags = libc::O_RDWR as u32 | libc::O_CLOEXEC as u32;
        self.ioctl(VIDIOC_EXPBUF, &mut e)?;
        // SAFETY: the kernel just handed us a fresh fd we own.
        Ok(unsafe { OwnedFd::from_raw_fd(e.fd) })
    }

    fn qbuf(&mut self, buf_type: u32, index: u32, length: u32, bytesused: u32, timestamp_us: u64) -> io::Result<()> {
        let mut planes = [v4l2_plane::default(); 1];
        planes[0].length = length;
        planes[0].bytesused = bytesused;
        let mut b = v4l2_buffer::mplane(buf_type, index, &mut planes);
        b.field = V4L2_FIELD_NONE;
        b.timestamp = split_ts(timestamp_us);
        self.ioctl(VIDIOC_QBUF, &mut b)
    }

    fn dqbuf(&mut self, buf_type: u32) -> io::Result<Option<Dequeued>> {
        let mut planes = [v4l2_plane::default(); 1];
        let mut b = v4l2_buffer::mplane(buf_type, 0, &mut planes);
        match self.ioctl(VIDIOC_DQBUF, &mut b) {
            Ok(()) => Ok(Some(Dequeued {
                index: b.index,
                bytesused: planes[0].bytesused,
                timestamp_us: join_ts(b.timestamp),
                flags: b.flags,
            })),
            Err(e) if e.raw_os_error() == Some(libc::EAGAIN) => Ok(None),
            Err(e) => Err(e),
        }
    }

    fn streamon(&mut self, buf_type: u32) -> io::Result<()> {
        let mut t = buf_type as i32;
        self.ioctl(VIDIOC_STREAMON, &mut t)
    }

    fn streamoff(&mut self, buf_type: u32) -> io::Result<()> {
        let mut t = buf_type as i32;
        self.ioctl(VIDIOC_STREAMOFF, &mut t)
    }

    fn subscribe(&mut self, event_type: u32) -> io::Result<()> {
        let mut s: v4l2_event_subscription = zeroed();
        s.type_ = event_type;
        self.ioctl(VIDIOC_SUBSCRIBE_EVENT, &mut s)
    }

    fn dqevent(&mut self) -> io::Result<Option<Event>> {
        let mut e: v4l2_event = zeroed();
        match self.ioctl(VIDIOC_DQEVENT, &mut e) {
            Ok(()) => Ok(Some(Event { kind: e.type_, changes: e.src_changes() })),
            Err(err) if err.raw_os_error() == Some(libc::ENOENT) => Ok(None),
            Err(err) => Err(err),
        }
    }

    fn poll(&mut self, timeout_ms: i32) -> io::Result<PollResult> {
        let mut pfd = libc::pollfd {
            fd: self.fd(),
            events: libc::POLLIN | libc::POLLOUT | libc::POLLPRI,
            revents: 0,
        };
        // SAFETY: one valid pollfd.
        let r = unsafe { libc::poll(&mut pfd, 1, timeout_ms) };
        if r < 0 {
            let e = io::Error::last_os_error();
            if e.kind() == io::ErrorKind::Interrupted {
                return Ok(PollResult::default());
            }
            return Err(e);
        }
        Ok(PollResult {
            readable: pfd.revents & libc::POLLIN != 0,
            writable: pfd.revents & libc::POLLOUT != 0,
            event: pfd.revents & libc::POLLPRI != 0,
        })
    }

    fn g_selection_compose(&mut self, buf_type: u32) -> io::Result<v4l2_rect> {
        let mut s: v4l2_selection = zeroed();
        s.type_ = buf_type;
        s.target = V4L2_SEL_TGT_COMPOSE;
        self.ioctl(VIDIOC_G_SELECTION, &mut s)?;
        Ok(s.r)
    }
}
```

- [ ] **Step 4: Write `fake.rs`**

```rust
// crates/castr-codec-v4l2/src/fake.rs
//! Scripted stand-in for the kernel, for unit tests of the queue and decoder
//! state machines. Records every call as a string and serves queued
//! dequeue results, events and formats that tests push in.

use crate::ops::*;
use crate::sys::*;
use std::collections::VecDeque;
use std::io;
use std::os::fd::OwnedFd;

pub(crate) struct FakeOps {
    pub calls: Vec<String>,
    pub caps: u32,
    pub output_formats: Vec<u32>,
    pub capture_formats: Vec<u32>,
    pub capture_format: FormatInfo,
    pub output_format: FormatInfo,
    pub compose: v4l2_rect,
    pub granted: Option<u32>,
    dequeues: VecDeque<(u32, Dequeued)>,
    events: VecDeque<Event>,
    pub polls: VecDeque<PollResult>,
    pub fail_next: Option<&'static str>,
    pub captures_filled: Vec<u8>,
}

impl FakeOps {
    pub fn new() -> Self {
        Self {
            calls: Vec::new(),
            caps: V4L2_CAP_VIDEO_M2M_MPLANE | V4L2_CAP_STREAMING | V4L2_CAP_DEVICE_CAPS,
            output_formats: vec![V4L2_PIX_FMT_H264],
            capture_formats: vec![V4L2_PIX_FMT_NV12],
            capture_format: FormatInfo {
                width: 640,
                height: 368,
                pixelformat: V4L2_PIX_FMT_NV12,
                bytesperline: 640,
                sizeimage: 640 * 368 * 3 / 2,
            },
            output_format: FormatInfo { pixelformat: V4L2_PIX_FMT_H264, sizeimage: 1 << 20, ..Default::default() },
            compose: v4l2_rect { left: 0, top: 0, width: 640, height: 360 },
            granted: None,
            dequeues: VecDeque::new(),
            events: VecDeque::new(),
            polls: VecDeque::new(),
            fail_next: None,
            captures_filled: Vec::new(),
        }
    }
    pub fn push_dequeue(&mut self, buf_type: u32, d: Dequeued) {
        self.dequeues.push_back((buf_type, d));
    }
    pub fn push_event(&mut self, e: Event) {
        self.events.push_back(e);
    }
    fn record(&mut self, s: String) -> io::Result<()> {
        self.calls.push(s);
        if let Some(name) = self.fail_next.take() {
            return Err(io::Error::other(name));
        }
        Ok(())
    }
}

impl Ops for FakeOps {
    fn query_cap(&mut self) -> io::Result<v4l2_capability> {
        self.record("query_cap".into())?;
        let mut c: v4l2_capability = zeroed();
        c.capabilities = self.caps;
        c.device_caps = self.caps & !V4L2_CAP_DEVICE_CAPS;
        c.driver[..13].copy_from_slice(b"bcm2835-codec");
        Ok(c)
    }
    fn enum_fmt(&mut self, buf_type: u32, index: u32) -> io::Result<Option<u32>> {
        self.record(format!("enum_fmt({buf_type},{index})"))?;
        let list = if buf_type == V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE { &self.output_formats } else { &self.capture_formats };
        Ok(list.get(index as usize).copied())
    }
    fn g_fmt(&mut self, buf_type: u32) -> io::Result<FormatInfo> {
        self.record(format!("g_fmt({buf_type})"))?;
        Ok(if buf_type == V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE { self.output_format } else { self.capture_format })
    }
    fn s_fmt(&mut self, buf_type: u32, want: &FormatInfo) -> io::Result<FormatInfo> {
        self.record(format!("s_fmt({buf_type},{:#x},{}x{})", want.pixelformat, want.width, want.height))?;
        if buf_type == V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE {
            self.output_format = FormatInfo { sizeimage: want.sizeimage, ..*want };
            Ok(self.output_format)
        } else {
            self.capture_format.pixelformat = want.pixelformat;
            Ok(self.capture_format)
        }
    }
    fn reqbufs(&mut self, buf_type: u32, count: u32) -> io::Result<u32> {
        self.record(format!("reqbufs({buf_type},{count})"))?;
        Ok(if count == 0 { 0 } else { self.granted.unwrap_or(count) })
    }
    fn querybuf(&mut self, buf_type: u32, index: u32) -> io::Result<PlaneInfo> {
        self.record(format!("querybuf({buf_type},{index})"))?;
        let len = if buf_type == V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE { self.output_format.sizeimage } else { self.capture_format.sizeimage };
        Ok(PlaneInfo { length: len, mem_offset: index * 0x1000 })
    }
    fn mmap(&mut self, length: usize, offset: u32) -> io::Result<Mapping> {
        self.record(format!("mmap({length},{offset})"))?;
        let mut m = Mapping::owned(length);
        if !self.captures_filled.is_empty() {
            let fill = self.captures_filled[0];
            m.as_mut_slice().fill(fill);
        }
        Ok(m)
    }
    fn expbuf(&mut self, buf_type: u32, index: u32) -> io::Result<OwnedFd> {
        self.record(format!("expbuf({buf_type},{index})"))?;
        // A real fd so OwnedFd's drop is sound: /dev/null is always openable on Linux.
        Ok(OwnedFd::from(std::fs::File::open("/dev/null")?))
    }
    fn qbuf(&mut self, buf_type: u32, index: u32, _length: u32, bytesused: u32, timestamp_us: u64) -> io::Result<()> {
        self.record(format!("qbuf({buf_type},{index},{bytesused},{timestamp_us})"))
    }
    fn dqbuf(&mut self, buf_type: u32) -> io::Result<Option<Dequeued>> {
        self.record(format!("dqbuf({buf_type})"))?;
        if let Some(pos) = self.dequeues.iter().position(|(t, _)| *t == buf_type) {
            return Ok(Some(self.dequeues.remove(pos).unwrap().1));
        }
        Ok(None)
    }
    fn streamon(&mut self, buf_type: u32) -> io::Result<()> {
        self.record(format!("streamon({buf_type})"))
    }
    fn streamoff(&mut self, buf_type: u32) -> io::Result<()> {
        self.record(format!("streamoff({buf_type})"))
    }
    fn subscribe(&mut self, event_type: u32) -> io::Result<()> {
        self.record(format!("subscribe({event_type})"))
    }
    fn dqevent(&mut self) -> io::Result<Option<Event>> {
        self.record("dqevent".into())?;
        Ok(self.events.pop_front())
    }
    fn poll(&mut self, timeout_ms: i32) -> io::Result<PollResult> {
        self.record(format!("poll({timeout_ms})"))?;
        Ok(self.polls.pop_front().unwrap_or_default())
    }
    fn g_selection_compose(&mut self, buf_type: u32) -> io::Result<v4l2_rect> {
        self.record(format!("g_selection({buf_type})"))?;
        Ok(self.compose)
    }
}
```

- [ ] **Step 5: Run the crate's tests inside the cross-build image (host x86_64 Linux)**

The Linux-gated modules cannot run on Windows. The Docker image built by `scripts/pi/build-pi.sh` is an x86_64 Debian with a Rust toolchain, so run the unit tests there natively (no emulation needed: nothing in these tests touches hardware):

```bash
export MSYS_NO_PATHCONV=1
docker run --rm -v "$(pwd -W):/src:ro" -v castr-xtarget:/work -v castr-xcargo:/root/.cargo/registry castr-xbuild:aarch64 \
  bash -c 'cd /src && cargo test -q --locked -p castr-codec-v4l2 --target-dir /work/host 2>&1 | tail -5'
```

Expected: `fake::` tests (2), `sys::` tests (4), `annexb::` tests (4) pass. Save this command as `scripts/pi/test-linux.sh` (with the `#!/usr/bin/env bash` and `cd "$(dirname "$0")/../.."` preamble used by `build-pi.sh`) since every later task uses it.

- [ ] **Step 6: Commit**

```bash
cargo fmt
git add crates/castr-codec-v4l2 scripts/pi/test-linux.sh
git commit -m "feat(v4l2): typed Ops trait, libc-backed RealOps, scripted FakeOps"
```

---

### Task 4: `queue.rs`: one V4L2 buffer queue

**Files:**
- Create: `crates/castr-codec-v4l2/src/queue.rs`

**Interfaces:**
- Consumes: `Ops`, `Mapping`, `PlaneInfo`, `Dequeued` from Task 3; constants from Task 1.
- Produces:

```rust
pub struct Buffer { pub mapping: Mapping, pub dmabuf: Option<OwnedFd>, pub queued: bool }
pub struct Queue { pub buf_type: u32, pub buffers: Vec<Buffer>, streaming: bool }
impl Queue {
    pub fn new(buf_type: u32) -> Queue;
    pub fn allocate<O: Ops>(&mut self, ops: &mut O, count: u32, export: bool) -> io::Result<()>;
    pub fn stream_on<O: Ops>(&mut self, ops: &mut O) -> io::Result<()>;
    pub fn stream_off<O: Ops>(&mut self, ops: &mut O) -> io::Result<()>;   // also marks all buffers unqueued
    pub fn free_slot(&self) -> Option<usize>;
    pub fn in_flight(&self) -> usize;
    pub fn queue<O: Ops>(&mut self, ops: &mut O, index: usize, bytesused: u32, timestamp_us: u64) -> io::Result<()>;
    pub fn dequeue<O: Ops>(&mut self, ops: &mut O) -> io::Result<Option<Dequeued>>;   // marks buffer unqueued
    pub fn release<O: Ops>(&mut self, ops: &mut O) -> io::Result<()>;   // stream_off if needed, drop mappings, reqbufs(0)
    pub fn is_streaming(&self) -> bool;
}
```

- [ ] **Step 1: Write the failing tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeOps;
    use crate::sys::*;

    const OUT: u32 = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;

    #[test]
    fn allocate_requests_queries_maps_and_exports_each_buffer() {
        let mut ops = FakeOps::new();
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 2, true).unwrap();
        assert_eq!(q.buffers.len(), 2);
        assert!(q.buffers.iter().all(|b| b.dmabuf.is_some() && !b.queued));
        assert_eq!(
            ops.calls,
            vec![
                "reqbufs(10,2)", "querybuf(10,0)", "mmap(1048576,0)", "expbuf(10,0)",
                "querybuf(10,1)", "mmap(1048576,4096)", "expbuf(10,1)",
            ]
        );
    }

    #[test]
    fn allocate_honours_the_count_the_driver_grants() {
        let mut ops = FakeOps::new();
        ops.granted = Some(5);
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 2, false).unwrap();
        assert_eq!(q.buffers.len(), 5);
        assert!(q.buffers.iter().all(|b| b.dmabuf.is_none()));
    }

    #[test]
    fn queue_and_dequeue_track_in_flight_and_free_slots() {
        let mut ops = FakeOps::new();
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 2, false).unwrap();
        assert_eq!(q.free_slot(), Some(0));
        q.queue(&mut ops, 0, 100, 1_000).unwrap();
        assert_eq!(q.in_flight(), 1);
        assert_eq!(q.free_slot(), Some(1));
        q.queue(&mut ops, 1, 50, 2_000).unwrap();
        assert_eq!(q.free_slot(), None);
        assert!(q.dequeue(&mut ops).unwrap().is_none());
        ops.push_dequeue(OUT, Dequeued { index: 0, bytesused: 0, timestamp_us: 1_000, flags: 0 });
        let d = q.dequeue(&mut ops).unwrap().unwrap();
        assert_eq!(d.index, 0);
        assert_eq!(q.in_flight(), 1);
        assert_eq!(q.free_slot(), Some(0));
        assert!(ops.calls.contains(&"qbuf(10,0,100,1000)".to_string()));
    }

    #[test]
    fn queueing_an_already_queued_buffer_is_an_error_not_a_kernel_call() {
        let mut ops = FakeOps::new();
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 1, false).unwrap();
        q.queue(&mut ops, 0, 1, 0).unwrap();
        let n = ops.calls.len();
        assert!(q.queue(&mut ops, 0, 1, 0).is_err());
        assert_eq!(ops.calls.len(), n);
    }

    #[test]
    fn stream_off_returns_all_buffers_and_release_frees_them() {
        let mut ops = FakeOps::new();
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 2, true).unwrap();
        q.stream_on(&mut ops).unwrap();
        q.queue(&mut ops, 0, 1, 0).unwrap();
        q.stream_off(&mut ops).unwrap();
        assert_eq!(q.in_flight(), 0);
        assert!(!q.is_streaming());
        q.release(&mut ops).unwrap();
        assert!(q.buffers.is_empty());
        assert_eq!(ops.calls.last().unwrap(), "reqbufs(10,0)");
    }

    #[test]
    fn release_while_streaming_stops_first() {
        let mut ops = FakeOps::new();
        let mut q = Queue::new(OUT);
        q.allocate(&mut ops, 1, false).unwrap();
        q.stream_on(&mut ops).unwrap();
        q.release(&mut ops).unwrap();
        let tail: Vec<_> = ops.calls.iter().rev().take(2).cloned().collect();
        assert_eq!(tail, vec!["reqbufs(10,0)", "streamoff(10)"]);
    }
}
```

- [ ] **Step 2: Run to see them fail**

Run: `bash scripts/pi/test-linux.sh`
Expected: compile errors in `queue::tests`.

- [ ] **Step 3: Implement**

```rust
// crates/castr-codec-v4l2/src/queue.rs
//! One V4L2 buffer queue (OUTPUT or CAPTURE): allocation, mapping, DMABUF
//! export, queue/dequeue bookkeeping and teardown. Pure bookkeeping over `Ops`,
//! so it is unit-tested with `FakeOps`.

use crate::ops::{Dequeued, Mapping, Ops};
use std::io;
use std::os::fd::OwnedFd;

pub struct Buffer {
    pub mapping: Mapping,
    /// DMABUF handle exported at allocation; unused for now, reserved for a
    /// zero-copy present path.
    pub dmabuf: Option<OwnedFd>,
    pub queued: bool,
}

pub struct Queue {
    pub buf_type: u32,
    pub buffers: Vec<Buffer>,
    streaming: bool,
}

impl Queue {
    pub fn new(buf_type: u32) -> Self {
        Self { buf_type, buffers: Vec::new(), streaming: false }
    }

    pub fn allocate<O: Ops>(&mut self, ops: &mut O, count: u32, export: bool) -> io::Result<()> {
        let granted = ops.reqbufs(self.buf_type, count)?;
        self.buffers.clear();
        for index in 0..granted {
            let info = ops.querybuf(self.buf_type, index)?;
            let mapping = ops.mmap(info.length as usize, info.mem_offset)?;
            let dmabuf = if export { Some(ops.expbuf(self.buf_type, index)?) } else { None };
            self.buffers.push(Buffer { mapping, dmabuf, queued: false });
        }
        Ok(())
    }

    pub fn is_streaming(&self) -> bool {
        self.streaming
    }

    pub fn stream_on<O: Ops>(&mut self, ops: &mut O) -> io::Result<()> {
        ops.streamon(self.buf_type)?;
        self.streaming = true;
        Ok(())
    }

    /// STREAMOFF returns every queued buffer to us without a DQBUF.
    pub fn stream_off<O: Ops>(&mut self, ops: &mut O) -> io::Result<()> {
        ops.streamoff(self.buf_type)?;
        self.streaming = false;
        for b in &mut self.buffers {
            b.queued = false;
        }
        Ok(())
    }

    pub fn free_slot(&self) -> Option<usize> {
        self.buffers.iter().position(|b| !b.queued)
    }

    pub fn in_flight(&self) -> usize {
        self.buffers.iter().filter(|b| b.queued).count()
    }

    pub fn queue<O: Ops>(&mut self, ops: &mut O, index: usize, bytesused: u32, timestamp_us: u64) -> io::Result<()> {
        let b = self.buffers.get_mut(index).ok_or_else(|| io::Error::other("buffer index out of range"))?;
        if b.queued {
            return Err(io::Error::other(format!("buffer {index} already queued")));
        }
        let length = b.mapping.len() as u32;
        ops.qbuf(self.buf_type, index as u32, length, bytesused, timestamp_us)?;
        b.queued = true;
        Ok(())
    }

    pub fn dequeue<O: Ops>(&mut self, ops: &mut O) -> io::Result<Option<Dequeued>> {
        let Some(d) = ops.dqbuf(self.buf_type)? else { return Ok(None) };
        match self.buffers.get_mut(d.index as usize) {
            Some(b) => b.queued = false,
            None => return Err(io::Error::other(format!("driver dequeued unknown buffer {}", d.index))),
        }
        Ok(Some(d))
    }

    pub fn release<O: Ops>(&mut self, ops: &mut O) -> io::Result<()> {
        if self.streaming {
            self.stream_off(ops)?;
        }
        self.buffers.clear(); // unmaps and closes DMABUF fds
        ops.reqbufs(self.buf_type, 0)?;
        Ok(())
    }
}
```

- [ ] **Step 4: Run the tests**

Run: `bash scripts/pi/test-linux.sh`
Expected: `queue::` 6 passed, plus earlier suites.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/castr-codec-v4l2
git commit -m "feat(v4l2): buffer queue with allocation, export and in-flight tracking"
```

---

### Task 5: `decoder.rs`: `V4l2Decoder`

**Files:**
- Create: `crates/castr-codec-v4l2/src/decoder.rs`

**Interfaces:**
- Consumes: `Queue` (Task 4), `Ops`/`RealOps`/`FakeOps` (Task 3), `annexb` (Task 2), `sys` (Task 1), `castr_media::{VideoDecoder, RawFrame, PixelFormat}`.
- Produces:

```rust
pub const DEFAULT_DEVICE: &str = "/dev/video10";
pub const OUTPUT_BUFFERS: u32 = 4;
pub const OUTPUT_BUFFER_SIZE: u32 = 1 << 20;
pub const CAPTURE_BUFFERS: u32 = 6;
pub const MAX_IN_FLIGHT: usize = 2;
pub const STALL_INPUTS: u32 = 60;
pub const STALL_POLL_MS: u64 = 2_000;
pub struct V4l2Decoder<O: Ops = RealOps> { .. }
impl V4l2Decoder<RealOps> { pub fn open() -> anyhow::Result<Self>; pub fn open_path(path: &str) -> anyhow::Result<Self>; }
impl<O: Ops> V4l2Decoder<O> { pub fn with_ops(ops: O) -> anyhow::Result<Self>; pub fn frame_size(&self) -> Option<(u32, u32)>; }
impl<O: Ops + Send> VideoDecoder for V4l2Decoder<O> { .. }   // name() == "v4l2-bcm2835"
```

- [ ] **Step 1: Write the failing state-machine tests**

```rust
#[cfg(test)]
mod tests {
    use super::*;
    use crate::fake::FakeOps;
    use crate::ops::{Dequeued, Event, FormatInfo, PollResult};
    use crate::sys::*;
    use castr_media::VideoDecoder;

    const OUT: u32 = V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE;
    const CAP: u32 = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
    const KEY: [u8; 9] = [0, 0, 0, 1, 0x67, 0, 0, 1, 0x65];
    const DELTA: [u8; 5] = [0, 0, 0, 1, 0x41];

    fn src_change() -> Event {
        Event { kind: V4L2_EVENT_SOURCE_CHANGE, changes: V4L2_EVENT_SRC_CH_RESOLUTION }
    }

    #[test]
    fn startup_configures_output_side_only() {
        let ops = FakeOps::new();
        let d = V4l2Decoder::with_ops(ops).unwrap();
        let calls = &d.ops.calls;
        assert!(calls.contains(&"query_cap".to_string()));
        assert!(calls.iter().any(|c| c.starts_with("s_fmt(10,")));
        assert!(calls.contains(&format!("subscribe({V4L2_EVENT_SOURCE_CHANGE})")));
        assert!(calls.contains(&format!("subscribe({V4L2_EVENT_EOS})")));
        assert!(calls.contains(&"reqbufs(10,4)".to_string()));
        assert!(calls.contains(&"streamon(10)".to_string()));
        assert!(!calls.iter().any(|c| c.starts_with("reqbufs(9,")), "capture must wait for SOURCE_CHANGE");
        assert!(d.frame_size().is_none());
    }

    #[test]
    fn open_fails_without_h264_or_nv12_support() {
        let mut ops = FakeOps::new();
        ops.output_formats = vec![V4L2_PIX_FMT_NV12];
        assert!(V4l2Decoder::with_ops(ops).is_err());
        let mut ops = FakeOps::new();
        ops.capture_formats = vec![];
        assert!(V4l2Decoder::with_ops(ops).is_err());
        let mut ops = FakeOps::new();
        ops.caps = V4L2_CAP_STREAMING;
        assert!(V4l2Decoder::with_ops(ops).is_err());
    }

    #[test]
    fn rejects_non_annex_b_and_oversized_input() {
        let mut d = V4l2Decoder::with_ops(FakeOps::new()).unwrap();
        assert!(d.decode(&[1, 2, 3], 0).is_err());
        let huge = vec![0u8; OUTPUT_BUFFER_SIZE as usize + 1];
        let mut au = KEY.to_vec();
        au.extend_from_slice(&huge);
        assert!(d.decode(&au, 0).is_err());
    }

    #[test]
    fn first_source_change_brings_up_capture_and_frames_flow() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        // Feeding the keyframe queues it, drains the event, configures capture.
        assert!(d.decode(&KEY, 33_000).unwrap().is_none());
        let c = &d.ops.calls;
        assert!(c.contains(&"qbuf(10,0,9,33000)".to_string()));
        assert!(c.iter().any(|x| x.starts_with("g_fmt(9)")));
        assert!(c.iter().any(|x| x == &format!("s_fmt(9,{:#x},640x368)", V4L2_PIX_FMT_NV12)));
        assert!(c.contains(&"reqbufs(9,6)".to_string()));
        assert!(c.contains(&"streamon(9)".to_string()));
        assert_eq!(c.iter().filter(|x| x.starts_with("qbuf(9,")).count(), 6, "all capture buffers queued");
        assert_eq!(d.frame_size(), Some((640, 360)));
        // A decoded picture appears: NV12, visible size, requeued.
        d.ops.push_dequeue(CAP, Dequeued { index: 2, bytesused: 640 * 368 * 3 / 2, timestamp_us: 33_000, flags: 0 });
        let f = d.decode(&DELTA, 66_000).unwrap().expect("frame");
        assert_eq!((f.width, f.height, f.stride), (640, 360, 640));
        assert_eq!(f.format, castr_media::PixelFormat::Nv12);
        assert_eq!(f.data.len(), 640 * 360 * 3 / 2);
        assert_eq!(f.timestamp_us, 33_000);
        assert!(d.ops.calls.iter().any(|x| x.starts_with("qbuf(9,2,")), "capture buffer 2 requeued");
    }

    #[test]
    fn capture_copy_crops_to_the_visible_rectangle() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        ops.captures_filled = vec![0x80];
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        d.ops.push_dequeue(CAP, Dequeued { index: 0, bytesused: 640 * 368 * 3 / 2, timestamp_us: 0, flags: 0 });
        let f = d.decode(&DELTA, 33_000).unwrap().unwrap();
        // Y plane is the first 640*360 bytes, UV plane follows immediately (stride == width).
        assert!(f.data.iter().all(|&b| b == 0x80));
        assert_eq!(f.data.len(), 640 * 360 + 640 * 180);
    }

    #[test]
    fn resolution_change_reallocates_capture() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        let before = d.ops.calls.len();
        d.ops.capture_format = FormatInfo { width: 1280, height: 720, pixelformat: V4L2_PIX_FMT_NV12, bytesperline: 1280, sizeimage: 1280 * 720 * 3 / 2 };
        d.ops.compose = v4l2_rect { left: 0, top: 0, width: 1280, height: 720 };
        d.ops.push_event(src_change());
        assert!(d.decode(&KEY, 33_000).unwrap().is_none());
        let after: Vec<_> = d.ops.calls[before..].to_vec();
        let pos = |s: &str| after.iter().position(|c| c.starts_with(s)).unwrap_or_else(|| panic!("missing {s}: {after:?}"));
        assert!(pos("streamoff(9)") < pos("reqbufs(9,0)"));
        assert!(pos("reqbufs(9,0)") < pos("s_fmt(9,"));
        assert!(pos("s_fmt(9,") < pos("reqbufs(9,6)"));
        assert!(pos("reqbufs(9,6)") < pos("streamon(9)"));
        assert_eq!(d.frame_size(), Some((1280, 720)));
    }

    #[test]
    fn at_most_two_inputs_in_flight_and_polling_dequeues_them() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        // The third input must wait: poll reports writable, and a dequeue is scripted.
        ops.polls.push_back(PollResult { writable: true, ..Default::default() });
        ops.push_dequeue(OUT, Dequeued { index: 0, bytesused: 0, timestamp_us: 0, flags: 0 });
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        d.decode(&DELTA, 33_000).unwrap();
        assert_eq!(d.output.in_flight(), 2);
        d.decode(&DELTA, 66_000).unwrap();
        assert_eq!(d.output.in_flight(), 2);
        assert!(d.ops.calls.iter().any(|c| c.starts_with("poll(")));
        assert!(d.ops.calls.contains(&"qbuf(10,0,5,66000)".to_string()), "slot 0 reused after dequeue");
    }

    #[test]
    fn stalls_after_sixty_unanswered_inputs() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        // Every wait for a slot immediately "completes" an output buffer.
        for _ in 0..200 {
            ops.polls.push_back(PollResult { writable: true, ..Default::default() });
        }
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        let mut err = None;
        for i in 1..=STALL_INPUTS + 2 {
            // Return the oldest queued output so a slot is always free.
            let idx = (i as u32 + 1) % OUTPUT_BUFFERS;
            d.ops.push_dequeue(OUT, Dequeued { index: idx, bytesused: 0, timestamp_us: 0, flags: 0 });
            match d.decode(&DELTA, i as u64 * 33_000) {
                Ok(None) => {}
                Ok(Some(_)) => panic!("no frames were scripted"),
                Err(e) => { err = Some(e); break; }
            }
        }
        let e = err.expect("stall error");
        assert!(e.to_string().contains("stalled"), "{e:#}");
    }

    #[test]
    fn polling_gives_up_after_two_seconds_without_progress() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.decode(&KEY, 0).unwrap();
        d.decode(&DELTA, 1).unwrap();
        // Third input: every poll times out (default PollResult) and nothing dequeues.
        let e = d.decode(&DELTA, 2).unwrap_err();
        assert!(e.to_string().contains("stalled"), "{e:#}");
        let polls = d.ops.calls.iter().filter(|c| c.starts_with("poll(")).count();
        assert_eq!(polls as u64, STALL_POLL_MS / 200);
    }

    #[test]
    fn ioctl_failure_is_reported() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        d.ops.fail_next = Some("EIO");
        assert!(d.decode(&KEY, 0).is_err());
    }

    #[test]
    fn drop_stops_both_queues() {
        let mut ops = FakeOps::new();
        ops.push_event(src_change());
        let mut d = V4l2Decoder::with_ops(ops).unwrap();
        // FakeOps is moved into the decoder; it copies its call log into this
        // sink when it is dropped, after the decoder's own Drop has run.
        let log = d.take_calls_on_drop();
        d.decode(&KEY, 0).unwrap();
        drop(d);
        let log = log.borrow();
        assert!(log.contains(&"streamoff(10)".to_string()));
        assert!(log.contains(&"streamoff(9)".to_string()));
    }
}
```

The last test needs a hook: `FakeOps` gets `pub sink: Option<Rc<RefCell<Vec<String>>>>` that, when set, receives a copy of `calls` in `FakeOps::drop`; `V4l2Decoder<FakeOps>::take_calls_on_drop()` (cfg(test)) sets it and returns the `Rc`. Both are given in Step 3 (the `Rc` is fine: tests are single-threaded; `V4l2Decoder<FakeOps>` is never sent across threads).

- [ ] **Step 2: Run to see them fail**

Run: `bash scripts/pi/test-linux.sh`
Expected: compile errors for `decoder::`.

- [ ] **Step 3: Implement**

```rust
// crates/castr-codec-v4l2/src/decoder.rs
//! `V4l2Decoder`: H.264 in, NV12 out, over the bcm2835 V4L2 M2M decoder.
//! See spec section 4 for the sequence this implements.

use crate::annexb;
use crate::ops::{FormatInfo, Ops, RealOps};
use crate::queue::Queue;
use crate::sys::*;
use anyhow::{anyhow, bail, Context};
use castr_media::{PixelFormat, RawFrame, VideoDecoder};

pub const DEFAULT_DEVICE: &str = "/dev/video10";
pub const OUTPUT_BUFFERS: u32 = 4;
pub const OUTPUT_BUFFER_SIZE: u32 = 1 << 20;
pub const CAPTURE_BUFFERS: u32 = 6;
/// Compressed frames queued but not yet consumed; bounds decoder latency.
pub const MAX_IN_FLIGHT: usize = 2;
/// Inputs queued with no picture back before we declare the decoder dead.
pub const STALL_INPUTS: u32 = 60;
/// Wall-clock budget for waiting on the hardware inside one `decode` call.
pub const STALL_POLL_MS: u64 = 2_000;
const POLL_STEP_MS: i32 = 200;

struct Capture {
    queue: Queue,
    /// Coded (allocated) size and stride reported by the driver.
    coded: FormatInfo,
    /// Visible picture inside the coded frame.
    visible: v4l2_rect,
}

pub struct V4l2Decoder<O: Ops = RealOps> {
    pub(crate) ops: O,
    pub(crate) output: Queue,
    capture: Option<Capture>,
    unanswered: u32,
    finished: bool,
    #[cfg(test)]
    drop_sink: Option<std::rc::Rc<std::cell::RefCell<Vec<String>>>>,
}

impl V4l2Decoder<RealOps> {
    pub fn open() -> anyhow::Result<Self> {
        let path = std::env::var("CASTR_V4L2_DEVICE").unwrap_or_else(|_| DEFAULT_DEVICE.into());
        Self::open_path(&path)
    }

    pub fn open_path(path: &str) -> anyhow::Result<Self> {
        let ops = RealOps::open(path).with_context(|| format!("open {path}"))?;
        Self::with_ops(ops).with_context(|| format!("initialise {path}"))
    }
}

impl<O: Ops> V4l2Decoder<O> {
    pub fn with_ops(mut ops: O) -> anyhow::Result<Self> {
        let cap = ops.query_cap().context("QUERYCAP")?;
        let caps = if cap.capabilities & V4L2_CAP_DEVICE_CAPS != 0 { cap.device_caps } else { cap.capabilities };
        if caps & V4L2_CAP_VIDEO_M2M_MPLANE == 0 {
            bail!("device is not a multiplanar memory-to-memory codec (caps {caps:#x})");
        }
        if !Self::supports(&mut ops, V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE, V4L2_PIX_FMT_H264)? {
            bail!("device does not accept H.264");
        }
        if !Self::supports(&mut ops, V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE, V4L2_PIX_FMT_NV12)? {
            bail!("device does not produce NV12");
        }
        ops.s_fmt(
            V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE,
            &FormatInfo { pixelformat: V4L2_PIX_FMT_H264, sizeimage: OUTPUT_BUFFER_SIZE, ..Default::default() },
        )
        .context("S_FMT output")?;
        ops.subscribe(V4L2_EVENT_SOURCE_CHANGE).context("subscribe SOURCE_CHANGE")?;
        ops.subscribe(V4L2_EVENT_EOS).context("subscribe EOS")?;
        let mut output = Queue::new(V4L2_BUF_TYPE_VIDEO_OUTPUT_MPLANE);
        output.allocate(&mut ops, OUTPUT_BUFFERS, false).context("allocate output buffers")?;
        output.stream_on(&mut ops).context("STREAMON output")?;
        Ok(Self {
            ops,
            output,
            capture: None,
            unanswered: 0,
            finished: false,
            #[cfg(test)]
            drop_sink: None,
        })
    }

    fn supports(ops: &mut O, buf_type: u32, fourcc: u32) -> anyhow::Result<bool> {
        for i in 0.. {
            match ops.enum_fmt(buf_type, i).context("ENUM_FMT")? {
                Some(f) if f == fourcc => return Ok(true),
                Some(_) => continue,
                None => return Ok(false),
            }
        }
        Ok(false)
    }

    /// Visible (width, height) once the stream's SPS has been parsed.
    pub fn frame_size(&self) -> Option<(u32, u32)> {
        self.capture.as_ref().map(|c| (c.visible.width, c.visible.height))
    }

    /// (Re)build the CAPTURE side after a SOURCE_CHANGE (spec 4.2 step 5, 4.4).
    fn reconfigure_capture(&mut self) -> anyhow::Result<()> {
        if let Some(mut old) = self.capture.take() {
            old.queue.release(&mut self.ops).context("release old capture buffers")?;
        }
        let cap_type = V4L2_BUF_TYPE_VIDEO_CAPTURE_MPLANE;
        let reported = self.ops.g_fmt(cap_type).context("G_FMT capture")?;
        let coded = self
            .ops
            .s_fmt(cap_type, &FormatInfo { pixelformat: V4L2_PIX_FMT_NV12, ..reported })
            .context("S_FMT capture")?;
        if coded.pixelformat != V4L2_PIX_FMT_NV12 {
            bail!("driver refused NV12 (gave {:#x})", coded.pixelformat);
        }
        let mut queue = Queue::new(cap_type);
        queue.allocate(&mut self.ops, CAPTURE_BUFFERS, true).context("allocate capture buffers")?;
        for i in 0..queue.buffers.len() {
            queue.queue(&mut self.ops, i, 0, 0).context("queue capture buffer")?;
        }
        queue.stream_on(&mut self.ops).context("STREAMON capture")?;
        let mut visible = self.ops.g_selection_compose(cap_type).unwrap_or(v4l2_rect {
            left: 0,
            top: 0,
            width: coded.width,
            height: coded.height,
        });
        if visible.width == 0 || visible.height == 0 {
            visible = v4l2_rect { left: 0, top: 0, width: coded.width, height: coded.height };
        }
        tracing::info!(
            "v4l2 decoder: {}x{} visible in {}x{} coded, stride {}",
            visible.width,
            visible.height,
            coded.width,
            coded.height,
            coded.bytesperline
        );
        self.capture = Some(Capture { queue, coded, visible });
        Ok(())
    }

    fn drain_events(&mut self) -> anyhow::Result<()> {
        while let Some(ev) = self.ops.dqevent().context("DQEVENT")? {
            match ev.kind {
                V4L2_EVENT_SOURCE_CHANGE => {
                    if ev.changes & V4L2_EVENT_SRC_CH_RESOLUTION != 0 || self.capture.is_none() {
                        self.reconfigure_capture()?;
                    }
                }
                V4L2_EVENT_EOS => self.finished = true,
                _ => {}
            }
        }
        Ok(())
    }

    fn drain_output(&mut self) -> anyhow::Result<bool> {
        let mut any = false;
        while self.output.dequeue(&mut self.ops).context("DQBUF output")?.is_some() {
            any = true;
        }
        Ok(any)
    }

    /// Copy one decoded picture out of a capture buffer and requeue it.
    fn take_capture(&mut self) -> anyhow::Result<Option<RawFrame>> {
        let Some(cap) = self.capture.as_mut() else { return Ok(None) };
        let Some(d) = cap.queue.dequeue(&mut self.ops).context("DQBUF capture")? else { return Ok(None) };
        let idx = d.index as usize;
        let frame = if d.bytesused == 0 {
            None
        } else {
            let (w, h) = (cap.visible.width as usize, cap.visible.height as usize);
            let stride = cap.coded.bytesperline as usize;
            let coded_h = cap.coded.height as usize;
            let src = cap.queue.buffers[idx].mapping.as_slice();
            let mut data = Vec::with_capacity(w * h * 3 / 2);
            let top = cap.visible.top.max(0) as usize;
            let left = cap.visible.left.max(0) as usize;
            for row in 0..h {
                let o = (top + row) * stride + left;
                data.extend_from_slice(&src[o..o + w]);
            }
            let uv_base = stride * coded_h;
            for row in 0..h / 2 {
                let o = uv_base + (top / 2 + row) * stride + left;
                data.extend_from_slice(&src[o..o + w]);
            }
            Some(RawFrame {
                format: PixelFormat::Nv12,
                width: w as u32,
                height: h as u32,
                stride: w as u32,
                data,
                timestamp_us: d.timestamp_us,
            })
        };
        cap.queue.queue(&mut self.ops, idx, 0, 0).context("requeue capture buffer")?;
        if frame.is_some() {
            self.unanswered = 0;
        }
        Ok(frame)
    }

    /// Wait until an OUTPUT slot is free (at most MAX_IN_FLIGHT queued),
    /// servicing events and pictures meanwhile. A picture found here is held
    /// in `pending` for the caller. Progress is measured in poll rounds, not
    /// wall clock, so the fake (whose poll returns at once) sees the same
    /// number of rounds as the hardware: STALL_POLL_MS / POLL_STEP_MS.
    fn wait_for_slot(&mut self, pending: &mut Option<RawFrame>) -> anyhow::Result<()> {
        let max_idle = STALL_POLL_MS / POLL_STEP_MS as u64;
        let mut idle = 0u64;
        while self.output.in_flight() >= MAX_IN_FLIGHT {
            let r = self.ops.poll(POLL_STEP_MS).context("poll")?;
            let mut progressed = false;
            if r.event {
                self.drain_events()?;
                progressed = true;
            }
            if self.drain_output()? {
                progressed = true;
            }
            if pending.is_none() {
                if let Some(f) = self.take_capture()? {
                    *pending = Some(f);
                    progressed = true;
                }
            }
            if progressed {
                idle = 0;
            } else {
                idle += 1;
                if idle >= max_idle {
                    bail!("decoder stalled: no progress for {STALL_POLL_MS} ms");
                }
            }
        }
        Ok(())
    }

    #[cfg(test)]
    pub(crate) fn take_calls_on_drop(&mut self) -> std::rc::Rc<std::cell::RefCell<Vec<String>>>
    where
        O: crate::fake::HasSink,
    {
        let sink = std::rc::Rc::new(std::cell::RefCell::new(Vec::new()));
        self.ops.set_sink(sink.clone());
        self.drop_sink = Some(sink.clone());
        sink
    }
}

impl<O: Ops + Send> VideoDecoder for V4l2Decoder<O> {
    fn decode(&mut self, data: &[u8], timestamp_us: u64) -> anyhow::Result<Option<RawFrame>> {
        annexb::check_access_unit(data)?;
        if data.len() > OUTPUT_BUFFER_SIZE as usize {
            bail!("access unit of {} bytes exceeds the {} byte decoder buffer", data.len(), OUTPUT_BUFFER_SIZE);
        }
        if self.finished {
            bail!("decoder reported end of stream");
        }
        let mut pending: Option<RawFrame> = None;
        self.drain_output()?;
        self.wait_for_slot(&mut pending)?;
        let slot = self.output.free_slot().ok_or_else(|| anyhow!("no free output buffer"))?;
        self.output.buffers[slot].mapping.as_mut_slice()[..data.len()].copy_from_slice(data);
        self.output.queue(&mut self.ops, slot, data.len() as u32, timestamp_us).context("QBUF output")?;
        self.unanswered += 1;
        self.drain_events()?;
        if pending.is_none() {
            pending = self.take_capture()?;
        }
        if pending.is_none() && self.unanswered >= STALL_INPUTS {
            bail!("decoder stalled: {STALL_INPUTS} access units queued with no picture");
        }
        Ok(pending)
    }

    fn name(&self) -> &'static str {
        "v4l2-bcm2835"
    }
}

impl<O: Ops> Drop for V4l2Decoder<O> {
    fn drop(&mut self) {
        if let Some(mut c) = self.capture.take() {
            if let Err(e) = c.queue.release(&mut self.ops) {
                tracing::warn!("v4l2 decoder: release capture: {e}");
            }
        }
        if let Err(e) = self.output.release(&mut self.ops) {
            tracing::warn!("v4l2 decoder: release output: {e}");
        }
    }
}
```

And in `fake.rs` add the sink hook:

```rust
pub(crate) trait HasSink {
    fn set_sink(&mut self, sink: std::rc::Rc<std::cell::RefCell<Vec<String>>>);
}
impl HasSink for FakeOps {
    fn set_sink(&mut self, sink: std::rc::Rc<std::cell::RefCell<Vec<String>>>) {
        self.sink = Some(sink);
    }
}
impl Drop for FakeOps {
    fn drop(&mut self) {
        if let Some(s) = &self.sink {
            *s.borrow_mut() = self.calls.clone();
        }
    }
}
```

with `pub sink: Option<Rc<RefCell<Vec<String>>>>` added to the struct (initialised `None`). Since `V4l2Decoder<O: Ops + Send>` implements `VideoDecoder` only for `Send` ops and `FakeOps` holds an `Rc`, mark `FakeOps` with `unsafe impl Send for FakeOps {}` under `#[cfg(test)]` with the comment that tests never share it across threads. Simplify the `drop_stops_both_queues` test body to: build, `let log = d.take_calls_on_drop();`, `d.decode(&KEY, 0)`, `drop(d)`, assert on `log.borrow()` (remove the unused `calls` Rc lines).

- [ ] **Step 4: Run the tests**

Run: `bash scripts/pi/test-linux.sh`
Expected: `decoder::` 11 passed; whole crate green. Fix any ordering mismatch by reading the recorded `calls` vector in the failure output, not by loosening the assertion, unless the assertion contradicts spec 4.2/4.4.

- [ ] **Step 5: Commit**

```bash
cargo fmt
git add crates/castr-codec-v4l2
git commit -m "feat(v4l2): V4l2Decoder with source-change handling and stall detection"
```

---

### Task 6: Hardware tests on the Pi

**Files:**
- Create: `crates/castr-codec-v4l2/tests/hw.rs`
- Create: `scripts/pi/run-hw-tests.sh`

**Interfaces:**
- Consumes: `V4l2Decoder::open()`, `open_path`, `frame_size`; `castr_media::sw::SwEncoder`, `castr_media::{EncoderConfig, Mode, RawFrame, PixelFormat, convert}`.

- [ ] **Step 1: Write the hardware tests**

```rust
// crates/castr-codec-v4l2/tests/hw.rs
//! Run on a Raspberry Pi with `--ignored`. See scripts/pi/run-hw-tests.sh.
#![cfg(target_os = "linux")]

use castr_codec_v4l2::V4l2Decoder;
use castr_media::sw::SwEncoder;
use castr_media::*;
use std::time::{Duration, Instant};

fn frame(w: u32, h: u32, i: u32) -> RawFrame {
    let mut data = Vec::with_capacity((w * h * 4) as usize);
    for y in 0..h {
        for x in 0..w {
            data.extend_from_slice(&[((x + i * 5) % 256) as u8, (y % 256) as u8, ((x ^ y) % 256) as u8, 255]);
        }
    }
    convert::convert(
        &RawFrame { format: PixelFormat::Bgra, width: w, height: h, stride: w * 4, data, timestamp_us: i as u64 * 33_333 },
        PixelFormat::I420,
    )
}

/// Encode `n` frames at `w`x`h` with the software encoder; returns (data, timestamp) per access unit.
fn clip(w: u32, h: u32, n: u32, first_ts: u64) -> Vec<(Vec<u8>, u64)> {
    let mut enc = SwEncoder::new(EncoderConfig { width: w, height: h, fps: 30, bitrate_bps: 4_000_000, mode: Mode::Game }).unwrap();
    let mut out = Vec::new();
    for i in 0..n {
        let mut f = frame(w, h, i);
        f.timestamp_us = first_ts + i as u64 * 33_333;
        if let Some(e) = enc.encode(&f).unwrap() {
            out.push((e.data, e.timestamp_us));
        }
    }
    assert!(out.len() >= n as usize - 2, "encoder produced {} of {n}", out.len());
    out
}

fn drain(dec: &mut V4l2Decoder, aus: &[(Vec<u8>, u64)]) -> Vec<RawFrame> {
    let mut frames = Vec::new();
    for (au, ts) in aus {
        if let Some(f) = dec.decode(au, *ts).unwrap() {
            frames.push(f);
        }
    }
    // Flush the tail: feed nothing new, just poll a few times via zero-length skips.
    let deadline = Instant::now() + Duration::from_millis(500);
    while Instant::now() < deadline && frames.len() < aus.len() {
        // A delta frame that only repeats the last picture keeps the pipeline moving.
        let (au, ts) = aus.last().unwrap();
        if let Some(f) = dec.decode(au, *ts).unwrap() {
            frames.push(f);
        }
        std::thread::sleep(Duration::from_millis(5));
    }
    frames
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
    assert!(ts.windows(2).all(|w| w[0] <= w[1]), "timestamps not monotonic: {ts:?}");
    let mid = &frames[frames.len() / 2];
    let y = &mid.data[..640 * 360];
    let distinct = y.iter().collect::<std::collections::HashSet<_>>().len();
    assert!(distinct > 16, "middle frame looks flat ({distinct} distinct luma values)");
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
        .flat_map(|r| base.iter().map(move |(d, ts)| (d.clone(), ts + r * 60 * 33_333)))
        .collect();
    let mut dec = V4l2Decoder::open().unwrap();
    let start = Instant::now();
    let mut worst = Duration::ZERO;
    let mut n = 0;
    for (au, ts) in &aus {
        let t = Instant::now();
        if dec.decode(au, *ts).unwrap().is_some() {
            n += 1;
        }
        worst = worst.max(t.elapsed());
    }
    let total = start.elapsed();
    eprintln!("1080p: {n} frames in {total:?}, worst call {worst:?}");
    assert!(total < Duration::from_secs(10), "too slow: {total:?}");
    assert!(worst < Duration::from_millis(40), "worst decode call {worst:?}");
    assert!(n >= 290);
}

#[test]
#[ignore]
fn open_fails_cleanly_on_a_non_device() {
    let e = V4l2Decoder::open_path("/dev/null").unwrap_err();
    let s = format!("{e:#}");
    assert!(s.contains("/dev/null"), "{s}");
}
```

- [ ] **Step 2: Write the runner script**

```bash
#!/usr/bin/env bash
# scripts/pi/run-hw-tests.sh <user@pi>
# Cross-builds the castr-codec-v4l2 test binary and runs its #[ignore] hardware
# tests on the Pi (from tmpfs; nothing is installed).
set -euo pipefail
cd "$(dirname "$0")/../.."
PI="${1:?usage: run-hw-tests.sh user@host}"
export MSYS_NO_PATHCONV=1
mkdir -p dist/tests
docker run --rm -v "$(pwd -W 2>/dev/null || pwd):/src:ro" -v "$(pwd -W 2>/dev/null || pwd)/dist:/out" \
  -v castr-xtarget:/work -v castr-xcargo:/root/.cargo/registry castr-xbuild:aarch64 bash -c '
    set -e
    cargo test --no-run --release --locked --target aarch64-unknown-linux-gnu -p castr-codec-v4l2 --target-dir /work/target --message-format=json 2>/dev/null \
      | grep -o "\"executable\":\"[^\"]*\"" | cut -d\" -f4 | sort -u > /out/tests/v4l2-list.txt
    rm -f /out/tests/v4l2-*bin
    i=0; for f in $(cat /out/tests/v4l2-list.txt); do cp "$f" "/out/tests/v4l2-$i.bin"; i=$((i+1)); done'
for f in dist/tests/v4l2-*.bin; do
  name=$(basename "$f")
  cat "$f" | ssh "$PI" "cat > /tmp/$name && chmod +x /tmp/$name && /tmp/$name --ignored --test-threads=1 2>&1 | tail -20; rm -f /tmp/$name"
done
```

- [ ] **Step 3: Run on the Pi**

Run: `bash scripts/pi/run-hw-tests.sh dietpi@192.168.88.157`
Expected: 4 passed for the `hw` binary (the lib's unit tests run too, with no ignored ones). The 1080p line prints timing; record it in the task report. If `decodes_1080p_in_real_time` fails on `worst` alone with a value under 60 ms, report it rather than adjusting the threshold: the number matters for the spec's latency target.

If `/dev/video10` is missing on the Pi, run `sudo modprobe bcm2835_codec` first; Task 8 makes this automatic.

- [ ] **Step 4: Commit**

```bash
cargo fmt
git add crates/castr-codec-v4l2/tests scripts/pi/run-hw-tests.sh
git commit -m "test(v4l2): hardware tests for decode, resolution change and 1080p throughput"
```

---

### Task 7: Receiver integration: selection, rebuild rule, stats, hostname

**Files:**
- Modify: `crates/castr-receiver/src/pipeline.rs` (`DecoderChoice`, `open_decoder`, decode thread, SDL loop)
- Modify: `crates/castr-receiver/src/main.rs` (`default_name`)

**Interfaces:**
- Consumes: `castr_codec_v4l2::V4l2Decoder::open()`.
- Produces: `DecoderChoice::V4l2`; `pub struct ErrorWindow` with `pub fn new(limit: usize, within: Duration) -> Self` and `pub fn record(&mut self, now: Instant) -> bool`; `pub struct PerfStats` (in `pipeline.rs`).

- [ ] **Step 1: Write the failing unit tests (in `pipeline.rs`'s test module)**

```rust
    #[test]
    fn error_window_trips_on_the_third_error_within_ten_seconds() {
        let t0 = Instant::now();
        let mut w = ErrorWindow::new(3, Duration::from_secs(10));
        assert!(!w.record(t0));
        assert!(!w.record(t0 + Duration::from_secs(4)));
        assert!(w.record(t0 + Duration::from_secs(9)));
        // Tripping clears the window.
        assert!(!w.record(t0 + Duration::from_secs(9)));
    }

    #[test]
    fn error_window_forgets_old_errors() {
        let t0 = Instant::now();
        let mut w = ErrorWindow::new(3, Duration::from_secs(10));
        w.record(t0);
        w.record(t0 + Duration::from_secs(1));
        assert!(!w.record(t0 + Duration::from_secs(12)));
    }

    #[test]
    fn perf_stats_report_averages_and_maxima() {
        let mut p = PerfStats::default();
        p.decode(Duration::from_millis(10));
        p.decode(Duration::from_millis(30));
        p.present(Duration::from_millis(5));
        let s = p.take_report(2, 1);
        assert!(s.contains("decoded 2"), "{s}");
        assert!(s.contains("decode avg 20.0 ms max 30.0 ms"), "{s}");
        assert!(s.contains("present avg 5.0 ms max 5.0 ms"), "{s}");
        assert!(s.contains("queue 2"), "{s}");
        assert!(s.contains("dropped 1"), "{s}");
        // Taking resets the counters.
        assert!(p.take_report(0, 0).contains("decoded 0"));
    }
```

- [ ] **Step 2: Run to see them fail**

Run: `cargo test -p castr-receiver`
Expected: compile errors, `ErrorWindow`/`PerfStats` missing.

- [ ] **Step 3: Implement `ErrorWindow` and `PerfStats`**

Add to `pipeline.rs` (near the top, after the `use` lines):

```rust
/// Counts decoder errors and trips when `limit` occur within `within`.
pub struct ErrorWindow {
    limit: usize,
    within: Duration,
    times: std::collections::VecDeque<Instant>,
}

impl ErrorWindow {
    pub fn new(limit: usize, within: Duration) -> Self {
        Self { limit, within, times: std::collections::VecDeque::new() }
    }
    /// Records an error at `now`; true when the window has tripped (and resets).
    pub fn record(&mut self, now: Instant) -> bool {
        while self.times.front().is_some_and(|&t| now.duration_since(t) > self.within) {
            self.times.pop_front();
        }
        self.times.push_back(now);
        if self.times.len() >= self.limit {
            self.times.clear();
            true
        } else {
            false
        }
    }
}

/// Decode/present timing shared between the decode thread and the SDL loop,
/// reported every few seconds (spec section 5).
#[derive(Default)]
pub struct PerfStats {
    decoded: u32,
    decode_total: Duration,
    decode_max: Duration,
    presented: u32,
    present_total: Duration,
    present_max: Duration,
}

impl PerfStats {
    pub fn decode(&mut self, d: Duration) {
        self.decoded += 1;
        self.decode_total += d;
        self.decode_max = self.decode_max.max(d);
    }
    pub fn present(&mut self, d: Duration) {
        self.presented += 1;
        self.present_total += d;
        self.present_max = self.present_max.max(d);
    }
    fn avg(total: Duration, n: u32) -> f64 {
        if n == 0 { 0.0 } else { total.as_secs_f64() * 1000.0 / n as f64 }
    }
    /// One log line, then reset.
    pub fn take_report(&mut self, queue_depth: u32, dropped: u32) -> String {
        let s = format!(
            "perf: decoded {} decode avg {:.1} ms max {:.1} ms, presented {} present avg {:.1} ms max {:.1} ms, queue {}, dropped {}",
            self.decoded,
            Self::avg(self.decode_total, self.decoded),
            self.decode_max.as_secs_f64() * 1000.0,
            self.presented,
            Self::avg(self.present_total, self.presented),
            self.present_max.as_secs_f64() * 1000.0,
            queue_depth,
            dropped
        );
        *self = Self::default();
        s
    }
}
```

- [ ] **Step 4: Decoder selection**

Change the enum and `open_decoder`:

```rust
#[derive(Clone, Copy, Debug, PartialEq, Eq, clap::ValueEnum)]
pub enum DecoderChoice {
    Auto,
    Mf,
    V4l2,
    Sw,
}

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
        if choice == DecoderChoice::V4l2 {
            anyhow::bail!("V4L2 decode is Linux only");
        }
    }
    #[cfg(target_os = "linux")]
    {
        if matches!(choice, DecoderChoice::Auto | DecoderChoice::V4l2) {
            match castr_codec_v4l2::V4l2Decoder::open() {
                Ok(d) => return Ok(Box::new(d)),
                Err(e) if choice == DecoderChoice::V4l2 => return Err(e),
                Err(e) => tracing::warn!("V4L2 decoder unavailable, falling back to openh264: {e:#}"),
            }
        }
        if choice == DecoderChoice::Mf {
            anyhow::bail!("Media Foundation is Windows only");
        }
    }
    #[cfg(not(any(windows, target_os = "linux")))]
    {
        if matches!(choice, DecoderChoice::Mf | DecoderChoice::V4l2) {
            anyhow::bail!("{choice:?} is not available on this platform");
        }
    }
    Ok(Box::new(SwDecoder::new()?))
}
```

(Keep whatever derive list `DecoderChoice` already has; only add `V4l2`.)

- [ ] **Step 5: Rebuild rule and perf timing in the decode thread**

Replace the decode thread body's inner loop with:

```rust
                tracing::info!("decoder: {}", decoder.name());
                let mut last_decoded: Option<u32> = None;
                let mut errors = ErrorWindow::new(3, Duration::from_secs(10));
                let mut choice = choice;
                loop {
                    let frame = jitter.lock().unwrap().pop(now_us(start));
                    let Some(f) = frame else {
                        std::thread::sleep(Duration::from_millis(2));
                        continue;
                    };
                    stats.lock().unwrap().decode_queue_depth =
                        jitter.lock().unwrap().depth() as u32;
                    tracing::debug!("decode frame {} key={}", f.frame_number, f.keyframe);
                    let t = Instant::now();
                    let result = decoder.decode(&f.data, f.timestamp_us);
                    perf.lock().unwrap().decode(t.elapsed());
                    match result {
                        Ok(Some(raw)) => {
                            last_decoded = Some(f.frame_number);
                            if ui.blocking_send(UiEvent::Frame(raw)).is_err() {
                                return;
                            }
                        }
                        Ok(None) => {}
                        Err(e) => {
                            tracing::warn!(
                                "decode error on frame {} (keyframe={}, last decoded {:?}): {e:#}",
                                f.frame_number,
                                f.keyframe,
                                last_decoded
                            );
                            jitter.lock().unwrap().require_keyframe();
                            if errors.record(Instant::now()) {
                                // Three failures in ten seconds: rebuild, then
                                // fall back to software for the session (spec 3.1).
                                tracing::warn!("rebuilding decoder after repeated errors");
                                decoder = match open_decoder(choice) {
                                    Ok(d) => d,
                                    Err(e) => {
                                        tracing::error!("decoder rebuild failed ({e:#}); using openh264 for the rest of the session");
                                        choice = DecoderChoice::Sw;
                                        match open_decoder(choice) {
                                            Ok(d) => d,
                                            Err(e) => {
                                                tracing::error!("software decoder failed too: {e:#}");
                                                let _ = ui.blocking_send(UiEvent::Quit);
                                                return;
                                            }
                                        }
                                    }
                                };
                                tracing::info!("decoder: {}", decoder.name());
                            }
                        }
                    }
                }
```

with `let perf = perf.clone();` captured alongside `stats`, where `let perf = Arc::new(Mutex::new(PerfStats::default()));` is created next to `stats` in `run()`.

- [ ] **Step 6: Present timing and the 5 s report in the SDL loop**

In the SDL main loop in `run()`, wrap the present call and add the periodic report:

```rust
        if let Some(f) = pending.take() {
            if clock.video_due(f.timestamp_us, now_us(start)) {
                let t = Instant::now();
                renderer.present(&f)?;
                perf.lock().unwrap().present(t.elapsed());
                last_video = Instant::now();
                streaming_seen = true;
            } else {
                pending = Some(f);
            }
        }
        if streaming_seen && last_perf.elapsed() >= Duration::from_secs(5) {
            last_perf = Instant::now();
            let depth = jitter.lock().unwrap().depth() as u32;
            let dropped = dropped_since_report.swap(0, std::sync::atomic::Ordering::Relaxed);
            tracing::info!("{}", perf.lock().unwrap().take_report(depth, dropped));
            if last_video.elapsed() > Duration::from_secs(5) {
                streaming_seen = false; // idle: stop reporting until video flows again
            }
        }
```

with `let mut last_perf = Instant::now();` and `let mut streaming_seen = false;` declared before the loop. `dropped_since_report` is an `Arc<AtomicU32>` created in `run()`, cloned into `NetConfig` as `pub dropped: Arc<AtomicU32>`, and added to in the stats tick of `stream()` right after `let dropped = cfg.jitter.lock().unwrap().dropped();` with `cfg.dropped.fetch_add(dropped, Ordering::Relaxed);`.

- [ ] **Step 7: Hostname default on Linux**

In `main.rs`:

```rust
fn default_name() -> String {
    if let Ok(n) = std::env::var("COMPUTERNAME").or_else(|_| std::env::var("HOSTNAME")) {
        if !n.trim().is_empty() {
            return n;
        }
    }
    // systemd services have no HOSTNAME in their environment.
    if let Ok(n) = std::fs::read_to_string("/etc/hostname") {
        let n = n.trim();
        if !n.is_empty() {
            return n.to_string();
        }
    }
    "castr receiver".into()
}
```

- [ ] **Step 8: Run tests, clippy, both builds**

Run: `cargo test -q --workspace 2>&1 | grep -E "test result|FAILED" | awk '{p+=$4} END {print p}'` (expect 114 + 3 new + the crate's Windows-runnable tests, all passing); `cargo clippy --workspace --tests` (no new warnings); `bash scripts/pi/build-pi.sh`.

Then a live check: deploy the binary to the Pi by hand (`cat dist/castr-receiver-aarch64 | ssh dietpi@192.168.88.157 'cat > ~/bin/castr-receiver.new && chmod +x ~/bin/castr-receiver.new && mv ~/bin/castr-receiver.new ~/bin/castr-receiver'`), `sudo modprobe bcm2835_codec`, start it (`SDL_VIDEODRIVER=kmsdrm nohup ~/bin/castr-receiver --name pi --fullscreen > ~/castr-receiver.log 2>&1 &`), and confirm the log says `decoder: v4l2-bcm2835`. Cast from Windows for 20 s (`castr-sender cast pi --mode game --duration 20` with something moving on screen) and check the `perf:` lines and that the sender stayed at 1920x802. Paste the perf lines into the task report.

- [ ] **Step 9: Commit**

```bash
cargo fmt
git add crates/castr-receiver
git commit -m "feat(receiver): V4L2 decoder selection, rebuild-then-fallback, perf stats line"
```

---

### Task 8: Pi setup script, systemd unit, deploy script, README

**Files:**
- Create: `scripts/pi/setup.sh`
- Create: `scripts/pi/castr-receiver.service`
- Create: `scripts/pi/deploy.sh`
- Modify: `README.md` (Raspberry Pi section)

- [ ] **Step 1: The unit file**

```ini
# scripts/pi/castr-receiver.service
[Unit]
Description=castr screen receiver
After=network-online.target sound.target
Wants=network-online.target

[Service]
User=castr
Group=castr
SupplementaryGroups=video render input audio
Environment=SDL_VIDEODRIVER=kmsdrm
Environment=XDG_CONFIG_HOME=/var/lib/castr/config
# Wait for udev to create the DRM device on early boot. No `$` in this line:
# systemd substitutes $WORD tokens before the shell sees them.
ExecStartPre=/bin/sh -c 'until [ -e /dev/dri/card0 ]; do sleep 0.2; done'
TimeoutStartSec=30
ExecStart=/usr/local/bin/castr-receiver --fullscreen
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
```

- [ ] **Step 2: The setup script**

```bash
#!/usr/bin/env bash
# scripts/pi/setup.sh [path-to-castr-receiver-binary]
# One-shot Raspberry Pi setup for the castr receiver. Idempotent. Run as root.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run as root: sudo $0" >&2; exit 1; }
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="${1:-$HERE/castr-receiver}"
REBOOT=0

CFG=/boot/firmware/config.txt
[ -f "$CFG" ] || CFG=/boot/config.txt
echo "== config.txt ($CFG)"
if ! grep -q '^dtoverlay=vc4-kms-v3d$' "$CFG"; then
  sed -i 's/^#\?dtoverlay=vc4-kms-v3d.*$/dtoverlay=vc4-kms-v3d/' "$CFG"
  grep -q '^dtoverlay=vc4-kms-v3d$' "$CFG" || echo 'dtoverlay=vc4-kms-v3d' >> "$CFG"
  echo "   enabled full KMS"; REBOOT=1
fi
# The VideoCore firmware only starts the H.264 decoder with >= 64 MB; KMS does
# not need gpu_mem, so 128 MB is purely for the codec.
if ! grep -q '^gpu_mem=128$' "$CFG"; then
  sed -i '/^gpu_mem\(_[0-9]\+\)\?=/d' "$CFG"
  echo 'gpu_mem=128' >> "$CFG"
  echo "   gpu_mem=128"; REBOOT=1
fi

echo "== decoder module at boot"
if [ ! -f /etc/modules-load.d/castr.conf ]; then
  echo bcm2835_codec > /etc/modules-load.d/castr.conf
  modprobe bcm2835_codec 2>/dev/null || true
  echo "   /etc/modules-load.d/castr.conf"
fi

echo "== packages"
export DEBIAN_FRONTEND=noninteractive
apt-get install -y -q libstdc++6 libasound2 libdrm2 libgbm1 libgles2 libegl1 v4l-utils >/dev/null

echo "== user castr"
if ! id castr >/dev/null 2>&1; then
  useradd --system --home-dir /var/lib/castr --create-home --shell /usr/sbin/nologin castr
  echo "   created"
fi
usermod -aG video,render,input,audio castr
install -d -o castr -g castr -m 0750 /var/lib/castr /var/lib/castr/config

echo "== binary"
if [ -f "$BIN" ]; then
  install -m 0755 "$BIN" /usr/local/bin/castr-receiver
  echo "   /usr/local/bin/castr-receiver"
else
  echo "   no binary at $BIN (deploy.sh will install one)"
fi

echo "== service"
install -m 0644 "$HERE/castr-receiver.service" /etc/systemd/system/castr-receiver.service
systemctl daemon-reload
systemctl enable castr-receiver >/dev/null 2>&1
if [ "$REBOOT" = 1 ]; then
  echo
  echo "REBOOT REQUIRED (config.txt changed). The service starts after reboot."
else
  systemctl restart castr-receiver
  sleep 3
  systemctl --no-pager --lines=5 status castr-receiver || true
fi
```

- [ ] **Step 3: The deploy script**

```bash
#!/usr/bin/env bash
# scripts/pi/deploy.sh <user@pi>
# Cross-build the receiver, push it to the Pi, install it and restart the
# service. First run on a fresh Pi copies setup.sh + the unit and runs setup.
set -euo pipefail
cd "$(dirname "$0")/../.."
PI="${1:?usage: deploy.sh user@host}"
bash scripts/pi/build-pi.sh
push() { cat "$1" | ssh "$PI" "cat > $2"; }
push dist/castr-receiver-aarch64 /tmp/castr-receiver
if ! ssh "$PI" 'test -f /etc/systemd/system/castr-receiver.service'; then
  echo "== first deploy: running setup.sh on $PI"
  push scripts/pi/setup.sh /tmp/castr-setup.sh
  push scripts/pi/castr-receiver.service /tmp/castr-receiver.service
  ssh "$PI" 'mkdir -p /tmp/castr-setup && mv /tmp/castr-setup.sh /tmp/castr-setup/setup.sh && mv /tmp/castr-receiver.service /tmp/castr-setup/ && mv /tmp/castr-receiver /tmp/castr-setup/castr-receiver && chmod +x /tmp/castr-setup/setup.sh && sudo /tmp/castr-setup/setup.sh'
  exit 0
fi
ssh "$PI" 'sudo install -m 0755 /tmp/castr-receiver /usr/local/bin/castr-receiver && rm -f /tmp/castr-receiver && sudo systemctl restart castr-receiver && sleep 5 && systemctl is-active castr-receiver' \
  || { echo "service not active after restart:"; ssh "$PI" 'journalctl -u castr-receiver -n 20 --no-pager'; exit 1; }
echo "deployed to $PI"
```

- [ ] **Step 4: README**

Replace the "Raspberry Pi receiver" section's setup and run instructions with:

```markdown
## Raspberry Pi receiver

Verified on a Pi 3 Model B running DietPi (Debian 13, 64-bit) with hardware
H.264 decode. Do not build on the Pi; cross-compile with Docker and deploy:

```
bash scripts/pi/deploy.sh dietpi@<pi>     # first run installs everything, later runs update + restart
```

The first run copies `setup.sh` over and runs it as root: it enables full KMS
and `gpu_mem=128` in `config.txt` (the VideoCore firmware does not start the
decoder below 64 MB), loads `bcm2835_codec` at boot, installs runtime packages,
creates a `castr` system user, and installs `castr-receiver.service`. If it
prints REBOOT REQUIRED, reboot; the receiver is then on screen about 20 s after
power-on, named after the hostname, and pairing state lives in
`/var/lib/castr/config/castr/receiver/`. Pair once from each sender after setup.

Logs: `journalctl -u castr-receiver -f`. The `perf:` line every 5 s shows
decode and present times; on a Pi 3 expect 1080p30 with decode under 15 ms.

`--decoder auto` (default) uses V4L2 hardware decode and falls back to openh264
if `/dev/video10` is missing; `--decoder sw` forces software.
```

Keep the existing paragraphs about the SDL renderer choice, the debug switches, the power supply, and the manual binary copy (relabelled "manual alternative").

- [ ] **Step 5: Run it against the Pi**

Run: `bash scripts/pi/deploy.sh dietpi@192.168.88.157`
Expected: first-run path executes `setup.sh`; output shows each `==` section. This Pi has `gpu_mem_256/512/1024=` lines from the earlier manual change, which setup normalises to a single `gpu_mem=128`, so it prints REBOOT REQUIRED. Reboot (`ssh dietpi@192.168.88.157 sudo reboot`), wait for it to return, then `ssh dietpi@192.168.88.157 'systemctl is-active castr-receiver; journalctl -u castr-receiver -n 5 --no-pager'` shows `active` and `decoder: v4l2-bcm2835`. Kill the old hand-started receiver first if it is still running (`pkill -x castr-receiver` as `dietpi`), since two receivers cannot share the display.

Run `deploy.sh` a second time: takes the update path, ends with `deployed to`.

- [ ] **Step 6: Commit**

```bash
git add scripts/pi README.md
git commit -m "feat(pi): setup.sh, systemd unit and deploy.sh for the receiver"
```

---

### Task 9: End-to-end verification on the Pi

**Files:**
- Create: `docs/superpowers/verification/2026-09-02-castr-pi-hardening-e2e.md`

- [ ] **Step 1: Re-pair and cast**

From Windows: `castr-sender pair pi` (PIN is shown on the Pi's screen; the service's identity is new). Then, with a window that keeps changing on screen, `castr-sender cast pi --mode game --fps 30 --duration 60`.

Collect: sender log lines (resolution and fps every few seconds), and `journalctl -u castr-receiver --since "-2 min" --no-pager | grep -E "perf:|decode error|requesting keyframe|decoder:"`.

Pass criteria (spec 8.3 step 2): sender stays at the native resolution and 30 fps; `perf:` decode avg < 15 ms, present avg < 20 ms, queue at most 2; zero `decode error` lines. If the Pi cannot hold 1080p30 in the present stage, record the numbers; the decoder target is what this sub-project promises, and a present-stage limit is the input to the zero-copy follow-up.

- [ ] **Step 2: Reboot and crash recovery**

`ssh dietpi@192.168.88.157 'sudo reboot'`; after it returns, `systemctl is-active castr-receiver` is `active` with no login, and the screen shows WAITING FOR SENDER (confirm with `CASTR_DUMP_FRAME` if nobody is at the monitor: `sudo systemctl set-environment` is not needed; instead run a one-off `sudo -u castr XDG_CONFIG_HOME=/var/lib/castr/config CASTR_DUMP_FRAME=/tmp/f.raw SDL_VIDEODRIVER=kmsdrm /usr/local/bin/castr-receiver --fullscreen` after stopping the service, then restart the service). Then `sudo pkill -x castr-receiver`; within 3 s `systemctl is-active` is `active` again.

- [ ] **Step 3: Network blip**

Mid-cast, `ssh dietpi@192.168.88.157 'sudo ip link set eth0 down; sleep 3; sudo ip link set eth0 up'` (adjust interface name from `ip link`). The cast resumes; the journal shows at most a few `requesting keyframe` lines and no service restart.

- [ ] **Step 4: Software fallback**

Temporarily run the receiver with `--decoder sw` (edit `ExecStart` via `sudo systemctl edit castr-receiver` drop-in, or run by hand as in Step 2) and cast for 15 s: it works as before this sub-project. Remove the drop-in.

- [ ] **Step 5: Write the log and commit**

Record every command and the observed numbers in the verification doc, including the 1080p hardware-test timing from Task 6 and the perf lines from Step 1. Then:

```bash
git add docs/superpowers/verification/2026-09-02-castr-pi-hardening-e2e.md
git commit -m "docs: sub-project 2 end-to-end verification on the Pi"
```

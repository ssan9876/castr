# castr sub-project 2: Raspberry Pi receiver hardening

Hardware H.264 decode on the Raspberry Pi 3 through V4L2, a one-shot Pi setup
script, and a systemd service so the receiver is on screen after power-on.
Builds on sub-project 1 (`2026-09-01-castr-core-design.md`), whose receiver
already runs on the Pi with software decode and KMS/DRM output through SDL2.

## 1. Goal

A Pi 3 Model B shows a 1920x1080 castr stream at 30 fps with decode-to-glass
under two frame intervals, without a desktop environment, starting
automatically at boot, and recovering from crashes and reboots without anyone
touching it. Today the same box decodes in software, so the sender's ladder
drops it to 960x400 and the viewer sees roughly 200 ms of lag.

Everything here is Linux-only and on the receiver. The Windows sender, the
protocol, and the Windows receiver are untouched.

## 2. Findings that shape the design (measured 2026-09-02 on the user's Pi)

- DietPi 10 / Debian 13 (trixie), 64-bit kernel `6.18.39+rpt-rpi-v8`, Mesa 26
  with the `vc4` driver. There is no `/opt/vc` MMAL/OpenMAX stack on 64-bit;
  V4L2 memory-to-memory is the only hardware decode path.
- The decoder is `/dev/video10` (`bcm2835-codec-decode`, driver
  `bcm2835-codec`, staging). Compressed input formats: `H264`, `MPG4`, `MJPG`,
  `H263`. Raw output formats: `YU12`, `YV12`, `NV12`, `NV21`, `NC12` (VC4
  column-tiled), `RGBP`, `AB24`, `BGR4`. `/dev/video11` is the encoder,
  `/dev/video12` the ISP (scaler / format converter), `/dev/video18` image
  effects, `/dev/video31` JPEG encode.
- The module `bcm2835_codec` is not loaded by default on DietPi and must be
  loaded at boot.
- With DietPi's default `gpu_mem=16` the VideoCore firmware does not start the
  codec service: `bcm2835_mmal_vchiq: Failed to open VCHI service connection
  (status=-22)` and no `/dev/video*` appears. `gpu_mem=128` fixes it (64 is
  the documented minimum; 128 leaves headroom for 1080p reference frames).
  Under full KMS (`vc4-kms-v3d`) the display no longer needs `gpu_mem`, so
  128 MB of the 1 GB is a fair price.
- SDL2's default renderer order picks desktop `opengl` and, with no `libGL`
  on the Pi, silently renders black. The receiver already forces `opengles2`
  on Linux (sub-project 1 fix `d2603b3`).
- Software decode ceiling measured: 1280x534 at 30 fps just keeps up,
  1920x802 does not (13 of 31 frames per second).
- The Pi 3's USB Ethernet drops parts of ~150-datagram keyframe bursts; the
  NACK path repairs them. Bitrate ceiling for Pi receivers stays 10 Mbps.

## 3. Architecture

```
castr-receiver (Linux)
  pipeline.rs      decoder selection: auto | v4l2 | sw   (was auto | mf | sw)
  render.rs        unchanged: SDL2 KMSDRM + opengles2, NV12 texture upload
crates/castr-codec-v4l2   NEW, Linux-only
  src/lib.rs       pub struct V4l2Decoder: impl castr_media::VideoDecoder
  src/device.rs    open/probe /dev/video10, capability checks
  src/queue.rs     one V4L2 buffer queue (OUTPUT or CAPTURE): reqbufs, mmap,
                   expbuf, qbuf/dqbuf, streamon/off
  src/annexb.rs    access-unit checks (start codes, NAL types, IDR detection)
  src/sys.rs       ioctl wrappers over v4l2-sys-mit structs via nix
scripts/pi/
  build-pi.sh      existing Docker cross-build
  setup.sh         NEW: run once on the Pi as root
  deploy.sh        NEW: build, push binary, restart service, from the dev box
  castr-receiver.service   NEW: systemd unit installed by setup.sh
```

The receiver keeps using `Box<dyn VideoDecoder>` from a dedicated decode
thread, popping from the jitter buffer and pushing `RawFrame`s to the SDL
thread. `V4l2Decoder` fits behind the existing trait:

```rust
pub trait VideoDecoder: Send {
    /// Feed one complete access unit (Annex B). May return zero or one frame.
    fn decode(&mut self, data: &[u8], timestamp_us: u64) -> anyhow::Result<Option<RawFrame>>;
    fn name(&self) -> &'static str;   // "v4l2-bcm2835"
}
```

`RawFrame { format: PixelFormat::Nv12, width, height, stride, data, timestamp_us }`
is what the Windows Media Foundation decoder already produces, and
`Renderer::present` already uploads NV12, so nothing above the decoder changes.

### 3.1 Decoder selection

`DecoderChoice` gains `V4l2`; `Mf` stays Windows-only and `V4l2` Linux-only
(each errors out with a clear message on the wrong platform). On Linux:

- `auto`: try `V4l2Decoder::open()`; on any error log a warning with the cause
  and use `SwDecoder`. The chosen decoder's name is logged once, as today.
- `v4l2`: hardware or fail.
- `sw`: openh264, as today.

Open probes `/dev/video10` (override with `CASTR_V4L2_DEVICE`), checks
`V4L2_CAP_VIDEO_M2M_MPLANE` (or `_M2M`), and checks that `H264` is listed as an
OUTPUT format and `NV12` as a CAPTURE format. A missing device or module is an
ordinary error, not a panic; `auto` then falls back to software.

If the hardware decoder fails mid-stream (an ioctl error, or no output for
2 s while input keeps flowing), `decode` returns `Err`. The receiver's
existing recovery (drop deltas, request a keyframe) applies. After three such
errors within 10 s the decode thread rebuilds the decoder from scratch
(close, reopen); only if that also fails does it fall back to software for the
rest of the session, logging why.

## 4. V4L2 decoder design

### 4.1 Queues and buffers

The bcm2835 decoder is a multiplanar M2M device with two queues:

| Queue | V4L2 type | Format | Count | Memory |
|---|---|---|---|---|
| OUTPUT (compressed in) | `VIDEO_OUTPUT_MPLANE` | `V4L2_PIX_FMT_H264`, one Annex B access unit per buffer, `sizeimage` 1 MiB | 4 | MMAP |
| CAPTURE (raw out) | `VIDEO_CAPTURE_MPLANE` | `V4L2_PIX_FMT_NV12`, size from the driver after the first SPS | driver minimum + 2 (typically 6) | MMAP, each also exported with `VIDIOC_EXPBUF` |

Per-buffer DMABUF fds are exported and kept alongside the mapping. Nothing
reads them in this sub-project; they exist so a zero-copy present path can be
added later without touching the queue code. A 1 MiB OUTPUT buffer holds any
keyframe we produce (the largest seen at 30 Mbps was 235 KB); an access unit
larger than the buffer is rejected with an error rather than truncated.

### 4.2 Startup sequence

1. `VIDIOC_QUERYCAP`, format checks (3.1).
2. `VIDIOC_S_FMT` on OUTPUT: `H264`, `sizeimage = 1 MiB`, width/height 0
   (the stream defines them).
3. `VIDIOC_SUBSCRIBE_EVENT` for `V4L2_EVENT_SOURCE_CHANGE` and `V4L2_EVENT_EOS`.
4. `VIDIOC_REQBUFS` + `VIDIOC_QUERYBUF` + `mmap` for OUTPUT, `VIDIOC_STREAMON`
   OUTPUT.
5. Bring CAPTURE up provisionally, at whatever format `G_FMT` reports (32x32
   on this driver): `S_FMT` `NV12`, `REQBUFS`, `QUERYBUF`, `mmap`, `EXPBUF`,
   queue all, `STREAMON`. An M2M job only runs when *both* queues are
   streaming with buffers queued, so without this the pipeline is idle during
   the warm-up and the pictures decoded from the access units fed meanwhile
   are lost. It does not make the first `SOURCE_CHANGE` any earlier (measured
   on a Pi 3: about 70 ms after the first access unit either way). Pictures
   dequeued from the provisional queue are recycled, never returned: they
   carry the driver's default geometry, not the stream's.
6. Feed access units. The decoder cannot size real CAPTURE buffers until it
   has parsed an SPS, so the CAPTURE side is rebuilt on the first
   `SOURCE_CHANGE` event, by the same path as any later one (4.4): `VIDIOC_G_FMT` CAPTURE (driver reports coded size,
   `bytesperline`, `sizeimage`), `VIDIOC_S_FMT` CAPTURE to `NV12`, `REQBUFS`,
   `QUERYBUF`, `mmap`, `EXPBUF`, queue all buffers, `STREAMON` CAPTURE.
7. Apply `VIDIOC_G_SELECTION` (`V4L2_SEL_TGT_COMPOSE`) to learn the visible
   rectangle inside the coded size (e.g. 1920x802 visible inside 1920x816),
   and crop rows when copying out.

### 4.3 Steady state

`decode(data, ts)`:

1. Reject data that does not start with an Annex B start code.
2. If no OUTPUT buffer is free, `poll` for one (timeout 20 ms per round),
   dequeuing completed OUTPUT buffers. The driver does not release a finished
   job's OUTPUT buffer until its CAPTURE buffer has been dequeued - while
   waiting it reports `POLLIN` with no `POLLOUT` - so a ready picture is
   collected here too, and the OUTPUT queue re-checked straight after; that is
   what frees the slot. Copy the access unit in, set `bytesused`,
   `timestamp = ts` (as `timeval`, microseconds), `VIDIOC_QBUF`.
3. Drain events (`VIDIOC_DQEVENT`, non-blocking). On `SOURCE_CHANGE` run the
   renegotiation in 4.4. On `EOS` mark the decoder as finished.
4. Non-blocking `VIDIOC_DQBUF` on CAPTURE. If a buffer is ready: build a
   `RawFrame` (NV12, visible width/height, `stride = bytesperline`, data copied
   out of the mapping and cropped to the visible height, `timestamp_us` from
   the buffer timestamp), immediately `QBUF` the buffer back, return the frame.
   If none is ready return `Ok(None)`; the next call collects it.

At most two OUTPUT buffers are left queued at a time; if two are already in
flight, step 2 waits for one to complete before queueing. This bounds decoder
latency at about two frames without starving it.

Timestamps round-trip through the driver, so the frame returned carries the
timestamp of the access unit it decoded, not the one just fed. The pipeline
already tolerates that (the Media Foundation decoder behaves the same way).

The copy out of the CAPTURE mapping is the only per-frame CPU work, and it is
the decoder's dominant cost. The mapping is uncached, so reading it runs at
about 105 MB/s on one Pi 3 core: 2.31 MB (1920x802) takes 26.8 ms on one
thread. It is done row by row with the visible width so the `RawFrame` stride
equals the width, matching what the Windows decoder produces and what
`Renderer::present` expects for NV12 (`tex.update(None, &data, w)`); one
`memcpy` per plane measured the same, so the shape of the copy is not the
cost. It does scale across cores (19.4 ms on two threads, 16.3 on three, 14.3
on four), so the rows are split across up to three scoped threads - about
15 ms at 1080p - leaving a core for the presenter. The threads are joined
before the call returns (4.6).

Only one picture is copied per `decode` call, on top of whatever step 2 has to
collect to free an OUTPUT buffer. `poll_frame` (on the `VideoDecoder` trait)
hands back a picture the decoder is still holding, with a zero poll timeout:
it never waits, because callers drain it in a loop until it returns `None` and
a blocking call there costs a whole poll step per frame for nothing.

A picture the driver produced on the provisional CAPTURE queue (4.2 step 5) is
recycled, not returned.

### 4.4 Resolution change

The game-mode ladder switches between 1920, 1280 and 960 wide every few
seconds under load, so this path is routine, not exceptional. On
`SOURCE_CHANGE` with `V4L2_EVENT_SRC_CH_RESOLUTION`:

1. `VIDIOC_STREAMOFF` CAPTURE (returns all CAPTURE buffers).
2. Unmap and close the old CAPTURE buffers, `REQBUFS(0)` to free them.
3. `G_FMT` / `S_FMT` CAPTURE, `REQBUFS`, `QUERYBUF`, `mmap`, `EXPBUF`, queue,
   `STREAMON` CAPTURE, refresh the compose rectangle.

OUTPUT streaming continues throughout; buffers queued before the change stay
valid. The first event after startup follows exactly the same steps, so there
is one code path.

A resolution change always arrives on a keyframe (the sender restarts the
encoder), and the jitter buffer already guarantees a keyframe follows any
gap, so the decoder never sees a delta frame for a size it has not been told
about.

### 4.5 Errors and timeouts

- Any ioctl failure other than `EAGAIN` on a non-blocking `DQBUF` is an
  `Err` from `decode`.
- If 60 consecutive access units have been queued with no CAPTURE buffer
  dequeued **and** 2 s have passed since the last picture, `decode` returns
  `Err("decoder stalled")`. Both halves are needed: 60 access units are 2 s of
  video only when the caller feeds in real time, and a receiver catching up
  after a jitter-buffer burst legitimately pushes a second of video into the
  hardware in milliseconds. The decode thread's rebuild rule in 3.1 handles
  repeats.
- A wait inside `decode` gives up after 2 s without progress. That budget is
  wall clock, not a count of poll rounds: `poll` returns immediately whenever a
  queue is readable, so rounds are no measure of time (measured on the Pi: 100
  rounds in 72 ms).
- `POLLERR`/`POLLHUP`/`POLLNVAL` on the device is fatal to the decoder
  immediately; on an M2M device it never clears by itself.
- `Drop` for `V4l2Decoder`: `STREAMOFF` both queues, unmap, close fds. Errors
  during drop are logged, not propagated.

### 4.6 Threading

`V4l2Decoder` is used from the receiver's single decode thread only; it is
`Send` and not `Sync`. It spawns no background threads and holds no shared
state; the only threads it creates are the scoped copy threads of 4.3, which
are joined before `decode` or `poll_frame` returns. `poll` timeouts keep the
decode thread responsive to the jitter buffer.

## 5. Receiver changes

- `DecoderChoice::V4l2` and the selection rules in 3.1.
- Capabilities advertised to the sender are unchanged (1920x1080, 30 fps,
  10 Mbps ceiling); those numbers now reflect what the box can actually do.
- Stats line: every 5 s while streaming, at `info`, one line with frames
  decoded, average and max decode call time, average and max present (upload
  + draw) time, current decode queue depth, and frames dropped. The present
  time is measured in the SDL loop around `renderer.present`. This is how the
  1080p30 target in section 1 is verified from a log.
- `--name` defaults to the machine hostname when not given (the service
  relies on this; today it is a required argument on the CLI).

## 6. Pi setup script (`scripts/pi/setup.sh`)

Idempotent, runs on the Pi as root (`sudo ./setup.sh`), Debian-family only.

1. `config.txt` (`/boot/firmware/config.txt`, falling back to
   `/boot/config.txt`): ensure `dtoverlay=vc4-kms-v3d` (replacing a commented
   or `,noaudio` variant), and `gpu_mem=128` (replacing any `gpu_mem*=` lines).
   Records whether a reboot is needed.
2. `/etc/modules-load.d/castr.conf` containing `bcm2835_codec`.
3. `apt-get install` the runtime packages: `libstdc++6 libasound2 libdrm2
   libgbm1 libgles2 libegl1 v4l-utils` (the last one for diagnostics).
4. System user `castr` (no login shell, home `/var/lib/castr`), groups
   `video render input audio`.
5. `/usr/local/bin/castr-receiver` from the binary next to the script (or a
   path given as `$1`), mode 0755.
6. `/etc/systemd/system/castr-receiver.service` (below), `systemctl enable`,
   and start it unless a reboot is pending.
7. Print what changed and, if needed, "reboot required".

The unit:

```ini
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
# The receiver needs the DRM device; wait for udev to create it on early boot.
# (No `$` in the line: systemd substitutes $WORD tokens before the shell runs.)
ExecStartPre=/bin/sh -c 'until [ -e /dev/dri/card0 ]; do sleep 0.2; done'
TimeoutStartSec=30
ExecStart=/usr/local/bin/castr-receiver --fullscreen
Restart=always
RestartSec=2

[Install]
WantedBy=multi-user.target
```

Identity, keys and the paired list therefore live in
`/var/lib/castr/config/castr/receiver/`. The Pi must be paired once more from
each sender after setup; the old state under `/home/dietpi/.config/castr` is
left alone.

Logs go to the journal: `journalctl -u castr-receiver -f`.

## 7. Deploy script (`scripts/pi/deploy.sh`)

From the dev machine: `scripts/pi/deploy.sh dietpi@192.168.88.157`. Runs
`build-pi.sh`, streams the binary over ssh (`cat | ssh ... 'cat > tmp && mv'`,
because DietPi's dropbear has no SFTP), and runs
`sudo install -m 0755 tmp /usr/local/bin/castr-receiver && sudo systemctl
restart castr-receiver`. First-time use copies `setup.sh` and the unit over
and runs setup instead. Exits non-zero if the service is not `active` five
seconds after restart, printing the last 20 journal lines.

## 8. Testing

### 8.1 Host-side unit tests (no hardware, run everywhere)

- `annexb`: start-code detection (3- and 4-byte), NAL type extraction,
  IDR/SPS presence checks, rejection of non-Annex-B input.
- Queue bookkeeping with a fake ioctl layer: the `sys` module is a trait
  (`V4l2Ops`) with the real implementation over `nix::ioctl` and a scripted
  fake in tests, so the startup, steady-state, source-change and stall paths
  in section 4 are exercised as state machines: correct ioctl order, buffers
  never double-queued, at most two OUTPUT buffers in flight, source-change
  reallocates, stall detection after 60 unanswered inputs.

### 8.2 Hardware tests (`#[ignore]`, run on the Pi)

Cross-built with `cargo test --no-run --target aarch64-unknown-linux-gnu` and
executed on the Pi with `--ignored`, as done for sub-project 1's suites:

- Decode a synthetic 640x360 clip produced by the software encoder
  (`castr_media::sw::SwEncoder`): every frame decodes, dimensions match, the
  timestamps come back in order, and the middle frame is not uniformly black.
- Source change: a clip that is 640x360 for 30 frames then 1280x720 for 30
  (two encoder instances, concatenated), checking the output sizes switch and
  no frame is lost across the boundary.
- Throughput: 300 frames of 1920x1080 decode in under 10 s wall clock, with
  no frame's `decode` call exceeding 40 ms.
- `open()` fails cleanly with `CASTR_V4L2_DEVICE=/dev/null`.

### 8.3 End to end (manual, logged in the verification doc)

1. `deploy.sh` to a Pi that has been through `setup.sh`; reboot; the receiver
   is on screen with no login. `systemctl status` shows active; killing the
   process brings it back within 3 s.
2. Cast from Windows for 60 s with screen activity: the sender log stays at
   1920x802 / 30 fps, the receiver stats line shows decode under 15 ms
   average, present under 20 ms, queue depth at most 2, and zero decode
   errors. Frame dump (`CASTR_DUMP_FRAME`) shows the desktop.
3. Pull the Ethernet cable for 3 s mid-cast: the stream resumes with one
   keyframe request and no receiver restart.
4. `--decoder sw` still works as the fallback.

## 9. Out of scope

Zero-copy present (DMABUF into GLES or KMS planes), Miracast, HEVC, the ISP
scaler, the Windows receiver, any sender change, Wi-Fi setup, an installable
SD-card image.

## 10. Key dependencies

- `nix` 0.31 (ioctl, mmap, poll), `v4l2-sys-mit` 0.3 (videodev2 bindings),
  both in `castr-codec-v4l2` only, `cfg(target_os = "linux")`.
- No new dependencies elsewhere. The Windows build must not compile or link
  the new crate (`[target.'cfg(target_os = "linux")'.dependencies]` in the
  receiver).

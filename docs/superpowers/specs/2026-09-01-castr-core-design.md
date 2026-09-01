# castr core: protocol, Windows sender, desktop receiver

Date: 2026-09-01
Status: approved design, sub-project 1 of 4
Language: Rust

## 1. Goal

A screen-sharing protocol in the spirit of Miracast, with both endpoints under
our control, that fixes Miracast's four recurring failures: flaky discovery
and pairing, mid-session disconnects, lag and stutter, and broken audio sync.

Sub-project 1 delivers the core protocol, a Windows sender, and a desktop
receiver binary that runs on Windows for development and cross-compiles to
Linux (Raspberry Pi 3 Model B is the first hardware target).

Later sub-projects (each with its own spec):

2. Pi receiver hardening: hardware decode, DRM/KMS output, boot autostart.
3. Android receiver.
4. Android sender.

## 2. Miracast failures and the corresponding design decisions

| Miracast pain | Root cause | Decision |
|---|---|---|
| Discovery/pairing flaky | Wi-Fi Direct group negotiation and WPS, driver dependent | Run over the existing LAN. mDNS plus UDP broadcast fallback. One-time PIN pairing that pins certificates; later connects are silent. |
| Random disconnects | RTSP keepalives, no session resume, fragile P2P link | QUIC with a session token. Reconnect resumes without renegotiation; sender emits a keyframe. |
| Lag/stutter/quality | MPEG-TS over RTP, no loss recovery, bitrate fixed at start | Receiver stats every 100 ms drive adaptive bitrate. NACK retransmit for keyframes, drop-late for delta frames. Hardware codecs. |
| Audio sync/missing | LPCM in MPEG-TS, independent clocks drift | Opus audio, one sender clock stamped on every packet, audio is the master clock on the receiver. |

## 3. Hardware constraints (Raspberry Pi 3 Model B)

- Hardware H.264 decode only, up to 1080p30. H.264 is the mandatory codec.
  HEVC is out of scope for this sub-project.
- Wi-Fi is 2.4 GHz 802.11n only; Ethernet is 100 Mbit on a shared USB bus.
  Bitrate ceiling for Pi receivers is 10 Mbps. Wired Ethernet is recommended.
- Expected game-mode glass-to-glass latency on the Pi 3 is roughly 60 ms,
  dominated by the decoder pipeline.

## 4. Workspace layout

Cargo workspace `castr` (name is a placeholder).

| Crate | Responsibility | Platform code allowed |
|---|---|---|
| `castr-proto` | Wire format, packet types, packetize/reassemble, session state machine. No I/O. | No |
| `castr-net` | QUIC transport (`quinn`), mDNS discovery, UDP broadcast fallback, pairing. | No |
| `castr-media` | `VideoEncoder`/`VideoDecoder` traits, software H.264 backend (`openh264`), Opus wrappers, jitter buffer, A/V clock, bitrate controller. | No |
| `castr-codec-win` | Media Foundation H.264 encoder and decoder implementing the `castr-media` traits. Hardware MFTs preferred, Microsoft software MFT as fallback. | Windows only |
| `castr-capture-win` | Desktop Duplication video capture, WASAPI loopback audio capture. | Windows only |
| `castr-sender` | CLI binary wiring capture, codecs, media, and net. | Yes |
| `castr-receiver` | CLI binary wiring net, codecs, media, and SDL2 rendering. Runs on Windows and Linux. | Yes |

Rule: `castr-proto`, `castr-net`, and `castr-media` must compile on any
target without platform-specific code, so Android can wrap them later.
Codec backends are selected at build time by target: `castr-codec-win` on
Windows, the `openh264` software backend elsewhere. A V4L2 M2M backend for
the Pi's hardware decoder is sub-project 2.

No FFmpeg anywhere. The software backend exists for tests, CI, and as a
last-resort fallback; it is not expected to reach 1080p on the Pi 3.

## 5. Discovery and pairing

### 5.1 Discovery

- The receiver advertises `_castr._udp.local` via mDNS. TXT record carries
  `name`, `fp` (SHA-256 fingerprint of its certificate, hex), and `ver`.
- Fallback: the receiver also listens on UDP port 7331 for a broadcast
  probe. Probe is the ASCII bytes `CASTR?` followed by the protocol
  version byte. Reply is the same TXT information encoded with `postcard`.
- The sender runs both methods in parallel and merges results by fingerprint.

### 5.2 Pairing

- Each endpoint generates a self-signed certificate and Ed25519 key on
  first run, stored in the platform config directory.
- First connect: the receiver displays a 6-digit PIN. The sender opens a
  QUIC connection accepting any certificate, then runs SPAKE2 over the
  control stream using the PIN as the password. Both sides derive the same
  key only if the PIN matched. Each side then sends an HMAC of its own
  certificate fingerprint under that key; the peer verifies it.
- On success both sides persist the peer fingerprint in a `paired.toml`
  file. All later connections require the peer certificate to match a
  stored fingerprint, otherwise the connection is refused before any media
  flows.
- Unpairing is deleting the entry. Three failed PIN attempts close the
  connection and the receiver rotates the PIN.

## 6. Session and transport

One QUIC connection per session. TLS 1.3 comes from QUIC. Datagram support
is required and negotiated at handshake.

### 6.1 Control stream

Bidirectional, reliable, opened by the sender. Messages are `postcard`
encoded with a 4-byte little-endian length prefix.

| Message | Direction | Content |
|---|---|---|
| `Hello` | sender to receiver | protocol version, sender name, session token (optional, for resume) |
| `HelloAck` | receiver to sender | receiver name, capabilities: max width/height/fps, max bitrate, codec list, audio support |
| `StartStream` | sender to receiver | chosen codec, width, height, fps, mode, initial bitrate |
| `SessionToken` | receiver to sender | 16 random bytes, valid for 60 s after disconnect |
| `SetMode` | sender to receiver | Game or Quality |
| `RequestKeyframe` | receiver to sender | none |
| `Stats` | receiver to sender | frames received, frames dropped, fragments lost, decode queue depth, interval ms |
| `Error` | either | code, human-readable message |
| `Goodbye` | either | reason |

### 6.2 Media datagrams

Every datagram begins with this 20-byte header (little-endian):

| Offset | Size | Field |
|---|---|---|
| 0 | 1 | stream id: 0 video, 1 audio |
| 1 | 1 | flags: bit 0 keyframe, bit 1 end of frame |
| 2 | 2 | fragment index |
| 4 | 2 | fragment count |
| 6 | 2 | reserved, zero |
| 8 | 4 | frame number (wraps) |
| 12 | 8 | sender timestamp, microseconds, monotonic |

Payload follows the header. Video payload is a slice of one H.264 access
unit in Annex B format. Audio payload is one 10 ms Opus frame; audio
frames always have fragment count 1.

Fragment size is path MTU minus QUIC and header overhead, as reported by
`quinn::Connection::max_datagram_size`, re-read before each frame.

### 6.3 Retransmit stream

Unidirectional, reliable, opened by the receiver. Carries `Nack { frame,
missing: Vec<u16> }` messages. The sender keeps the last 500 ms of sent
fragments in a ring buffer and resends a fragment only if the frame is a
keyframe or is younger than one frame interval. Other NACKs are ignored;
the receiver will skip to the next keyframe.

### 6.4 Why datagrams

QUIC streams retransmit and deliver in order, which would stall every later
frame behind one lost packet. Datagrams give per-packet loss with QUIC's
encryption, MTU discovery, and connection migration intact.

## 7. Media pipeline

### 7.1 Sender

1. Desktop Duplication acquires the next frame with a timeout of one frame
   interval. On timeout (unchanged desktop) the last frame is re-encoded
   only if 500 ms have passed since the last sent frame, so the receiver
   never sees a stall.
2. The acquired texture is copied to a staging texture and mapped to CPU
   memory as BGRA, then converted to NV12. (Passing the D3D11 texture
   straight to the encoder MFT for zero-copy encode is an optimization for
   later; the trait accepts either.)
3. Encoder selection via `MFTEnumEx` for `MFVideoFormat_H264` output with
   `MFT_ENUM_FLAG_HARDWARE | MFT_ENUM_FLAG_SORTANDFILTER`. The first
   hardware MFT that activates and accepts the negotiated type is used and
   logged. If none, fall back to the Microsoft software H.264 encoder MFT,
   then to `openh264`. The MFT is run in asynchronous mode with
   `CODECAPI_AVLowLatencyMode`, `CODECAPI_AVEncCommonRateControlMode` CBR
   for game mode and VBR for quality mode, and `CODECAPI_AVEncMPVGOPSize`
   per section 8.2. Bitrate changes apply live via
   `CODECAPI_AVEncCommonMeanBitRate` without reopening the encoder.
4. Encoder output is packetized per section 6.2 and sent as datagrams.
5. Audio runs on its own thread from WASAPI loopback through Opus and is
   sent as it is produced.

### 7.2 Receiver

1. Datagrams are parsed and placed in a reassembly map keyed by frame
   number. A frame is complete when all fragments are present. Incomplete
   frames older than 500 ms are discarded and a NACK is sent for anything
   younger that is a keyframe.
2. Complete frames enter the jitter buffer (see section 8 for depth).
3. Decoder selection: on Windows, the Microsoft H.264 decoder MFT with
   `MF_SA_D3D11_AWARE` and a D3D11 device manager so decode happens on the
   GPU and output is an NV12 texture; on other targets, `openh264`
   software decode. (V4L2 M2M for the Pi is sub-project 2.)
4. Decoded frames are uploaded to an SDL2 texture and presented according
   to the A/V clock in section 9.
5. Drop rule: if a newer complete frame exists and the current one has not
   started decoding, discard the current one. If the discarded frame was a
   keyframe, keep it and discard the newer delta frames instead, so the
   decoder reference chain stays valid.

## 8. Adaptation and modes

### 8.1 Stats and bitrate control

The receiver sends `Stats` every 100 ms. The sender computes loss ratio as
fragments lost over fragments sent in the interval, and reads RTT from
`quinn`. The bitrate controller:

- Floor 1 Mbps, ceiling from `HelloAck` max bitrate (10 Mbps for the Pi,
  40 Mbps default for Windows receivers).
- On loss above 2% in an interval: multiply bitrate by 0.7, no more than
  once per 500 ms.
- On decode queue depth above 3 frames: multiply by 0.85.
- After 1 s with loss below 0.5% and queue depth at most 1: add 5% of the
  ceiling.
- Resolution steps down (1080p to 720p to 540p) only in game mode when
  bitrate has been at the floor for 2 s, and steps back up after 5 s clean.

### 8.2 Modes

| Setting | Game | Quality |
|---|---|---|
| Encoder tuning | zero latency, no B-frames, intra-refresh, rate control CBR | low latency, B-frames allowed, 2 s GOP, VBR |
| Jitter buffer | 0 frames, render newest | 150 ms |
| Late frame | drop immediately | wait one frame interval, then drop |
| Resolution under loss | steps down | held, bitrate lowered instead |
| Audio buffer | 40 ms | 100 ms |

Switching mode mid-session: sender sends `SetMode`, reconfigures the
encoder, and emits a keyframe. Receiver flushes the jitter buffer and
resizes the audio buffer.

## 9. Audio and sync

- Capture: WASAPI loopback, 48 kHz stereo float, converted to 16-bit.
- Encode: Opus, 128 kbps, 10 ms frames, application mode `restricted low
  delay`.
- Every audio and video packet carries the sender's monotonic clock in
  microseconds, taken at capture time.
- Receiver keeps audio as the master clock. Audio plays continuously from
  a ring buffer sized per mode. A video frame is presented when the audio
  playback position reaches the frame's timestamp, or immediately in game
  mode if no audio is flowing.
- Clock drift between machines is corrected by resampling audio by at most
  0.5%, never by dropping or repeating video frames.
- If audio stops arriving for 200 ms the receiver falls back to presenting
  video against its own monotonic clock offset by the last known
  sender-to-receiver delta.

## 10. Errors and reconnection

- After `Hello`/`HelloAck`, the receiver issues a `SessionToken`.
- If the QUIC connection is lost, the sender continues capturing and
  encoding into its ring buffer and reconnects with exponential backoff
  starting at 200 ms, doubling to a 5 s cap, for 30 s total.
- On reconnect the sender sends `Hello` with the token. The receiver
  accepts it if the token matches and is within 60 s of disconnect,
  skips capability exchange, and the sender emits a keyframe immediately.
- The receiver shows a "Reconnecting" overlay from the moment of loss
  until the first decoded frame after resume.
- Unrecoverable failures (encoder or decoder init failure, unsupported
  capability) are sent as `Error` so the peer displays the message
  instead of a black screen, then `Goodbye`.
- Timeouts: QUIC idle timeout is 3 s. The receiver treats 1 s without
  video as stalled and requests a keyframe.

## 11. Configuration and CLI

`castr-sender`:
- `list` prints discovered receivers.
- `pair <name|fp>` runs the PIN flow.
- `cast <name|fp> [--mode game|quality] [--fps 30|60] [--max-bitrate N]`.

`castr-receiver`:
- `--name <display name>`, `--fullscreen`, `--max-bitrate N`
  (default 10 Mbps on ARM Linux, 40 Mbps elsewhere), `--decoder auto|mf|sw`
  (`v4l2` is added in sub-project 2).
- Displays PIN when an unpaired sender connects.

Config and pairing files live in the platform config directory under
`castr/`.

## 12. Testing

| Layer | Tests |
|---|---|
| `castr-proto` | Packetize/reassemble round trip. Out-of-order fragments. Lost fragments produce correct NACK lists. Frame number wraparound. Session state machine transitions including resume with valid, expired, and wrong tokens. |
| `castr-media` | Jitter buffer ordering and drop rules per mode. A/V clock: video scheduled against audio timestamps, drift correction bounds. Bitrate controller responds to synthetic stats as specified in 8.1. Encode/decode round trip through the `openh264` backend so CI needs no GPU. |
| `castr-codec-win` | Encode a synthetic frame sequence with the MF encoder and decode it with the MF decoder; assert frame count, dimensions, and that a keyframe is produced on request. Cross-check by decoding MF output with `openh264`. Runs only on Windows. |
| `castr-net` | Two endpoints in one process over loopback. A lossy shim drops a configurable fraction of datagrams. Verifies pairing success and failure, session resume, and NACK retransmit of keyframes only. |
| End to end | Sender and receiver on one Windows machine. Then Windows sender to Pi 3 over Ethernet. Measure glass-to-glass latency with a timer on screen and a phone camera. |

## 13. Out of scope for this sub-project

Android, DRM/KMS direct output on the Pi, V4L2 hardware decode on the Pi,
remote input, multiple receivers, HEVC, zero-copy GPU encode, internet
(non-LAN) use.

## 14. Key dependencies

`quinn`, `rustls`, `rcgen`, `mdns-sd`, `spake2`, `postcard`, `serde`,
`openh264` (bundles Cisco's library, built with cmake), `audiopus` with
bundled libopus, `sdl2` with the `bundled` feature, `windows` (Desktop
Duplication, WASAPI, Media Foundation, D3D11), `tokio`, `tracing`, `clap`.

Build prerequisites: a C compiler and cmake on every platform for the
bundled `openh264`, `opus`, and SDL2 builds. No system media libraries
are required on Windows. No FFmpeg on any platform.

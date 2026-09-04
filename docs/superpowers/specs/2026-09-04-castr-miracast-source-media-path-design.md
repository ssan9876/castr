# castr sub-project 7: Miracast source, the media path — design (2026-09-04)

Part 1 of 4 in the Miracast source project. The others, in order: the Windows
Wi-Fi Direct connection layer, product integration in the sender, and interop
hardening against real displays.

## 1. Goal

Let the Windows sender cast to ordinary Miracast displays — televisions,
dongles, other Windows PCs — as well as to a castr receiver.

This sub-project builds the source half of the media path: the MPEG-TS
multiplexer, the RTP packetizer, the RTSP source session, and the capability
intersection that chooses what to send. It deliberately owns **no radio**. The
whole of it is exercised against our own Pi sink over Ethernet, which is also
the Miracast-over-Infrastructure transport, so this work unblocks MS-MICE as a
side effect rather than as extra scope.

Success: `castr-sender` streams a real desktop to the Pi's Miracast sink over
an ordinary IP connection, negotiated through RTSP M1-M7, with picture and
sound, and every stage of failure reported by name.

## 2. What already exists

`castr-miracast` is entirely sink-direction, and that asymmetry is the work:

| Module | Today | Needed for a source |
|---|---|---|
| `ts.rs` | demux | a multiplexer |
| `rtp.rs` | depacketize and reorder | a packetizer |
| `session.rs` | sink state machine | a source state machine |
| `wfd.rs` | builds sink capability bodies | reads them, and chooses |
| `rtsp.rs` | already bidirectional | unchanged |

Reusable as-is: `MfEncoder` (hardware H.264), `LoopbackCapture` (WASAPI, 48 kHz
stereo — already the format Wi-Fi Display mandates for LPCM), `DesktopCapture`,
and the capture/encode plumbing in `castr-sender/src/cast.rs`.

## 3. What the spike established (2026-09-04)

Recorded because the design leans on it. From unprivileged Rust using the
`windows` crate, against the Pi sink:

- Wi-Fi Direct peers enumerate, and the Wi-Fi Display information element is
  readable — so a display can be told from a printer before connecting, and its
  RTSP port, maximum throughput and HDCP capability read in advance.
- Pairing with a PIN completes programmatically, with no human at the keyboard.
- A group forms, DHCP issues an address, and the sink answers RTSP `OPTIONS`
  with `RTSP/1.0 200 OK` and its full `Public` list.

So M1 already works end to end. Everything from M2 onward is this sub-project.

## 4. Scope

**In:** `ts_mux`, `rtp_pack`, `lpcm`, `caps`, `source::session`, and a driver
that owns the sockets, the encoder and the capture. Audio from the start, LPCM
48 kHz stereo, because the capture already produces that format and deferring
it would mean designing the multiplexer twice.

**Out:** the radio (part 2), sender UI and CLI (part 3), interop quirks against
third-party displays (part 4), HDCP in any form, and any Miracast *sink* role
on Windows.

## 5. Components

Five pure units under a `source` namespace inside `castr-miracast`, plus one
impure driver. The sink's modules do not move, and `rtsp.rs` and `wfd.rs` serve
both roles unchanged. The crate's own documentation stops describing a sink and
starts describing both roles.

- **`source::ts_mux`** — H.264 access units and LPCM audio with presentation
  timestamps in, 188-byte transport packets out. Owns PES packetization,
  continuity counters, PAT/PMT repetition and PCR insertion.
- **`source::rtp_pack`** — seven transport packets to a 1316-byte payload,
  payload type 33, sequence numbers, 90 kHz timestamps.
- **`source::lpcm`** — the Wi-Fi Display LPCM framing, which is big-endian and
  carries its own header, so it is not plain PCM.
- **`source::caps`** — capability intersection. The sink's CEA, VESA and
  handheld bitmaps against what we can capture and encode; honours profile,
  level and maximum resolution; selects exactly one mode, because M4 names a
  single choice rather than a menu.
- **`source::session`** — the source state machine. Bytes in, actions out, in
  the same shape as `session.rs`, so a negotiation replays without a socket.
- **The driver** — the only impure part: RTSP over TCP, RTP over UDP, the
  encoder, the capture and the clock.

## 6. Data flow and timing

Desktop Duplication frame to `MfEncoder` to an Annex-B access unit with its
capture timestamp. WASAPI loopback to 48 kHz stereo `i16` to LPCM framing. Both
into `ts_mux`, then `rtp_pack`, then the UDP socket.

The timing model **inverts** castr's own protocol. Ours makes the receiver's
Opus playback the master clock and adapts the sender to receiver statistics.
Wi-Fi Display makes the source authoritative: it stamps a 90 kHz PTS on every
access unit and writes a program clock reference into the stream, and the sink
slaves to it. There is no back-channel of receiver statistics.

That is a simplification worth stating plainly: **lip sync becomes the sink's
responsibility**, which is why a compliant display can be trusted with it.

One monotonic origin is taken at session start and shared by audio and video,
so the two cannot drift apart. Bitrate is bounded by the throughput the sink
advertises in its information element — 54 Mbps for a Samsung television, 10
for our Pi — read before connecting rather than guessed.

The PCR interval and the PAT/PMT repetition rate are to be taken from the
specification text during implementation, not from memory. They are exactly the
kind of constant that quietly breaks interop when it is wrong.

## 7. Negotiation

Confirmed against the sink already implemented in `session.rs`:

1. **M1** source sends `OPTIONS`; sink replies with `Public`.
2. **M2** sink sends `OPTIONS`; source replies. Both ends act as client and
   server over one TCP connection, which is why `rtsp.rs` is bidirectional.
3. **M3** source reads `wfd_video_formats`, `wfd_audio_codecs`,
   `wfd_content_protection`, `wfd_client_rtp_ports`.
4. **M4** source sets the chosen mode, its RTP ports and the presentation URL.
5. **M5** source sets `wfd_trigger_method=SETUP`.
6. **M6/M7** the sink turns client: `SETUP`, then `PLAY`. Streaming begins.

Two rules are built in deliberately:

- **Unknown parameters are ignored, never fatal.** Our own sink already emits
  `microsoft_max_bitrate` and other vendor extensions, and a real television
  will emit some we have never seen. This gets its own test.
- **An empty intersection fails loudly**, reporting what each side offered. It
  is the stage most likely to fail against an unfamiliar display, so it is the
  stage that must explain itself best.

## 8. Error handling

Every failure names its stage and reports what was known at that point.
"Connecting" forever is the behaviour being replaced.

The full taxonomy spans this part and the radio layer in part 2. The stage
names are fixed here so both parts report in the same vocabulary, but only the
last three are built and verified in this sub-project.

| Stage | Part | Failure | Reported as |
|---|---|---|---|
| Discovery | 2 | no Wi-Fi Display element | the display is not in mirroring mode, and how to fix that |
| Association | 2 | no group, or a group with no beacon | the reason quoted from the WLAN stack |
| WPS | 2 | wrong PIN, or expired walk time | the two distinguished, with different advice |
| Connect | 1 | RTSP port unreachable, or M1 unanswered | the address tried and the timeout that expired |
| Negotiation | 1 | no common format | what each side offered |
| Session | 1 | sink powered off, or input changed | detected by keepalive, not by silence |
| Teardown | 1 | none | `TEARDOWN` always sent; part 2 also drops the group |

RTP is one-way UDP with no feedback, so silence on the media path cannot
distinguish an idle desktop from a dead display. Liveness comes from RTSP
keepalives on the control channel.

Carried forward into part 2, where it applies: a group is always torn down on
exit. Leaving one up is how a peer comes to hold credentials for a group that
no longer exists — a failure this project has already paid for once.

## 9. Testing

Almost all of it runs without hardware, and two properties are unusually
strong:

- **Round-trip against ourselves.** `ts_mux` into the existing `ts::Demux` must
  return the access units that went in; `rtp_pack` into `rtp::parse` likewise.
  A property test over arbitrary frame sequences exercises both directions.
- **Both state machines, no network.** `source::session` can be driven directly
  against `session.rs` in-process, one output being the other input.

Above that: source against the Pi over Ethernet (the MS-MICE path, no radio);
source against the Pi over Wi-Fi Direct, as the spike did by hand; and in part
4, against real displays, where captured `wfd_video_formats` replies become
fixtures for `caps`.

## 10. Risks

- **HDCP.** A Samsung television advertises HDCP 2.0 capability. If any display
  *requires* content protection rather than offering it, we cannot satisfy it,
  and the honest outcome is an explicit refusal, not a black picture. Settled in
  part 4, with hardware in hand.
- **Encoder constraints.** `MfEncoder` must be held to the profile and level a
  display advertises. Whether Media Foundation honours every constraint we set
  is unverified.
- **Timing constants.** See section 6.

## 11. Done when

`castr-sender` streams desktop and audio to the Pi's Miracast sink over
Ethernet, negotiated M1-M7, sustained for ten minutes without a stall; the
round-trip and cross-machine tests pass in the workspace suite; and the four
failure stages this part owns — connect, negotiation, session, teardown — have
each been provoked deliberately and report themselves by name. The three radio
stages carry their names from here but are proved in part 2.

Teardown is verified by confirming the sink returns to idle and accepts a fresh
session immediately after one ends. Dropping the P2P group belongs to part 2,
where there is a group to drop.

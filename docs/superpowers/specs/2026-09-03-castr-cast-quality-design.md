# castr sub-project 5: cast quality — design (2026-09-03)

## 1. Goal

Two defects you can see while using castr, and one number nobody has measured:

- **The mouse cursor is missing from the cast.** You cannot point at anything,
  which makes demonstrating or walking someone through something impossible.
- **The picture freezes for about 150 ms, a few times a minute**, whenever a
  delta frame loses a fragment.
- **Lip sync has never been measured**, so we do not know whether it is a
  problem.

The first two are built. The third is measured, and only becomes a build if
the measurement is bad.

## 2. Findings that shape the design (read from the code, 2026-09-03)

- **Nothing captures the cursor.** `crates/castr-capture-win/src/dxgi.rs`
  calls `AcquireNextFrame` and copies the texture; it never reads
  `FrameInfo.PointerPosition` or calls `GetFramePointerShape`. Windows
  excludes the cursor from the duplicated texture by design, so it is simply
  absent. This is new work, not a bug fix.
- **Most of the repair machinery already exists.** The sender's
  `RetransmitBuffer::lookup` (`crates/castr-net/src/retransmit.rs`) will
  already resend a delta that is less than one frame interval old. The
  receiver is what refuses: `Reassembler::tick`
  (`crates/castr-proto/src/reassemble.rs:124`) guards its NACK generation
  with `else if p.keyframe`, so a delta is never asked for.
- **The 150 ms budget already exists and is already being spent.**
  `GAP_WAIT_US` in `crates/castr-media/src/jitter.rs` is 150 000 µs: when the
  jitter buffer sees a gap in frame numbers it waits that long before giving
  up on the GOP and requiring a keyframe. That wait is the freeze. A repair
  that lands inside it costs nothing, because the stall is already happening.
- **Both retention windows are 500 ms.** The sender's `RetransmitBuffer` and
  the receiver's `Reassembler` are both constructed with `500_000`, which
  comfortably contains a 150 ms repair window.
- **Round-trip time is already available on the receiver.**
  `Link::rtt()` (`crates/castr-net/src/transport.rs:203`) is already used at
  `crates/castr-receiver/src/pipeline.rs:922` to space NACKs.
- **The audio/video clock looks correct.** The sender stamps audio and video
  from one monotonic base (`audio_frame_timestamps` in
  `crates/castr-sender/src/cast.rs` re-anchors every drain to the wall clock),
  and the receiver anchors on `ts_us - audio.buffered_us()`
  (`pipeline.rs:550`), so the output queue is accounted for. No defect found
  by reading. Hence measurement rather than repair.

## 3. Decisions taken during design

- **The cursor is composited into the frame at the sender**, not sent
  separately and drawn by the receiver. It is always consistent with the
  picture, it needs no protocol change, and it costs nothing on the receiver.
  The price is that the cursor moves at video frame rate and carries the same
  latency as the picture, which is the right trade for a screen-sharing tool.
- **Compositing happens on the CPU**, into the BGRA buffer already copied out
  of the staging texture. A 32×32 cursor is ~1 000 pixels against ~2 000 000
  already being touched. A GPU path would need a shader, a second render
  target, and per-pixel boolean logic for the legacy mask formats — a great
  deal of machinery for a rounding error.
- **A lost delta is repaired only within the stall that already happens.** No
  latency is added in either mode. A repair that cannot arrive before
  `GAP_WAIT_US` expires is not requested at all.
- **The repair policy moves entirely to the receiver.** It is the only side
  that knows its playout deadline, its mode, and the round-trip time.
- **Lip sync is measured, not fixed.** A fix without a demonstrated defect
  would be guesswork.

## 4. The cursor

### 4.1 What Windows gives us, and why it is awkward

`AcquireNextFrame` fills a `DXGI_OUTDUPL_FRAME_INFO` carrying
`PointerPosition` (a point plus a `Visible` flag) and `PointerShapeBufferSize`.
`GetFramePointerShape` returns the image plus a
`DXGI_OUTDUPL_POINTER_SHAPE_INFO` with a hotspot and a type.

Two properties drive the design:

**A shape arrives only when it changes.** On most frames
`PointerShapeBufferSize` is zero. The last shape must be cached, or the cursor
disappears the moment it stops changing — which is most of the time.

**There are three pixel formats, and two are from the 1980s:**

| Type | Layout | Blend rule |
|---|---|---|
| `COLOR` | 32-bit BGRA, top-down | Ordinary alpha blend |
| `MONOCHROME` | 1 bit per pixel, AND mask stacked above XOR mask, buffer is double height | Per pixel: AND=0,XOR=0 → black; AND=0,XOR=1 → white; AND=1,XOR=0 → transparent; AND=1,XOR=1 → invert destination |
| `MASKED_COLOR` | 32-bit BGRA | Alpha is a boolean, not a blend: 0x00 → copy RGB; 0xFF → XOR RGB with destination |

The standard arrow and the text I-beam are `MONOCHROME`. Getting that table
wrong renders the text cursor as a black rectangle, so it is specified here
rather than left to the implementer.

### 4.2 Structure

A new file, `crates/castr-capture-win/src/cursor.rs`, split so that the
difficult part needs no hardware:

- **`CursorShape`** — owns `width`, `height`, `hotspot_x`, `hotspot_y`, `kind`
  and the raw bytes. Plain data, no Windows types.
- **`CursorCache`** — holds the last `CursorShape`, the last position, and
  visibility. `update(position, visible, shape: Option<CursorShape>)` records
  what changed; `current(&self) -> Option<(&CursorShape, i32, i32)>` returns
  what should be drawn.
- **`blend(shape: &CursorShape, x: i32, y: i32, dst: &mut [u8], width: u32,
  height: u32, stride: u32)`** — a pure function. All three formats, all
  clipping, no D3D, no `windows` crate types. This is where the bugs live and
  where the tests go.

`dxgi.rs` keeps only what must touch the API: read `FrameInfo`, call
`GetFramePointerShape` when a shape is offered, convert it into a
`CursorShape`, hand both to the cache, and call `blend` on the copied buffer
before returning the `RawFrame`.

### 4.3 Details that produce visible bugs if missed

- **The hotspot is subtracted from the position.** Draw at
  `(position.x - hotspot_x, position.y - hotspot_y)` so the arrow's tip lands
  where the user believes it is.
- **Clip, do not skip.** A cursor overhanging any edge draws its visible part.
  Skipping makes the cursor vanish near screen edges, which is exactly where
  people point at things.
- **Honour `Visible`.** Full-screen games and video players hide the cursor;
  drawing it anyway would be wrong and conspicuous.
- **A `MONOCHROME` shape's declared height is twice its real height.** The
  real cursor is `height / 2` rows; the second half is the XOR mask.

### 4.4 Miracast needs nothing

Windows composites its own cursor before the frames ever reach our sink, so
the Miracast path already shows one. This section changes castr's own protocol
path only.

## 5. Delta-frame repair

### 5.1 What happens today

A delta loses one fragment. The `Reassembler` holds the incomplete frame and,
because of the `p.keyframe` guard, never asks for the missing piece. The
jitter buffer sees a gap in the frame numbers, waits `GAP_WAIT_US` (150 ms),
gives up on the GOP, and requires a keyframe. The user sees a freeze followed
by a keyframe wait.

### 5.2 What happens instead

The `Reassembler` asks for the missing fragment immediately. The repair takes
about one round trip — 2–5 ms on a local network — and the frame completes far
inside the 150 ms the jitter buffer was going to wait anyway. Playback
continues with no visible interruption.

The hitch does not get shorter. It disappears, because the frame arrives long
before a deadline it was already going to miss.

### 5.3 The two changes

**Receiver — `Reassembler::tick`.** Replace the `else if p.keyframe` guard so
that any incomplete frame may be NACKed, subject to one new condition: a
repair must still be able to arrive in time.

```
now + rtt + DECODE_MARGIN < first_seen_us + GAP_WAIT_US
```

`DECODE_MARGIN` is 10 000 µs, covering the decode and present of the repaired
frame. Past that point the frame is doomed however fast the repair travels, so
asking for it wastes upstream bandwidth on a link that has just demonstrated
it is lossy. `Reassembler::tick` therefore needs two new arguments: the
round-trip time, and the repair deadline. `GAP_WAIT_US` is a *private*
constant in `castr-media`'s `jitter` module, and `castr-proto` does not depend
on `castr-media` at all, so rather than duplicating the number or adding a
dependency, the receiver — which already holds both crates — computes the
deadline and passes it in. That also keeps `Reassembler` free of any
assumption about the jitter buffer's policy.

**Sender — `RetransmitBuffer::lookup`.** Delete the rule that refuses a delta
older than one frame interval. Any NACK for a frame still held is honoured;
the existing 500 ms retention window is the bound. The receiver now owns the
policy, and the sender's independent guess would only ever contradict it — in
quality mode the receiver can still use a repair 100 ms later, which the
current sender rule refuses.

The receiver's existing NACK spacing (one per frame per RTT, at
`pipeline.rs:922`) remains the only throttle, which is the correct place for
it.

## 6. Lip sync: measurement

Not a build. One verification step, recorded whatever the outcome:

Play a marker on the PC that flashes the screen and clicks simultaneously.
Film the television at 60 fps and count frames between the flash appearing and
the click being audible. Repeat in both game and quality modes.

Judge against ITU-R BT.1359: audio leading video by more than **45 ms**, or
lagging by more than **125 ms**, is where viewers begin to notice. Inside that
band, record the number and close the question. Outside it, the measurement
becomes the evidence for a separate sub-project — this one does not grow to
absorb it.

## 7. Error handling

The cursor path fails soft in every case, because a missing cursor over a
correct desktop is always better than a corrupted frame:

| Failure | Behaviour |
|---|---|
| `GetFramePointerShape` fails | Keep the cached shape; log once, not per frame |
| Shape buffer larger than the cache | Grow the cache buffer |
| Unrecognised shape type | Draw nothing; log once |
| Cursor overhangs an edge | Clip to the visible region |
| No shape cached yet | Draw nothing |
| `Visible` is false | Draw nothing |

The repair path has one risk worth naming: a NACK storm on a very lossy link.
Two mechanisms prevent it. The existing per-frame RTT spacing throttles
requests, and the new deadline condition stops requests entirely once repairs
cannot arrive in time — so load sheds automatically at the moment the link is
worst.

## 8. Testing

**Pure, no hardware.** `blend` is a pure function over a byte buffer, so the
code most likely to be subtly wrong is the code that needs no hardware to
test:

- `COLOR`: alpha blending over a known background, including fully
  transparent and fully opaque pixels.
- `MONOCHROME`: a hand-built mask asserting all four AND/XOR combinations
  pixel by pixel, and that the source height is halved.
- `MASKED_COLOR`: alpha as a boolean — 0x00 copies, 0xFF inverts.
- Clipping: one test per edge, plus a cursor larger than the screen.
- The hotspot offset places the tip at the reported position.
- An invisible cursor draws nothing; an empty cache draws nothing.

**Repair, no hardware.** Three tests that fail against today's code:

- A delta missing one fragment produces a NACK.
- A delta whose deadline has passed produces none.
- `RetransmitBuffer::lookup` returns fragments for a delta NACK it currently
  refuses.

Then one end-to-end test: drop a fragment of a delta, prove the repaired frame
reaches the decoder in order, and prove no keyframe was requested — the direct
inverse of the behaviour being removed.

**Hardware.** One `#[ignore]`d capture test asserting a real frame contains a
cursor, following the existing capture tests' pattern. Then a cast with
deliberate packet loss, counting visible hitches before and against after; the
lip-sync measurement of section 6; and the encoder bitrate difference with a
moving cursor versus a still one, so the cursor's cost is measured rather than
assumed.

## 9. Out of scope

Sending the cursor position separately for sub-frame smoothness — rejected in
favour of compositing. Keeping the cursor region at higher encode quality.
Repairing a delta by any means other than retransmission (no FEC). Any change
to the Miracast path. Fixing lip sync, unless section 6's measurement shows a
problem. Cursor capture on Linux, which has no sender.

## 10. Dependencies

None. No new crates. `GetFramePointerShape` and the pointer-shape structures
are already present in the `windows` crate version the project uses.

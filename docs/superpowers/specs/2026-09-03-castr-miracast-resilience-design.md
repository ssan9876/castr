# castr sub-project 4: Miracast resilience — design (2026-09-03)

## 1. Goal

A Miracast cast to the Pi should survive the things that end it today: a
Bluetooth blip on the sending PC, a few seconds of radio trouble, a link that
degrades rather than fails. When a session does end, the PC should be able to
walk straight back in — same group, same credentials, no PIN, no rediscovery.

The target is not "never drops". It is **a drop becomes a few seconds you do
not act on, instead of "disconnected, start over"**.

Everything here is sink-side. Four of the five mechanisms need nothing on the
PC at all, which keeps the promise the sink was built for: cast from Windows
with nothing installed.

## 2. Why the current sink drops sessions

Read from the implementation merged at `820ccab`:

- **The group is destroyed on every session end.** `sink::one_group` treats one
  pass as one group *and* one session. When the RTSP connection closes, the
  function returns, `GroupGuard` sends `P2P_GROUP_REMOVE`, and the next pass
  builds a new group from scratch. A PC that dropped for two seconds has to
  rediscover a device that no longer exists.
- **Detection takes a minute.** `KEEPALIVE_EVERY` is 30 s and
  `KEEPALIVE_TIMEOUT` is 60 s. A radio that died two seconds ago is noticed up
  to a minute later, and the TCP connection can sit there looking healthy for
  the whole time.
- **Nothing consumes the loss counters.** `rtp::Reorder::lost()` and
  `ts::DemuxStats::continuity_errors` exist and feed an IDR request, but no
  code ever asks the source to send less. A congested link stays congested
  until it collapses.
- **The group owner may be throwing the client off.** As GO we are the access
  point, and the supplicant's AP behaviour includes deauthenticating a client
  whose acknowledgements fail — which is exactly what a shared Wi-Fi/Bluetooth
  antenna looks like from our side.

## 3. Decisions taken during design

- **Degrade hard and fast, recover slowly.** One bad second drops the bitrate
  request to the floor; ten clean seconds buy one step back up.
- **Hold the screen for 30 seconds, hold the group indefinitely.** A blip
  resumes on the same screen with no interruption. After 30 s the display is
  released so another protocol can use it, but the group and its credentials
  stay up, so the PC can still return instantly.
- **Sink-side only.** No changes to the Windows machine are in scope. The
  PC-side adapter tuning is the health check's territory (sub-project 3, part
  one) and belongs to a later sub-project.
- **Prove the uncertain levers on hardware before designing around them.**
  Two things are unverified: whether `brcmfmac` honours the GO knobs in P2P
  mode, and whether Windows honours `microsoft_max_bitrate`. Both get proven
  early; the fallbacks are named in section 6.

## 4. Radio configuration

`scripts/pi/wpa_supplicant-p2p.conf` gains three settings. All three were
confirmed present in the config parser of the supplicant on the test Pi
(`wpasupplicant 2:2.10-24`, aarch64) by checking the binary's parser strings.

| Setting | Value | What it does |
|---|---|---|
| `disassoc_low_ack` | `0` | Stops the group owner deauthenticating a client whose acknowledgements are failing. This is the Bluetooth-blip case: acks stop for under a second and today we throw the PC off. |
| `ap_max_inactivity` | `300` | How long a quiet client is tolerated before the AP considers it gone. Generous, because a paused cast is not a departed peer. |
| `p2p_go_ctwindow` | `0` | Stops the group owner going quiet on a power-save schedule. The Pi is mains-powered; there is nothing to save and a client that misses the window sees a stalled link. |

`beacon_int` and `dtim_period` are left at their defaults. They are plausible
tuning targets and there is no measurement justifying a change, so changing
them would be guessing.

**Risk:** a driver may parse a setting and ignore it. Section 8 makes
confirming each one a verification step rather than an assumption.

## 5. Session and group lifecycle

The structural change. Today one pass through `sink::one_group` is a group and
a session together. They separate into a state machine:

```
                  group created
                        |
                        v
   +------------> Advertising <-------------- hold expires (30 s)
   |            (group up, credentials       (release the display)
   |             valid, listening)                    ^
   |                    |                             |
   |            RTSP connection,                      |
   |            display acquired                      |
   |                    v                             |
   |               Streaming ----------------> Holding
   |            (session running)   peer lost  (group up, screen held,
   |                    ^                       overlay shown)
   |                    |                             |
   |                    +--- same peer returns -------+
   |                         (no PIN, no rediscovery)
   |
   +-- radio error only: tear the group down and rebuild
```

The group's lifetime becomes the service's lifetime rather than the session's.
`P2P_GROUP_REMOVE` is sent only when the radio itself errors or the sink stops.

**Holding** keeps `Owner::Miracast` on the display arbiter, so a castr sender
arriving during those thirty seconds still receives "display busy". That is a
real cost of holding the screen, and it is bounded at thirty seconds.

The screen during Holding keeps its last frame under a "Reconnecting…" overlay,
through the existing `UiEvent::Overlay` path. Saying what is happening beats
freezing silently or blanking.

**Returning** is the same peer's MAC re-associating and opening an RTSP
connection inside the hold window. A different peer connecting during Holding
is treated as a new session: the hold is for the PC that was casting, not for
the first PC to arrive.

## 6. Detecting that the peer is gone

Three signals, because each catches cases the others miss:

1. **RTSP keep-alive**, tightened from 30 s/60 s to **5 s/10 s**. Catches a
   peer that is still associated but whose session is dead.
2. **RTP silence**, new: a streaming session that sees no datagram for **2 s**.
   This is the signal that matters most, because the radio can die while the
   TCP connection sits there looking healthy for minutes.
3. **`AP-STA-DISCONNECTED`** from the supplicant, the explicit case.

Any of the three moves Streaming to Holding.

The keep-alive tightening cuts both ways and the numbers are chosen with that
in mind: 5 s between keep-alives is frequent enough to detect quickly and far
too infrequent to matter for bandwidth, and a 10 s timeout is long enough that
a single lost keep-alive on a busy link does not end a healthy session.

## 7. Bitrate steering

A new pure module, `crates/castr-miracast/src/quality.rs`: loss numbers in, a
bitrate ceiling out. No sockets, so its tests run in the Windows workspace
suite like every other pure layer in that crate.

**Input:** once a second, the change in `Reorder::lost()` plus the change in
`DemuxStats::continuity_errors`.

**Ladder:** 8000 → 4000 → 2000 kbps. Three rungs. More rungs would be noise at
720p30 on a 2.4 GHz radio.

**Falling** is instant: any second whose loss exceeds the threshold requests the
floor immediately. **Rising** requires ten consecutive clean seconds per step.
The asymmetry is the design: it is what stops the request oscillating on a link
that is marginal rather than broken. At most one request per second.

**Threshold:** a second counts as bad at 5 or more lost packets or continuity
errors. At 720p30 a frame is roughly 24 datagrams, so 5 is well above the
single-packet noise floor and well below a visibly damaged second.

**The message** is a `SET_PARAMETER` carrying `microsoft_max_bitrate`, which we
already advertise support for in our capabilities.

**Fallback if Windows ignores it:** a format change to a smaller CEA mode, which
`microsoft_format_change_support` covers. That trades resolution rather than
bitrate — a blunter instrument — and is built only if the hardware test shows
the bitrate request is ignored. Designing both up front would be building
something we may not need.

## 8. Testing

**Pure, no hardware.** The lifecycle becomes a state machine with events in and
actions out, the same shape as the existing `rtsp::Negotiation`, so all four
states and every transition are tested on Windows with no radio: a blip
returns to Streaming without a PIN, an expired hold releases the display, a
different peer during Holding starts a new session, a radio error rebuilds the
group. The bitrate ladder gets its own tests: a loss spike drops to the floor
in one second, ten clean seconds climb one step, an alternating good/bad signal
does not oscillate.

**On the Pi, no radio peer.** The loopback source
(`crates/castr-miracast/examples/loopback-source.rs`) gains two flags: drop a
percentage of datagrams, and vanish mid-session. That makes Holding, the
resume, and the bitrate ladder provable against the running service with no
Miracast peer at all.

**On the hardware, with the Windows machine.** Three things cannot be proven
any other way, and each is a named verification step:

1. Does `brcmfmac` honour `disassoc_low_ack`, `ap_max_inactivity` and
   `p2p_go_ctwindow` in GO mode? Confirmed by reading back the group's
   behaviour, not by the settings being accepted.
2. Does Windows honour `microsoft_max_bitrate`? Measured as a drop in received
   bitrate after the request.
3. **Does a real Bluetooth blip now survive?** Cast, play audio over Bluetooth
   headphones, and confirm the session holds where it previously dropped. This
   is the measurement the whole sub-project exists to make.

## 9. Out of scope

PC-side adapter tuning, which needs software on the PC and belongs with the
health check. Miracast over Infrastructure. Any change to the castr protocol's
own path — this is Miracast only. Making Windows reconnect automatically, which
has no supported API. Anything that helps when casting to a third-party dongle:
we are not a party to that conversation.

## 10. Dependencies

None beyond what the sink already has. No new crates. The supplicant settings
are configuration, and the two code changes live in crates that already exist.

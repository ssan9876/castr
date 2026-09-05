# Stopping and observing a Miracast cast — end-to-end verification (2026-09-04)

Hardware: Windows PC `DESKTOP-C6QHH2A` (NVIDIA H.264 encoder MFT, Realtek
8821CE) casting to the Pi 3 B sink over Wi-Fi Direct. Also in range and not
connected to: a Samsung 75" Crystal UHD, an Amazon Fire TV Stick, an Epson
printer, and — new since the radio work — an LG webOS TV UN7000PUB.

The goal was `miracast-stop` and `miracast-status` reaching a cast running in
another process, and Ctrl-C ending a cast the way `--duration` already did.

## Results

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 1 | A running cast can be observed from another shell | PASS | Full field set at 17:11:06, seven seconds in; see the readout below |
| 2 | Status reports the negotiated mode, not a guess | PASS | `mode 1280x720@30` against the Pi's `playing 1280x720@30` |
| 3 | Status reports throughput actually sent | PASS | `mbps 10.5` then `10.2`, moving between samples; `bytes 8825560` after 7 s |
| 4 | `keepalive_age_s` reflects the display answering | PASS | `2` while playing, against a 5 s keep-alive interval |
| 5 | `miracast-stop` ends a running cast | PASS | Stopped at 17:11:17, `elapsed_s 17` of a `--duration 90` — ended by request, not by the timer |
| 6 | Stopping tears the session down properly | PASS | `teardown: stopped by request`, then `releasing the group with "DietPi"` |
| 7 | The sink sees a clean ending and returns to idle | PASS | `session ended: source closed the connection, holding the group`, `ClientDisconnected`, then `PIN 32580945 is on the screen` and `WPS_PIN` re-armed |
| 8 | The record is removed when the cast ends | PASS | `miracast-status` reported no cast running immediately after; the file is gone from `%APPDATA%\castr\sender\` |
| 9 | **A second cast works immediately afterwards** | PASS | Stopped 17:11:17, second cast playing 17:12:42. Nothing was left behind |
| 10 | A cast is not disturbed by being observed | PASS | Every `perf:` window across both casts reports `dropped 0` |
| 11 | A stale record is cleaned up rather than blocking | PASS | A record naming port 1 was planted; `miracast-status` reported `cleaned up a stale record from a cast started 1757001234` and removed it |
| 12 | Nothing running is reported as such | PASS | `no Miracast cast is running`, exit 0, from both commands |
| 13 | A forged token cannot stop a cast | PASS | Unit-tested (`a_forged_token_is_refused_and_stops_nothing`); not provoked against a live cast |
| 14 | Ctrl-C ends a cast the way `miracast-stop` does | NOT RUN | See below |
| 15 | A second `miracast-cast` is refused while one runs | NOT RUN | Unit-tested via `client::running`; not provoked on hardware |
| 16 | A cast survives its control channel failing to bind | NOT RUN | Cannot be staged without occupying an ephemeral port we do not choose |

## The readout

Seven seconds into a cast to the Pi:

```
display          DietPi
address          192.168.173.1:7236
mode             1280x720@30
ceiling_mbps     10
mbps             10.5
video_units      202
audio_units      718
datagrams        6979
bytes            8825560
repeated_frames  0
elapsed_s        7
keepalive_age_s  2
```

## What the readout found on its first run

**We send more than the display said it could carry.** The encoder is held to
the display's advertised ceiling — `encoding 1280x720p30 at 8000 kbps` — but
the measured wire rate is 10.2 to 10.5 Mbps against a sink advertising 10.

The arithmetic accounts for it exactly. Wi-Fi Display audio is uncompressed
LPCM at 48 kHz stereo 16-bit, which is 1.536 Mbps and is not in the video
budget at all, and MPEG-TS, RTP, UDP and IP framing sit on top of both:

```
8.0 video + 1.54 audio + framing  ~=  10.4 Mbps on the wire
```

So the bitrate policy caps *video* against a ceiling that has to carry video
**plus** audio **plus** container. Against our own Pi it is harmless — every
`perf:` window reports `dropped 0` — because the Pi advertises 10 Mbps as a
statement about its radio rather than a limit it polices.

A display that enforces its advertised ceiling is a different matter, and this
is a plausible way to be refused by one. Not changed here: choosing a new
budget without evidence from a real display would be guesswork, and part 4 is
where that evidence comes from. Recorded so the question is already framed when
a television is available.

Worth noting how it surfaced. Nothing was looking for this; it fell out of
having a number for what is actually written to the socket, which did not exist
before today.

## Ctrl-C

Not proven, and deliberately not claimed.

`kill -INT` from Git Bash does not deliver a console control event to a native
Windows process: the cast ignored it and ran to its full 120 s, ending with
`teardown: the requested duration elapsed`. That is a fact about the test
harness and says nothing about the handler.

What *is* proven is the path Ctrl-C shares with `miracast-stop`: the same
`Command::Stop`, the same channel, the same exit from the loop, the same
teardown and group release, all exercised in rows 5 to 9. What remains untested
is only whether the `tokio::signal::ctrl_c` task fires and reaches that channel.

## What is tested and where

175 tests in `castr-miracast` and 90 in `castr-sender` pass, including 28 new
ones across `control::record`, `control::wire`, `control::stats` and
`control::server`. `cargo clippy --workspace --tests` introduces no new warning;
the four that remain pre-date this branch, confirmed by running clippy against
a stash of the working tree.

**The Linux leg does not cover this code.** `scripts/pi/test-linux.sh` exits 0,
but it builds `castr-codec-v4l2` and `castr-miracast` only — `castr-sender` is
not in the Docker image, so none of the control tests run there. The design
claimed otherwise and has been corrected. The code is portable (`std::net`, no
platform call), but portable is not exercised.

## A test that was wrong

`throughput_is_measured_over_the_window` asserted 8 Mbps for 1 MB spread over
5 s. 1 MB is 8 Mbit, and 8 Mbit over 5 s is 1.6 Mbps. The expectation was
wrong, not the code; the test now sends 1 MB a second and expects 8 Mbps.

The same shape as the mode-negotiation test that failed yesterday: when a
freshly written test disagrees with freshly written code, the test is a
coin-flip, not an oracle. Doing the arithmetic on paper first would have caught
both.

## Not yet answered

- Interoperability, still. Every cast here was to our own sink.
- Whether a display that enforces its bandwidth ceiling refuses us, which the
  overhead finding above makes a live question rather than a theoretical one.
- Whether the control channel behaves when a cast is wedged rather than
  running. `miracast-stop` has a two-second connect timeout and the loop drains
  commands once per 200 ms tick, so a loop blocked elsewhere would accept the
  connection and never act on it. Untested.

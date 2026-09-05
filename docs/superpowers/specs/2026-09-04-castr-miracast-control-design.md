# castr sub-project 9: stopping and observing a Miracast cast — design (2026-09-04)

Part 3a of the Miracast source project. Parts 1 (the media path) and 2 (the
Wi-Fi Direct radio) are merged, along with picture-mode negotiation. The
sender's GUI is part 3b and interop hardening against real displays is part 4.

## 1. Goal

Give a running Miracast cast a way to be stopped and a way to be observed.

Today `castr-sender miracast-cast` ends only when `--duration` expires. There
is no way to stop it deliberately and no way to see whether anything is
reaching the display. Both gaps are worth closing on their own, and the GUI in
part 3b cannot be built without them: a Stop button and a throughput readout
are the two things a cast window is for.

Success: a cast can be stopped from another shell and from Ctrl-C, both leaving
the display idle and ready for the next session, and `miracast-status` reports
what is being sent while a cast is running.

## 2. A defect this work fixes

`miracast-cast` installs no Ctrl-C handler. The `cast` subcommand does — it
spawns a `tokio::signal::ctrl_c` task that sends `CastCommand::Stop` — but the
Miracast path was written as a synchronous loop and never grew the equivalent.

So Ctrl-C terminates the process before `TEARDOWN` is written and before the
Wi-Fi Direct group is released. The display is left believing a session is
live, which is the failure mode the part 1 design (§8) records the project as
having already paid for twice. The teardown code is correct and unreachable on
the path a person is most likely to take.

This is a correctness fix, not only a feature, and it is the reason the work is
worth doing before the GUI rather than as part of it.

## 3. What is being added

Two commands, and a control channel for them to speak over:

```
castr-sender miracast-status      # what the running cast is sending
castr-sender miracast-stop        # end it cleanly
```

Both address the cast running on this machine, in another process. Neither
takes a target: only one Miracast cast runs at a time (§7).

## 4. Structure

A new `castr-sender/src/control/` module. The split is the one this project
uses everywhere and the reason the source could be built in a day: every
decision in pure code, every socket in a thin shell.

| Unit | Pure | Responsibility |
|---|---|---|
| `control/record.rs` | yes | The state file — serialize, parse, and decide staleness |
| `control/wire.rs` | yes | The request and response protocol, both directions |
| `control/stats.rs` | yes | Accumulate what was sent; produce a snapshot |
| `control/server.rs` | no | Bind, write the record, accept, remove the record on drop |
| `control/client.rs` | no | Read the record, connect, print |

Three quarters of this is testable without a socket, a radio or a display, and
the remaining quarter is testable on Linux as well as Windows, which matters:
the workspace suite runs in the Docker image and the control channel should not
be another thing only provable by hand on hardware.

## 5. The control channel

The cast binds `127.0.0.1:0` and writes a record to
`<config>/sender/miracast-cast.toml`. TOML, matching `paired.toml` beside it;
`serde_json` is not a workspace dependency and nothing here needs it to become
one.

```toml
pid = 12345
port = 54321
token = "8f3a1c..."           # 128 bits, hex
display = "DietPi"
address = "192.168.173.1:7236"
started = 1757001234          # unix seconds
```

The port is ephemeral and recorded rather than fixed, so two casts cannot
collide over a well-known number and nothing needs a port reserved for it.

**The token is the access control.** A loopback socket is reachable by any
process on the machine, so a request without the token in the record is
refused. The record sits in the user's own configuration directory, which is
where the identity key already lives; a process that can read it can already do
worse than stop a cast.

The protocol is a single line in each direction, text, no framing beyond the
newline:

| Request | Response |
|---|---|
| `STOP <token>` | `OK stopping` |
| `STATUS <token>` | `OK <key>=<value>⇥<key>=<value>⇥...` |
| anything, wrong token | `ERR unauthorised` |
| anything unparseable | `ERR bad request` |

Text rather than a serialization format because the codebase already reads
line-oriented text well, the payload is a dozen scalars, and a protocol a
person can type into `nc` while debugging a cast is worth more here than one
that is convenient to derive.

Status fields are separated by a **tab**, not a space. Found while
implementing: a display name legitimately contains spaces — the television in
range is named `75" Crystal UHD` — so space-separated fields let a name swallow
the one after it. A value that can forge the next field is a parser bug waiting
to be written, and it has a test.

`castr-sender` gains `toml`, `serde` and `rand` from the existing workspace
dependency set — all three are already built for other crates, so this adds
nothing to the tree. The five-second window in §9 matches the cadence of the
receiver's `perf:` line, so the two readouts can be compared directly when a
cast is being watched from both ends.

## 6. Lifecycle, and what a crash leaves behind

The record is RAII, exactly as `Connection` owns the Wi-Fi Direct group in part
2: written when the cast begins serving, removed on every path out. That is
deliberate and for the same reason — state outliving the thing that created it
is a failure this project has paid for repeatedly.

A crash or a kill still leaves the file. **Connection-refused is the
discriminator**, not the recorded pid: a pid can be reused by an unrelated
process, and a check that says "something with that pid exists" is not evidence
that a cast is running. So a client that cannot connect to the recorded port
declares the record stale, removes it, and reports that no cast is running and
that it cleaned up a stale record from whenever it was started.

The pid is recorded anyway, because it costs nothing and it is the first thing
anyone wants when a cast will not die.

## 7. One cast at a time

`miracast-cast` reads the record before doing anything else. A live one is
refused, naming the display already being cast to and the command that stops
it. A stale one is removed and the new cast proceeds.

This matches the hardware. There is one Wi-Fi Direct radio, one group, and one
encoder on the monitor; two casts at once is not a thing the machine can
usefully do. It also keeps `stop` and `status` unambiguous — neither needs a
target argument, and neither can act on the wrong session.

## 8. Stopping

Three things stop a cast, and all three take the same path:

| Trigger | Route |
|---|---|
| `miracast-stop` | listener thread → command channel → the cast loop |
| Ctrl-C | `tokio::signal::ctrl_c` on the runtime `main` already builds → the same channel |
| `--duration` | unchanged, the existing elapsed check |

All three break the loop where `--duration` breaks it today, so `TEARDOWN` is
sent by the existing code and the group is dropped by the existing
`drop(connection)` in `main.rs`. No second teardown path is introduced, because
a second teardown path is a second thing that can be wrong.

A second Ctrl-C aborts the process outright. Teardown writes to a display that
may have just been switched off, and a person pressing Ctrl-C twice has said
what they want.

## 9. What status reports, and what it cannot

Wi-Fi Display is source-authoritative. The source stamps the clock and the sink
slaves to it; there is no back-channel of receiver statistics, which is the
inversion the part 1 design records in §6. castr's own protocol has receiver
stats driving adaptive bitrate. This path has none and never will.

So every number here is **sent-side**, and the documentation says so rather
than leaving a reader to assume otherwise:

| Field | Meaning |
|---|---|
| `display`, `address` | what is being cast to |
| `mode` | the negotiated picture, `1280x720@30` |
| `ceiling_mbps` | what the display advertised it can carry |
| `mbps` | what was actually written to the socket, over the last 5 s |
| `video_units`, `audio_units` | access units and audio frames muxed |
| `datagrams`, `bytes` | RTP as sent |
| `repeated_frames` | frames re-sent because the desktop did not change |
| `elapsed_s` | since the session started playing |
| `keepalive_age_s` | since the display last answered `GET_PARAMETER` |

`keepalive_age_s` is the only field that says anything about the far end. The
source already sends a keepalive every 5 s and gives up at 10 s
(`source/session.rs`), so this is a signal that exists rather than one invented
for the readout.

There is deliberately no `rtt` and no `loss`. `CastStatus` carries both for
castr's own protocol, and copying the struct across would ship two fields that
are permanently meaningless on this path and invite exactly the wrong reading.

`repeated_frames` is worth its place because a still desktop is the normal
case: Desktop Duplication yields nothing when nothing changes, and the source
repeats the last frame so the display does not read silence as a dead source. A
cast that is working on a still screen and a cast that has stopped capturing
look identical in `mbps` alone.

## 10. Failure

| Situation | Reported as |
|---|---|
| No record | `no Miracast cast is running` |
| Record, nothing listening | the same, plus that a stale record from `<time>` was cleaned up |
| Token refused | `ERR unauthorised` — the record was replaced under us; try again |
| `miracast-cast` while one runs | the display already being cast to, and `miracast-stop` |
| The listener cannot bind | logged; **the cast starts anyway** |

That last row is the rule this design will not bend: **a cast must never die
because its control channel did.** The control channel is an accessory to the
thing that matters. If the socket cannot bind or the listener thread dies, the
cast runs, `stop` stops working, Ctrl-C still works, and the log says why.

## 11. Testing

Pure, and tested normally: record round-trip, a record with a missing or
malformed field, the staleness decision, every request form including a wrong
token and a truncated line, and the statistics accumulator including the
derived rate over a window.

Impure, and tested in-process on any platform — so it runs in the Linux Docker
image, not only on the machine with the radio: a server bound to loopback, a
client that connects and sends `STOP`, and the assertion that the command
arrives on the channel; a `STATUS` that returns the published snapshot; a wrong
token refused; a record left behind by a server that is gone, cleaned up.

Then hardware, which is the only thing that proves it. Against the Pi:

1. Cast, and from a second shell see `miracast-status` report a mode, a
   non-zero `mbps` and a `keepalive_age_s` under 5.
2. `miracast-stop` ends it; the sink logs the teardown and returns to
   advertising.
3. **A second cast immediately afterwards succeeds.** This is the one that
   matters — a group left behind or a display still believing a session is live
   shows here and nowhere else.
4. Ctrl-C on a cast produces the same clean ending as `miracast-stop`, which is
   the defect in §2 and must be demonstrated, not assumed.
5. `miracast-cast` while one is running is refused by name.
6. A cast killed outright leaves a record; the next `miracast-status` reports
   no cast running and cleans it up.

## 12. Out of scope

The GUI (part 3b, which consumes this). Interop quirks against real displays
(part 4). Pausing or resuming a cast, changing mode mid-cast, or casting to
more than one display at once — none of these are asked for by anything, and
`SetMode` already exists unused on the other path as a reminder of what
speculative control commands cost.

Stopping a cast on another machine. This is loopback only, deliberately.

## 13. Done when

`miracast-status` reports a live cast's mode and throughput from another shell;
`miracast-stop` ends it and the Pi returns to advertising; Ctrl-C does the same;
a second cast succeeds immediately after each; a second concurrent
`miracast-cast` is refused by name; and a record left by a killed cast is
cleaned up rather than blocking the next one.

Each of these is provable against our own sink, which is the point of doing
this part before part 4 — it is the last piece of the Miracast source that can
be finished without a television.

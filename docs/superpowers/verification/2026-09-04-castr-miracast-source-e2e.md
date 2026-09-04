# Miracast source and sink pairing — end-to-end verification (2026-09-04)

Hardware: Windows PC `DESKTOP-C6QHH2A` (Realtek 8821CE, Wi-Fi idle, LAN on
Ethernet) casting to the Pi 3 B sink at `192.168.88.157` over Wi-Fi Direct.

Two things were verified: that the Miracast **sink** can now be paired with at
all, which it could not before today, and that castr can act as a Miracast
**source**, which it could not before today either.

## Results

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 1 | A Wi-Fi Direct peer's Wi-Fi Display element is readable before connecting | PASS | Samsung `00 0006 0111 1c44 0036`: primary sink, RTSP port 7236, 54 Mbps, HDCP-capable. The Epson printer and an idle Fire TV publish none, so a display is distinguishable from a peer that is not one |
| 2 | Pairing completes programmatically, with nobody at the keyboard | PASS | `pair status: DevicePairingResultStatus(0)` from unprivileged Rust using the `windows` crate |
| 3 | The sink issues a DHCP lease to a Windows source | PASS | `local=192.168.173.2 remote=192.168.173.1` |
| 4 | The sink answers RTSP M1 | PASS | `RTSP/1.0 200 OK` with the full `Public` list |
| 5 | Pairing works long after the service started | PASS | Was the core defect; see "What was wrong" below |
| 6 | The sink can be paired with more than once per start | PASS | Two consecutive fresh pairings, `DevicePairingResultStatus(0)` both times, with a fresh PIN minted between them |
| 7 | Our source negotiates M1-M7 with the sink | PASS | Sink logged `playing 1280x720@30` |
| 8 | Video reaches the sink and decodes in hardware | PASS | `v4l2 decoder: 1280x720 visible in 1280x720 coded`, then ~150 pictures per 5 s window |
| 9 | A cast is stable | PASS | See row 12 |
| 10 | A second cast works without restarting anything | PASS | Two sessions at 19:24 and 19:26, the second after the first ended |
| 11 | The connect stage names itself on failure | PASS | `connect: 192.168.88.157:7777 did not answer within 10s`, with the OS cause beneath |
| 12 | A cast is stable for ten minutes | PASS | `playing 1280x720@30` at 19:30:46, `session ended: source closed the connection` at 19:40:46 — 600 s exactly, ended by the duration and not by a fault. 120 five-second performance windows, **none** reporting a dropped frame, ~150 pictures each |
| 13 | Audio arrives and is audible | INCONCLUSIVE | Nothing in the path reported an error, but audio was never listened to. Needs a person |
| 14 | The negotiation stage names itself on failure | PARTIAL | Unit-tested (`no_common_format_tears_down_rather_than_streaming_blindly`), never provoked against hardware |
| 15 | The session stage notices a display that stops answering | NOT RUN | Keep-alive timeout is unit-tested only |
| 16 | Any third-party display works | NOT RUN | Needs the television or the dongle; nobody was home |

## What was wrong with pairing

Pairing had never once succeeded by hand. Five defects were fixed yesterday and
it still failed, so the remaining cause was found by measurement today.

**The WPS registrar's window expires.** `WPS_PIN` opens a registration window of
about two minutes. The sink armed it once, when the group came up, and never
again. So pairing worked for the first two minutes after the service started
and was impossible ever after — which is every attempt anyone had made.

The failure gave nothing away, which is why it survived so long. The PIN stayed
on the television and the sink stayed discoverable, so a source would offer to
connect, spend thirty seconds finding no network it could enrol with, and give
up. Nothing arrived at the sink to explain it, because nothing reached the sink
at all.

Measured, minutes apart:

| Fresh pairing attempt | PIN age | Result |
|---|---|---|
| 19:12 | 57 min | FAIL |
| after re-arming the registrar | 8 s | PASS, through to RTSP |

**And a joined sink never offered a PIN again.** A station joining clears the
PIN, which is right — nobody should be typing one while a cast is up — but
nothing put a new one back. The sink could therefore pair exactly once per
start. Afterwards it stayed discoverable, showed nothing, and refused every
enrolment; the log shows `WPS-ENROLLEE-SEEN` from the same PC repeatedly with
no registrar to answer it.

Both are fixed, and both were verified from the sink's own log:

```
19:19:07  ClientDisconnected
19:19:07  PIN 11217756 is on the screen     <- minted 200 ms later
19:19:07  WPS_PIN any 11217756
19:19:53  WPS_PIN any 11217756              <- re-armed at 46 s
```

## A hypothesis that was wrong

The first reading of the evidence was that the group stopped beaconing with
age: Windows reported `The specific network is not available. RSSI: 255`, and a
scan at the time showed no `DIRECT-yN` while showing every neighbour.

A 75-minute poll refuted it. The group stayed visible in 67 of 75 samples, and
all eight absences line up exactly with service restarts from deploys. A
connection at 56 minutes — past the age at which it had failed — succeeded.

The real discriminator was not age but whether the attempt needed WPS: every
failure was a fresh pairing, every success an already-paired join. Recorded
because the wrong hypothesis was plausible, cost an hour, and was only killed
by measuring it.

## Note on diagnostics

None of this was visible until the event parser stopped discarding what it did
not recognise. The sink logged *nothing at all* through two failed connections.
`WPS-ENROLLEE-SEEN`, `CTRL-EVENT-EAP-STARTED`, `WPS-REG-SUCCESS` and
`EAPOL-4WAY-HS-COMPLETED` are all now in the log, and the second defect was
found directly by reading them.

The cost is noise: about five events a minute on an idle sink, mostly
`RX-PROBE-REQUEST` from neighbours' printers and phones. Worth tuning if it
proves annoying, but not at the price of going blind again.

## Not yet answered

- Whether any real display interoperates. Everything above is castr talking to
  castr, which proves consistency, not correctness.
- Whether the Wi-Fi Display LPCM header exists as implemented. The sink now
  strips it only when the declared length matches, so both readings behave
  correctly, but the specification has not been read.
- Lip sync, still unmeasured, still needing a person.

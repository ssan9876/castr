# Interop with a third-party display — verification (2026-09-05)

Part 4. **castr cast a picture to Miracast hardware it did not write.**

Hardware: an **MR-A202 wireless display adapter**, with a Samsung 75" Crystal
UHD, an LG webOS TV UN7000PUB, a TCL 55" Roku TV and an Amazon Fire TV Stick
also in range and not connected to.

An adapter was chosen as the first interop target deliberately: it advertises
continuously rather than only while a mirroring page is open, so it can be
tested repeatedly without anyone standing in front of a television. That
decision paid for itself many times over — the four defects below took roughly
twenty sessions to find.

## Results

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 1 | A third-party display is found by name | PASS | `miracast-cast MR-A202` |
| 2 | Its RTSP port is read from its element, not assumed | PASS | It advertises **554**; every other device here says 7236 |
| 3 | How it pairs is read from its element | PASS | `it offers display, push-button` (`0x2288`) |
| 4 | castr pairs with it unattended | PASS | `pairing with "MR-A202" by PushButton` — nobody touched anything |
| 5 | castr pairs with it by PIN | PASS | PIN `74380022`, read from its screen, accepted |
| 6 | A Wi-Fi Direct group forms | PASS | `"MR-A202" is up at 192.168.157.1` |
| 7 | DHCP issues an address | PASS | `endpoint local=192.168.157.100 remote=192.168.157.1` |
| 8 | An RTSP control connection is established | PASS | `192.168.157.1:57100 connected to us` |
| 9 | M1-M7 complete | PASS | Full exchange logged; `playing 1920x1080p30` |
| 10 | A mode is negotiated from its real capabilities | PASS | It offered 1080p and 720p sets; Quality took **1920x1080@30** |
| 11 | Media reaches it | PASS | 13.3 Mbps, 11358 datagrams, 14.5 MB in 12 s |
| 12 | Keyframe requests are honoured | PASS | `the display asked for a keyframe`, forwarded to the encoder |
| 13 | It stays alive through the session | PASS | `keepalive_age_s 4` throughout |
| 14 | **A picture appears on it** | PASS | Confirmed visually |
| 15 | Teardown is clean | PASS | `teardown: the requested duration elapsed`, `releasing the group` |
| 16 | The Pi still works after these changes | **NOT RUN** | The Pi was off the network all session |
| 17 | Audio is audible on it | NOT RUN | Never listened to |
| 18 | Lip sync | NOT RUN | Unmeasured, as everywhere else |
| 19 | HDCP refusal is clean | NOT RUN | It advertises HDCP 2.1 but did not demand it |

## What the displays in range offer

Read from each device's WPS element (attribute `0x1008`), which says how it is
willing to be paired. These are the fixtures the ceremony tests use — real
bytes from four vendors:

| Device | Bits | Offers |
|---|---|---|
| MR-A202 adapter | `0x2288` | display, push-button |
| Samsung 75" Crystal UHD | `0x4388` | display, push-button, keypad |
| LG webOS TV UN7000PUB | `0x3388` | display, push-button, keypad |
| Amazon Fire TV Stick | `0x4108` | display, keypad — **no push-button** |
| Epson WF-2960 printer | `0x0000` | nothing |

## Four defects, none findable against our own sink

Each was invisible against the Pi **by construction**, because we wrote both
ends of that conversation and made them agree with each other rather than with
the specification.

### 1. The connection direction was backwards

Real sinks are the TCP initiator. Measured while Windows cast to the adapter
successfully:

```
Established  local 192.168.157.100:7236  <-  remote 192.168.157.1:57096  (WUDFHost)
```

Windows holds 7236 and the adapter dials *it*. A port sweep of the adapter
during a working Windows session found nothing open but port 80 — it listens on
no RTSP port at all, including the 554 it advertises.

castr dialled out. Now it binds 7236 **and** dials, taking whichever connects
first, so our own sink keeps working and real displays reach us.

### 2. M3 was sent before M2

```
->  OPTIONS *                       (M1)
<-  200 OK
->  GET_PARAMETER .../streamid=0    (M3)   <- too early
<-  OPTIONS *  User-Agent: MSFT-WDD (M2)
->  200 OK
    silence; closed after 27 s
```

The order is M1, **M2**, M3. The adapter discarded the premature M3 and waited
for one that had already been and gone. Our sink answers M3 whenever it
arrives. Now M3 follows M2, with a two second grace period so a sink that never
sends M2 behaves exactly as before.

### 3. Keyframe requests were answered and ignored

The adapter sent `wfd_idr_request` three times. It fell into the catch-all
"answer anything we do not recognise with 200 OK" branch, so it got three
polite acknowledgements and no keyframe. A sink that joins mid-GOP has nothing
to decode from: the session ran to completion, exchanging keep-alives, showing
black. `Action::Keyframe` now reaches the encoder, which already had
`request_keyframe`.

**This is what stood between a healthy-looking session and a picture.**

### 4. The PIN was requested before the display was asked to show one

`pair_with_pin` called the PIN callback *before* starting the pairing, though
its own comment said it must not. Our sink displays a PIN permanently, so this
never showed. The adapter shows one only once pairing is under way, so we
prompted for a number that did not exist. Moved into the `PairingRequested`
handler, behind a deferral so the prompt can block while somebody reads it.

## The pairing is single-use

Reproducible: after a successful session, the next association fails with
`The operation was cancelled`, and unpairing fixes it. It happened after the
PIN pairing and again after the push-button pairing.

The existing recovery — unpair and retry — was gated on the radio's wording
matching a known phrase, and this device's wording matches nothing. The gate
existed because re-pairing cost somebody a PIN prompt.

**That reasoning does not survive push-button.** Re-pairing by button is free
and unattended, so the gate is now the *cost*, not the wording: a display that
pairs by button is re-paired without ceremony; one that would prompt for a PIN
still needs the wording to justify interrupting someone.

## Diagnostics added

The RTSP exchange is now logged verbatim at debug, both directions. None of
defects 2 or 3 was visible before that — the session simply ended with nothing
to say. This is the third time in this project that the fix began with "stop
discarding what we are not looking at".

## Wrong turns, recorded

- **A retry on the RTSP connect.** The adapter refused in two seconds; the
  plausible story was that its server had not started. A 45-second port sweep
  refuted it — nothing ever opened. Never written.
- **A COM apartment fix.** The radio scan looked hung on a worker thread; the
  hypothesis was a WinRT async awaited without a multi-threaded apartment. It
  was implemented, with a confident comment, and was wrong: timing both paths
  showed discovery takes ~50 s on *any* thread. Reverted rather than kept as
  harmless, because it carried an explanation that was untrue.
- **An error message that lied.** "did not answer within 10s" was reported for
  a connection refused outright after two. Now distinguished.

## Not yet answered

- **The Pi regression.** It was unreachable for this entire session, so nothing
  here confirms castr still casts to its own sink after the direction change.
  The dial path is untouched and the M2 change has a grace-period fallback, but
  untouched is not tested. This is the first thing to run when the Pi is back.
- Audio on a real display, still never listened to.
- Whether a display that *requires* HDCP fails cleanly.
- The Fire TV, which offers no push-button and would need the PIN path.
- The presentation URL is still `rtsp://localhost/...`, which is meaningless to
  the far end. The adapter accepted it, so it has not been changed.

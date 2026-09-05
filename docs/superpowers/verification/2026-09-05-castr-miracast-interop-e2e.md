# Interop with a third-party display — verification (2026-09-05)

Part 4, and the first time castr has been pointed at Miracast hardware it did
not write. Hardware: an **MR-A202 wireless display adapter**, plus a Samsung 75"
Crystal UHD, an LG webOS TV UN7000PUB and an Amazon Fire TV Stick in range.

An adapter was chosen as the first interop target deliberately: it advertises
continuously rather than only while a mirroring page is open, so it can be
tested repeatedly without anyone standing in front of a television.

## What the displays in range actually offer

Read from each device's WPS information element (attribute 0x1008), which says
how a device is willing to be paired:

| Device | Config methods | Offers |
|---|---|---|
| MR-A202 adapter | `0x2288` | Display, PushButton, PhysicalPushButton |
| Samsung 75" Crystal UHD | `0x4388` | Display, PushButton, Keypad, PhysicalPushButton |
| LG webOS TV UN7000PUB | `0x3388` | Display, PushButton, Keypad, PhysicalPushButton |
| Amazon Fire TV Stick | `0x4108` | Display, Keypad — no push-button |
| Epson WF-2960 printer | `0x0000` | nothing |

Three of the four displays offer push-button, which castr does not implement at
all. It is not yet known to be *needed* — the adapter paired on a PIN — but it
is the most common ceremony in the room.

## Results

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 1 | A third-party display is found by name | PASS | `miracast-cast MR-A202` resolved it from its advertisement |
| 2 | Its RTSP port is read from its own element, not assumed | PASS | It advertises **554**, where every other device here uses 7236. A hardcoded port would have failed immediately |
| 3 | castr pairs with a third-party display | PASS | PIN `74380022`, read off the adapter's screen and accepted |
| 4 | A Wi-Fi Direct group forms with it | PASS | `"MR-A202" is up at 192.168.157.1` |
| 5 | DHCP issues an address | PASS | `endpoint local=192.168.157.100 remote=192.168.157.1` |
| 6 | An RTSP control connection is established | PASS | `192.168.157.1:57097 connected to us` — see "The direction was wrong" below |
| 7 | M1 is answered by a third-party display | PASS | `RTSP/1.0 200 OK, Public: org.wfa.wfd1.0, GET_PARAMETER, SET_PARAMETER` |
| 8 | M2 arrives and is answered | PASS | Its `OPTIONS *` with `User-Agent: MSFT-WDD`, answered 200 |
| 9 | M3 is accepted and capabilities returned | **FAIL** | Never answered; see "The ordering was wrong" |
| 10 | A picture reaches a third-party display | NOT RUN | Blocked by 9 |
| 11 | The M2 ordering fix works on hardware | NOT RUN | Unit-tested; the adapter stopped forming groups before it could be retried |
| 12 | Push-button pairing | NOT RUN | Not implemented |
| 13 | HDCP refusal is clean | NOT RUN | The adapter advertises HDCP but did not demand it |

## The direction was wrong

**Real Miracast sinks are the TCP initiator.** castr dialled out to the port
the sink advertises. That is backwards.

Measured while Windows was casting to the adapter successfully:

```
Established  local 192.168.157.100:7236  <-  remote 192.168.157.1:57096  (WUDFHost)
```

Windows holds 7236 and the adapter connects *to it* from an ephemeral port. A
port sweep of the adapter during a working Windows session found **nothing**
open except port 80 — it listens on no RTSP port at all, including the 554 it
advertises in its own information element.

Our own sink listens and we dial it, because we wrote both halves and made them
agree. No amount of testing against the Pi could have found this; it is true by
construction there.

Fixed by doing both: bind 7236 *and* dial the display, take whichever connects
first. The Pi keeps working because we still dial it. Everything above the
socket is untouched — who opened the connection has no bearing on the RTSP
exchange that follows. Verified: the adapter dialled in, twice.

## The ordering was wrong

With the connection established, the exchange was:

```
->  OPTIONS *                       (M1)
<-  200 OK  Public: ...
->  GET_PARAMETER .../streamid=0    (M3)   <- too early
<-  OPTIONS *  User-Agent: MSFT-WDD (M2)
->  200 OK
    silence; the adapter closed the session after 27 s
```

The sequence is M1, then **M2**, then M3. castr sent M3 the instant M1 was
answered. The adapter ignored the premature M3, sent its M2, and then waited
for an M3 that had already been and gone.

Our sink answers M3 whenever it arrives, so this was invisible against it too.

Fixed in `source::session`: M3 now goes out when M2 is answered, with a two
second grace period after which it is sent anyway — so a sink that never sends
M2 behaves exactly as before. Five existing tests encoded the wrong sequence
and were corrected; five new ones pin the rule.

**Not yet verified on hardware.** The adapter stopped forming groups
(`association: could not form a group ... The operation was cancelled`) after
roughly a dozen sessions, and a power cycle was needed. Row 11 stays NOT RUN
until it has been re-run.

## Diagnostics added

The RTSP exchange is now logged verbatim at debug level, both directions. None
of the above was visible before that: the session simply ended after 27 seconds
with nothing to say. This is the third time in this project that the fix has
begun with "stop discarding what we are not looking at."

## Wrong turns, recorded

- **A retry on the RTSP connect.** The adapter refused the connection in two
  seconds; the plausible story was that its server had not started yet, and
  `address_of` already retries for exactly that reason with DHCP. A 45-second
  port sweep refuted it: nothing ever opened. The retry was never written.
- **The error message lied.** "did not answer within 10s" was reported for a
  connection refused outright after two seconds, because `connect_timeout`
  returns immediately on a reset. Now distinguished: refused, timed out, or
  never dialled.

## Not yet answered

- Whether M3 is accepted once it is correctly ordered. Everything downstream of
  it — M4 to M7, the media path, a picture on a real display — depends on that
  and is untested.
- Whether the presentation URL matters. Ours is `rtsp://localhost/...`, which
  is meaningless to the far end; Windows uses the source's real address. Not
  changed, because nothing has yet blamed it.
- Push-button pairing, still unimplemented, and offered by three of the four
  displays here.
- The Pi was unreachable during this session, so the regression that castr
  still dials its own sink correctly has not been re-run since the direction
  change.

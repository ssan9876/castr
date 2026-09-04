# Wi-Fi Direct radio layer — end-to-end verification (2026-09-04)

Hardware: Windows PC `DESKTOP-C6QHH2A` (Realtek 8821CE) to the Pi 3 B sink at
`192.168.88.157`. Also in range while testing: a Samsung 75" television, an
Amazon Fire TV Stick and an Epson printer, none of which were connected to.

The goal was `castr-sender miracast-cast DietPi` — a display's name, no
address, nothing prepared.

## Results

| # | Claim | Verdict | Evidence |
|---|---|---|---|
| 1 | Wi-Fi Display elements are readable without asking for the property | PASS | `miracast-list` reads all four devices; the plan assumed an additional property was required and it is not |
| 2 | A display is distinguished from a device that is not one | PASS | See the listing below: the Samsung and the Pi are displays, the printer and an idle Fire TV are not |
| 3 | A third-party display's element parses correctly | PASS | Samsung: `RTSP 7236, up to 54 Mbps, HDCP` — read from its own beacon, matching the bytes decoded by hand earlier |
| 4 | Casting by name works when already paired | PASS | `wifidirect: "DietPi" is up at 192.168.173.1, RTSP on 7236`, then a cast, then `releasing the group` |
| 5 | Casting by name works **cold**, prompting for a PIN | PASS | `Enter the PIN shown on "DietPi":` then `WpsSuccess` on the sink, `playing 1280x720@30`, no dropped frames |
| 6 | The port comes from the display, not an assumption | PASS | `RTSP on 7236` taken from the parsed element |
| 7 | The group is torn down with the connection | PASS | `releasing the group with "DietPi"` on every exit; the sink logs `holding the group` and returns to advertising |
| 8 | Two casts in a row, no restart | PASS | 21:16 and 21:18, the second connecting silently |
| 9 | A fresh PIN appears between casts | PASS | `PIN 82128616 is on the screen` after the previous session ended |
| 10 | An unknown display names its stage and advises | PASS | `Error: discovery: no display of that name is advertising. Open Screen Mirroring on it - most displays advertise only while that screen is up` |
| 11 | A device that is not a display is named as such | PASS | `Error: discovery: "DIRECT-D0-EPSON-WF-2960 Series" is a Wi-Fi Direct device but not a display` |
| 12 | A refused PIN names its stage | PASS | An empty PIN gave `Error: pairing: "DietPi" - pairing failed with status 17; the radio said: The operation was cancelled.` Status 17 was then given a name |
| 13 | An ambiguous name lists the candidates | NOT RUN | No two devices in range share a prefix. Unit-tested only |
| 14 | Association failure quotes the WLAN log | NOT RUN | Cannot be staged reliably; unit-tested against the real 8002 text captured earlier |
| 15 | A stale pairing is retried once after unpairing | NOT RUN | Same: the condition cannot be created on demand. Unit-tested |
| 16 | Any third-party display can be cast to | NOT RUN | Needs the television or the dongle. Part 4 |

## The listing

```
DietPi                           display, RTSP 7236, up to 10 Mbps
Stephanie's Fire TV Stick        not a display
75" Crystal UHD                  display, RTSP 7236, up to 54 Mbps, HDCP
DIRECT-D0-EPSON-WF-2960 Series   not a display
```

The Fire TV is a display; it simply publishes no Wi-Fi Display element unless
its mirroring screen is open, which is what row 10's advice is about.

## A sink defect this work exposed

The sink stopped minting a PIN after its first cast, so it could be paired with
exactly once per start — the same defect fixed earlier in the day, reintroduced
one layer down by the fix itself.

The cause: the supplicant reports a station joining on **both** control
attachments, and the two copies land in the same drain only sometimes. The
`dedup` that collapses adjacent duplicates therefore caught them only sometimes,
and a counter of joins minus leaves drifted upward. The sink never believed it
was idle again.

Replaced with a set of station addresses, which is idempotent under duplication.
Verified: `PIN 82128616 is on the screen` after a session ended.

Worth recording as a pattern — *count events at your peril when the source may
repeat them* — because the first fix looked correct and passed its tests.

## What the plan got wrong

The plan required an additional property, `System.Devices.WiFiDirect.InformationElements`,
when enumerating, on the strength of the documentation. Constructing the
`IIterable<HSTRING>` that needs has no ergonomic form in windows-rs 0.58, so it
was tested without — and the elements are readable anyway. The requirement was
dropped rather than worked around.

## Not yet answered

- Interoperability. Every connection here was to our own sink. The Samsung's
  element parses, which is real evidence about the parser and none at all about
  the rest.
- Push-button pairing, deliberately not implemented: it cannot be exercised
  without a display that demands it.
- Whether a display that requires HDCP fails cleanly. The Samsung advertises
  support, which is not the same as requiring it.

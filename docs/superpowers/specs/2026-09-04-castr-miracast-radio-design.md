# castr sub-project 8: the Windows Wi-Fi Direct radio layer — design (2026-09-04)

Part 2 of 4 in the Miracast source project. Part 1, the media path, is merged.
Parts 3 and 4 are product integration and interop hardening.

## 1. Goal

Make `castr-sender miracast-cast "Living Room TV"` work: find the display, pair
with it, form the Wi-Fi Direct group, hand the address to the media path, and
tear the group down afterwards.

Today the command takes an IP address and nothing forms the group. Everything
below it works — a ten-minute cast to the Pi ran with no dropped frames — so
this sub-project is the missing half of a working feature, not new ground.

Success: `castr-sender miracast-cast DietPi`, with no address and no
preparation, casts to the Pi and leaves nothing behind.

## 2. What the spike established

Recorded because the design depends on it. Every step below was run from
unprivileged Rust using the `windows` crate, on this hardware, on 2026-09-04:

- Wi-Fi Direct peers enumerate through `WiFiDirectDevice::GetDeviceSelector2`
  and `DeviceInformation::FindAllAsyncAqsFilter`.
- Their Wi-Fi Display information elements are readable before connecting.
- `DeviceInformationCustomPairing` completes a PIN ceremony with no human at the
  keyboard: `DevicePairingResultStatus(0)`.
- `WiFiDirectDevice::FromIdAsync` forms the group and
  `GetConnectionEndpointPairs` yields the sink's address.
- Windows holds the group for roughly 60 seconds after the owning process exits.

So no hardware question remains open. This is productising code already run.

## 3. Structure

A new crate, `castr-wifidirect-win`, matching the convention
`castr-capture-win` and `castr-codec-win` already set: Windows APIs isolated
behind a small crate. `castr-miracast` keeps no Windows dependency; it is
already Linux-gated for the sink's radio, and putting a second platform's radio
in the same manifest is the mixing this avoids.

Because WinRT calls need real hardware, nothing in that crate is unit-testable.
Every **decision** therefore lives outside it, in pure code, leaving a thin
impure shell:

| Decision | Where | Pure |
|---|---|---|
| Parse a Wi-Fi Display information element | `castr-miracast/src/wfd.rs` | yes |
| Match a display by name | `castr-wifidirect-win/src/select.rs` | yes |
| Choose the pairing ceremony | `castr-wifidirect-win/src/select.rs` | yes |
| Wait-or-give-up policy | `castr-wifidirect-win/src/select.rs` | yes |
| Map a Windows failure to a stage | `castr-wifidirect-win/src/failure.rs` | yes |
| Enumerate, pair, connect, tear down | `castr-wifidirect-win/src/radio.rs` | no |

The parser belongs beside `device_info_subelement`, which builds the same
structure for the sink. One layout, one place, tested on every platform.

## 4. Reading the information element

The device-information subelement is a 2-byte flags field, a 2-byte RTSP port
and a 2-byte throughput ceiling. Bits 0-1 are the device type (01 = primary
sink), bits 4-5 session availability, bit 8 content protection.

Captured from real hardware today, which is what the parser is tested against:

| Device | Element | Reading |
|---|---|---|
| Samsung 75" Crystal UHD | `00 0006 0111 1c44 0036` | primary sink, available, HDCP-capable, port 7236, 54 Mbps |
| Pi (our sink) | `00 0006 0011 1c44 000a` | primary sink, available, port 7236, 10 Mbps |
| Amazon Fire TV Stick | none | not advertising as a display |
| Epson WF-2960 | none | a printer |

This is what tells a television from a printer, and it gives the RTSP port and
the bandwidth ceiling before a single frame is encoded.

## 5. The connection

The crate is one RAII object. `Connection` owns the `WiFiDirectDevice`, and the
group lives exactly as long as it does; dropping it is the teardown. That is
deliberate: a group outliving what created it is a failure mode this project has
already paid for twice, most recently as a peer holding credentials for a group
that no longer existed and sitting on "connecting" for ever.

1. **Discover**, polling until the named display appears or the timeout expires.
   Displays usually are not there when the command is run: a Fire TV publishes
   no Wi-Fi Display element until its mirroring screen is open, and televisions
   behave the same way. So the command prints what it is waiting for and keeps
   looking for up to 60 seconds.
2. **Filter** to primary sinks with a Wi-Fi Display element. A printer is never
   offered.
3. **Pair**, unless Windows already has. The ceremony is chosen from the
   display's advertised configuration methods. The PIN arrives through a
   callback, so this crate never owns the terminal and part 3 can substitute a
   dialog without touching it.
4. **Connect**, holding the device object.
5. **Learn the address** from the endpoint pair, retrying briefly because DHCP
   must complete first. The RTSP port comes from the element, not an assumption.
6. **Watch** `ConnectionStatusChanged`, so a display switched off mid-cast is
   noticed by the radio rather than only by a keep-alive expiring.

`miracast-cast "name"` becomes: discover, connect, take address and port, call
part 1's `cast_to` unchanged, drop the connection. Part 1 needs no modification.

**We do not unpair on teardown.** The stored pairing is what makes the second
cast silent; discarding it would reintroduce a PIN prompt every time.

**Windows holds the group for about a minute after we exit.** Anything
reconnecting immediately must tolerate that; it is not a bug to work around.

## 6. Failure

The stage vocabulary extends part 1's backwards: **discovery, pairing,
association, address**, then connect, negotiation, session, teardown. Each
failure names its stage.

| Failure | Reported as |
|---|---|
| Named display never appeared | not advertising: open Screen Mirroring on the display |
| Found, but no Wi-Fi Display element | that device is not a display (named, and excluded from the list) |
| Ambiguous name | the candidates, so the user can be specific |
| PIN refused | the PIN was not accepted, and that the display shows a new one |
| Association failed | the reason quoted from the WLAN AutoConfig log |
| No address | associated but no lease, which points at the display's DHCP |

Two rules earned from experience:

- **The association failure reason is read from Windows' own WLAN AutoConfig
  log and quoted.** `Failure Reason: The specific network is not available.
  RSSI: 255` was the sentence that solved a whole afternoon, and no user should
  have to find that in Event Viewer.
- **A paired display that fails to associate gets one automatic retry after
  unpairing**, saying why. Stale credentials for a group that no longer exists
  produce an indefinite hang with nothing to explain it. The cost is a PIN
  prompt when the failure was in fact transient, which is far cheaper.

## 7. Testing

The parser is tested against the four real elements in section 4 — genuine bytes
from three vendors we did not write, which is the only real interop evidence
available before a television can be tried.

Everything else decidable is pure and tested normally: name matching, ceremony
choice, the waiting policy, failure mapping.

The impure shell gets one integration test: `miracast-cast DietPi` with no
address, end to end against the Pi, which is a real sink we control and can run
repeatedly. Third-party displays are part 4.

## 8. Out of scope

The sender's GUI and a display picker (part 3); interop quirks (part 4); HDCP in
any form; any non-Windows source.

## 9. Done when

`castr-sender miracast-cast DietPi` — no address, nothing prepared — discovers
the Pi, pairs if needed, forms the group, casts, and tears down, leaving the Pi
advertising again and ready for the next cast. Then twice in a row, since a
second cast is where a group left behind would show.

Of the failures in section 6, four can be provoked deliberately and must report
themselves by name: an unknown display, an ambiguous name, a device that is not
a display, and a refused PIN. Association and address failures cannot be staged
reliably — they need a display that is broken in a particular way — so their
handling is unit-tested against captured status codes and log text, and marked
as such in the verification rather than claimed as proven.

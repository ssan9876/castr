# castr sub-project 3: Miracast sink and Windows Wi-Fi health check

A Wi-Fi Direct Miracast sink on the Raspberry Pi, so a Windows or Android
device can cast to it with nothing installed, plus a diagnostic in the Windows
sender that finds and removes the local causes of Miracast disconnects.
Builds on sub-project 2 (`2026-09-02-castr-pi-hardening-design.md`): the
decoder, jitter buffer, clock, renderer and service are reused unchanged.

## 1. Goal

Press Windows+K, pick the Pi, type the PIN it shows on the TV, and see the
desktop mirrored with less lag and fewer drops than a commercial dongle
delivers. Separately, tell a Windows user why their Miracast keeps dropping
and fix the parts of that which are safely fixable.

castr keeps two protocols. Its own is better in every measurable way and needs
a 10 MB executable. Miracast is the zero-install path for a guest, and this
sub-project builds it.

## 2. Findings that shape the design (measured 2026-09-02)

**The Pi's radio** (`iw list`, brcmfmac / BCM43438):

- Supported interface modes include `P2P-GO`, `P2P-client` and `P2P-device`,
  so Wi-Fi Direct group-owner mode is available.
- Valid combinations: `managed <= 2, P2P-device <= 1, {P2P-client, P2P-GO} <= 1,
  total <= 3, #channels <= 2`. A P2P group can therefore coexist with a station
  connection, on at most two channels.
- **Band 1 only**: 2412, 2437 and 2462 MHz (channels 1, 6, 11) at 20 dBm. There
  is no 5 GHz. Single spatial stream. Realistic throughput is about 20 Mbps on
  a quiet channel and much less on a busy one, against the 50 Mbps or more a
  5 GHz Miracast link would carry.
- `wpasupplicant` 2.10-24 is already installed (the package has no underscore,
  which is why an earlier check missed it).
- `wlan0` is down and unused: the Pi's network is `eth0`. The radio is free to
  dedicate to P2P.

**The Windows sender** (`netsh wlan show driver`, `Get-NetAdapter`):

- Adapter: Realtek 8821CE 1x1 802.11ac PCI-E, native Wi-Fi driver version
  2024.10.139.3 dated 2024-01-15.
- `Wireless Display Supported: Yes (Graphics Driver: Yes, Wi-Fi Driver: Yes)`,
  so Miracast source mode is available. `Hosted network supported: No`, which
  is the legacy SoftAP feature and is not needed for Wi-Fi Direct.
- The Wi-Fi is disconnected; the machine runs on 1 Gbps Ethernet.
- Bluetooth is active. The 8821CE is a Wi-Fi/Bluetooth combo with a single
  antenna, so a Miracast session shares that antenna with every Bluetooth
  device. This is a well-known source of P2P drops and is the leading
  explanation for this machine's disconnects, ahead of the band-split theory
  that applies when a station link is also active.

## 3. Decisions taken during design

| Decision | Choice |
|---|---|
| Video ceiling | Advertise 1280x720p30 as the top mode, so the source never sends more than 2.4 GHz can carry |
| Who may cast | WPS PIN display method: the Pi shows an 8-digit PIN, Windows prompts for it, the peer is remembered afterwards |
| Two protocols, one screen | First to connect owns the display; the other is refused until it frees |
| Build vs borrow | `wpa_supplicant` for the radio and WPS; RTSP, RTP, MPEG-TS and DHCP are ours, in Rust |
| Order of work | Health check first, then the Wi-Fi Direct sink |
| Miracast over Infrastructure | Next sub-project, once the sink is proven |

## 4. Part one: the Windows Wi-Fi health check

Ships first because it is small, useful immediately, and tells us what a real
sender looks like before the sink is designed around it.

### 4.1 Interface

`castr-sender diagnose` prints a report; the GUI gains a "Check my Wi-Fi"
button that runs the same code and shows the same findings. Exit code is 0
when nothing is wrong, 1 when a warning was found, so it can be scripted.

### 4.2 Checks

Each check yields a severity (`ok`, `warn`, `fail`), a one-line statement of
what was found, and a one-line statement of why it matters.

| Check | Source | Fails when |
|---|---|---|
| Wireless display support | `netsh wlan show driver` | either the graphics or Wi-Fi driver reports No |
| Adapter identity and driver age | `netsh wlan show driver`, `Get-NetAdapter` | driver older than three years (warn) |
| Combo Wi-Fi/Bluetooth chip with Bluetooth active | adapter name matched against a small table, `Get-PnpDevice -Class Bluetooth` | both present (warn: shared antenna) |
| Station band vs sink band | `netsh wlan show interfaces` | connected on 5 GHz while the sink is 2.4 GHz (warn: the radio must time-slice) |
| Signal strength and channel | `netsh wlan show interfaces` | signal below 60 percent (warn) |
| Adapter power saving in the active power plan | `powercfg /q SCHEME_CURRENT SUB_WIFI` | any setting other than maximum performance (warn, fixable) |
| "Allow the computer to turn off this device" | `Get-NetAdapterPowerManagement` | power-off permitted (warn, fixable) |
| USB selective suspend | `powercfg /q SCHEME_CURRENT SUB_USB`, only for USB adapters | enabled (warn, fixable) |

### 4.3 Fixes

Only the three marked fixable are offered, each behind an explicit prompt,
each reversible, each printing its undo command:

- Wireless adapter power saving to maximum performance, on AC and battery.
- Clear the adapter's power-off permission.
- Disable USB selective suspend, when the adapter is USB.

All three need administrator rights. When the tool is not elevated it prints
the exact commands rather than failing, so the user can run them or re-run
elevated. Nothing else is touched: no driver settings, no reinstalls, no
disabling Bluetooth, because that is where machines get broken.

### 4.4 Honesty requirement

The report states plainly that this cannot improve Windows' own Miracast
implementation, which lives in the operating system. It removes local causes
of drops and nothing more. When the adapter is a combo chip, or the station
band differs from the sink's, the report says so and recommends castr's own
protocol over the wire for the user's own machines.

## 5. Part two: the Wi-Fi Direct sink

### 5.1 Structure

A new Linux-only crate, `castr-miracast`, with four layers:

```
crates/castr-miracast/
  src/p2p.rs        wpa_supplicant control socket: WFD advertising, group,
                    WPS PIN, peer events
  src/dhcp.rs       minimal DHCP server for the group interface
  src/rtsp.rs       RTSP/1.0 server on port 7236: M1-M7, keep-alive, teardown
  src/wfd.rs        WFD parameter encoding/decoding, including the Microsoft
                    extensions and the capability bitmaps
  src/rtp.rs        RTP receive, sequence reordering, loss detection
  src/ts.rs         MPEG-TS demux: PAT/PMT, PID filter, continuity, PES
  src/session.rs    ties the layers together, owns one sink session
```

The receiver gains an arbiter and a second event source; nothing else changes.

### 5.2 Radio and pairing

`wpa_supplicant` runs on `wlan0` from a dedicated config with a control socket
we own. Over that socket the sink:

1. Enables Wi-Fi Display advertising (`SET wifi_display 1`) and sets the WFD
   device information subelement: primary sink, session available, RTSP control
   port 7236, and a maximum throughput hint derived from the 720p30 ceiling.
   The exact subelement bytes are built in `wfd.rs` and pinned by a unit test
   against a known-good string.
2. Chooses the least busy of channels 1, 6 and 11 from a scan, then creates a
   persistent group as group owner (`P2P_GROUP_ADD persistent freq=<f>`), so a
   returning source rejoins without renegotiating.
3. Runs WPS with the display method: the Pi generates an eight-digit PIN, shows
   it through the receiver's existing overlay, and authorises the enrolment
   (`WPS_PIN any <pin>`). Windows prompts the user for that PIN.
4. Records the peer on success in `paired.toml`, in a `[miracast]` section
   keyed by P2P device address, so a known device skips the PIN. castr peers
   are keyed by certificate fingerprint and are untouched by this.
5. Tears the group down when the display is released, and re-advertises.

The group interface (`p2p-wlan0-0`) needs an address on the source side, which
Miracast leaves to DHCP. `dhcp.rs` answers DISCOVER and REQUEST with a fixed
`/29` on 192.168.173.0, gateway and server the Pi. That range is deliberately
far from the 192.168.88.0/24 LAN the Pi sits on, so the source cannot confuse
the two default routes. It binds only to the group
interface, hands out exactly one address, and ignores everything else. A
system `dnsmasq` is the fallback if this proves troublesome, at the cost of
system-wide configuration on a box we otherwise keep clean.

### 5.3 Session negotiation

RTSP/1.0 over TCP on port 7236, bound to the group interface. The standard
Miracast exchange, with the sink's side of each message:

| Step | Direction | Sink behaviour |
|---|---|---|
| M1 | source to sink | answer OPTIONS with our supported methods |
| M2 | sink to source | ask OPTIONS, learn what the source supports |
| M3 | source to sink | answer GET_PARAMETER with our capabilities (below) |
| M4 | source to sink | accept SET_PARAMETER carrying the chosen format |
| M5 | source to sink | accept the SETUP trigger |
| M6 | sink to source | SETUP, creating the session and naming our RTP port |
| M7 | sink to source | PLAY, after which media flows |

Capabilities we advertise:

- `wfd_video_formats`: native 1280x720p30, with every higher CEA, VESA and
  handheld mode cleared. The bitmaps come from the WFD 1.1 tables and are
  pinned by a golden-string unit test.
- `wfd_audio_codecs`: LPCM 48 kHz stereo, 16-bit.
- `wfd_content_protection`: none.
- `wfd_client_rtp_ports`: `RTP/AVP/UDP;unicast <port> 0 mode=play`.
- `wfd_uibc_capability`: none. There is no input back-channel here.
- `microsoft_max_bitrate`: a ceiling we set from the negotiated link rate,
  starting at 8 Mbps and lowered when loss persists.
- `microsoft_latency_management_capability`: supported, requesting the
  source's low-latency mode.
- `microsoft_format_change_support`: supported, so resolution can drop
  mid-session instead of the session dying.

Keep-alive is a GET_PARAMETER every 30 seconds in each direction; a missed
reply for 60 seconds ends the session. TEARDOWN releases the display.

### 5.4 Media path

RTP arrives on the advertised UDP port, with a 4 MiB receive buffer for the
same reason castr needs one: a keyframe is a burst and the default buffer
drops its tail.

1. `rtp.rs` parses the header, checks the payload type is MP2T, and reorders on
   sequence number with a 32-packet, 100 ms window. Gaps are counted and
   reported to the session.
2. Each payload is a whole number of 188-byte transport packets. `ts.rs`
   reads the PAT to find the PMT, the PMT to find the video PID (stream type
   0x1B, H.264) and the audio PID (0x83, LPCM), and tracks continuity counters
   per PID.
3. Packetised elementary stream payloads are reassembled on the unit-start
   flag. The 33-bit, 90 kHz presentation timestamp from each PES header is
   converted to microseconds.
4. Video payloads are already Annex B, which is what `V4l2Decoder` takes. Each
   complete PES payload is one access unit.
5. LPCM audio payloads carry the WFD audio header, then big-endian 16-bit
   samples, which are byte-swapped into the receiver's existing audio output.
6. Timestamps drive the existing `AvClock` with audio as master, exactly as the
   castr path does.

### 5.5 Recovery, which is the point

- A continuity or sequence gap marks the current access unit damaged; it is
  dropped rather than fed to the decoder.
- When the decoder reports a lost reference, or a gap hit a video PID, the sink
  sends `SET_PARAMETER` with `wfd_idr_request`, rate-limited to one per 500 ms.
  A dongle waits for the source's next scheduled keyframe, which is where
  seconds of corruption come from; we ask for one immediately.
- Sustained loss lowers `microsoft_max_bitrate` by 30 percent, with a 2 second
  guard, and raises it 5 percent after 5 clean seconds, mirroring castr's
  controller.
- If the decode queue stays deep with no loss, we request a format change to a
  lower resolution rather than let the picture stutter.
- An RTSP transport error, or 60 seconds without a keep-alive, tears the
  session down, releases the display and re-advertises within 2 seconds.

### 5.6 One display, two protocols

The receiver gains a `DisplayOwner`: idle, castr, or Miracast. A session
acquires it on the first frame-producing message and releases it on teardown or
disconnect. While it is held by one protocol, the other refuses: castr answers
`Error { code: 5, message: "display busy" }`, and the sink answers RTSP 503 at
SETUP. Refusals are logged and shown briefly on screen. Neither protocol can
preempt the other.

## 6. Configuration and control

- `castr-receiver --miracast on|off|auto` (default `auto`: on when `wlan0`
  exists and `wpa_supplicant` starts, off otherwise, with the reason logged).
- `--miracast-name <name>` for the name shown in Windows' cast list, defaulting
  to the hostname.
- `--miracast-channel 1|6|11|auto`, default `auto`.
- The systemd unit gains nothing; the sink lives inside the receiver process so
  the display arbiter is a plain mutex rather than an inter-process protocol.

## 7. Testing

**Unit, no hardware:** RTSP message parse and format against captured
exchanges; the capability strings against golden values; WFD subelement
encoding; transport-stream demux driven by a synthetic stream (PAT, PMT,
video and audio PIDs, deliberate continuity gaps); PES assembly and timestamp
extraction; RTP reordering and gap detection; the DHCP responder against
captured DISCOVER and REQUEST packets.

**Integration on the Pi, no radio:** a recorded transport stream replayed over
UDP to the sink's media path, decoded and presented, verified by the frame
dump. This makes the whole media path testable without a Miracast source.

**Hardware, with the Windows machine:** connect from Windows+K, enter the PIN,
confirm the desktop mirrors; measure glass-to-glass latency with the camera
method; a 10 minute soak recording every disconnect; reconnect after teardown
without re-entering the PIN; a run with Bluetooth disabled and one with it
active, since this adapter shares an antenna, recorded as evidence either way.

**Health check:** run against this machine and confirm the findings match the
values measured by hand in section 2; apply each fix, confirm the reported
undo command restores the previous value.

## 8. Out of scope

Miracast over Infrastructure, which is the next sub-project. HDCP, which needs
licensed keys: DRM-protected video will show as black and this is documented,
not worked around. AAC audio. The user input back-channel. Miracast source
mode, so castr will not cast to third-party dongles. 5 GHz, which this radio
does not have. Android as a sink.

## 9. Dependencies

`wpa_supplicant` 2.10, already present on the Pi, driven over its control
socket. No new Rust crates: RTSP, RTP, MPEG-TS and DHCP are hand-written for
the same reasons the V4L2 layer was, and the Windows health check shells out to
`netsh` and `powercfg`, which ship with the operating system.

# castr sub-project 3 (Miracast sink) end-to-end verification (2026-09-03)

Pi receiver: `dietpi@192.168.88.157` (DietPi / Debian 13 trixie, Pi 3 B,
`brcmfmac` 2.4 GHz radio, `bcm2835-codec` V4L2 decoder), branch `miracast-sink`
at HEAD `35f2f01`. Windows host: `DESKTOP-C6QHH2A`, Realtek 8821CE adapter.
Every step below was run by an automated agent over SSH except where it says
otherwise.

**Status: the sink is verified from the supplicant up to the decoder, on real
hardware, with no radio peer. The one part still outstanding is the cast from
Windows itself (Windows+K, PIN, desktop on screen), which cannot be driven from
a shell: it needs a person at the PC.** Section "What is not yet verified" says
exactly what that leaves open.

## Summary

| # | Step | Result | Evidence |
|---|------|--------|----------|
| 1 | Manual supplicant sequence by hand (spec 5.2) | PASS | own supplicant instance, `p2p-wlan0-0` in `mode=P2P GO` on 2437 MHz |
| 2 | `WFD_SUBELEM_SET` accepted | PASS after a fix | spaced hex answered `FAIL`; unspaced answered `OK` and reads back identically |
| 3 | Sink under the service, unprivileged | PASS after a fix | supplicant started, group created, interface addressed, RTSP and RTP listening, all as user `castr` |
| 4 | Channel choice from a live scan | PASS | `scan counts [4, 8, 3] for channels [1, 6, 11], choosing 11`, and on a later pass `[5, 4, 6] ... choosing 6` |
| 5 | Full RTSP negotiation over real sockets (M1-M7) | PASS | every message and reply reproduced below; sink reached `playing 1280x720@30` |
| 6 | Media path to the decoder | PASS | 60 access units delivered in order; the V4L2 decoder rejected them as expected for a synthetic payload |
| 7 | Session teardown and re-advertisement | PASS | group removed and a fresh group up 2 s later, twice |
| 8 | Cast from Windows | NOT RUN | needs a person at the PC |
| 9 | Latency, soak, Bluetooth comparison | NOT RUN | all depend on step 8 |

## Step 1: the manual sequence (plan Task 10, step 4)

`iw` and `rfkill` are not installed on this Pi and it has no route to the
internet, so `wpa_cli` and `ip` stand in for `iw dev`. `wpasupplicant` was
already present.

```
$ ssh dietpi@192.168.88.157 'ls /sys/class/net; lsmod | grep cfg80211'
eth0
lo
wlan0
cfg80211             1085440  2 brcmfmac_cyw,brcmfmac

$ sudo ip link set wlan0 up
$ sudo /usr/sbin/wpa_supplicant -i wlan0 -c /etc/castr/wpa_supplicant-p2p.conf -B
Successfully initialized wpa_supplicant
WPS: Converting display to virtual_display for WPS 2.0 compliance

$ sudo ls -l /run/wpa_supplicant_castr/
srwxrwx--- 1 root castr 0 Sep  3 15:54 p2p-dev-wlan0
srwxrwx--- 1 root castr 0 Sep  3 15:54 wlan0

$ sudo /sbin/wpa_cli -p /run/wpa_supplicant_castr -i wlan0 status
wpa_state=DISCONNECTED
p2p_device_address=ba:27:eb:05:1c:c1
address=b8:27:eb:05:1c:c1
```

## Step 2: the subelement hex (a defect the hardware found)

```
$ $W set wifi_display 1
OK
$ $W wfd_subelem_set 0 "00060011 1c44 000a"
FAIL
$ $W wfd_subelem_set 0 000600111c44000a
OK
$ $W wfd_subelem_get 0
000600111c44000a
```

The supplicant parses the argument as a hexdump and rejects embedded spaces.
`wfd::device_info_subelement` and its two unit tests were corrected to emit one
unbroken string (commit `35f2f01`). No test could have caught this: the plan's
fixture carried the spaces too.

## Step 3: the group, by hand

```
$ $W p2p_group_add persistent freq=2437
OK
$ ip -br link | grep p2p
p2p-wlan0-0      UP             ba:27:eb:05:9c:c1 <BROADCAST,MULTICAST,UP,LOWER_UP>
$ sudo /sbin/wpa_cli -p /run/wpa_supplicant_castr -i p2p-wlan0-0 status
bssid=ba:27:eb:05:9c:c1
freq=2437
ssid=DIRECT-iq
mode=P2P GO
key_mgmt=WPA2-PSK
wpa_state=COMPLETED

$ sudo ip addr add 192.168.173.1/29 dev p2p-wlan0-0
$ ip -br addr show p2p-wlan0-0
p2p-wlan0-0      UP             192.168.173.1/29
```

`mode=P2P GO` is the confirmation the plan asked `iw dev` for.

## Step 4: the sink under the service (a second defect the hardware found)

The first deploy failed on every attempt, once every seven seconds:

```
lchown[ctrl_interface=/run/wpa_supplicant_castr,gid=987]: Operation not permitted
Failed to initialize control interface 'DIR=/run/wpa_supplicant_castr GROUP=castr'.
WARN castr_miracast::sink: miracast: wpa_supplicant exited with exit status: 255
```

`wpa_supplicant` chowns its control directory to the configured group at
startup, and only the directory's owner may do that. The `tmpfiles.d` entry
made it `root`-owned while the service runs as `castr`. Changing the entry to
`d /run/wpa_supplicant_castr 0770 castr castr -` fixed it (commit `35f2f01`).
Worth noting for its own sake: the sink's retry loop kept the receiver alive
and legible through the whole failure rather than dying at startup.

After the fix, from a clean deploy:

```
INFO castr_miracast::sink: miracast: starting wpa_supplicant for wlan0
Successfully initialized wpa_supplicant
INFO castr_miracast::sink: miracast: SET device_name DietPi -> OK
INFO castr_miracast::sink: miracast: SET wifi_display 1 -> OK
INFO castr_miracast::sink: miracast: WFD_SUBELEM_SET 0 000600111c44000a -> OK
INFO castr_miracast::sink: miracast: scan counts [4, 8, 3] for channels [1, 6, 11], choosing 11
INFO castr_miracast::sink: miracast: advertising as "DietPi" on channel 11
INFO castr_miracast::sink: miracast: P2P_GROUP_ADD persistent freq=2462 -> OK
INFO castr_miracast::sink: miracast: ip addr flush dev p2p-wlan0-0 -> ok
INFO castr_miracast::sink: miracast: ip addr add 192.168.173.1/29 dev p2p-wlan0-0 -> ok
INFO castr_miracast::sink: miracast: ip link set p2p-wlan0-0 up -> ok
INFO castr_miracast::sink: miracast: group p2p-wlan0-0 up, RTSP on 192.168.173.1:7236, RTP on 192.168.173.1:5000
```

Every one of those runs as the unprivileged `castr` user, with only the three
ambient capabilities the unit grants.

## Step 5: the full RTSP negotiation, on real sockets

This is the spec's "integration on the Pi, no radio" test. A cross-built
loopback source (`crates/castr-miracast/examples/loopback-source.rs`) speaks the
source half of the negotiation against the running service, using the same
fixtures the unit tests use, then sends a synthetic transport stream to the RTP
port.

```
$ /tmp/loopback-source 192.168.173.1:7236 48
connecting to 192.168.173.1:7236
> OPTIONS * RTSP/1.0
< RTSP/1.0 200 OK
< CSeq: 1
< Public: org.wfa.wfd1.0, SETUP, TEARDOWN, PLAY, PAUSE, GET_PARAMETER, SET_PARAMETER
< OPTIONS * RTSP/1.0
< CSeq: 100
< Require: org.wfa.wfd1.0
> GET_PARAMETER rtsp://x RTSP/1.0
< RTSP/1.0 200 OK
< CSeq: 2
< Content-Type: text/parameters
< Content-Length: 336
< wfd_video_formats: 40 00 02 04 00000020 00000000 00000000 00 0000 0000 00 none none
< wfd_audio_codecs: LPCM 00000002 00
< wfd_content_protection: none
< wfd_client_rtp_ports: RTP/AVP/UDP;unicast 5000 0 mode=play
< microsoft_max_bitrate: 8000
< microsoft_latency_management_capability: supported
< microsoft_format_change_support: supported
> SET_PARAMETER rtsp://x RTSP/1.0
< RTSP/1.0 200 OK
< CSeq: 3
> SET_PARAMETER rtsp://x RTSP/1.0
< RTSP/1.0 200 OK
< CSeq: 4
< SETUP rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
< CSeq: 101
< Transport: RTP/AVP/UDP;unicast;client_port=5000
> RTSP/1.0 200 OK
< PLAY rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
< CSeq: 102
< Session: abcdef12
sending 48 access units to 192.168.173.1:5000
sent 49 datagrams
```

M1 through M7 in order, each answered, and on the sink's side:

```
INFO castr_miracast::sink: miracast: RTSP connection from 192.168.173.1:59040
INFO castr_miracast::sink: miracast: playing 1280x720@30
```

`playing 1280x720@30` is the negotiated mode: the 720p30 ceiling the spec set,
chosen from the source's `wfd_video_formats` bitmap.

## Step 6: access units reach the decoder

With `RUST_LOG=info,castr_receiver=debug` set through a temporary drop-in and
60 units sent:

```
DEBUG castr_receiver::pipeline: decode frame 45 key=true
...
DEBUG castr_receiver::pipeline: decode frame 59 key=true
WARN  castr_receiver::pipeline: decode error on frame 59 (keyframe=true, last decoded None):
      decoder stalled: 60 access units queued with no picture for 30611 ms
```

All 60 units arrived, in order, numbered contiguously, and were handed to the
V4L2 decoder. The decoder rejecting them is the correct outcome and the point
of the test: the payload is synthetic, not real H.264, so the only thing this
proves — and the thing that was untested until now — is that the socket layer,
the reordering, the demux, the jitter buffer and the decoder are connected end
to end. The drop-in was removed afterwards and the service restarted clean.

## Step 7: teardown and re-advertisement

```
INFO castr_miracast::sink: miracast: session ended: source closed the connection
INFO castr_receiver::pipeline: miracast: source closed the connection
INFO castr_miracast::sink: miracast: P2P_GROUP_REMOVE p2p-wlan0-0 -> OK
INFO castr_miracast::sink: miracast: group ended, re-advertising
INFO castr_miracast::sink: miracast: SET device_name DietPi -> OK
INFO castr_miracast::sink: miracast: WFD_SUBELEM_SET 0 000600111c44000a -> OK
INFO castr_miracast::sink: miracast: scan counts [5, 4, 6] for channels [1, 6, 11], choosing 6
INFO castr_miracast::sink: miracast: P2P_GROUP_ADD persistent freq=2437 -> OK
INFO castr_miracast::sink: miracast: group p2p-wlan0-1 up, RTSP on 192.168.173.1:7236, RTP on 192.168.173.1:5000
```

Two seconds from teardown to a fresh advertisement, on a re-scanned channel,
observed twice. Note the second group is `p2p-wlan0-1`: the sink tracks the
interface name it is given rather than assuming one.

## Step 8: the Windows health check on the sending PC

Run on `DESKTOP-C6QHH2A` (part one of this sub-project, already merged):

```
Adapter: Realtek 8821CE Wireless LAN 802.11ac PCI-E NIC
[ok  ] Wireless display support - the graphics and Wi-Fi drivers both support wireless display
[ok  ] Driver age - version 2024.10.139.3, dated 2024
[warn] Shared Wi-Fi and Bluetooth antenna - Bluetooth is active
[--  ] Station band vs sink band - the Wi-Fi radio is not connected to a network
[warn] Wireless adapter power saving - power saving is on
[?   ] Adapter power-off permission - Get-NetAdapterPowerManagement: A device attached to the system is not functioning.
[--  ] USB selective suspend - not a USB adapter
```

The PC can do wireless display, and its radio is idle, which is the best case
for casting. The two warnings are the antenna sharing and power saving — the
first is the leading suspect for the drops that started this sub-project.

## What is not yet verified

Everything that needs a person at the Windows machine:

- **The cast itself.** Press Windows+K, pick `DietPi`, type the PIN shown on the
  television. Until that is done, the PIN path (`P2P-PROV-DISC-*` to
  `WPS_PIN any`), the DHCP exchange with a real peer, and real H.264 on the
  screen are all unexercised against a real source. Every layer beneath them is
  now proven, so a failure there will be in the radio or the pairing, and the
  journal will say which.
- **Glass-to-glass latency**, which needs the camera-and-stopwatch method.
- **The ten-minute soak** and its disconnect count.
- **Reconnect without a PIN**, which depends on the persistent group's stored
  credentials.
- **The Bluetooth on/off comparison**, the measurement this sub-project exists
  to make.
- **A frame dump of the desktop**, which must be captured against the synthetic
  test pattern rather than real desktop content.

Two smaller gaps, both benign: `iw` and `rfkill` are not installed on this Pi
(no internet route), so `wpa_cli` and `ip` stood in; the sink treats `rfkill` as
optional and ignores its absence. And the group is torn down and rebuilt after
every session, so a peer that had stored credentials will be offered a fresh
group — whether Windows re-uses its credentials against it is exactly what the
reconnect test above will settle.

## How to run the outstanding steps

1. Confirm the service is up: `ssh dietpi@192.168.88.157 'sudo systemctl is-active castr-receiver'`.
2. On the PC, press Windows+K and look for `DietPi`.
3. Type the PIN the television shows.
4. Watch the journal:
   `ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver -f'`.

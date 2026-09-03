# castr sub-project 4 (Miracast resilience) end-to-end verification (2026-09-03)

Pi receiver: `dietpi@192.168.88.157` (DietPi / Debian 13 trixie, Pi 3 B,
`brcmfmac` 2.4 GHz radio), branch `miracast-resilience` at HEAD `7e6da4d`,
deployed fresh for this run. Windows host: `DESKTOP-C6QHH2A`, Realtek 8821CE
adapter. Every step below was run by an automated agent over SSH except where
it says otherwise.

**Status: the hold, the resume, and the bitrate ladder are all verified on
real hardware, with no radio peer.** The three hardware questions in step 5
need a person at the Windows PC pressing Windows+K, entering a PIN, and using
Bluetooth headphones — none of that can be driven from a shell, so none of it
was attempted. Section "What was not run" says exactly what that leaves open.

## Summary

| # | Step | Result | Evidence |
|---|------|--------|----------|
| 1 | Add `--drop` and `--vanish` to the loopback source | PASS | builds and cross-compiles clean |
| 2 | Cross-build and push the example | PASS | binary built and copied to `/tmp/loopback-source` on the Pi |
| 3 | Hold and resume, no radio peer | PASS | `no media for 2 s`, `holding the group`, no `P2P_GROUP_REMOVE`, second `RTSP connection from` — all on `p2p-wlan0-0` |
| 4 | Bitrate ladder, no radio peer | PASS | `asking the source for 2000 kbps`, exactly once |
| 5.1 | `brcmfmac` honours the GO settings (real blip) | NOT RUN | needs a person at the Windows PC |
| 5.2 | Windows honours `microsoft_max_bitrate` | NOT RUN | needs a person at the Windows PC |
| 5.3 | A real Bluetooth blip survives | NOT RUN | needs a person at the Windows PC |

## Step 1 and 2: the two flags, built and pushed

The brief's drop condition would have discarded datagram 0, which carries the
PAT/PMT tables the demux needs to learn the stream PIDs at all — without them
sustained loss and total silence look identical, so it was corrected to
`i > 0 && (i as u32 % 100) < drop_percent`. The brief's final `println!` also
still reported `count` (the pre-drop total) rather than `sent`, which would
misreport every `--drop` run; it was changed to print `sent`.

```
$ docker run --rm ... castr-xbuild:aarch64 bash -c 'cargo build --release --locked \
    --target aarch64-unknown-linux-gnu -p castr-miracast --example loopback-source ...'
   Compiling castr-media v0.1.0 (/src/crates/castr-media)
   Compiling castr-miracast v0.1.0 (/src/crates/castr-miracast)
    Finished `release` profile [optimized] target(s) in 4.49s

$ cat dist/loopback-source-aarch64 | ssh dietpi@192.168.88.157 \
    'cat > /tmp/loopback-source && chmod +x /tmp/loopback-source && ls -la /tmp/loopback-source'
-rwxr-xr-x 1 dietpi dietpi 398088 Sep  3 21:44 /tmp/loopback-source
```

The receiver itself was also rebuilt and deployed fresh with `deploy.sh`, so
this run exercises the current `miracast-resilience` code (HEAD `7e6da4d`),
not whatever was already installed on the Pi:

```
$ bash scripts/pi/deploy.sh dietpi@192.168.88.157
...
active
deployed to dietpi@192.168.88.157
```

## Step 3: the hold and the resume

The group was already up as `p2p-wlan0-0` before the test began.

```
$ ssh dietpi@192.168.88.157 'ip -br link | grep p2p'
p2p-wlan0-0      UP             ba:27:eb:05:9c:c1 <BROADCAST,MULTICAST,UP,LOWER_UP>
```

First connection, sent with `--vanish` at 60 of 120 units, then the source
stops sending and stops answering without closing the socket:

```
$ ssh dietpi@192.168.88.157 '/tmp/loopback-source 192.168.173.1:7236 120 --vanish'
connecting to 192.168.173.1:7236
> OPTIONS * RTSP/1.0
< RTSP/1.0 200 OK
...
< PLAY rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
< CSeq: 102
< Session: abcdef12
sending 120 access units to 192.168.173.1:5000
vanishing after 60 datagrams
```

Second connection, run within 30 seconds of the vanish, in a second shell:

```
$ ssh dietpi@192.168.88.157 '/tmp/loopback-source 192.168.173.1:7236 60'
connecting to 192.168.173.1:7236
> OPTIONS * RTSP/1.0
< RTSP/1.0 200 OK
...
< PLAY rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
< CSeq: 102
< Session: abcdef12
sending 60 access units to 192.168.173.1:5000
sent 61 datagrams
< SET_PARAMETER rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
< CSeq: 103
< Session: abcdef12
< Content-Type: text/parameters
< Content-Length: 17
< wfd_idr_request
done
```

The journal for the whole window, in order:

```
$ ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver --since "-3min" --no-pager | grep -i miracast'
Sep 03 21:44:53 DietPi castr-receiver[23343]: ... group p2p-wlan0-0 up, RTSP on 192.168.173.1:7236, RTP on 192.168.173.1:5000
Sep 03 21:45:07 DietPi castr-receiver[23343]: ... RTSP connection from 192.168.173.1:58406
Sep 03 21:45:07 DietPi castr-receiver[23343]: ... playing 1280x720@30
Sep 03 21:45:11 DietPi castr-receiver[23343]: ... session ended: no media for 2 s, holding the group
Sep 03 21:45:11 DietPi castr-receiver[23343]: ... miracast: no media for 2 s
Sep 03 21:45:46 DietPi castr-receiver[23343]: ... RTSP connection from 192.168.173.1:42696
Sep 03 21:45:47 DietPi castr-receiver[23343]: ... playing 1280x720@30
Sep 03 21:45:51 DietPi castr-receiver[23343]: ... session ended: no media for 2 s, holding the group
Sep 03 21:45:51 DietPi castr-receiver[23343]: ... miracast: no media for 2 s
```

`no media for 2 s` fired at `21:45:11`, four seconds after the source went
silent (a 2-second stall timer plus turnaround), and logged `holding the
group` rather than removing it. The second `RTSP connection from` at
`21:45:46` — 35 seconds after the first session started and about 25 seconds
into the 30-second hold — was accepted and reached `playing 1280x720@30`
again. No `P2P_GROUP_REMOVE` appears anywhere in this window. The interface
was checked again afterward and is still `p2p-wlan0-0`:

```
$ ssh dietpi@192.168.88.157 'ip -br link | grep p2p'
p2p-wlan0-0      UP             ba:27:eb:05:9c:c1 <BROADCAST,MULTICAST,UP,LOWER_UP>
```

The interface number never incremented across the two sessions — the proof
that the group, its credentials, and the television screen all survived the
gap between them.

## Step 4: the bitrate ladder

```
$ ssh dietpi@192.168.88.157 '/tmp/loopback-source 192.168.173.1:7236 300 --drop 20'
connecting to 192.168.173.1:7236
...
< PLAY rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
< CSeq: 102
< Session: abcdef12
sending 300 access units to 192.168.173.1:5000
sent 241 datagrams
< SET_PARAMETER rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
< CSeq: 103
< Session: abcdef12
< Content-Type: text/parameters
< Content-Length: 17
< wfd_idr_request
< SET_PARAMETER rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
< CSeq: 104
< Session: abcdef12
< Content-Type: text/parameters
< Content-Length: 29
< microsoft_max_bitrate: 2000
< SET_PARAMETER rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
...
done
```

241 of 300 datagrams sent (300 minus 20% of the remaining 299, i.e. datagram 0
was never a candidate for dropping), and the sink answered with a
`microsoft_max_bitrate: 2000` `SET_PARAMETER` — the source-facing half of the
ladder.

```
$ ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver --since "-2min" --no-pager | grep -i "asking the source"'
Sep 03 21:46:14 DietPi castr-receiver[23343]: ... miracast: loss is up, asking the source for 2000 kbps
```

`asking the source for 2000 kbps` appears exactly once, not repeatedly, over
the whole run. The journal was also checked for supplicant parse errors with
a case-insensitive grep (this build logs `Line N:`, capital L):

```
$ ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver --since "-10min" --no-pager | grep -iE "failed to parse|parse error"'
(no output)
```

None found.

## Step 5: the three hardware questions

None of these were run. Each needs a person sitting at `DESKTOP-C6QHH2A`,
pressing Windows+K, entering the PIN shown on the television, and — for
question 3 — pairing and using Bluetooth headphones. There is no such person
in this session, and simulating the result would misrepresent what was
proved.

1. **Does `brcmfmac` honour the GO settings under a real radio blip?** NOT RUN
   — needs a person to cast from Windows, walk out of range for five seconds,
   and walk back, and to check the journal for `AP-STA-DISCONNECTED` during
   the blip.
2. **Does Windows honour `microsoft_max_bitrate`?** NOT RUN — needs a person
   to cast, force real loss (a large file copy over the same band, or the
   loopback source alongside a real cast), and read the `perf:` lines to see
   whether received bitrate actually falls.
3. **Does a real Bluetooth blip now survive?** NOT RUN — needs a person to
   cast, play audio to Bluetooth headphones from the same PC, and use them for
   two minutes, recording every disconnect.

## What was not run

All three of step 5's hardware questions, and only those. Everything in steps
1 through 4 — the two loopback-source flags, the cross-build and push, the
hold, the resume with the group surviving, and the bitrate ladder — was run
against real hardware (the Pi's own radio and group interface) with no
simulation and no workaround, and all four passed.

The three open questions above are the actual point of this sub-project: this
document proves the sink-side mechanics work, but whether `brcmfmac` actually
stops dropping the association during a blip, whether Windows actually turns
down the bitrate when asked, and whether a real Bluetooth radio-sharing blip
survives now, are all unmeasured. A person at the Windows PC needs to run
step 5 and update this document with the result — including a plainly stated
failure if the bitrate does not fall, since the fallback (a format change to a
smaller CEA mode) was deliberately not built in this sub-project.

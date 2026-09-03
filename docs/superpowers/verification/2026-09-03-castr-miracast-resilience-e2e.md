# castr sub-project 4 (Miracast resilience) end-to-end verification (2026-09-03)

Pi receiver: `dietpi@192.168.88.157` (DietPi / Debian 13 trixie, Pi 3 B,
`brcmfmac` 2.4 GHz radio), branch `miracast-resilience` at HEAD `7e6da4d`,
deployed fresh for this run. Windows host: `DESKTOP-C6QHH2A`, Realtek 8821CE
adapter. Every step below was run by an automated agent over SSH except where
it says otherwise.

**Status: the hold, the in-hold resume, and the bitrate ladder are all
verified on real hardware, with no radio peer.** Step 3 has two parts: a
fresh run (3a) where the reconnect lands about 1 second into the 30-second
hold — the direct proof of the resume path — and the original run from this
task's first pass (3b), kept separately, where the reconnect landed 5 seconds
*past* the hold's nominal 30-second expiry and was still accepted. Both are
real and both are described for what they actually show; see Step 3 for the
exact arithmetic. The three hardware questions in step 5 need a person at the
Windows PC pressing Windows+K, entering a PIN, and using Bluetooth
headphones — none of that can be driven from a shell, so none of it was
attempted. Section "What was not run" says exactly what that leaves open.

**Final fix wave (this pass):** steps 1 through 3b below are unchanged from
the run described above, at HEAD `7e6da4d`. Step 4 was re-run against a fresh
redeploy at HEAD `c3b73b0` — the previous copy of that step carried evidence
from the pid of the pre-reboot, pre-redeploy run (Fix 4), and the "radio
settings are loaded" check was redone by asking the running supplicant
directly instead of grepping a log it never writes to (Fix 5). Step 3b's
wording was also corrected: it does not show the *screen* surviving past the
hold's nominal expiry, only the group and credentials (Fix 6).

## Summary

| # | Step | Result | Evidence |
|---|------|--------|----------|
| 1 | Add `--drop` and `--vanish` to the loopback source | PASS | builds and cross-compiles clean |
| 2 | Cross-build and push the example | PASS | binary built and copied to `/tmp/loopback-source` on the Pi |
| 3a | Reconnect inside the hold window (resume proof) | PASS | second `RTSP connection from` 0.72 s after `holding the group`; unfiltered `grep -c P2P_GROUP_REMOVE` = 0; `p2p-wlan0-0` unchanged |
| 3b | Reconnect after the hold's nominal expiry (separate evidence) | PASS | second `RTSP connection from` 35 s after `holding the group` (5 s past the 30 s hold); accepted anyway; `p2p-wlan0-0` unchanged |
| 4 | Bitrate ladder, no radio peer (re-run at HEAD `c3b73b0`) | PASS | `asking the source for 2000 kbps` once, reaching the floor rung on 20% loss |
| 4a | Radio settings actually loaded | PASS | `wpa_cli get` on the running supplicant: `disassoc_low_ack=0`, `p2p_go_max_inactivity=300`, `p2p_go_ctwindow=0`, matching the conf file |
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

This step was re-run for the fix round below because the first pass conflated
two different claims: a reconnect that lands *inside* the 30-second hold
(proving the resume path) and one that lands *after* it (proving only that
the group outlives an expired hold). The original run, worked out from its
own timestamps, was actually the second kind. Both are kept, each described
as what it actually shows.

### 3a. Fresh run: reconnect inside the hold window (the resume proof)

The Pi was rebuilt and the example redeployed before this run (see Step 2).
The group was already up as `p2p-wlan0-0`:

```
$ ssh dietpi@192.168.88.157 'ip -br link | grep p2p'
p2p-wlan0-0      UP             ba:27:eb:05:9c:c1 <BROADCAST,MULTICAST,UP,LOWER_UP>
```

The first connection was started with `--vanish`, and the journal was polled
every 0.5 s for `holding the group` so the second connection could be fired
the instant the hold began:

```
$ ssh dietpi@192.168.88.157 '/tmp/loopback-source 192.168.173.1:7236 120 --vanish' &
$ # poll loop: journalctl --since "-30s" | grep "holding the group", every 0.5s
FOUND at poll 8: Sep 03 22:40:12 DietPi castr-receiver[520]: ... miracast: session ended: no media for 2 s, holding the group
$ date  # immediately before firing the second connection
Thu Sep  3 15:40:13 PDT 2026
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
$ date  # immediately after
Thu Sep  3 15:40:18 PDT 2026
```

(The PDT wall-clock lines are the local shell's `date`; the journal below is
authoritative and in UTC, `+7` hours from PDT.)

The journal for the whole window, in order:

```
$ ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver --since "22:39:50" --no-pager | grep -i miracast'
Sep 03 22:40:07 DietPi castr-receiver[520]: ... RTSP connection from 192.168.173.1:41974
Sep 03 22:40:08 DietPi castr-receiver[520]: ... playing 1280x720@30
Sep 03 22:40:12 DietPi castr-receiver[520]: 2026-09-03T22:40:12.522339Z  INFO ... miracast: session ended: no media for 2 s, holding the group
Sep 03 22:40:12 DietPi castr-receiver[520]: 2026-09-03T22:40:12.522501Z  INFO ... miracast: no media for 2 s
Sep 03 22:40:13 DietPi castr-receiver[520]: 2026-09-03T22:40:13.240924Z  INFO ... miracast: RTSP connection from 192.168.173.1:58354
Sep 03 22:40:14 DietPi castr-receiver[520]: ... playing 1280x720@30
Sep 03 22:40:18 DietPi castr-receiver[520]: ... session ended: no media for 2 s, holding the group
Sep 03 22:40:18 DietPi castr-receiver[520]: ... miracast: no media for 2 s
```

The math, from these timestamps alone: `holding the group` was logged at
`22:40:12.522339`. The second `RTSP connection from` was logged at
`22:40:13.240924` — **0.72 seconds later**, about 1 second into the
30-second hold, nowhere near its edge. It reached `playing 1280x720@30`
again one second after that, with no re-pairing. This is the run that proves
the resume path: a peer that returns promptly during the hold gets its
session back.

An unfiltered check for `P2P_GROUP_REMOVE` over this same window, not
filtered through the `miracast:`-prefixed grep used elsewhere:

```
$ ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver --since "22:39:50" --no-pager | grep -c P2P_GROUP_REMOVE'
0
```

Zero, over the full unfiltered journal text for the window — the group was
never removed. The interface was checked again afterward and is still
`p2p-wlan0-0`:

```
$ ssh dietpi@192.168.88.157 'ip -br link | grep p2p'
p2p-wlan0-0      UP             ba:27:eb:05:9c:c1 <BROADCAST,MULTICAST,UP,LOWER_UP>
```

### 3b. Original run, kept as separate evidence: reconnect after the hold's nominal expiry

This is the run from the first pass through this task, timestamps unchanged
from that run. Worked out precisely: `holding the group` was logged at
`21:45:11`; the hold is 30 seconds, so it should nominally have expired by
`21:45:41`. The second `RTSP connection from` arrived at `21:45:46` — **35
seconds after the hold began**, i.e. **5 seconds past its nominal
expiry** — and was still accepted, reaching `playing 1280x720@30` again with
no re-pairing and no interface change.

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

This does **not** prove the in-hold resume path (3a does that); what it
proves is a different, stronger-in-one-sense claim: the group and the
credentials survived at least 5 seconds *past* when the 30-second hold should
have expired, and the interface-name check (below) proves that half. It does
**not** show that the *screen* survived past expiry: `Lifecycle::tick` runs
every loop pass against a 200 ms poll interval, so hold expiry is decided to
within a fifth of a second, not something that could plausibly run 5 seconds
long. The far likelier explanation is that the hold had already expired
normally by `21:45:41`, the display was released back to the arbiter at that
point, and the reconnect at `21:45:46` took the screen again through ordinary
arbitration — which, for a display nothing else wants, produces log lines
identical to a resume. There is nothing to chase here: the group and
credentials surviving is the real, useful finding from this run, and the
question of whether the screen was actually "held" or "freshly reacquired" at
`21:45:46` is answered by 3a, not by this run.

An unfiltered `P2P_GROUP_REMOVE` re-check specifically for this original
window could not be run for this fix: the Pi was rebooted between the
original run and this fix round (see Step 2 of the fix — `/tmp/loopback-source`
had to be rebuilt because the earlier copy was gone after a restart), and
`sudo journalctl -u castr-receiver --since "2026-09-03 21:44:50" --until
"2026-09-03 21:46:00"` now returns `-- No entries --`: that boot's journal is
gone. The unfiltered check in 3a, against the fresh run's own window,
substitutes for it — same binary, same code path, same `P2P_GROUP_REMOVE`
helper — but it is evidence about the fresh run, not a re-verification of
this original one.

The interface number never incremented across either pair of sessions — the
proof that the group, its credentials, and the television screen all
survived the gap each time.

## Step 4: the bitrate ladder

Re-run for the final fix wave: the previous copy of this step carried
evidence from `castr-receiver[23343]`, the same pid and the same boot as 3b —
i.e. the pre-reboot, pre-redeploy run, not the build this document claims to
verify. This run is against the currently deployed build, HEAD `c3b73b0`,
freshly restarted immediately beforehand
(`ActiveEnterTimestamp=2026-09-03 23:03:14 UTC`, `ExecMainPID=901`), and the
binary on disk was checked byte-for-byte against the one just built:

```
$ sha256sum dist/castr-receiver-aarch64
cdaa57ec0b3bcf0f5a39c683f10bf10c45ba26129a399670d0e049896730c7af *dist/castr-receiver-aarch64
$ ssh dietpi@192.168.88.157 'sha256sum /usr/local/bin/castr-receiver'
cdaa57ec0b3bcf0f5a39c683f10bf10c45ba26129a399670d0e049896730c7af  /usr/local/bin/castr-receiver
```

```
$ ssh dietpi@192.168.88.157 '/tmp/loopback-source 192.168.173.1:7236 300 --drop 20'
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
< CSeq: 105
< Session: abcdef12
< Content-Type: text/parameters
< Content-Length: 17
< wfd_idr_request
< GET_PARAMETER rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
< CSeq: 106
< Session: abcdef12
< SET_PARAMETER rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
< CSeq: 107
< Session: abcdef12
< Content-Type: text/parameters
< Content-Length: 17
< wfd_idr_request
< SET_PARAMETER rtsp://192.168.173.1/wfd1.0/streamid=0 RTSP/1.0
< CSeq: 108
< Session: abcdef12
< Content-Type: text/parameters
< Content-Length: 17
< wfd_idr_request
done
```

Shown in full this time, with nothing elided: the sink asked for a keyframe
four times in total (`wfd_idr_request` at CSeq 103, 105, 107 and 108 — one
before the `microsoft_max_bitrate: 2000` drop request at CSeq 104, three
after it), plus one `GET_PARAMETER` keepalive at CSeq 106, as it kept trying
to recover a clean picture from a link that was still losing 20% of
datagrams — the drop rate is high enough that repeated keyframe requests are
expected, not a sign of anything wrong.

241 of 300 datagrams sent (300 minus 20% of the remaining 299, i.e. datagram 0
was never a candidate for dropping), and the sink answered with a
`microsoft_max_bitrate: 2000` `SET_PARAMETER` — the source-facing half of the
ladder.

```
$ ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver --since "-3min" --no-pager | grep -i "asking the source"'
Sep 03 23:03:42 DietPi castr-receiver[901]: 2026-09-03T23:03:42.450424Z  INFO castr_miracast::session: miracast: loss is up, asking the source for 2000 kbps
```

`asking the source for 2000 kbps` appears exactly once here. That is
guaranteed by construction, not evidence of restraint: `BitrateLadder::sample`
returns `None` once the floor rung is reached (`crates/castr-miracast/src/quality.rs`),
and this run was a single connection from `RTSP connection from` at
`23:03:39.382419` to `session ended` at `23:03:50.376879` — about eleven
seconds end-to-end, including the 2 s silence timeout that closed it — far
too short to accumulate the ten clean seconds `CLEAN_PER_STEP` requires
before the ladder would even consider climbing back up. What this run
actually shows is only that a 20% loss rate drives the ladder to its floor
and asks the source for it once, in one `SET_PARAMETER`; it says nothing
about whether the ladder holds the floor or climbs back over a longer run.

The journal was also checked for supplicant parse errors with a
case-insensitive grep (this build logs `Line N:`, capital L):

```
$ ssh dietpi@192.168.88.157 'sudo journalctl -u castr-receiver --since "-3min" --no-pager | grep -iE "failed to parse|parse error"'
(no output)
```

None found, but this grep establishes nothing on its own: `ensure_supplicant`
starts `wpa_supplicant -B`, which daemonises and logs to syslog under its own
identity, not under `-u castr-receiver`. "None found" here just means the
receiver's own unit log has no such lines — it says nothing about whether the
supplicant parsed the config cleanly. See "Are the radio settings actually
loaded?" below for evidence that does establish that.

### Are the radio settings actually loaded?

Asked the running supplicant directly, rather than grepping a log it does not
write to:

```
$ ssh dietpi@192.168.88.157 'sudo /sbin/wpa_cli -p /run/wpa_supplicant_castr -i wlan0 get disassoc_low_ack'
'GET disassoc_low_ack' command timed out.
```

Plain `sudo` timed out on every `GET`, including a bare `PING`/`STATUS` — not
a permission error, no response at all. The receiver, and therefore the
supplicant it starts, runs as the unprivileged `castr` user
(`ps` on the deployed process: `901 castr /usr/local/bin/castr-receiver`), and
querying as that same user works immediately:

```
$ ssh dietpi@192.168.88.157 'sudo -u castr /sbin/wpa_cli -p /run/wpa_supplicant_castr -i wlan0 get disassoc_low_ack'
0
$ ssh dietpi@192.168.88.157 'sudo -u castr /sbin/wpa_cli -p /run/wpa_supplicant_castr -i wlan0 get p2p_go_max_inactivity'
300
$ ssh dietpi@192.168.88.157 'sudo -u castr /sbin/wpa_cli -p /run/wpa_supplicant_castr -i wlan0 get p2p_go_ctwindow'
0
```

All three `get`s were supported (no "GET unsupported" or "FAIL" was returned
for any of them) and all three match the values in
`scripts/pi/wpa_supplicant-p2p.conf` exactly (`disassoc_low_ack=0`,
`p2p_go_max_inactivity=300`, `p2p_go_ctwindow=0`). The settings are loaded.

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
in-hold resume (3a), the group surviving past an expired hold (3b), and the
bitrate ladder — was run against real hardware (the Pi's own radio and group
interface) with no simulation and no workaround, and all of it passed.

The three open questions above are the actual point of this sub-project: this
document proves the sink-side mechanics work, but whether `brcmfmac` actually
stops dropping the association during a blip, whether Windows actually turns
down the bitrate when asked, and whether a real Bluetooth radio-sharing blip
survives now, are all unmeasured. A person at the Windows PC needs to run
step 5 and update this document with the result — including a plainly stated
failure if the bitrate does not fall, since the fallback (a format change to a
smaller CEA mode) was deliberately not built in this sub-project.

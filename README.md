# castr

castr is a screen-casting protocol in the spirit of Miracast with both
endpoints under our control, built to fix Miracast's four recurring failures:
flaky discovery and pairing, mid-session disconnects, lag and stutter, and
broken audio sync. It runs over the existing LAN on QUIC with one-time PIN
pairing that pins certificates, resumable sessions, receiver stats driving
adaptive bitrate, and Opus audio as the master clock. This repository is
sub-project 1: the core protocol, a zero-install Windows sender, and a desktop
receiver that runs on Windows and cross-compiles to Linux (Raspberry Pi 3 is
the first hardware target). See `docs/superpowers/specs/2026-09-01-castr-core-design.md`.

## Build prerequisites (Windows)

- Rust stable (`rustup default stable`).
- CMake 3.x or 4.x, on `PATH` — needed for the bundled `openh264`, `opus`, and
  SDL2 builds. `.cargo/config.toml` sets `CMAKE_POLICY_VERSION_MINIMUM = "3.5"`
  so bundled SDL2's pre-3.5 `CMakeLists.txt` still configures under CMake 4.x.
- Visual Studio Build Tools with the "Desktop development with C++" workload
  (MSVC toolchain and Windows SDK).

No FFmpeg, no system media libraries, and no runtime DLLs: the sender is a
single portable exe.

```
cargo build --release
```

Binaries land in `target/release/castr-sender.exe` and `castr-receiver.exe`.

## Installing

There is an installer, and there is no need to use it: `castr-sender.exe` is a
single portable exe that runs from wherever you put it. The installer exists
for machines where a Start Menu entry, a working `castr-sender` on the PATH and
a pre-authorised firewall rule are worth having — and because an MSI can be
deployed by group policy to managed machines.

Prebuilt installers are attached to each release:
<https://github.com/ssan9876/castr/releases/latest>. There are two, holding the
same exe:

| | Installs to | Elevation | Firewall rule |
|---|---|---|---|
| `castr-<version>-x64.msi` | `C:\Program Files\castr` | asks once | included |
| `castr-<version>-x64-peruser.msi` | `%LOCALAPPDATA%\Programs\castr` | none at all | added separately |

The per-machine one is for a machine you administer, and is the one group
policy can deploy. The per-user one prompts for nothing at all — it writes only
inside your own profile — and is the answer when you do not have administrator
rights, or would rather not be asked for them to install a screen caster.

To build them yourself:

```
powershell -File scripts\windows\build-msi.ps1              # -> dist\castr-<version>-x64.msi
powershell -File scripts\windows\build-msi.ps1 -PerUser     # -> dist\castr-<version>-x64-peruser.msi
```

Both are built from the same `packaging\windows\castr.wxs`, which the WiX
preprocessor takes two ways through. They are separate packages with separate
UpgradeCodes and separate component GUIDs, not two builds of one package: the
same component GUID pointing at two different directories is a component-rules
violation, and Windows would reference-count the exe in Program Files and the
one in LocalAppData as though they were the same file.

Double-clicking either runs a wizard: licence, install location, then the
things you can decline — Start Menu shortcut, Desktop shortcut, add to PATH,
and, in the per-machine package, a Windows Firewall rule.

### The firewall rule

Only the per-machine package carries one, and that is not an omission. A
firewall rule lives in the machine-wide policy store; there is no such thing as
a per-user rule, so no installer can add one without elevation. Once, from an
administrator terminal:

```
castr-sender firewall --allow      # castr-sender firewall  says whether it is there
```

It is worth having. `miracast-cast` listens on 7236 because **real Miracast
sinks are the TCP initiator** — a wireless display adapter dials you and listens
on nothing itself — so without the rule Windows drops that connection and the
cast times out. What still works without it: casting to a castr receiver, and
to castr's own sink. Both are dialled out from here, and nothing else the
sender does wants inbound traffic — RTP is send-only and the control socket is
loopback.

The command names whichever exe is running, which the installer's rule cannot:
that one names the installed path, so the portable exe run from anywhere else
was never covered by it. `castr-sender firewall --remove` takes it away again.

Uninstalling is Windows' own Apps & Features entry. It removes the PATH entry,
the shortcuts and (per-machine) the firewall rule as well as the files — but
**deliberately keeps `%APPDATA%\castr\`**, which holds the identity certificate
and `paired.toml`. Deleting those would silently discard every pairing,
including the Miracast ones, and reinstalling would mean pairing everything
again. A rule added by `firewall --allow` is yours to remove.

Building the MSI needs the WiX toolset once per machine, as a per-user tool
with no administrator rights:

```
dotnet tool install --global wix --version 5.0.2
wix extension add --global WixToolset.UI.wixext/5.0.2
wix extension add --global WixToolset.Firewall.wixext/5.0.2
```

WiX 5 rather than the current 7, which requires accepting the Open Source
Maintenance Fee licence; 5 is supported and carries no such condition.

**The MSIs are unsigned**, so Windows will call the publisher unknown and
SmartScreen will warn about them. Signing needs a code-signing certificate,
which is a purchase rather than a code change.

`scripts\windows\verify-msi.ps1` installs the package silently, checks
everything it claims to do, uninstalls it, and checks it left nothing behind.
Run it from an elevated PowerShell for the per-machine package, and with
`-PerUser` from an ordinary one for the other — running that one unelevated is
most of the point, since what it is testing is that nothing asks. It also
checks the per-user package's absences: no firewall rule, nothing in Program
Files, the machine PATH untouched.

The application icon lives at `assets/castr.ico` and is committed;
`scripts\windows\make-icon.ps1` regenerates it if the artwork changes.

Releases are cut by pushing a tag that matches the workspace version — `v0.1.0`
for `version = "0.1.0"`. `.github/workflows/release.yml` then builds the MSI on
a Windows runner and attaches both packages to the release; it refuses a tag
that disagrees with `Cargo.toml`, so a release can never carry an installer
that names a different version. The same workflow can be run by hand from the
Actions tab: it builds the MSI and leaves it as a run artifact, or, with
`publish` ticked, tags the commit it ran on and makes the release from there.

## Running

Start the receiver on the display machine:

```
castr-receiver --name "living room" [--fullscreen] [--max-bitrate 40000000] [--decoder auto|mf|sw]
```

It advertises itself over mDNS (`_castr._udp.local`) and answers UDP broadcast
probes on port 7331; QUIC listens on 7332.

### Pairing and casting from the GUI

Run `castr-sender` with no arguments (or double-click the exe). It lists
everywhere the screen can go — castr receivers *and* Miracast displays — in one
list, each row saying which it is, with Pair and Cast buttons and a Game/Quality
toggle. Pairing a receiver is two-phase: press Pair, read the 6-digit PIN shown
on the receiver's screen and window title, type it into the sender. Later
connections are silent.

A Miracast display has no separate Pair step — it pairs during the connect,
asking for its 8-digit PIN only the first time. Finding displays takes about a
minute (the radio enumeration is slow, and the window says so while it waits),
and most displays appear only while their Screen Mirroring page is open.

Where the machine has more than one monitor, a picker chooses which to cast; a
rotated monitor is labelled, because the cast of one arrives sideways.

The window has not been click-tested end to end — see
`docs/superpowers/verification/2026-09-04-castr-miracast-gui-e2e.md` for exactly
what is proven and what is not.

### Pairing and casting from the CLI

```
castr-sender list
castr-sender pair "living room"          # prompts for the PIN shown on the receiver
castr-sender cast "living room" [--mode game|quality] [--fps 30|60] [--max-bitrate N] [--duration SECS]
```

`--duration N` stops the cast automatically after N seconds; it exists mainly
for testing and smoke runs. Ctrl-C stops a cast cleanly otherwise.

On a multi-monitor machine the cast is the first duplication output.
`CASTR_OUTPUT=1` (or 2, ...) casts another one; there is no UI for it yet, and
the numbering is the graphics adapter's, not the order shown in Windows display
settings, so it is worth a short test cast to see which is which.

### Casting to an ordinary Miracast display

castr can also act as a Wi-Fi Display *source*, casting to a television, dongle
or any other Miracast sink rather than to a castr receiver:

```
castr-sender miracast-list
castr-sender miracast-cast "Living Room TV" [--mode quality|game] [--duration SECS]
                                            [--pair auto|push|pin]
castr-sender miracast-cast 192.168.173.1:7236       # or by address
castr-sender miracast-status                        # what it is sending
castr-sender miracast-stop                          # end it
castr-sender firewall                               # is the inbound rule there?
castr-sender logs                                   # where this run was logged
```

If a cast to a real display times out having never connected, check `firewall`
first: the display dials *us* on 7236, and without the rule Windows drops that
connection silently. See [The firewall rule](#the-firewall-rule).

`miracast-list` shows the Wi-Fi Direct devices in range and which of them are
displays, with the RTSP port and bandwidth each advertises.

Given a name, castr finds the display, pairs with it the first time, forms the
Wi-Fi Direct group, casts, and drops the group when it exits. Most displays
advertise only while their screen mirroring page is open, so the command waits
up to a minute for one to appear.

How it pairs is read from the display's own advertisement. A display offering
push-button is paired with by button, which needs nobody present; one that only
offers a PIN prompts for the PIN it shows. `--pair pin` or `--pair push` forces
the choice. `miracast-list` shows which each display offers.

The picture mode is negotiated with the display: castr offers every mode it can
encode that the display's advertised bandwidth will carry, and takes the first
the display also offers. `--mode quality` prefers a bigger picture (1080p30 over
720p60), `--mode game` a faster one — the same distinction the toggle makes for
castr's own protocol.

A cast ends on `--duration`, on Ctrl-C, or on `miracast-stop` from another
shell, and all three tear the session down properly and drop the Wi-Fi Direct
group. Only one Miracast cast runs at a time; a second is refused, naming the
display already being cast to.

One caveat about Ctrl-C, which is PowerShell's behaviour rather than castr's:
**if you redirect the output to a file in PowerShell** (`castr-sender
miracast-cast ... > log.txt`), PowerShell terminates the process on Ctrl-C
instead of letting it handle the event, so no teardown runs. Without the
redirect it works, and so does `cmd`. If you want a log *and* a clean exit, stop
the cast with `miracast-stop` rather than Ctrl-C. A cast killed this way leaves
a stale record, which the next `miracast-status`, `miracast-stop` or
`miracast-cast` cleans up by itself.

If the display reports that it is losing packets, it asks the source to send
less, and castr obeys — dropping the encoder's bitrate rather than pushing the
same rate into a link that is already failing. `CASTR_MIRACAST_DROP_PCT=10`
deliberately discards that percentage of outgoing media, which is how that path
is tested without a degraded radio; it is a test switch and logs a warning
whenever it is set.

`miracast-status` reports the negotiated mode, what the display said it can
carry, the throughput actually being sent, and how long ago the display last
answered a keep-alive. **Every number except that last one is sent-side.**
Wi-Fi Display makes the source authoritative and gives it no back-channel of
receiver statistics, so unlike a cast to a castr receiver there is no round
trip time and no loss figure — nothing here knows what arrived, only what was
sent. `repeated_frames` counts frames re-sent because the desktop did not
change, which is worth watching: a still screen is normal, and without that
count it looks identical to a capture that has stopped.

Given an address instead, the radio is skipped entirely: on the ordinary LAN
that is Miracast over Infrastructure, and over an existing Wi-Fi Direct group it
is ordinary Miracast — the media path does not care which. `CASTR_OUTPUT`
chooses the monitor here too.

The picture is negotiated with the display and sent as H.264 in MPEG-TS over
RTP, with audio as LPCM. See the "Known gaps" below before relying on it: it has
only ever been tested against castr's own sink.

### Logs

Every run writes a log, so a session on real hardware can be handed over whole
rather than described from memory.

```
castr-sender logs           # where they are, newest first
castr-sender logs --open    # open the folder
```

They live in `%APPDATA%\castr\logs\` on Windows and
`~/.config/castr/logs/` on the Pi, one file per run, named for the moment it
started: `castr-sender-20260910-140311-04216.log`. The window offers **Open log
folder** at its foot, because the double-clicked exe has no console and its
user may have no terminal.

The console keeps exactly the output it always had. The file gets the same
events plus everything castr's own crates log at `debug`, which is the part
that was previously invisible — and the GUI path detaches its console
entirely, so before this a crash there left nothing behind at all. Panics go to
the log too. `RUST_LOG` still works and turns both up.

A log opens with what was run, when, on what:

```
app        castr-sender 0.1.0
started    2026-09-10 14:03:11Z
command    castr-sender miracast-cast "Living Room TV"
os         windows x86_64
exe        C:\Program Files\castr\castr-sender.exe
```

A Miracast cast also records whether the firewall rule covered the exe before
it tried to connect, since that is the first question any failure to connect
raises. `diagnose`'s findings go into the log as well.

**What a log contains, since handing one over should be an informed act:** the
command line, this machine's OS, the exe's path, and then the run — display
names, Wi-Fi Direct device names, and the addresses on the peer-to-peer link.
It does not contain pairing PINs, the identity key, or anything from
`paired.toml`; none of those are logged anywhere, at any level.

The newest 30 runs are kept and older ones deleted, so a directory cannot grow
without bound on an SD card. Short runs count towards that, so a log worth
keeping is worth copying somewhere else. `castr-sender logs` deliberately
writes no log of its own — otherwise the newest file would always be the one
that just listed the directory.

### Wi-Fi health check

Miracast drops are usually caused by the sending machine, not the display.

```
castr-sender diagnose         # report only
castr-sender diagnose --fix   # offers each safe fix, prompting for every one
```

It checks whether the graphics and Wi-Fi drivers support wireless display, how
old the driver is, whether the adapter shares one antenna with Bluetooth,
whether the station link is on a different band from the sink, the signal
strength, and the three power settings that park a radio mid-session. The three
power settings are the only things it will change, always after a prompt, and
it prints the command to undo each one before applying it. It never touches
driver settings and never disables Bluetooth.

It cannot change how Windows implements Miracast, which lives in the operating
system. For your own machines, castr's own protocol avoids the radio entirely.

### Where state lives

Certificates, keys, and the paired-peer list live under the platform config
directory, `%APPDATA%\castr\` on Windows and `~/.config/castr/` on Linux, split
into `sender/` and `receiver/`. Each holds `identity.crt`, `identity.key`, and
`paired.toml`. Unpairing is deleting the entry from `paired.toml`.

## Known deviations from the spec

- **Encoder tuning on the software backend.** The `openh264` crate's builder
  exposes no GOP-size or profile knob, so spec 8.2's "2 s GOP" and profile
  selection apply only to the Media Foundation encoder. The software backend is
  a fallback for tests and non-Windows targets.
- **MF decoder outputs CPU NV12.** Spec 7.2 asks for D3D11-backed NV12 textures
  from the Media Foundation decoder; the current implementation copies decoded
  frames to CPU memory before uploading them to SDL.
- **Reconnecting to a restarted receiver.** Spec 10 expects a resume against a
  restarted receiver to fail with `Error { code: 1 }`. In practice the sender
  transparently establishes a fresh session instead (no re-pairing needed,
  since the certificate fingerprint is persisted) and the cast continues. This
  is a better outcome, but it is not what the spec describes.
- **Console flash on double-click.** `castr-sender` is a console-subsystem exe
  so `list`, `pair`, and `cast` have working stdin/stdout. Double-clicking it
  briefly flashes a console window, which the GUI path closes immediately via
  `FreeConsole()`.

## Raspberry Pi receiver

Verified on a Pi 3 Model B running DietPi (Debian 13, 64-bit) with hardware
H.264 decode. Do not build on the Pi; cross-compile with Docker and deploy:

```
bash scripts/pi/deploy.sh dietpi@<pi>     # first run installs everything, later runs update + restart
```

The first run copies `setup.sh` over and runs it as root: it enables full KMS
and `gpu_mem=128` in `config.txt` (the VideoCore firmware does not start the
decoder below 64 MB), loads `bcm2835_codec` at boot, installs runtime packages,
creates a `castr` system user, and installs `castr-receiver.service`. If it
prints REBOOT REQUIRED, reboot; the receiver is then on screen about 20 s after
power-on, named after the hostname, and pairing state lives in
`/var/lib/castr/config/castr/receiver/`. Pair once from each sender after setup.

Logs: `journalctl -u castr-receiver -f`. The `perf:` line every 5 s shows
decode and present times; on a Pi 3 expect 1080p30 with decode under 15 ms. Its
current format is:

```
perf: pictures P (decode calls C avg X ms max Y ms, drain avg X ms max Y ms), presented N present avg X ms max Y ms, queue Q, dropped D
```

`--decoder auto` (default) uses V4L2 hardware decode and falls back to openh264
if `/dev/video10` is missing; `--decoder sw` forces software.

Known limitations: the V4L2 decoder's dominant per-frame cost is copying each
picture out of its uncached CAPTURE mapping (~15 ms at 1080p on a Pi 3); there
is no zero-copy present path yet, so that copy stays on the hot path even
though DMABUFs are already exported for every CAPTURE buffer (unused for
now). Quality mode's `perf:` line legitimately shows `queue 5-7` - its 150 ms
playout delay means several pictures are buffered waiting to be shown, not
that anything is backing up.

The receiver asks SDL for its `opengles2` renderer on Linux. SDL's default
order tries desktop `opengl` first, and on a Pi without libGL that renderer is
created with shaders disabled and draws a black screen. Debug switches for a
headless box: `CASTR_DUMP_FRAME=/tmp/f.raw` writes the rendered output every
2 s (RGB24 with a `CASTRDUMP w h` header), `CASTR_SDL_VERBOSE=1` prints SDL's
internal log, and `CASTR_SOFTWARE_RENDER=1 SDL_RENDER_DRIVER=software` bypasses
the GPU entirely.

Use a proper 2.5 A power supply. On a weak one the kernel logs "Undervoltage
detected", the SD card slows to a crawl (2 MB/s reads instead of 20+), and
package installs stall for hours.

### Manual alternative

`deploy.sh` and the systemd unit are the supported path; running the binary by
hand under a login shell still works for one-off testing. Cross-compile, then
copy the binary to the Pi (DietPi ships dropbear without SFTP, so `scp` fails;
pipe it over ssh):

```
bash scripts/pi/build-pi.sh        # -> dist/castr-receiver-aarch64 (~10 MB, needs only glibc/libstdc++)
cat dist/castr-receiver-aarch64 | ssh dietpi@<pi> 'mkdir -p ~/bin && cat > ~/bin/castr-receiver && chmod +x ~/bin/castr-receiver'
```

One-time Pi setup, no desktop needed. Bundled SDL2 draws straight to HDMI via
KMS/DRM, which needs the full KMS driver and device-group membership:

```
sudo apt install -y libstdc++6 libasound2 libdrm2 libgbm1 libgles2 libegl1
sudo sed -i 's/^#dtoverlay=vc4-kms-v3d.*/dtoverlay=vc4-kms-v3d/' /boot/firmware/config.txt
sudo usermod -aG video,render,input,audio $USER
sudo reboot
```

Run it from an SSH session or the console (not inside a desktop):

```
SDL_VIDEODRIVER=kmsdrm ~/bin/castr-receiver --name pi --fullscreen
```

The generic Linux recipe (untested beyond the Docker image) is the package list
in `scripts/pi/Dockerfile` plus `LIBOPUS_LIB_DIR=/usr/lib/<triplet>
OPUS_NO_PKG_CONFIG=1` so Opus links statically; without those the audiopus
crate links `libopus.so` dynamically or tries to run autotools.

## Casting from Windows without installing anything

The Pi receiver also answers Miracast, so a Windows PC can cast to it with no
castr software installed at all.

1. Press **Windows+K** on the PC and pick the Pi from the list. It appears
   under the receiver's name (`--miracast-name` overrides it, and it defaults
   to the hostname).
2. The television shows an eight-digit PIN. Type it on the PC.
3. The desktop appears. The PC remembers the Pi, so later casts skip the PIN.

Three limits, stated plainly:

- **720p30.** The Pi's radio is 2.4 GHz only, and 1080p over it drops frames
  rather than degrading gracefully. The sink offers 720p30 and nothing else.
- **No HDCP.** Protected video � Netflix, Amazon, most streaming apps � shows
  as a black rectangle. That needs licensed keys, which castr does not have.
  Everything else mirrors normally.
- **One protocol at a time.** The Pi has one screen. Whichever protocol
  connects first owns it until it disconnects; the other is refused with
  "display busy" rather than taking the screen from someone mid-presentation.

If the link wobbles, the Pi asks your PC to send less data rather than ending
the session, and if the connection does break outright, the Pi keeps the
group and your screen for thirty seconds so you come straight back with no
PIN. A drop that lasts longer than that returns the Pi to its idle screen, and
you can reconnect from Windows+K without re-pairing.

The sink runs by default when a wireless interface exists. `--miracast off`
turns it off, `--miracast on` forces it on, and `--miracast-channel 1|6|11`
pins the Wi-Fi Direct channel instead of picking the least busy one.

If Windows drops the cast repeatedly, run `castr-sender diagnose` on the PC: it
checks the local causes � a shared Wi-Fi/Bluetooth antenna, adapter power
saving, driver age � and offers to fix the safe ones.

## Known gaps

- The mouse cursor is composited into the cast, and a delta frame that loses a
  fragment is now repaired by NACK when the repair can arrive before the jitter
  buffer's 150 ms hold expires. A continuously moving cursor costs about
  98 kbps; a still one costs nothing measurable. The repair's benefit is
  unmeasured on hardware: over six five-minute casts at 0%, 0.5% and 2% induced
  loss, the Pi 3 B never entered the hold the repair shortens, so there was
  nothing to improve — see
  `docs/superpowers/verification/2026-09-03-castr-cast-quality-e2e.md`.
- Casting a monitor that Windows has rotated arrives rotated: the capture does
  not consult the duplication API's rotation.
- From the CLI, only the first monitor is cast unless `CASTR_OUTPUT` names
  another duplication output. The GUI has a picker; the CLI does not.
- PIN pairing locks out for 60 s after 3 failed attempts within a minute.
- V4L2 hardware decode and DRM/KMS output landed in sub-project 2.
- Casting *to* an ordinary Miracast display works against a real wireless
  display adapter: paired by push-button with nobody present, negotiated
  1080p30, and a picture on screen. See
  `docs/superpowers/verification/2026-09-05-castr-miracast-interop-e2e.md`.
  No **television** has been cast to yet — the Samsung, LG and TCL sets in
  range advertise but have not been tried — so treat televisions as unproven.
  HDCP is not supported at all, so a display that *requires* content protection
  cannot be cast to, and that refusal path is untested.
- Some displays invalidate their pairing after every session. castr notices an
  association failure and re-pairs by itself when the display pairs by button,
  since that costs nothing; a display that would prompt for a PIN is only
  re-paired when the radio's own words suggest the stored pairing is stale.
- A cast to a Miracast display has been verified for ten minutes against castr's
  own sink, with no dropped frames, but its audio has never been listened to and
  lip sync is unmeasured.
- The bitrate budget caps *video* at the ceiling the display advertises, but
  the wire also carries uncompressed LPCM audio (1.536 Mbps) and MPEG-TS/RTP/IP
  framing on top. Measured against the Pi, which advertises 10 Mbps: 8 Mbps of
  video becomes about 10.4 Mbps sent. The Pi does not police its figure and
  drops nothing, but a display that enforces its own ceiling may refuse us.
  Unresolved until a real display can be tested — see
  `docs/superpowers/verification/2026-09-04-castr-miracast-control-e2e.md`.
- Lip sync has not been measured against ITU-R BT.1359 in either mode, and
  Miracast audio is carried as uncompressed LPCM.

## Testing

```
cargo test --workspace
cargo clippy --workspace --tests
```

The Media Foundation encoder/decoder and Desktop Duplication tests run only on
Windows; a few tests that need a real display or network are `#[ignore]`d.

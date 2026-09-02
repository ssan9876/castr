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

## Running

Start the receiver on the display machine:

```
castr-receiver --name "living room" [--fullscreen] [--max-bitrate 40000000] [--decoder auto|mf|sw]
```

It advertises itself over mDNS (`_castr._udp.local`) and answers UDP broadcast
probes on port 7331; QUIC listens on 7332.

### Pairing and casting from the GUI

Run `castr-sender` with no arguments (or double-click the exe). It lists
discovered receivers with Pair and Cast buttons and a Game/Quality toggle.
Pairing is two-phase: press Pair, read the 6-digit PIN shown on the receiver's
screen and window title, type it into the sender. Later connections are silent.

### Pairing and casting from the CLI

```
castr-sender list
castr-sender pair "living room"          # prompts for the PIN shown on the receiver
castr-sender cast "living room" [--mode game|quality] [--fps 30|60] [--max-bitrate N] [--duration SECS]
```

`--duration N` stops the cast automatically after N seconds; it exists mainly
for testing and smoke runs. Ctrl-C stops a cast cleanly otherwise.

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

## Known gaps

- The mouse cursor is not composited into the cast yet.
- Only keyframes are NACK-repaired. A delta frame that loses a fragment costs
  a 150 ms hold and a fresh keyframe; on a Pi 3 over Ethernet that happens a
  few times a minute.
- PIN pairing locks out for 60 s after 3 failed attempts within a minute.
- V4L2 hardware decode, DRM/KMS output, and Miracast sink mode are later
  sub-projects.

## Testing

```
cargo test --workspace
cargo clippy --workspace --tests
```

The Media Foundation encoder/decoder and Desktop Duplication tests run only on
Windows; a few tests that need a real display or network are `#[ignore]`d.

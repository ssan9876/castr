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

## Linux build (not yet verified)

The receiver and the platform-independent crates are meant to build on Linux,
but this has not been run end-to-end yet. From WSL Ubuntu or a Linux host:

```
sudo apt update && sudo apt install -y build-essential cmake pkg-config libx11-dev libxext-dev libasound2-dev
cd /mnt/d/miracast
source "$HOME/.cargo/env"
cargo build -p castr-receiver -p castr-proto -p castr-media -p castr-net --target-dir target-linux
cargo test -p castr-proto -p castr-media -p castr-net --target-dir target-linux
cargo tree -p castr-receiver --target-dir target-linux | grep -i windows   # expect no output
```

## Known gaps

- The mouse cursor is not composited into the cast yet.
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

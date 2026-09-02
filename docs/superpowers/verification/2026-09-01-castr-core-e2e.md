# castr-core end-to-end verification (2026-09-01/02)

Machine: single Windows 11 Pro host (`DESKTOP-C6QHH2A`), NVIDIA + AMD GPUs present, MSVC 2022 Build Tools 14.44.35207, rustc via rustup. All steps below were run interactively by an automated agent; steps requiring a phone camera, network-adapter access, lock screen/UAC, or `clumsy` could not be performed and are written up at the end for a person to run.

## Step 1: Release build and dependency check

`cargo build --release --workspace` (LTO, `castr-sender`/`castr-receiver`/etc.) succeeded cleanly both before and after the static-CRT change; only warnings were harmless `LNK4099` (missing PDBs for vendored `opus`/`h264` C sources) and, after the CRT switch, `LNK4098`/`LNK4217` (CRT conflict/duplicate `malloc`/`free` symbols) — both non-fatal, executables link and run.

### Before static CRT (dynamic MSVC CRT)

`dumpbin /dependents target\release\castr-sender.exe`:
```
kernel32.dll, bcryptprimitives.dll, api-ms-win-core-synch-l1-2-0.dll, advapi32.dll,
ws2_32.dll, bcrypt.dll, ntdll.dll, oleaut32.dll, OPENGL32.dll, SHLWAPI.dll,
KERNEL32.dll, USER32.dll, SHELL32.dll, GDI32.dll, ADVAPI32.dll, uiautomationcore.dll,
d3d11.dll, ole32.dll, mfplat.dll, dwmapi.dll, imm32.dll, iphlpapi.dll, uxtheme.dll,
VCRUNTIME140.dll, api-ms-win-crt-math-l1-1-0.dll, api-ms-win-crt-string-l1-1-0.dll,
api-ms-win-crt-runtime-l1-1-0.dll, api-ms-win-crt-stdio-l1-1-0.dll,
api-ms-win-crt-heap-l1-1-0.dll, api-ms-win-crt-locale-l1-1-0.dll
```

`dumpbin /dependents target\release\castr-receiver.exe`:
```
bcryptprimitives.dll, kernel32.dll, api-ms-win-core-synch-l1-2-0.dll, ws2_32.dll,
bcrypt.dll, ntdll.dll, oleaut32.dll, ole32.dll, mfplat.dll, shell32.dll, advapi32.dll,
iphlpapi.dll, user32.dll, gdi32.dll, winmm.dll, setupapi.dll, version.dll, imm32.dll,
cfgmgr32.dll, VCRUNTIME140.dll, api-ms-win-crt-string-l1-1-0.dll,
api-ms-win-crt-math-l1-1-0.dll, api-ms-win-crt-heap-l1-1-0.dll,
api-ms-win-crt-runtime-l1-1-0.dll, api-ms-win-crt-stdio-l1-1-0.dll,
api-ms-win-crt-locale-l1-1-0.dll
```
Both executables depend on `VCRUNTIME140.dll` and the `api-ms-win-crt-*.dll` forwarders (dynamic CRT), so a bare copy of the exe would not run on a machine without the VC++ runtime installed.

### Static CRT change

Added to `.cargo/config.toml`:
```toml
[target.x86_64-pc-windows-msvc]
rustflags = ["-C", "target-feature=+crt-static"]
```
Rebuilt `cargo build --release --workspace`: **succeeded** (same 1m-ish link time). SDL2 (bundled/static-link) and openh264 (source feature) both linked fine with the static CRT; no build-script or link failures. Warnings seen and judged non-fatal:
- `LINK : warning LNK4098: defaultlib 'MSVCRT' conflicts with use of other libs; use /NODEFAULTLIB:library`
- `LINK : warning LNK4217: symbol 'free'/'malloc' defined in 'libucrt.lib' is imported by libaudiopus_sys ...`

`dumpbin /dependents target\release\castr-sender.exe` (static CRT):
```
kernel32.dll, bcryptprimitives.dll, api-ms-win-core-synch-l1-2-0.dll, advapi32.dll,
ws2_32.dll, bcrypt.dll, ntdll.dll, oleaut32.dll, OPENGL32.dll, SHLWAPI.dll,
KERNEL32.dll, USER32.dll, SHELL32.dll, GDI32.dll, ADVAPI32.dll, uiautomationcore.dll,
d3d11.dll, ole32.dll, mfplat.dll, dwmapi.dll, imm32.dll, iphlpapi.dll, uxtheme.dll
```

`dumpbin /dependents target\release\castr-receiver.exe` (static CRT):
```
bcryptprimitives.dll, kernel32.dll, api-ms-win-core-synch-l1-2-0.dll, ws2_32.dll,
bcrypt.dll, ntdll.dll, oleaut32.dll, ole32.dll, mfplat.dll, shell32.dll, advapi32.dll,
iphlpapi.dll, user32.dll, gdi32.dll, winmm.dll, setupapi.dll, version.dll, imm32.dll,
cfgmgr32.dll
```

`VCRUNTIME140.dll`, `msvcp140.dll` and all `api-ms-win-crt-*.dll` forwarders are gone from both lists. Remaining dependencies are all system DLLs (kernel32, user32, gdi32, shell32, advapi32, ole32/oleaut32, ws2_32, ntdll, bcrypt/bcryptprimitives, d3d11, mfplat, dwmapi, uxtheme, uiautomationcore, imm32, iphlpapi, winmm, setupapi, version, cfgmgr32, OPENGL32, SHLWAPI) — present on any stock Windows 10/11 install. **Result: satisfies the single-exe / portable-copy requirement (spec 1.1).**

The static-CRT flag is kept in `.cargo/config.toml`.

## Step 2: Same-machine session

Receiver and sender were already paired from earlier smoke tests (`%APPDATA%\castr\{receiver,sender}\paired.toml` existed), so no PIN exchange was needed.

Started receiver: `RUST_LOG=info target\release\castr-receiver.exe` (background).
Ran: `RUST_LOG=info target\release\castr-sender.exe cast DESKTOP-C6QHH2A --duration 20`.

Receiver log (decoder selection and lifecycle):
```
receiver 'DESKTOP-C6QHH2A' fingerprint 146076798dec7f7a762d03465168b0be2a169ece6cfda78fca9c660f980a0093
listening on 0.0.0.0:7332 (QUIC), probe port 7331
using decoder Microsoft H264 Video Decoder MFT
decoder: mf-h264
connection from 192.168.88.165:58985 fp 1dd544936580
stream 1920x802@30 Game 20000000 bps
goodbye: stopped
session ended
```

Sender log (encoder selection): the AMD MFT (`AMDh264Encoder`) was tried first and rejected (`SetOutputType H264: The input type is not supported for D3D device. (0xC00D6D76)`); the sender fell back to the NVIDIA hardware encoder:
```
using encoder NVIDIA H.264 Encoder MFT (mf-hardware)
encoder: mf-hardware
```

Steady-state sender status lines (localhost, no network path so rtt/loss are not meaningful here — see "Requires a person" below for a real network run):
- Resolution: 1920x802 (capture area), mode Game, target fps 30
- Bitrate ramped from 20.0 Mbps up to a ceiling of **40.0 Mbps** over ~7s of the 18s cast and held there (adaptive bitrate step-up logic, capped since no loss was ever observed)
- fps as reported in the status line fluctuated between roughly 20 and 68 (bursty on this loopback path — not representative of over-the-wire fps; the same-machine loop back doesn't throttle to real frame pacing the way a network path does)
- rtt: 0 ms throughout (localhost)
- loss: 0.0% throughout
- Final line: `stopped 1920x802 40.0 Mbps rtt 0 ms loss 0.0% 67 fps`
- Receiver goodbye line: `goodbye: stopped` followed by `session ended`

**Audio audibility/sync cannot be checked by an automated agent** (no speakers/microphone verification available in this environment) — this needs a person to play a video with lip movement on the sender and confirm sync is imperceptible (<~80 ms), per the brief.

## Step 4b: Receiver restart mid-cast

Started a 40s cast (`castr-sender.exe cast DESKTOP-C6QHH2A --duration 40`) against the already-running receiver from Step 2. After ~10s, killed `castr-receiver.exe` (`taskkill /F /IM castr-receiver.exe`), waited ~3s, and started a brand-new receiver process before the 10s window in the brief elapsed.

Sender log around the kill/restart:
```
[..15.658Z] using encoder NVIDIA H.264 Encoder MFT (mf-hardware)      <- steady casting
[..26.078Z] WARN castr_sender::cast: connection lost, reconnecting
[..26.078Z] INFO castr_sender: reconnecting 1920x802 34.0 Mbps rtt 0 ms loss 0.0% 2 fps
[..27.083Z] INFO castr_sender::cast: discarded 136 capture/audio frames while reconnecting
[..27.1xxZ] INFO castr_sender: casting 1920x802 34.0 Mbps rtt 0 ms loss 0.0% ... fps   (resumed)
...
[..52.947Z] INFO castr_sender: stopped 1920x802 40.0 Mbps rtt 0 ms loss 0.0% 20 fps
```

New receiver process log:
```
[..26.158Z] receiver 'DESKTOP-C6QHH2A' fingerprint 146076798dec7f7a762d03465168b0be2a169ece6cfda78fca9c660f980a0093
[..26.346Z] listening on 0.0.0.0:7332 (QUIC), probe port 7331
[..26.380Z] using decoder Microsoft H264 Video Decoder MFT
[..26.384Z] decoder: mf-h264
[..27.083Z] connection from 192.168.88.165:62749 fp 1dd544936580
[..27.083Z] stream 1920x802@30 Game 34000000 bps
[..52.947Z] goodbye: stopped
[..52.947Z] session ended
```

**Observed behavior differs from the brief's expected "Error{code:1}/resume-failed" outcome**: the sender logged `connection lost, reconnecting`, discarded 136 buffered frames, and then transparently established a **brand-new QUIC connection/stream** to the new receiver process (new source port 62749, new `stream ...` line) about 1 second after the new receiver process came up — it did not attempt to resume the stale session and did not surface an `Error{code:1}`. No re-pairing was needed (the paired-store fingerprint/cert check passed against the persisted `paired.toml`). The cast then continued for the remainder of the 40s duration and both sides logged a clean `stopped`/`goodbye: stopped` at the end. This is a *better* outcome than the brief anticipated (graceful fresh reconnect instead of a hard error), but it means the specific `Error{code:1}` code path described in the brief was not exercised/observed in this run — noting this as a finding for the reviewer to confirm is intended behavior rather than a symptom the code silently swallowing a real error.

## Step 7: Linux build check (WSL Ubuntu)

WSL distro `Ubuntu` (WSL2) is installed. `sudo` requires an interactive password (`sudo -n true` → `sudo: interactive authentication is required`); no password was available to this session, so **apt package installation was skipped** per the scope ruling (`build-essential cmake pkg-config libx11-dev libxext-dev libasound2-dev` were NOT installed).

Rust was not installed in the WSL distro; installed non-interactively:
```
curl --proto '=https' --tlsv1.2 -sSf https://sh.rustup.rs | sh -s -- -y
```
→ `rustc 1.98.0 (88d9e12ae 2026-08-18)`, stable-x86_64-unknown-linux-gnu.

Ran, from `/mnt/d/miracast`, with a separate target dir:
```
cargo build -p castr-receiver -p castr-proto -p castr-media -p castr-net --target-dir target-linux
```
**Result: build failed**, as expected without `build-essential`. Even trivial build-script crates (`libc`, `proc-macro2`, `quote`, `serde`, `thiserror`, `generic-array`, ...) failed to link with:
```
rust-lld: error: cannot open Scrt1.o: No such file or directory
rust-lld: error: cannot open crti.o: No such file or directory
rust-lld: error: unable to find library -lutil / -lrt / -lpthread / -lm / -ldl / -lc
rust-lld: error: cannot open crtn.o: No such file or directory
collect2: error: ld returned 1 exit status
```
This is the glibc/crt startup objects and libc being absent — i.e. no C toolchain at all (no `build-essential`), so the build never got far enough to need the SDL2/X11 headers specifically. `cargo test -p castr-proto -p castr-media -p castr-net` was not attempted since the build itself cannot succeed without a C toolchain.

**This step could not be completed** because it needs `sudo apt install build-essential ...` and no sudo password was available. Added `target-linux/` to `.gitignore` regardless, since the (failed) build still created a `target-linux/` directory tree with partial artifacts.

**To finish this step**, a person with sudo access should run, in WSL Ubuntu:
```
sudo apt update && sudo apt install -y build-essential cmake pkg-config libx11-dev libxext-dev libasound2-dev
cd /mnt/d/miracast
source "$HOME/.cargo/env"
cargo build -p castr-receiver -p castr-proto -p castr-media -p castr-net --target-dir target-linux
cargo test -p castr-proto -p castr-media -p castr-net --target-dir target-linux
cargo tree -p castr-receiver --target-dir target-linux | grep -i windows   # expect no output
```

## Requires a person at the machine

The following steps from the brief need a human (phone camera, physical network-adapter access, Win+L/UAC/resolution-change interaction, or the `clumsy` GUI tool) and were not run by this automated pass. Commands and expected outcomes are reproduced from the brief for whoever runs them next.

### Step 3: Glass-to-glass latency

Open a browser stopwatch with millisecond display on the sender screen, place the receiver window beside it, photograph both with a phone. Latency is the difference between the two readings. Take 5 samples in Game mode and 5 in Quality mode; record the median of each.
- Expected on one machine with hardware encode: Game under 50 ms, Quality about 150–200 ms.

### Step 4a: Network adapter toggle (live reconnect)

While casting, disable and re-enable the network adapter (or on a two-machine setup unplug Ethernet for 5 s).
- Expected: receiver overlay shows "Reconnecting" within 3 s, stream resumes within 2 s of the link returning, no re-pairing.

(Note: the receiver-*process*-restart half of Step 4 was completed above as Step 4b, with a caveat about the observed reconnect behavior differing from the brief's expectation.)

### Step 5: Capture edge cases

While casting: lock the screen (Win+L) and unlock; trigger a UAC prompt; change display resolution.
- Expected: the capture thread logs "access lost" and reopens; the stream continues within 2 s after each event.

### Step 6: Loss handling

Run the sender with `--max-bitrate 60000000` against the receiver on a Wi-Fi laptop if available, or simulate loss with `clumsy` (https://jagt.github.io/clumsy/) set to 3% drop on UDP.
- Expected: sender status shows loss, bitrate steps down within a second, picture stays clean (no long green/grey corruption) because keyframe fragments are retransmitted and the receiver skips to the next keyframe on delta loss. Record the lowest bitrate reached and time to recover after removing the loss.

## Files touched

- `.cargo/config.toml` — added `[target.x86_64-pc-windows-msvc] rustflags = ["-C", "target-feature=+crt-static"]`
- `.gitignore` — added `target-linux/`
- `docs/superpowers/verification/2026-09-01-castr-core-e2e.md` — this file

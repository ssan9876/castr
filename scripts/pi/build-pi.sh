#!/usr/bin/env bash
# Cross-compile castr-receiver for a 64-bit Raspberry Pi (aarch64, Debian 12/13
# userland) using Docker. Produces dist/castr-receiver-aarch64.
#
# The Pi 3's SD card and 1 GB of RAM make a native build take hours; this takes
# a few minutes on a desktop and only the ~7 MB binary needs to reach the Pi.
#
# Opus: on Linux the audiopus crate links libopus dynamically when pkg-config
# finds it, and refuses to link Debian's static archive because it lives in a
# system directory. LIBOPUS_LIB_DIR + OPUS_NO_PKG_CONFIG force the static link
# so the binary needs no libopus package on the Pi.
set -euo pipefail
cd "$(dirname "$0")/../.."
export MSYS_NO_PATHCONV=1   # Git Bash on Windows: keep /usr/... paths intact
docker build -t castr-xbuild:aarch64 scripts/pi
docker volume create castr-xtarget >/dev/null
docker volume create castr-xcargo >/dev/null
mkdir -p dist
docker run --rm \
  -e LIBOPUS_LIB_DIR=/usr/lib/aarch64-linux-gnu -e OPUS_NO_PKG_CONFIG=1 \
  -v "$(pwd -W 2>/dev/null || pwd):/src:ro" -v "$(pwd -W 2>/dev/null || pwd)/dist:/out" \
  -v castr-xtarget:/work -v castr-xcargo:/root/.cargo/registry \
  castr-xbuild:aarch64 bash -c '
    set -e
    cargo build --release --locked --target aarch64-unknown-linux-gnu -p castr-receiver --target-dir /work/target
    cp /work/target/aarch64-unknown-linux-gnu/release/castr-receiver /out/castr-receiver-aarch64
    aarch64-linux-gnu-readelf -d /out/castr-receiver-aarch64 | grep NEEDED'
echo "built dist/castr-receiver-aarch64"

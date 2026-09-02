#!/usr/bin/env bash
# Lint and run castr-codec-v4l2's unit tests natively on x86_64 Linux inside
# the cross-build image. The crate's Linux-gated modules (ops, fake, queue,
# decoder) can't be compiled or tested on Windows, but nothing in their unit
# tests touches real hardware, so the host arch inside the container is
# enough -- no emulation needed.
set -euo pipefail
cd "$(dirname "$0")/../.."
export MSYS_NO_PATHCONV=1   # Git Bash on Windows: keep /usr/... paths intact
docker run --rm \
  -e LIBOPUS_LIB_DIR=/usr/lib/x86_64-linux-gnu -e OPUS_NO_PKG_CONFIG=1 \
  -e PKG_CONFIG_LIBDIR=/usr/lib/x86_64-linux-gnu/pkgconfig \
  -v "$(pwd -W 2>/dev/null || pwd):/src:ro" -v castr-xtarget:/work -v castr-xcargo:/root/.cargo/registry \
  castr-xbuild:aarch64 \
  bash -c 'cd /src \
    && cargo clippy -q --locked -p castr-codec-v4l2 --tests --target-dir /work/host -- -D warnings \
    && cargo test -q --locked -p castr-codec-v4l2 --target-dir /work/host'

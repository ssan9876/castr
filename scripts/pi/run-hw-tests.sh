#!/usr/bin/env bash
# scripts/pi/run-hw-tests.sh <user@pi>
# Cross-builds the castr-codec-v4l2 test binary and runs its #[ignore] hardware
# tests on the Pi (from tmpfs; nothing is installed).
set -euo pipefail
cd "$(dirname "$0")/../.."
PI="${1:?usage: run-hw-tests.sh user@host}"
export MSYS_NO_PATHCONV=1
mkdir -p dist/tests
docker run --rm -v "$(pwd -W 2>/dev/null || pwd):/src:ro" -v "$(pwd -W 2>/dev/null || pwd)/dist:/out" \
  -v castr-xtarget:/work -v castr-xcargo:/root/.cargo/registry castr-xbuild:aarch64 bash -c '
    set -e
    cargo test --no-run --release --locked --target aarch64-unknown-linux-gnu -p castr-codec-v4l2 --target-dir /work/target --message-format=json 2>/dev/null \
      | grep -o "\"executable\":\"[^\"]*\"" | cut -d\" -f4 | sort -u > /out/tests/v4l2-list.txt
    rm -f /out/tests/v4l2-*bin
    i=0; for f in $(cat /out/tests/v4l2-list.txt); do cp "$f" "/out/tests/v4l2-$i.bin"; i=$((i+1)); done'
status=0
for f in dist/tests/v4l2-*.bin; do
  name=$(basename "$f")
  cat "$f" | ssh "$PI" "cat > /tmp/$name && chmod +x /tmp/$name
    set -o pipefail
    /tmp/$name --ignored --test-threads=1 --nocapture 2>&1 | grep -vF '[OpenH264]' | tail -20
    rc=\$?
    rm -f /tmp/$name
    exit \$rc" || status=1
done
exit $status

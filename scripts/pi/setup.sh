#!/usr/bin/env bash
# scripts/pi/setup.sh [path-to-castr-receiver-binary]
# One-shot Raspberry Pi setup for the castr receiver. Idempotent. Run as root.
set -euo pipefail
[ "$(id -u)" = 0 ] || { echo "run as root: sudo $0" >&2; exit 1; }
HERE="$(cd "$(dirname "$0")" && pwd)"
BIN="${1:-$HERE/castr-receiver}"
REBOOT=0

CFG=/boot/firmware/config.txt
[ -f "$CFG" ] || CFG=/boot/config.txt
echo "== config.txt ($CFG)"
# config.txt is sectioned by [filter] headers (e.g. [pi4], [cm5], [all]); a
# setting appended after a model-specific header only applies to that model.
# If our additions would land after a non-[all] header, open a fresh [all]
# section first so they apply everywhere, as intended.
last_header=$(grep -o '^\[[^]]*\]' "$CFG" | tail -1)
if [ -n "$last_header" ] && [ "$last_header" != "[all]" ] \
  && { ! grep -q '^dtoverlay=vc4-kms-v3d$' "$CFG" || ! grep -q '^gpu_mem=128$' "$CFG"; }; then
  echo '[all]' >> "$CFG"
  echo "   opened [all] section (last header was $last_header)"
fi
if ! grep -q '^dtoverlay=vc4-kms-v3d$' "$CFG"; then
  sed -i 's/^#\?dtoverlay=vc4-kms-v3d.*$/dtoverlay=vc4-kms-v3d/' "$CFG"
  grep -q '^dtoverlay=vc4-kms-v3d$' "$CFG" || echo 'dtoverlay=vc4-kms-v3d' >> "$CFG"
  echo "   enabled full KMS"; REBOOT=1
fi
# The VideoCore firmware only starts the H.264 decoder with >= 64 MB; KMS does
# not need gpu_mem, so 128 MB is purely for the codec.
if ! grep -q '^gpu_mem=128$' "$CFG"; then
  sed -i '/^gpu_mem\(_[0-9]\+\)\?=/d' "$CFG"
  echo 'gpu_mem=128' >> "$CFG"
  echo "   gpu_mem=128"; REBOOT=1
fi

echo "== decoder module at boot"
# DietPi ships a blanket blacklist for bcm2835_codec (headless images don't
# need the camera/codec stack); systemd-modules-load honours blacklists even
# for modules named explicitly in modules-load.d, so it has to come out or our
# entry below is silently skipped at boot.
files=$(grep -rl '^blacklist bcm2835_codec$' /etc/modprobe.d/ 2>/dev/null || true)
if [ -n "$files" ]; then
  sed -i '/^blacklist bcm2835_codec$/d' $files
  echo "   removed bcm2835_codec from modprobe blacklist: $files"
fi
if [ ! -f /etc/modules-load.d/castr.conf ]; then
  echo bcm2835_codec > /etc/modules-load.d/castr.conf
  modprobe bcm2835_codec 2>/dev/null || true
  echo "   /etc/modules-load.d/castr.conf"
fi

echo "== packages"
export DEBIAN_FRONTEND=noninteractive
apt-get update -q >/dev/null
apt-get install -y -q libstdc++6 libasound2 libdrm2 libgbm1 libgles2 libegl1 v4l-utils >/dev/null

echo "== user castr"
if ! id castr >/dev/null 2>&1; then
  useradd --system --home-dir /var/lib/castr --create-home --shell /usr/sbin/nologin castr
  echo "   created"
fi
usermod -aG video,render,input,audio castr
install -d -o castr -g castr -m 0750 /var/lib/castr /var/lib/castr/config

echo "== binary"
if [ -f "$BIN" ]; then
  install -m 0755 "$BIN" /usr/local/bin/castr-receiver
  echo "   /usr/local/bin/castr-receiver"
else
  echo "   no binary at $BIN (deploy.sh will install one)"
fi

echo "== service"
install -d -m 0755 /usr/local/lib/castr
install -m 0755 "$HERE/wait-devices.sh" /usr/local/lib/castr/wait-devices.sh
install -m 0644 "$HERE/castr-receiver.service" /etc/systemd/system/castr-receiver.service
systemctl daemon-reload
systemctl enable castr-receiver >/dev/null 2>&1
if [ "$REBOOT" = 1 ]; then
  echo
  echo "REBOOT REQUIRED (config.txt changed). The service starts after reboot."
else
  systemctl restart castr-receiver
  sleep 3
  systemctl --no-pager --lines=5 status castr-receiver || true
fi

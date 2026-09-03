#!/usr/bin/env bash
# scripts/pi/deploy.sh <user@pi>
# Cross-build the receiver, push it to the Pi, install it and restart the
# service. First run on a fresh Pi also runs setup.sh (config.txt, module
# load, user/group, packages). Every run - first or not - pushes and installs
# the current unit file and wait-devices.sh, so unit changes always reach an
# already-provisioned Pi.
set -euo pipefail
cd "$(dirname "$0")/../.."
PI="${1:?usage: deploy.sh user@host}"
bash scripts/pi/build-pi.sh
push() { cat "$1" | ssh "$PI" "cat > $2"; }
push dist/castr-receiver-aarch64 /tmp/castr-receiver
if ! ssh "$PI" 'test -f /etc/systemd/system/castr-receiver.service'; then
  echo "== first deploy: running setup.sh on $PI"
  push scripts/pi/setup.sh /tmp/castr-setup.sh
  push scripts/pi/castr-receiver.service /tmp/castr-receiver.service
  push scripts/pi/wait-devices.sh /tmp/castr-wait-devices.sh
  ssh "$PI" 'mkdir -p /tmp/castr-setup && mv /tmp/castr-setup.sh /tmp/castr-setup/setup.sh && mv /tmp/castr-receiver.service /tmp/castr-setup/ && mv /tmp/castr-wait-devices.sh /tmp/castr-setup/wait-devices.sh && mv /tmp/castr-receiver /tmp/castr-setup/castr-receiver && chmod +x /tmp/castr-setup/setup.sh && sudo /tmp/castr-setup/setup.sh'
  exit 0
fi
# Not just the binary: push the unit and wait script too and reinstall them,
# so a unit-only change (this branch added a device wait and an After=) still
# reaches a Pi that was provisioned before that change. Idempotent - installs
# are just `install -m`, and daemon-reload before restart picks up the new
# unit content.
push scripts/pi/castr-receiver.service /tmp/castr-receiver.service
push scripts/pi/wait-devices.sh /tmp/castr-wait-devices.sh
# The Miracast supplicant configuration travels the same way and for the same
# reason: a Pi provisioned before the sink existed has neither the file nor the
# control-socket directory.
push scripts/pi/wpa_supplicant-p2p.conf /tmp/castr-wpa-p2p.conf
# sudo, not plain systemctl/journalctl: these Pis have no dbus daemon, and
# without it an unprivileged user can't talk to systemd's system bus at all
# (root bypasses dbus via /run/systemd/private).
ssh "$PI" '
  set -e
  sudo install -m 0755 /tmp/castr-receiver /usr/local/bin/castr-receiver
  sudo install -d -m 0755 /usr/local/lib/castr
  sudo install -m 0755 /tmp/castr-wait-devices.sh /usr/local/lib/castr/wait-devices.sh
  sudo install -m 0644 /tmp/castr-receiver.service /etc/systemd/system/castr-receiver.service
  sudo install -d -m 0755 /etc/castr
  sudo install -m 0644 /tmp/castr-wpa-p2p.conf /etc/castr/wpa_supplicant-p2p.conf
  echo "d /run/wpa_supplicant_castr 0770 castr castr -" | sudo tee /etc/tmpfiles.d/castr.conf >/dev/null
  sudo systemd-tmpfiles --create /etc/tmpfiles.d/castr.conf || true
  # ensure_supplicant() in the sink only starts wpa_supplicant when its control
  # socket is missing, so a config change installed above would otherwise sit
  # unread until the next reboot: the old supplicant process keeps running
  # with the old settings, and its socket keeps the sink from starting a new
  # one. Kill it here; the sink starts a fresh instance, with the config just
  # installed, on its next pass.
  sudo pkill -f "wpa_supplicant .*-c /etc/castr/wpa_supplicant-p2p.conf" || true
  rm -f /tmp/castr-receiver /tmp/castr-receiver.service /tmp/castr-wait-devices.sh /tmp/castr-wpa-p2p.conf
  sudo systemctl daemon-reload
  sudo systemctl restart castr-receiver
  sleep 5
  sudo systemctl is-active castr-receiver
' || { echo "service not active after restart:"; ssh "$PI" 'sudo journalctl -u castr-receiver -n 20 --no-pager'; exit 1; }
echo "deployed to $PI"

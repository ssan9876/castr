#!/usr/bin/env bash
# scripts/pi/deploy.sh <user@pi>
# Cross-build the receiver, push it to the Pi, install it and restart the
# service. First run on a fresh Pi copies setup.sh + the unit and runs setup.
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
  ssh "$PI" 'mkdir -p /tmp/castr-setup && mv /tmp/castr-setup.sh /tmp/castr-setup/setup.sh && mv /tmp/castr-receiver.service /tmp/castr-setup/ && mv /tmp/castr-receiver /tmp/castr-setup/castr-receiver && chmod +x /tmp/castr-setup/setup.sh && sudo /tmp/castr-setup/setup.sh'
  exit 0
fi
# sudo, not plain systemctl/journalctl: these Pis have no dbus daemon, and
# without it an unprivileged user can't talk to systemd's system bus at all
# (root bypasses dbus via /run/systemd/private).
ssh "$PI" 'sudo install -m 0755 /tmp/castr-receiver /usr/local/bin/castr-receiver && rm -f /tmp/castr-receiver && sudo systemctl restart castr-receiver && sleep 5 && sudo systemctl is-active castr-receiver' \
  || { echo "service not active after restart:"; ssh "$PI" 'sudo journalctl -u castr-receiver -n 20 --no-pager'; exit 1; }
echo "deployed to $PI"

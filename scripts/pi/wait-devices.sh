#!/bin/sh
# scripts/pi/wait-devices.sh
# Installed by setup.sh to /usr/local/lib/castr/wait-devices.sh and called from
# castr-receiver.service's ExecStartPre. Waits (bounded) for the DRM device and
# the V4L2 codec device to appear, then exits 0 regardless, so a box without
# the hardware decoder still boots the service (it falls back to software).
#
# Unit files must not contain a literal `$`; systemd substitutes `$WORD`
# tokens in ExecStart/ExecStartPre lines before the shell ever sees them, so
# the wait loops live here instead of inline in the unit.
wait_for() {
  dev="$1"
  tries="$2"
  i=0
  while [ ! -e "$dev" ] && [ "$i" -lt "$tries" ]; do
    sleep 0.2
    i=$((i + 1))
  done
}

# DRM device: needed for the SDL kmsdrm video driver.
wait_for /dev/dri/card0 150

# V4L2 codec device: needed for hardware H.264 decode. About 5 s, then give up
# and let the receiver start anyway - `--decoder auto` falls back to openh264
# rather than the process failing to start.
wait_for /dev/video10 25

exit 0

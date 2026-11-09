#!/usr/bin/env bash
# Captures rature's window on a file, in a nested headless Weston (with Xwayland: GPUI needs a
# seat) and a private bus, never the user's session. Prints the start-up timing.
# usage: tools/capture.sh <file.md> <out.png>
# needs: weston (with Xwayland), weston-screenshooter, dbus-daemon, ImageMagick.
set -u
here=$(cd "$(dirname "$0")" && pwd)
BIN=${RATURE_BIN:-$here/../target/release/rature}
FILE=$(realpath "$1"); OUT=$(realpath -m "$2")
W=960; H=${H:-1700}
D=$(mktemp -d /tmp/rature-capture.XXXX)
mkdir -p "$D"/{home,runtime,wrun}; chmod 700 "$D"/*
cleanup() {
  kill "${APID:-}" "${WPID:-}" 2>/dev/null
  [ -f "$D/dbus.pid" ] && kill "$(cat "$D/dbus.pid")" 2>/dev/null
  sleep 1
  [ -n "${KEEP:-}" ] || rm -rf "$D"
}
trap cleanup EXIT
export HOME=$D/home XDG_RUNTIME_DIR=$D/runtime
unset WAYLAND_DISPLAY DBUS_SESSION_BUS_ADDRESS

XDG_RUNTIME_DIR=$D/wrun weston --backend=headless --renderer=gl --xwayland --shell=kiosk --fake-seat --debug \
  --socket=wl-rature --width=$W --height=$H >"$D/weston.log" 2>&1 &
WPID=$!
for _ in $(seq 50); do grep -q 'xserver listening on display' "$D/weston.log" && break; sleep 0.2; done
DISP=$(grep -o 'listening on display :[0-9]*' "$D/weston.log" | head -1 | grep -o ':[0-9]*')
cat >"$D/dbus.conf" <<CONF
<!DOCTYPE busconfig PUBLIC "-//freedesktop//DTD D-Bus Bus Configuration 1.0//EN" "http://www.freedesktop.org/standards/dbus/1.0/busconfig.dtd">
<busconfig><type>session</type><listen>unix:path=$D/bus</listen><policy context="default"><allow send_destination="*" eavesdrop="true"/><allow eavesdrop="true"/><allow own="*"/></policy></busconfig>
CONF
dbus-daemon --config-file="$D/dbus.conf" --fork --print-pid >"$D/dbus.pid"
export DBUS_SESSION_BUS_ADDRESS=unix:path=$D/bus

DISPLAY=$DISP RATURE_TIMING=1 "$BIN" "$FILE" >"$D/app.log" 2>&1 &
APID=$!
sleep 8
(cd "$D" && XDG_RUNTIME_DIR=$D/wrun WAYLAND_DISPLAY=wl-rature weston-screenshooter >/dev/null 2>&1)
cat "$D/app.log"; echo "dir: $D"

magick "$D"/wayland-screenshot*.png -strip "$OUT" && echo "wrote $OUT"

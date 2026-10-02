#!/bin/sh
# Wayland checks inside a container:
#   1. sway (supports the wlr data-control protocol): the Wayland backend path, with X11 unavailable.
#   2. weston + XWayland (no data-control, like GNOME): checks that we fall back to X11 and still sync.
#
#   docker run --rm -v "$PWD":/src:ro -v uclip-cargo:/usr/local/cargo/registry \
#     -e CARGO_TARGET_DIR=/tmp/target rust:1-bookworm sh /src/scripts/linux-wayland-test.sh
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null && apt-get install -y -qq sway weston xwayland wl-clipboard >/dev/null 2>&1
cd /src
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/tmp/target}
cargo build -q -p uclip
U=$CARGO_TARGET_DIR/debug/uclip
export XDG_RUNTIME_DIR=/tmp/xdg; mkdir -p $XDG_RUNTIME_DIR; chown 1000:1000 $XDG_RUNTIME_DIR; chmod 700 $XDG_RUNTIME_DIR

pair_and_run() {   # $1 = label
  A=$(mktemp -d); B=$(mktemp -d); LOG=$(mktemp -d)
  (echo y | UCLIP_HOME=$A $U --port 47474 pair host > $LOG/host.log 2>&1 &)
  sleep 2
  PORT=$(sed -n "s/.*Listening on port \([0-9]*\).*/\1/p" $LOG/host.log)
  echo y | UCLIP_HOME=$B $U --port 47475 pair join --addr 127.0.0.1:$PORT > $LOG/join.log 2>&1
  (UCLIP_HOME=$A $U --port 47474 run --no-mdns > $LOG/a.log 2>&1 &)
  (UCLIP_HOME=$B $U --port 47475 run --no-mdns --clipboard file:$B/clip.txt > $LOG/b.log 2>&1 &)
  sleep 5
  echo "[$1] wl-copy -> other device"
  printf "copied on wayland" | wl-copy
  sleep 2; echo "    other device has: $(cat $B/clip.txt 2>/dev/null)"
  echo "[$1] other device -> wl-paste"
  printf "written by other device" > $B/clip.txt
  sleep 2; echo "    wl-paste reads:   $(wl-paste -n 2>&1)"
  grep -h -i -E "no clipboard|warn|error" $LOG/a.log | sed "s/\x1b\[[0-9;]*m//g" | head -3
  pkill -f "uclip " || true; sleep 1
}

echo "=== 1. sway, data-control, no X11 (polling path)"
# sway refuses to run as root, so drop to uid 1000 for the compositor only
setpriv --reuid=1000 --regid=1000 --clear-groups env WLR_BACKENDS=headless WLR_LIBINPUT_NO_DEVICES=1 WLR_RENDERER=pixman sway -c /dev/null > /tmp/sway.log 2>&1 &
SWAY=$!; sleep 4; tail -3 /tmp/sway.log
export WAYLAND_DISPLAY=$(ls $XDG_RUNTIME_DIR | grep '^wayland-[0-9]*$' | head -1)
echo "compositor socket: $WAYLAND_DISPLAY"
( unset DISPLAY; pair_and_run sway )
kill $SWAY 2>/dev/null || true; sleep 1; rm -f $XDG_RUNTIME_DIR/wayland-*

echo "=== 2. weston + XWayland, no data-control (GNOME-like)"
setpriv --reuid=1000 --regid=1000 --clear-groups weston --backend=headless-backend.so --socket=wl-weston --xwayland --idle-time=0 > /tmp/weston.log 2>&1 &
sleep 4
grep -i -E "xserver|xwayland|display" /tmp/weston.log | head -3
XDISP=$(sed -n 's/.*listening on display \(:[0-9]*\).*/\1/p' /tmp/weston.log | head -1)
[ -n "$XDISP" ] || XDISP=:0
export WAYLAND_DISPLAY=wl-weston DISPLAY=$XDISP
echo "WAYLAND_DISPLAY=$WAYLAND_DISPLAY DISPLAY=$DISPLAY"
pair_and_run weston

#!/bin/sh
# Runs inside a Linux container with a virtual X11 display. From the repo root:
#
#   docker run --rm -v "$PWD":/src:ro -v uclip-cargo:/usr/local/cargo/registry \
#     -e CARGO_TARGET_DIR=/tmp/target rust:1-bookworm sh /src/scripts/linux-test.sh
#
# 1. unit and end-to-end tests under Xvfb (includes the real X11 clipboard test)
# 2. a live run: pair two instances, one on the real (virtual) X11 clipboard, one file backed,
#    then copy with xclip and check the other side, both ways.
set -e
export DEBIAN_FRONTEND=noninteractive
apt-get update -qq >/dev/null && apt-get install -y -qq xvfb xclip >/dev/null
cd /src
export CARGO_TARGET_DIR=${CARGO_TARGET_DIR:-/tmp/target}

echo "=== tests"
xvfb-run -a cargo test 2>&1 | grep -E "test |test result|error|warning" || true
cargo build -q -p uclip

echo "=== live run on X11"
xvfb-run -a sh -c '
  set -e
  U=$CARGO_TARGET_DIR/debug/uclip
  A=$(mktemp -d); B=$(mktemp -d); LOG=$(mktemp -d)
  (echo y | UCLIP_HOME=$A $U --port 47474 pair host > $LOG/host.log 2>&1 &)
  sleep 2
  PORT=$(sed -n "s/.*Listening on port \([0-9]*\).*/\1/p" $LOG/host.log)
  echo y | UCLIP_HOME=$B $U --port 47475 pair join --addr 127.0.0.1:$PORT > $LOG/join.log 2>&1
  grep "Pairing code" $LOG/host.log $LOG/join.log
  (UCLIP_HOME=$A $U --port 47474 run --no-mdns > $LOG/a.log 2>&1 &)
  (UCLIP_HOME=$B $U --port 47475 run --no-mdns --clipboard file:$B/clip.txt > $LOG/b.log 2>&1 &)
  sleep 5
  echo "--- X11 clipboard -> other device"
  printf "copied with xclip" | xclip -selection clipboard
  sleep 2; echo "other device has: $(cat $B/clip.txt)"
  echo "--- other device -> X11 clipboard"
  printf "written by other device" > $B/clip.txt
  sleep 2; echo "xclip reads:      $(xclip -selection clipboard -o)"
  echo "--- unicode and multiline"
  printf "h\303\251llo \342\234\223\nsecond line" | xclip -selection clipboard
  sleep 2; echo "other device has: $(cat $B/clip.txt | tr "\n" "|")"
  echo "--- logs"; grep -h -E "connected|copied|received" $LOG/a.log $LOG/b.log | sed "s/\x1b\[[0-9;]*m//g" | cut -c1-110
  pkill -f "uclip " || true
'

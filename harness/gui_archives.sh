#!/bin/bash
# Walk into an archive, look around, extract out of it, read the account.
set -u
export DISPLAY=:99
export LIBGL_ALWAYS_SOFTWARE=1
export XDG_CONFIG_HOME=/tmp/aconfig
SHOT="${SHOT:-/tmp/lostc-shots}"; mkdir -p "$SHOT"
BIN="${BIN:-$(dirname "$0")/../target/debug/lostc-gui}"

rm -f "$SHOT"/arch-*.png
rm -rf /tmp/awork /tmp/aconfig
mkdir -p /tmp/awork/here /tmp/awork/there /tmp/awork/build/docs/deep
echo "at the top"  > /tmp/awork/build/readme.txt
echo "in a folder" > /tmp/awork/build/docs/notes.txt
echo "two down"    > /tmp/awork/build/docs/deep/buried.txt
head -c 200000 /dev/urandom > /tmp/awork/build/docs/blob.bin
(cd /tmp/awork/build && zip -qr ../here/papers.zip readme.txt docs)
(cd /tmp/awork/build && tar -czf ../here/bundle.tar.gz readme.txt docs)
(cd /tmp/awork/build && zip -q -P opensesame -r ../here/locked.zip readme.txt docs)
rm -rf /tmp/awork/build
ls -1 /tmp/awork/here

pkill -x lostc-gui 2>/dev/null; pkill -f Xvfb 2>/dev/null; sleep 2
Xvfb :99 -screen 0 1400x860x24 >/dev/null 2>&1 &
sleep 2
"$BIN" /tmp/awork/here /tmp/awork/there >/dev/null 2>&1 &
sleep 8
xdotool windowfocus "$(xdotool search --name 'lost-commander' | head -1)"; sleep 1

echo "== walk into the zip =="
xdotool key --clearmodifiers Home; sleep 0.4
xdotool key --clearmodifiers Down; sleep 0.4   # first entry after ..
xdotool key --clearmodifiers Down; sleep 0.4
xdotool key --clearmodifiers Down; sleep 0.4   # papers.zip
scrot -o "$SHOT/arch-0-before.png"
xdotool key --clearmodifiers Return; sleep 2.5
scrot -o "$SHOT/arch-1-inside.png"

echo "== down into docs =="
xdotool key --clearmodifiers Down; sleep 0.4
xdotool key --clearmodifiers Return; sleep 1.5
scrot -o "$SHOT/arch-2-docs.png"

echo "== extract everything here with F5 =="
xdotool key --clearmodifiers ctrl+a; sleep 0.8
xdotool key --clearmodifiers F5; sleep 1.5
xdotool key --clearmodifiers Return; sleep 3.5
scrot -o "$SHOT/arch-3-extracted.png"

echo "== what landed on disk =="
find /tmp/awork/there -type f | sort

echo "== a write is refused =="
xdotool key --clearmodifiers F8; sleep 1.5
scrot -o "$SHOT/arch-4-refused.png"

echo "== the account =="
xdotool key --clearmodifiers ctrl+j; sleep 2.5
scrot -o "$SHOT/arch-5-account.png"

find /tmp/aconfig -name '*.jsonl' -exec sh -c 'echo "--- $1"; cat "$1"' _ {} \;

#!/usr/bin/env bash
# Makes the README's images again from examples/weather-station, through tools/capture.sh:
#   docs/images/start.png    started with nothing open, a few recent files and folders listed;
#   docs/images/folder.png   the folder, its tree and its README;
#   docs/images/changed.png  sensors.md read, then changed: words changed, a block added, one removed.
# usage: tools/screenshots.sh (from anywhere). Needs a release build (cargo build --release) and
# what tools/capture.sh needs. OMARCHY=1 is passed on to it. The examples are never changed: the
# change is made to a copy.
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
capture=$root/tools/capture.sh
images=$root/docs/images
if [ ! -x "$root/target/release/rature" ]; then
  echo "screenshots: no release build: cargo build --release first" >&2
  exit 1
fi
# A short, neutral path, the one the window shows: never the clone's own (a home folder).
tmp=/tmp/hillside
if ! mkdir -m 700 "$tmp"; then
  echo "screenshots: $tmp already exists: remove it first" >&2
  exit 1
fi
trap 'rm -rf "$tmp"' EXIT
cp -r "$root/examples/weather-station" "$tmp/weather-station"
example=$tmp/weather-station

# start.png: what Rature keeps between runs, as if these had been opened lately (seconds ago).
mkdir -m 700 "$tmp/state"
python3 -I - "$tmp/state/state.json" "$example" <<'PY'
import json, sys, time
out, example = sys.argv[1], sys.argv[2]
now = int(time.time())
opened = [
    ("", 4 * 60),
    ("/sensors.md", 35 * 60),
    ("/data/readings.csv", 3 * 3600),
    ("/hardware.md", 26 * 3600),
    ("/data/sensors.json", 3 * 86400),
    ("/firmware/dewpoint.rs", 9 * 86400),
]
recent = [example + name for name, _ in opened]
with open(out, "w") as f:
    json.dump({
        "positions": {},
        "recent": recent,
        "opened_at": {path: now - ago for path, (_, ago) in zip(recent, opened)},
    }, f)
PY
W=640 H=550 STATE="$tmp/state" "$capture" "" "$images/start.png"

# folder.png
W=800 H=700 "$capture" "$example" "$images/folder.png"

# changed.png: the first opening takes the snapshot, then the file is replaced by a rename, as an
# agent writes it, and the reader shows what changed since.
file=$example/sensors.md
text=$(<"$file")
change() {
  local before=$text
  text=${text/"$1"/"$2"}
  if [ "$text" = "$before" ]; then
    echo "screenshots: \"${1%%$'\n'*}\" is no longer in sensors.md: update tools/screenshots.sh" >&2
    exit 1
  fi
}
change "every 60 seconds" "every 30 seconds"
change "the last five readings" "the last ten readings"
change $'> The rain gauge is not sampled: it counts each tip of its bucket, 0.2 mm of rain a tip.\n\n' ""
change "Dew point uses" $'Each reading also carries the battery voltage, to spot a tired solar panel before the\nstation goes quiet.\n\nDew point uses'
printf '%s\n' "$text" >"$tmp/sensors.changed.md"
W=640 H=550 THEN="mv '$tmp/sensors.changed.md' '$file'" "$capture" "$file" "$images/changed.png"

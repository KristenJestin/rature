#!/usr/bin/env bash
# Renders the PNGs of assets/brand from its SVGs, and copies the icon to the launcher's icon in
# packaging/linux. The SVGs hold no text, only outlines, so no font is needed. Needs
# rsvg-convert (librsvg). The PNGs are committed: run this again after changing an SVG.
# usage: tools/brand.sh (from anywhere).
set -euo pipefail
root=$(cd "$(dirname "$0")/.." && pwd -P)
brand=$root/assets/brand
png=$brand/png

if ! command -v rsvg-convert >/dev/null; then
  echo "brand: rsvg-convert not found: install librsvg (its rsvg-convert tool)" >&2
  exit 1
fi

mkdir -p "$png"
for size in 128 256 512 1024; do
  rsvg-convert -w "$size" -h "$size" "$brand/icon.svg" -o "$png/icon-$size.png"
done
for size in 16 32 64; do
  rsvg-convert -w "$size" -h "$size" "$brand/favicon.svg" -o "$png/favicon-$size.png"
done
rsvg-convert -w 512 -h 512 "$brand/mark.svg" -o "$png/mark-512.png"
rsvg-convert -w 512 -h 512 "$brand/mark-on-light.svg" -o "$png/mark-on-light-512.png"
for name in social-preview social-preview-fr; do
  rsvg-convert -w 1280 -h 640 "$brand/$name.svg" -o "$png/$name.png"
done
cp "$brand/icon.svg" "$root/packaging/linux/icons/hicolor/scalable/apps/rature.svg"

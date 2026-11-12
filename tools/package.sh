#!/usr/bin/env bash
# The archive of a build (docs/releases.md): `rature-<version>-x86_64-linux.tar.gz`, holding the
# binary, Rature's licence, the fonts' licence and the launcher's files (`share/`: the desktop
# entry and the icons, from packaging/linux) in one folder, and its SHA-256 sum beside it, as
# `sha256sum --check` reads it. Linux x86_64 only.
#   tools/package.sh <version> <binary> <out folder>
set -euo pipefail

version=${1:?package.sh: the version}
binary=${2:?package.sh: the binary}
out=${3:?package.sh: the folder to write to}
root=$(cd "$(dirname "$0")/.." && pwd)
name="rature-$version"
archive="$name-x86_64-linux.tar.gz"

work=$(mktemp -d)
trap 'rm -rf "$work"' EXIT
mkdir "$work/$name"
install -m 0755 "$binary" "$work/$name/rature"
install -m 0644 "$root/LICENSE" "$work/$name/LICENSE"
install -m 0644 "$root/assets/fonts/OFL.txt" "$work/$name/LICENSE-fonts.txt"
(cd "$root/packaging/linux" && find rature.desktop icons -type f | sort) | while read -r file; do
  case "$file" in
    rature.desktop) target="share/applications/$file" ;;
    *) target="share/$file" ;;
  esac
  install -D -m 0644 "$root/packaging/linux/$file" "$work/$name/$target"
done
mkdir -p "$out"
# The same archive from the same files: no owner, no time of the build.
tar --sort=name --owner=0 --group=0 --numeric-owner --mtime=@0 -C "$work" -cf - "$name" |
  gzip -9 -n >"$out/$archive"
(cd "$out" && sha256sum "$archive" >"$archive.sha256")
echo "$out/$archive"

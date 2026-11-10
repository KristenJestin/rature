#!/bin/sh
# Builds Rature from this clone and installs it into ~/.local/bin, without root: `rt` is a link
# to `rature`. The launcher's entry and icon go to $XDG_DATA_HOME, the entry running the binary
# by its real path. Run again to update.
#
#   ./install.sh               build and install (or update)
#   ./install.sh --no-rt       the same, without the `rt` link
#   ./install.sh --uninstall   remove what it installed (config and state are kept)
set -eu
cd "$(dirname "$0")"

link=yes
uninstall=no
for arg in "$@"; do
  case "$arg" in
    --no-rt) link=no ;;
    --uninstall) uninstall=yes ;;
    -h | --help)
      sed -n 's/^#   //p' "$0"
      exit 0
      ;;
    *)
      echo "install.sh: unknown option $arg (--no-rt, --uninstall)" >&2
      exit 2
      ;;
  esac
done

bin=$HOME/.local/bin
data=${XDG_DATA_HOME:-}
case "$data" in /*) ;; *) data=$HOME/.local/share ;; esac
icons=$(cd packaging/linux && find icons -type f)

# Our `rt` is the relative link this script makes; anything else there is someone else's.
ours() {
  [ -L "$bin/rt" ] && [ "$(readlink "$bin/rt")" = rature ]
}

if [ "$uninstall" = yes ]; then
  rm -f "$bin/rature"
  removed="$bin/rature"
  if ours; then
    rm -f "$bin/rt"
    removed="$removed and its rt link"
  fi
  rm -f "$data/applications/rature.desktop"
  for icon in $icons; do rm -f "$data/$icon"; done
  if [ -d "$data/icons/hicolor" ]; then touch "$data/icons/hicolor"; fi
  if command -v update-desktop-database >/dev/null; then
    update-desktop-database "$data/applications" 2>/dev/null || true
  fi
  echo "Removed $removed, and Rature's launcher entry and icon. Kept its config and state."
  exit 0
fi

# Checked before the build: a link that is not ours is never replaced.
if [ "$link" = yes ] && { [ -e "$bin/rt" ] || [ -L "$bin/rt" ]; } && ! ours; then
  echo "install.sh: $bin/rt exists and is not Rature's: left alone." >&2
  echo "Remove it, or run ./install.sh --no-rt to install without the rt link." >&2
  exit 1
fi

nice -n 19 cargo build --release --locked

mkdir -p "$bin"
# Installed beside, then moved: a running Rature keeps its own binary.
install -m 0755 target/release/rature "$bin/.rature.new"
mv -f "$bin/.rature.new" "$bin/rature"
if [ "$link" = yes ]; then
  ln -sfn rature "$bin/rt"
  found=$(command -v rt || true)
  if [ -n "$found" ] && [ "$found" != "$bin/rt" ]; then
    echo "install.sh: another rt, $found, comes first in PATH." >&2
  fi
fi

real=$(cd "$bin" && pwd -P)/rature
newline='
'
case "$real" in
  *"$newline"*)
    echo "install.sh: $real holds a newline: no launcher entry." >&2
    exit 1
    ;;
esac

# An argument of Exec=, per the Desktop Entry spec: in double quotes, with `"`, `$`, backtick
# and backslash escaped by a backslash, that backslash doubled again as any in a string value,
# and `%` doubled.
exec_arg() {
  rest=$1
  out=
  while [ -n "$rest" ]; do
    tail=${rest#?}
    c=${rest%"$tail"}
    rest=$tail
    case "$c" in
      \\) out=$out'\\\\' ;;
      \") out=$out'\\"' ;;
      \$) out=$out'\\$' ;;
      \`) out=$out'\\`' ;;
      %) out=$out'%%' ;;
      *) out=$out$c ;;
    esac
  done
  printf '"%s"' "$out"
}

case "$real" in
  *[!A-Za-z0-9/._+-]*) command="$(exec_arg "$real") %f" ;;
  *) command="$real %f" ;;
esac

mkdir -p "$data/applications"
# Line by line rather than with sed: the path is never read as a pattern or a replacement.
new=$data/applications/.rature.desktop.new
while IFS= read -r line || [ -n "$line" ]; do
  case "$line" in
    Exec=*) printf '%s\n' "Exec=$command" ;;
    *) printf '%s\n' "$line" ;;
  esac
done <packaging/linux/rature.desktop >"$new"
mv -f "$new" "$data/applications/rature.desktop"
for icon in $icons; do
  mkdir -p "$data/$(dirname "$icon")"
  install -m 0644 "packaging/linux/$icon" "$data/$icon"
done
# A desktop's cache of the icon theme is stale when the folder is newer than it.
touch "$data/icons/hicolor"
if command -v update-desktop-database >/dev/null; then
  update-desktop-database "$data/applications"
fi

if [ "$link" = yes ]; then
  echo "Installed $bin/rature (and rt), and added Rature to the application launcher ($data)."
else
  echo "Installed $bin/rature, and added Rature to the application launcher ($data)."
fi

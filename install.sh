#!/bin/sh
# Builds Rature from this clone and installs it into ~/.local/bin, without root: `rt` is a link
# to `rature`. The launcher's entry and icon go to $XDG_DATA_HOME, the entry running the binary
# by its real path. Run again to update.
set -eu
cd "$(dirname "$0")"

nice -n 19 cargo build --release --locked

bin=$HOME/.local/bin
mkdir -p "$bin"
# Installed beside, then moved: a running Rature keeps its own binary.
install -m 0755 target/release/rature "$bin/.rature.new"
mv -f "$bin/.rature.new" "$bin/rature"
ln -sfn rature "$bin/rt"

data=${XDG_DATA_HOME:-}
case "$data" in /*) ;; *) data=$HOME/.local/share ;; esac
real=$(cd "$bin" && pwd -P)/rature
case "$real" in
  *[!A-Za-z0-9/._+-]*) command="\"$real\" %f" ;;
  *) command="$real %f" ;;
esac
mkdir -p "$data/applications"
sed "s|^Exec=.*|Exec=$command|" packaging/linux/rature.desktop >"$data/applications/.rature.desktop.new"
mv -f "$data/applications/.rature.desktop.new" "$data/applications/rature.desktop"
for icon in $(cd packaging/linux && find icons -type f); do
  mkdir -p "$data/$(dirname "$icon")"
  install -m 0644 "packaging/linux/$icon" "$data/$icon"
done
# A desktop's cache of the icon theme is stale when the folder is newer than it.
touch "$data/icons/hicolor"
command -v update-desktop-database >/dev/null && update-desktop-database "$data/applications"

echo "Installed $bin/rature (and rt), and added Rature to the application launcher ($data)."

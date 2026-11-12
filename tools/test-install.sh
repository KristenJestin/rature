#!/usr/bin/env bash
# Runs install.sh against fake releases, never GitHub's: a small local server on 127.0.0.1
# (python3) answers the API and serves fake archives, named to install.sh by RATURE_RELEASES.
# HOME, XDG_DATA_HOME and TMPDIR are temporary folders, never yours. A fake archive holds a
# `rature` script that answers --version: no build.
#   tools/test-install.sh
# TEST_SH is the shell install.sh runs under: dash when it is installed, sh otherwise.
set -euo pipefail

root=$(cd "$(dirname "$0")/.." && pwd)
script="$root/install.sh"
if [ -n "${TEST_SH:-}" ]; then
  read -ra shell <<<"$TEST_SH"
elif command -v dash >/dev/null 2>&1; then
  shell=(dash)
else
  shell=(sh)
fi
command -v python3 >/dev/null 2>&1 || {
  echo "test-install.sh: python3 is needed for the local server" >&2
  exit 1
}

tmp=$(mktemp -d)
server=
cleanup() {
  if [ -n "$server" ]; then kill "$server" 2>/dev/null || true; fi
  rm -rf "$tmp"
}
trap cleanup EXIT

# The server: a path's `?` is read as `@` in the file's name; `<file>.status` holds another
# status than 200; a file not there is a 404. Every path asked for is written to the log. It
# stops by itself after ten minutes.
mkdir -p "$tmp/www"
: >"$tmp/requests.log"
python3 -I -c '
import http.server, os, sys, threading
root, port_file, log = sys.argv[1:4]
class Handler(http.server.BaseHTTPRequestHandler):
    def do_GET(self):
        with open(log, "a") as f:
            f.write(self.path + "\n")
        path = os.path.normpath(os.path.join(root, self.path.lstrip("/").replace("?", "@")))
        status, body = 404, b"{\"message\": \"Not Found\"}"
        if path.startswith(root + os.sep):
            if os.path.isfile(path):
                status = 200
                with open(path, "rb") as f:
                    body = f.read()
            if os.path.isfile(path + ".status"):
                with open(path + ".status") as f:
                    status = int(f.read())
        self.send_response(status)
        self.send_header("Content-Length", str(len(body)))
        self.end_headers()
        self.wfile.write(body)
    def log_message(self, *args):
        pass
server = http.server.HTTPServer(("127.0.0.1", 0), Handler)
threading.Timer(600, os._exit, [0]).start()
with open(port_file + ".new", "w") as f:
    f.write(str(server.server_address[1]))
os.rename(port_file + ".new", port_file)
server.serve_forever()
' "$tmp/www" "$tmp/port" "$tmp/requests.log" &
server=$!
for _ in $(seq 100); do
  [ -s "$tmp/port" ] && break
  sleep 0.05
done
[ -s "$tmp/port" ] || {
  echo "test-install.sh: the local server did not start" >&2
  exit 1
}
base="http://127.0.0.1:$(cat "$tmp/port")"
download="$base/dl/releases/download"

# --- Fake releases ---------------------------------------------------------------------------

# A fake archive of the version $1, as tools/package.sh lays it out, its `rature` answering
# --version for the channel $2. With $3 = broken, its `rature` fails as a binary missing a
# library does; with $3 = wrong-sum, the sum published is not its own.
make_archive() {
  local version=$1 channel=$2 how=${3:-}
  local folder="$tmp/build/rature-$version" out="$tmp/www/dl/releases/download/v$version"
  local name="rature-$version-x86_64-linux.tar.gz"
  mkdir -p "$folder/share/applications" "$folder/share/icons/hicolor/scalable/apps" "$out"
  if [ "$how" = broken ]; then
    printf '#!/bin/sh\necho "rature: error while loading shared libraries: libfake.so.1" >&2\nexit 127\n' >"$folder/rature"
  else
    printf '#!/bin/sh\nif [ "${1:-}" = --version ]; then echo "rature %s (%s, 0123456789ab)"; fi\n' \
      "$version" "$channel" >"$folder/rature"
  fi
  chmod 0755 "$folder/rature"
  cp "$root/LICENSE" "$folder/LICENSE"
  cp "$root/packaging/linux/rature.desktop" "$folder/share/applications/"
  cp "$root/packaging/linux/icons/hicolor/scalable/apps/rature.svg" \
    "$folder/share/icons/hicolor/scalable/apps/"
  tar -C "$tmp/build" -czf "$out/$name" "rature-$version"
  (cd "$out" && sha256sum "$name" >"$name.sha256")
  if [ "$how" = wrong-sum ]; then
    printf '%064d  %s\n' 0 "$name" >"$out/$name.sha256"
  fi
}

# A release of the API, as GitHub writes it: its tag, its dates (published_at null for a draft)
# and its assets, then its notes, holding commas and an install line.
release_json() {
  local version=$1 published=$2 prerelease=false draft=false at
  case "$version" in *-*) prerelease=true ;; esac
  if [ "$published" = null ]; then
    draft=true
    at=null
  else
    at="\"$published\""
  fi
  local name="rature-$version-x86_64-linux.tar.gz"
  printf '{"url":"%s/api/releases/1","id":1,"author":{"login":"ci","type":"Bot"},"tag_name":"v%s","target_commitish":"dev","name":"Rature %s","draft":%s,"prerelease":%s,"created_at":"2026-01-01T00:00:00Z","published_at":%s,"assets":[{"name":"%s","state":"uploaded","created_at":"2026-01-01T00:00:00Z","browser_download_url":"%s/v%s/%s"},{"name":"%s.sha256","created_at":"2026-01-01T00:00:00Z","browser_download_url":"%s/v%s/%s.sha256"}],"tarball_url":"%s/tarball/v%s","body":"Rature %s, built from 0123456789ab.\\n\\nLinux x86_64. Install: `curl -fsSL https://example.invalid/install.sh | sh -s -- --channel beta`, then run it."}' \
    "$base" "$version" "$version" "$draft" "$prerelease" "$at" \
    "$name" "$download" "$version" "$name" "$name" "$download" "$version" "$name" \
    "$base" "$version" "$version"
}

# The API at $1 (a folder of the server): the releases "version published-at" read from stdin,
# newest first as GitHub lists them; `latest` the stable one named by $2, when given.
make_api() {
  local api="$tmp/www/$1" latest=${2:-} version published first=1
  rm -rf "$api"
  mkdir -p "$api/releases/tags"
  {
    printf '['
    while read -r version published; do
      [ -n "$version" ] || continue
      release_json "$version" "$published" >"$api/releases/tags/v$version"
      if [ "$published" = null ]; then rm "$api/releases/tags/v$version"; fi
      if [ -z "$first" ]; then printf ','; fi
      first=
      release_json "$version" "$published"
    done
    printf ']'
  } >"$api/releases@per_page=100"
  if [ -n "$latest" ]; then cp "$api/releases/tags/v$latest" "$api/releases/latest"; fi
}

make_archive 0.1.0 stable
make_archive 0.1.1 stable
make_archive 0.2.0-beta.12 beta
make_archive 0.1.2-beta.3 beta
make_archive 0.2.0-beta.13 beta
make_archive 0.2.0-alpha.feat-a-b.3 alpha
make_archive 0.2.0-alpha.feat-a-b.10 alpha
make_archive 0.2.0-alpha.feat-a-b-c.20 alpha
make_archive 0.2.0-alpha.feat-a.30 alpha
make_archive 0.3.0 stable broken
make_archive 0.4.0 stable wrong-sum

releases() {
  cat <<'EOF'
0.2.0-beta.13 null
0.2.0-alpha.feat-a-b.3 2026-11-10T12:00:00Z
0.1.2-beta.3 2026-11-10T09:00:00Z
0.2.0-alpha.feat-a-b-c.20 2026-11-09T12:00:00Z
0.2.0-alpha.feat-a.30 2026-11-09T11:00:00Z
0.2.0-alpha.feat-a-b.10 2026-11-09T10:00:00Z
0.2.0-beta.12 2026-11-08T09:00:00Z
0.4.0 2026-11-07T09:00:00Z
0.3.0 2026-11-06T09:00:00Z
0.1.1 2026-11-05T09:00:00Z
0.1.0 2026-11-01T09:00:00Z
EOF
}
# Before 0.1.1: 0.1.0 is the latest stable.
grep -v -e '^0\.1\.1 ' -e '^0\.3\.0 ' -e '^0\.4\.0 ' < <(releases) | make_api api 0.1.0
make_api empty </dev/null
mkdir -p "$tmp/www/broken/releases"
echo 500 >"$tmp/www/broken/releases/latest.status"
# A port nothing listens on: the server's, once it is stopped, is not known; 1 is reserved.
nowhere="http://127.0.0.1:1"

# --- Running install.sh ----------------------------------------------------------------------

passed=0
failed=0
ok() {
  passed=$((passed + 1))
  echo "ok - $1"
}
not_ok() {
  failed=$((failed + 1))
  echo "FAIL - $1"
  echo "  exit $status; stdout:"
  sed 's/^/    | /' "$tmp/out"
  echo "  stderr:"
  sed 's/^/    | /' "$tmp/err"
}
check() {
  local what=$1
  shift
  if "$@"; then ok "$what"; else not_ok "$what"; fi
}
said() { grep -qF -- "$1" "$tmp/out" "$tmp/err"; }
exited() { [ "$status" -eq "$1" ]; }

# A new home, with a space in its path; its data folder, and an empty TMPDIR, inside it.
fresh_home() {
  home="$tmp/home $1"
  rm -rf "$home"
  mkdir -p "$home/tmp" "$tmp/cwd"
  bin="$home/.local/bin"
  data="$home/data"
}

# Runs install.sh as `curl … | sh -s -- <args>` does, from a folder that is not the clone, with
# the API $api_root (a folder of the server, or an address with ://).
api_root=api
install_sh() {
  local api=$api_root
  case "$api" in *://*) ;; *) api="$base/$api" ;; esac
  status=0
  (cd "$tmp/cwd" && env HOME="$home" XDG_DATA_HOME="$data" TMPDIR="$home/tmp" \
    RATURE_RELEASES="$api" "${shell[@]}" -s -- "$@" <"$script" >"$tmp/out" 2>"$tmp/err") ||
    status=$?
}
installed() { [ -x "$bin/rature" ] && [ "$("$bin/rature" --version)" = "$1" ]; }
nothing_installed() { [ ! -e "$bin/rature" ] && [ ! -e "$data/applications/rature.desktop" ]; }
temp_removed() { [ -z "$(ls -A "$home/tmp")" ]; }
requests() { wc -l <"$tmp/requests.log"; }
no_request_since() { [ "$(requests)" -eq "$1" ]; }
ours() { [ -L "$bin/rt" ] && [ "$(readlink "$bin/rt")" = rature ]; }
exec_line() {
  local real
  real="$(cd "$bin" && pwd -P)/rature"
  grep -qxF "Exec=\"$real\" %f" "$data/applications/rature.desktop"
}
icon() { cmp -s "$data/icons/hicolor/scalable/apps/rature.svg" "$root/packaging/linux/icons/hicolor/scalable/apps/rature.svg"; }

echo "# install.sh under ${shell[*]}, against $base"

# Help and wrong uses: exit 2, nothing asked of the server, nothing installed.
fresh_home usage
install_sh --help
check "--help prints the options" eval 'exited 0 && said "--channel alpha --branch"'
before=$(requests)
for args in "--channel" "--branch" "--version" "--version " "--bogus" "--channel nightly" \
  "--channel alpha" "--branch feat/x" "--channel beta --branch feat/x" \
  "--version 0.1.0 --channel beta" "--version 0.1.0 --channel stable" \
  "--version 0.1.0 --branch feat/x" "--version 1.2" "--version 1.2.3/../x" \
  "--version v1.2.3" "--version 01.2.3" "--uninstall --channel beta"; do
  read -ra words <<<"$args"
  if [ "$args" = "--version " ]; then words=(--version ""); fi
  install_sh "${words[@]}"
  check "'$args' is refused (exit 2)" eval 'exited 2 && nothing_installed'
done
check "the alpha channel without --branch says so" eval 'install_sh --channel alpha; said "needs --branch"'
check "--version with --channel says why" eval 'install_sh --version 0.1.0 --channel beta; said "goes without --channel and --branch"'
check "no wrong use reached the server" no_request_since "$before"

# Install, then update.
fresh_home main
install_sh
check "install: the latest stable is installed" eval 'exited 0 && installed "rature 0.1.0 (stable, 0123456789ab)"'
check "install: --version's answer is printed" said "rature 0.1.0 (stable, 0123456789ab)"
check "install: rt is a relative link to rature" ours
check "install: the desktop entry runs the real path, quoted" exec_line
check "install: the entry keeps the package's other lines" grep -qx "StartupWMClass=rature" "$data/applications/rature.desktop"
check "install: the icon is in the data folder" icon
check "install: ~/.local/bin not in PATH is said" said "Add $bin to your PATH"
check "install: nothing left beside the binary" eval '[ ! -e "$bin/.rature.new" ] && [ ! -e "$data/applications/.rature.desktop.new" ]'
check "install: the download folder is removed" temp_removed
check "install: a first install says nothing of an open window" eval '! said "already open"'

releases | make_api api 0.1.1
install_sh
check "update: the new stable replaces the former" eval 'exited 0 && installed "rature 0.1.1 (stable, 0123456789ab)" && ours'
check "update: an open window keeping the former version is said" said "already open keeps the former version"

# --version.
install_sh --version 0.2.0-beta.12
check "--version installs that release exactly" eval 'exited 0 && installed "rature 0.2.0-beta.12 (beta, 0123456789ab)"'
install_sh --version 0.2.0-beta.99
check "--version of a release not there: exit 1, said" eval 'exited 1 && said "there is no release 0.2.0-beta.99"'
check "--version of a release not there: close versions given" said "close versions: 0.2.0-alpha.feat-a.30 0.2.0-beta.12 0.2.0-beta.13 0.3.0 0.4.0"
check "--version of a release not there: the installed one is kept" installed "rature 0.2.0-beta.12 (beta, 0123456789ab)"
check "--version of a release not there: nothing downloaded" eval '! grep -q "rature-0.2.0-beta.99" "$tmp/requests.log"'

# Beta: the last published, not the highest; never a draft.
install_sh --channel beta
check "beta: the one published last, not the highest nor the draft" eval 'exited 0 && installed "rature 0.1.2-beta.3 (beta, 0123456789ab)"'

# Alpha: the highest of the branch's slug, whole.
install_sh --channel alpha --branch feat/a_b
check "alpha feat/a_b: the highest of feat-a-b, not feat-a-b-c's" eval 'exited 0 && installed "rature 0.2.0-alpha.feat-a-b.10 (alpha, 0123456789ab)"'
install_sh --channel alpha --branch feat/a
check "alpha feat/a: feat-a's own, not feat-a-b's" eval 'exited 0 && installed "rature 0.2.0-alpha.feat-a.30 (alpha, 0123456789ab)"'
install_sh --channel alpha --branch feat/none
check "alpha of a branch with none: exit 1, said" eval 'exited 1 && said "there is no alpha release for feat/none"'
check "alpha of a branch with none: the installed one is kept" installed "rature 0.2.0-alpha.feat-a.30 (alpha, 0123456789ab)"

# A binary that does not answer stays installed, and that is said.
install_sh --version 0.3.0
check "a binary that does not answer --version stays installed, said" \
  eval 'exited 0 && [ -x "$bin/rature" ] && said "did not answer" && said "libfake.so.1"'

# From the clone, as ./install.sh.
status=0
(cd "$root" && env HOME="$home" XDG_DATA_HOME="$data" TMPDIR="$home/tmp" \
  RATURE_RELEASES="$base/api" "${shell[@]}" ./install.sh --version 0.1.0 >"$tmp/out" 2>"$tmp/err") ||
  status=$?
check "./install.sh from the clone installs as well" eval 'exited 0 && installed "rature 0.1.0 (stable, 0123456789ab)"'

# The launcher's files failing is said, not undone.
fresh_home readonly
real_data=$data
data="$home/a file"
: >"$data"
install_sh
check "a data folder that cannot be written: installed all the same, said" \
  eval 'exited 0 && installed "rature 0.1.1 (stable, 0123456789ab)" && said "the launcher entry was not written"'
data=$real_data

# A wrong sum installs nothing.
fresh_home sum
install_sh --version 0.4.0
check "a wrong sum: exit 1, said" eval 'exited 1 && said "is not the one published: nothing was installed"'
check "a wrong sum: nothing installed" eval 'nothing_installed && [ ! -e "$bin/rt" ]'
check "a wrong sum: the download folder is removed" temp_removed

# 404, network and GitHub failures.
fresh_home failures
api_root=empty
install_sh
check "no stable release (404): exit 1, said" eval 'exited 1 && said "there is no stable release yet" && nothing_installed'
install_sh --channel beta
check "no beta release: exit 1, said" eval 'exited 1 && said "there is no beta release"'
install_sh --version 0.1.0
check "--version with no release (404): exit 1, said" eval 'exited 1 && said "there is no release 0.1.0"'
api_root=$nowhere
install_sh
check "the network failing is said as such" eval 'exited 1 && said "is the network up?"'
api_root=broken
install_sh
check "GitHub failing is said as such" eval 'exited 1 && said "GitHub answered 500"'
api_root=api

# An rt that is not Rature's: left alone, checked before anything is downloaded.
fresh_home rt
mkdir -p "$bin"
printf '#!/bin/sh\necho mine\n' >"$bin/rt"
before=$(requests)
install_sh
check "a foreign rt file: exit 1, said" eval 'exited 1 && said "is not Rature'"'"'s: left alone"'
check "a foreign rt file: left as it was" eval '[ ! -L "$bin/rt" ] && grep -qx "echo mine" "$bin/rt"'
check "a foreign rt file: nothing installed nor asked of the server" eval 'nothing_installed && no_request_since "$before"'
rm "$bin/rt"
ln -s /usr/bin/true "$bin/rt"
install_sh
check "a foreign rt link: exit 1, left alone" eval 'exited 1 && [ "$(readlink "$bin/rt")" = /usr/bin/true ] && nothing_installed'
install_sh --no-rt
check "--no-rt installs beside a foreign rt, left alone" \
  eval 'exited 0 && installed "rature 0.1.1 (stable, 0123456789ab)" && [ "$(readlink "$bin/rt")" = /usr/bin/true ]'

# --uninstall, without a clone nor the network.
api_root=$nowhere
install_sh --uninstall
check "--uninstall beside a foreign rt: removes Rature's files, keeps that rt" \
  eval 'exited 0 && [ ! -e "$bin/rature" ] && [ "$(readlink "$bin/rt")" = /usr/bin/true ] && said "left alone"'
fresh_home uninstall
api_root=api
install_sh
check "an install to remove" eval 'exited 0 && ours && exec_line && icon'
api_root=$nowhere
install_sh --uninstall
check "--uninstall: exit 0" exited 0
check "--uninstall: rature and its rt removed" eval '[ ! -e "$bin/rature" ] && [ ! -e "$bin/rt" ] && [ ! -L "$bin/rt" ]'
check "--uninstall: the entry and the icon removed" \
  eval '[ ! -e "$data/applications/rature.desktop" ] && [ ! -e "$data/icons/hicolor/scalable/apps/rature.svg" ]'
install_sh --uninstall
check "--uninstall again: nothing to remove, exit 0" exited 0
api_root=api

echo "# $passed passed, $failed failed (install.sh under ${shell[*]})"
[ "$failed" -eq 0 ]

#!/bin/sh
# Installs Rature from a release, into ~/.local/bin, without root and without Rust
# (docs/releases.md, "Installing"):
#   curl -fsSL https://raw.githubusercontent.com/KristenJestin/rature/main/install.sh | sh
#   … | sh -s -- --channel beta
#   … | sh -s -- --channel alpha --branch feat/12-search
#   … | sh -s -- --version 0.2.0-beta.12
#   ./install.sh [options]            the same from a clone
# The archive's SHA-256 sum is checked before anything is installed; `rt` is a link to `rature`.
# The launcher's entry and icon go to $XDG_DATA_HOME. `RATURE_RELEASES` names another place of
# the releases' API (for tests). Everything is in `main`, called on the last line: a download cut
# short runs nothing.
set -eu

usage() {
  cat <<'EOF'
Installs Rature from a release into ~/.local/bin (and rt, a link to it), and adds it to the
application launcher. Run it again to update.

  install.sh                                  the latest stable release
  install.sh --channel beta                   the beta published last
  install.sh --channel alpha --branch <name>  the highest alpha of a branch
  install.sh --version X.Y.Z[-pre]            that release exactly, of any channel
  install.sh --no-rt                          without the rt link
  install.sh --uninstall                      remove what it installed (config and state are kept)
  install.sh --help                           this help
EOF
}

# printf rather than echo: a path or an answer may hold a backslash, which dash's echo reads.
say() {
  printf '%s\n' "$*"
}

fail() {
  say "install.sh: $*" >&2
  exit 1
}

# A wrong use of the options: exit 2.
misuse() {
  say "install.sh: $*" >&2
  exit 2
}

# The API's answer at $1 in the file $2: 0 when it is there, 1 when it is not (404), and a
# failure said as one otherwise, the network's or GitHub's.
read_api() {
  code=$(curl -sSL -o "$2" -w '%{http_code}' -- "$1") || fail "cannot read $1 (is the network up?)"
  case "$code" in
    200) return 0 ;;
    404) return 1 ;;
    *) fail "cannot read $1: GitHub answered $code" ;;
  esac
}

# The archives' addresses in an answer of the API, one per line.
archives() {
  tr ',' '\n' <"$1" | sed -n 's/.*"browser_download_url": *"\([^"]*-x86_64-linux\.tar\.gz\)".*/\1/p'
}

# The published releases' archives in a list of releases of the API, one line each: when its
# release was published, the version and the address. A `tag_name` and `published_at` come
# before a release's `assets` in GitHub's answer, so each address is read with the date of its
# own release: reset at each tag, and empty for a draft (`published_at` null), which is never
# offered. The dates are UTC, all of one shape, so they sort as text.
published() {
  tr ',' '\n' <"$1" | sed -n \
    -e 's/.*"tag_name": *"[^"]*".*/T/p' \
    -e 's/.*"published_at": *"\([^"]*\)".*/P \1/p' \
    -e 's/.*"published_at": *null.*/P/p' \
    -e 's|.*"browser_download_url": *"\([^"]*/rature-\([^/]*\)-x86_64-linux\.tar\.gz\)".*|U \2 \1|p' |
    awk '$1 == "T" { at = "" } $1 == "P" { at = $2 } $1 == "U" && at != "" { print at, $2, $3 }'
}

# The archive of the beta published last in a list of releases, not the highest version; ties
# go to the higher version.
latest_beta() {
  published "$1" | awk '$2 ~ /^[0-9]+\.[0-9]+\.[0-9]+-beta\.[0-9]+$/ { print }' |
    LC_ALL=C sort -k 1,1 -k 2,2V | tail -n 1 | cut -d ' ' -f 3
}

# The archive of the highest version among the alphas of the branch whose slug is $2, in a list
# of releases: `*-alpha.<slug>.<n>`, the slug whole (`feat-a` is not `feat-a-b`).
highest_alpha() {
  published "$1" | awk -v slug="$2" '{
      v = $2; sub(/^[0-9]+\.[0-9]+\.[0-9]+-alpha\./, "", v)
      if (v != $2 && v ~ /\.[0-9]+$/) { sub(/\.[0-9]+$/, "", v); if (v == slug) print }
    }' | sort -k 2,2V | tail -n 1 | cut -d ' ' -f 3
}

# The versions of the archives in the answer $1, up to five of those nearest the version $2.
close_versions() {
  archives "$1" | sed -n 's|.*/rature-\([^/]*\)-x86_64-linux\.tar\.gz$|\1|p' |
    { cat; echo "$2"; } | sort -uV |
    awk -v want="$2" '{ v[NR] = $0; if ($0 == want) at = NR }
      END { from = at - 2; if (from + 5 > NR) from = NR - 5; if (from < 1) from = 1
            for (i = from; i <= from + 5 && i <= NR; i++) if (i != at) print v[i] }' | head -n 5
}

# Our `rt` is the relative link this script makes; anything else there is someone else's.
ours() {
  [ -L "$1/rt" ] && [ "$(readlink "$1/rt")" = rature ]
}

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

# The desktop entry and the icons of the package unpacked at $1/share, written into the data
# folder $3, the entry running the binary $2 by its real path. Every step is checked: called as
# a condition, `set -e` does not stop it.
desktop_files() {
  share=$1/share
  [ -f "$share/applications/rature.desktop" ] || return 0
  real=$(cd "$(dirname "$2")" && pwd -P) || return 1
  real=$real/$(basename "$2")
  newline='
'
  case "$real" in
    *"$newline"*)
      say "install.sh: $real holds a newline: no launcher entry." >&2
      return 1
      ;;
    *[!A-Za-z0-9/._+-]*) command="$(exec_arg "$real") %f" ;;
    *) command="$real %f" ;;
  esac
  mkdir -p "$3/applications" || return 1
  # Line by line rather than with sed: the path is never read as a pattern or a replacement.
  while IFS= read -r line || [ -n "$line" ]; do
    case "$line" in
      Exec=*) printf '%s\n' "Exec=$command" ;;
      *) printf '%s\n' "$line" ;;
    esac
  done <"$share/applications/rature.desktop" >"$3/applications/.rature.desktop.new" || return 1
  mv -f "$3/applications/.rature.desktop.new" "$3/applications/rature.desktop" || return 1
  icons=$(cd "$share" && find icons -type f) || return 1
  for icon in $icons; do
    mkdir -p "$3/$(dirname "$icon")" || return 1
    install -m 0644 "$share/$icon" "$3/$icon" || return 1
  done
  # A desktop's cache of the icon theme is stale when the folder is newer than it.
  touch "$3/icons/hicolor" || return 1
  if command -v update-desktop-database >/dev/null 2>&1; then
    update-desktop-database "$3/applications" 2>/dev/null || true
  fi
  say "Added Rature to the application launcher ($3)."
}

# Removes what an install wrote, the files named here: no clone nor archive needed.
uninstall() {
  rm -f "$1/rature" "$1/.rature.new"
  removed="$1/rature"
  if ours "$1"; then
    rm -f "$1/rt"
    removed="$removed and its rt link"
  elif [ -e "$1/rt" ] || [ -L "$1/rt" ]; then
    say "install.sh: $1/rt is not Rature's: left alone." >&2
  fi
  rm -f "$2/applications/rature.desktop" "$2/icons/hicolor/scalable/apps/rature.svg"
  if [ -d "$2/icons/hicolor" ]; then touch "$2/icons/hicolor"; fi
  if command -v update-desktop-database >/dev/null 2>&1 && [ -d "$2/applications" ]; then
    update-desktop-database "$2/applications" 2>/dev/null || true
  fi
  say "Removed $removed, and Rature's launcher entry and icon. Kept its config and state."
}

main() {
  api=${RATURE_RELEASES:-https://api.github.com/repos/KristenJestin/rature}
  channel=
  branch=
  version=
  link=yes
  remove=no
  while [ $# -gt 0 ]; do
    case "$1" in
      --channel | --branch | --version)
        if [ $# -lt 2 ] || [ -z "$2" ]; then misuse "$1 needs a value"; fi
        case "$1" in
          --channel) channel=$2 ;;
          --branch) branch=$2 ;;
          *) version=$2 ;;
        esac
        shift 2
        ;;
      --no-rt) link=no && shift ;;
      --uninstall) remove=yes && shift ;;
      -h | --help)
        usage
        exit 0
        ;;
      *) misuse "unknown option $1 (see --help)" ;;
    esac
  done

  bin=$HOME/.local/bin
  data=${XDG_DATA_HOME:-}
  case "$data" in /*) ;; *) data=$HOME/.local/share ;; esac

  if [ "$remove" = yes ]; then
    if [ -n "$channel$branch$version" ]; then
      misuse "--uninstall goes without --channel, --branch and --version"
    fi
    uninstall "$bin" "$data"
    exit 0
  fi

  if [ -n "$version" ] && [ -n "$channel$branch" ]; then
    misuse "--version names a release of any channel: it goes without --channel and --branch"
  fi
  case "${channel:-stable}" in stable | beta | alpha) ;; *)
    misuse "the channel is stable, beta or alpha"
    ;;
  esac
  if [ "$channel" = alpha ] && [ -z "$branch" ]; then
    misuse "the alpha channel needs --branch <name>"
  fi
  if [ -n "$branch" ] && [ "$channel" != alpha ]; then
    misuse "--branch goes with --channel alpha"
  fi
  channel=${channel:-stable}
  # Written into an address below: a semantic version, nothing else.
  if [ -n "$version" ]; then
    case "$version" in
      *[!0-9A-Za-z.-]*) misuse "$version is not a version (X.Y.Z, or X.Y.Z-pre)" ;;
    esac
    printf '%s\n' "$version" | grep -Eqx '(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)\.(0|[1-9][0-9]*)(-[0-9A-Za-z-]+(\.[0-9A-Za-z-]+)*)?' ||
      misuse "$version is not a version (X.Y.Z, or X.Y.Z-pre)"
  fi

  for program in curl tar sha256sum sort uname; do
    command -v "$program" >/dev/null 2>&1 || fail "$program is needed"
  done
  [ "$(printf '1.10\n1.9\n' | sort -V 2>/dev/null | head -n 1)" = 1.9 ] ||
    fail "a sort that compares versions (GNU sort, sort -V) is needed"
  [ "$(uname -s)-$(uname -m)" = Linux-x86_64 ] || fail "there are packages for Linux x86_64 only"

  # Checked before the download: an rt that is not ours is never replaced.
  if [ "$link" = yes ] && { [ -e "$bin/rt" ] || [ -L "$bin/rt" ]; } && ! ours "$bin"; then
    say "install.sh: $bin/rt exists and is not Rature's: left alone, nothing was installed." >&2
    echo "Remove it, or install without the rt link with --no-rt." >&2
    exit 1
  fi

  work=$(mktemp -d)
  trap 'rm -rf "$work"' EXIT
  answer="$work/answer.json"
  if [ -n "$version" ]; then
    name="rature-$version-x86_64-linux.tar.gz"
    url=
    if read_api "$api/releases/tags/v$version" "$answer"; then
      for found in $(archives "$answer"); do
        case "$found" in */"$name") url=$found ;; esac
      done
    fi
    if [ -z "$url" ]; then
      say "install.sh: there is no release $version for Linux x86_64." >&2
      if read_api "$api/releases?per_page=100" "$work/list.json"; then
        close=$(close_versions "$work/list.json" "$version" | tr '\n' ' ' | sed 's/ $//')
        [ -z "$close" ] || say "install.sh: close versions: $close" >&2
      fi
      exit 1
    fi
  else
    case "$channel" in
      stable)
        read_api "$api/releases/latest" "$answer" || fail "there is no stable release yet"
        url=$(archives "$answer" | head -n 1)
        ;;
      beta)
        read_api "$api/releases?per_page=100" "$answer" || : >"$answer"
        url=$(latest_beta "$answer")
        [ -n "$url" ] || fail "there is no beta release"
        ;;
      alpha)
        # Each byte but ASCII letters, digits and dashes is a dash, as CI names the alpha.
        slug=$(printf '%s' "$branch" | LC_ALL=C tr -c 'A-Za-z0-9-' '-')
        # A branch whose alphas are all deleted has none, as one that never had any.
        read_api "$api/releases?per_page=100" "$answer" || : >"$answer"
        url=$(highest_alpha "$answer" "$slug")
        [ -n "$url" ] || fail "there is no alpha release for $branch (alpha.$slug)"
        ;;
    esac
  fi
  [ -n "$url" ] || fail "there is no $channel release for Linux x86_64"
  name=${url##*/}
  case "$name" in
    rature-*-x86_64-linux.tar.gz) ;;
    *) fail "unexpected archive $url" ;;
  esac
  folder=${name%-x86_64-linux.tar.gz}

  curl -fsSL -o "$work/$name" -- "$url" || fail "cannot download $url"
  curl -fsSL -o "$work/$name.sha256" -- "$url.sha256" || fail "cannot download $url.sha256"
  # `sha256sum --check` checks the files its lines name, whatever they are: one line, as
  # tools/package.sh writes it, naming this very archive, or another file would be checked.
  awk -v name="$name" '{ line = $0 } END {
      if (NR != 1 || length(line) != 66 + length(name)) exit 1
      if (substr(line, 1, 64) !~ /^[0-9a-f]+$/) exit 1
      mode = substr(line, 65, 2)
      exit !((mode == "  " || mode == " *") && substr(line, 67) == name)
    }' "$work/$name.sha256" ||
    fail "the SHA-256 sum published for $name is not one line naming it: nothing was installed"
  (cd "$work" && sha256sum --check --status "$name.sha256") ||
    fail "the SHA-256 sum of $name is not the one published: nothing was installed"
  mkdir "$work/unpacked"
  tar -xzf "$work/$name" -C "$work/unpacked" || fail "cannot unpack $name"
  [ -f "$work/unpacked/$folder/rature" ] || fail "$name holds no $folder/rature: nothing was installed"

  updating=no
  if [ -e "$bin/rature" ]; then updating=yes; fi
  mkdir -p "$bin"
  # Beside it, then renamed over it: a running Rature keeps its own file.
  install -m 0755 "$work/unpacked/$folder/rature" "$bin/.rature.new"
  mv -f "$bin/.rature.new" "$bin/rature"
  if [ "$link" = yes ]; then
    ln -sfn rature "$bin/rt"
    found=$(command -v rt || true)
    if [ -n "$found" ] && [ "$found" != "$bin/rt" ]; then
      say "install.sh: another rt, $found, comes first in PATH." >&2
    fi
  fi
  # The binary is installed: the launcher's files failing (a folder that cannot be written) is
  # said, not undone.
  desktop_files "$work/unpacked/$folder" "$bin/rature" "$data" ||
    echo "install.sh: the launcher entry was not written: see above." >&2
  if [ "$link" = yes ]; then
    say "Installed ${folder#rature-} in $bin (rature, and rt)."
  else
    say "Installed ${folder#rature-} in $bin (rature)."
  fi
  # Installed is installed: a binary that cannot run here (a system library missing) is said,
  # not undone.
  if said=$("$bin/rature" --version 2>&1); then
    say "$said"
  else
    say "install.sh: $bin/rature --version did not answer: $said" >&2
    echo "install.sh: it stays installed; see the release's notes for the system libraries it needs." >&2
  fi
  case ":${PATH:-}:" in *":$bin:"*) ;; *) say "Add $bin to your PATH to run it." ;; esac
  if [ "$updating" = yes ]; then
    echo "A Rature window already open keeps the former version until it is closed."
  fi
}

main "$@"

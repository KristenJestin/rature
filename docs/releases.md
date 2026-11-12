# Releases

Every branch that matters publishes its own build, with a version computed from its commits,
never typed. CI makes the tags and the releases itself: no tag is pushed by hand. The model is
Flottille's (KristenJestin/flottille, `docs/releases.md`).

| Branch | Channel | Version (example) | Published as |
|---|---|---|---|
| `main` | stable | `0.2.0` | release `v0.2.0`, latest |
| `dev` | beta | `0.2.0-beta.12` | pre-release `v0.2.0-beta.12` |
| `feat/*`, `fix/*` | alpha | `0.2.0-alpha.feat-12-search.4` | a pre-release per push, all deleted with the branch |

Nothing is published anywhere but GitHub's releases of the repository. A published version is
never replaced, its tag never moved nor its files changed: each build is its own release, an
alpha's too.

## Branches

`dev` is the default and integration branch. Branches start from `origin/dev` and reach it by
squash. `main` only gets releases, and stays an ancestor of `dev`.

## Releasing

A release does not squash nor merge: **`main` moves forward to a commit of `dev`** (a
fast-forward), and is then that very commit.

- **The pull request `dev → main` is opened for review**, and never merged with the button. The
  `release` workflow (`.github/workflows/release.yml`) closes it: started by hand
  (`gh workflow run release.yml --ref dev`, or "Run workflow" from `dev`), with the commit of
  `dev` to release as its input, a full SHA or `dev` for its head (the default). Once `main` is
  the pull request's head, GitHub shows it merged.
- **What it checks**: it runs from `dev` only; the input is a full SHA or `dev`; the commit is on
  `dev`; `main` is its ancestor. Then it moves `main` through GitHub's API, not forced, so GitHub
  refuses anything but a fast-forward. Any of these refused, nothing moves and the run fails,
  saying why. Its job runs no code of any branch: no checkout, no git, only `gh`.
- **Then the stable build**: a ref moved with the workflow's own token starts no `push` run, so
  the `release` workflow starts `ci.yml` on `main` itself. Run again when `main` is already the
  commit, it moves nothing and builds again, which publishes nothing already published.
- **The tag `vX.Y.Z` is put on the released commit**, the same on `main` and on `dev`.
- **The stable version is the one `dev` already had**: its beta's without the pre-release
  (`0.2.0-beta.12` releases `0.2.0`). Its changelog lists the commits of `dev` since the previous
  stable tag.
- A push to `main` that is not a commit of `dev` publishes nothing and fails its run.

## The version

`tools/version.sh stable|beta|alpha [--branch <name>] [--at <commit>]`, for HEAD by default:

- **It counts from the last release**: the highest stable tag `vX.Y.Z`, compared as versions,
  from its own commit on. Pre-release tags do not count. Without a stable tag, from `0.0.0` and
  the first commit. `stable` exits 3, printing nothing, when nothing was committed since.
- **The next version** follows the Angular commits counted, merges left out: `feat` bumps the
  minor, `fix` and `perf` the patch, a `!` after the type or a `BREAKING CHANGE:` footer the
  major; nothing that bumps is the next patch. **Before 1.0, everything bumps one level less**: a
  breaking change the minor, a `feat` the patch.
- **`Release-As: X.Y.Z`**, a footer of a commit since the last release, sets the next version
  when it is above the computed one (equal changes nothing; with several, the highest wins). A
  value below it, or not `X.Y.Z`, makes `tools/version.sh` exit 1 naming the commit, and so fails
  the build. The squash that brought CI into `dev` ends with `Release-As: 0.1.0`: the first
  release is `0.1.0`.
- **First parents only**: the commits counted, the types that bump and the changelog follow the
  build's own line, a merge being one commit of it.
- **The beta and alpha numbers** count those commits: each squash into `dev` makes the next beta,
  and the first one after a release is `-beta.1`.
- **An alpha names its branch**: each byte of the name but ASCII letters, digits and dashes is a
  dash, `feat/12-search` giving `alpha.feat-12-search.<n>`. CI and its cleanup follow that one
  rule; `feat/a_b` and `feat/a-b` share their alphas. A force-push with fewer commits gives a
  lower number.

`tools/changelog.sh [--at <rev> | <range>]` lists the Angular commits since the last release,
grouped (breaking changes, features, fixes, performance, other), `- **<scope>**: <subject>`, in
the commits' own language.

`Cargo.toml` keeps its own version. CI gives the computed one to the build
(`RATURE_BUILD_VERSION`, `RATURE_BUILD_CHANNEL`, `RATURE_BUILD_BRANCH`, read by `build.rs`).
`rature --version` says the version, the channel and the commit:
`rature 0.2.0-beta.12 (beta, 3f2a9c1d7b44)`. A build CI did not make is `local`:
`rature 0.1.0 (local, 3f2a9c1d7b44)`.

## What CI does

`.github/workflows/ci.yml` runs on every pull request, on a push to `main`, `dev`, `feat/**`
and `fix/**`, and by hand. A newer push cancels the older runs of its ref, but on `dev` and
`main`.

- **`verify`**: `cargo fmt --check`, `cargo clippy --all-targets --locked -- -D warnings` and
  `cargo test --locked`, on Rust 1.95. No test opens a window.
- **`build`**, on a push (and on `main` when the `release` workflow starts it): with a token that
  can only read, on Ubuntu 22.04, old enough a C library (glibc 2.35) that the binary runs on
  current Arch and Debian stable. It computes the channel (`main` stable, `dev` beta, any other
  branch alpha) and the version, builds with `cargo build --release --locked`, packages with
  `tools/package.sh` and writes the notes: the version, the channel, the commit, the install
  line, the window's system libraries and the changelog.
- **`publish`**, once `verify` and `build` passed: the only job that writes. It runs no code of
  the repository, no git, only `gh`. It reads the branch's tip again first: a branch that moved
  or was deleted publishes nothing. A version already published stays as it is; a draft left by a
  failed upload is made again. Stable releases are marked latest, the others pre-releases.

`.github/workflows/alpha-cleanup.yml` deletes a branch's alphas, with their tags, when the branch
is deleted.

## The archive

`tools/package.sh <version> <binary> <out folder>` makes `rature-<version>-x86_64-linux.tar.gz`:
one folder `rature-<version>/` with `rature`, `LICENSE`, `LICENSE-fonts.txt` and `share/` (the
desktop entry and the icons of `packaging/linux`), the same bytes from the same files. Its
`.sha256` beside it is read by `sha256sum --check`. Linux x86_64 only.

## Installing

```
curl -fsSL https://raw.githubusercontent.com/KristenJestin/rature/main/install.sh | sh
curl -fsSL https://raw.githubusercontent.com/KristenJestin/rature/dev/install.sh | sh -s -- --channel beta
curl -fsSL https://raw.githubusercontent.com/KristenJestin/rature/dev/install.sh | sh -s -- --channel alpha --branch feat/12-search
curl -fsSL https://raw.githubusercontent.com/KristenJestin/rature/dev/install.sh | sh -s -- --version 0.2.0-beta.12
```

`main` has an `install.sh` once the first stable release is out; until then take `dev`'s, or
the line each release's notes give, of its own commit. From a clone, `./install.sh` does the
same. No Rust and no root are needed: `curl`, `tar`, `sha256sum`, GNU `sort` (it compares
versions) and `uname`, on Linux x86_64, all checked first. Its body is one function called on its
last line, so a download cut short runs nothing. The model is Flottille's `install.sh`.

- **Which release**, read from GitHub's API (`/releases/latest`, `/releases/tags/v<version>`,
  `/releases?per_page=100`) with `curl`, `sed`, `awk`, `tr` and `sort`, no `jq`:
  - stable (the default): the latest release;
  - `--channel beta`: the `-beta.` pre-release **published last**, not the highest version; a
    draft is never offered;
  - `--channel alpha --branch <name>`: the **highest version** among the branch's alphas,
    `*-alpha.<slug>.<n>` of the 100 newest releases, the slug made as CI makes it and matched
    whole, so `feat-a` never takes `feat-a-b`'s. A branch without one is said.
  - `--version X.Y.Z[-pre]`: that release exactly, of any channel, checked as a version before it
    goes into an address, and refused with `--channel` or `--branch`. One that is not there is
    said with up to five close versions.

  `--branch` goes with `--channel alpha` only. An option without its value, or an unknown one,
  exits 2. A release that is not there (404), the network failing and GitHub failing are said
  apart.
- **The sum first**: the archive and its `.sha256` go to a `mktemp -d` folder removed on exit,
  and `sha256sum --check` runs before anything is installed: a mismatch installs nothing.
- **Installed**: the binary is written beside (`~/.local/bin/.rature.new`), then renamed over
  `rature`, so a window already open keeps its own file, and the former version until it is
  closed. `rt` is a relative link to it, never replacing an `rt` that is not Rature's: that is
  checked before the download, and the install stops (`--no-rt` installs without the link). The
  desktop entry and the icon go from the archive's `share/` to `$XDG_DATA_HOME`
  (`~/.local/share` by default), the entry's `Exec=` naming the binary's real path, quoted and
  escaped as the Desktop Entry spec says when it holds another character than letters, digits
  and `/._+-`. The launcher's files failing is said, not undone.
- **Then** `~/.local/bin/rature --version` is run and printed. A binary that does not answer (a
  system library missing) stays installed, and that is said. It also says when `~/.local/bin` is
  not in `PATH`.
- **`--uninstall`** needs no clone, no archive and no network: it removes `rature`, `rt` when it
  is Rature's link, `applications/rature.desktop` and `icons/hicolor/scalable/apps/rature.svg`,
  and keeps the config and the state.

`RATURE_RELEASES` names another root of the API than
`https://api.github.com/repos/KristenJestin/rature`, for tests: `tools/test-install.sh` serves
fake releases and archives on `127.0.0.1` (with `python3`) and runs `install.sh` in a temporary
`HOME`, `XDG_DATA_HOME` and `TMPDIR`, under `dash` when it is installed (`TEST_SH` names another
shell).

## What only a real push proves

The version and changelog scripts are run on throwaway repositories, the archive is made from a
binary, and `install.sh` runs against a fake API. What cannot run off GitHub is the workflows themselves: a `feat/` push publishing
an alpha, a second one the next beside it; deleting the branch removing them; the first squash
into `dev` publishing `0.1.0-beta.N`; the first `release` run moving `main` with the workflow's
token, the pull request shown merged, `ci.yml` started on `main`, `v0.1.0` tagged on the commit
released and marked latest, its binary starting without Rust; a run from another branch or of a
commit not on `dev` moving nothing and saying why; GitHub's API answering as `install.sh` reads
it, and the curl lines installing the stable, a beta and a branch's alpha on a machine without
Rust, `rature --version` printing each one's version and channel.

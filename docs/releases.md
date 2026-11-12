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

## What only a real push proves

The version and changelog scripts are run on throwaway repositories, and the archive is made
from a binary. What cannot run off GitHub is the workflows themselves: a `feat/` push publishing
an alpha, a second one the next beside it; deleting the branch removing them; the first squash
into `dev` publishing `0.1.0-beta.N`; the first `release` run moving `main` with the workflow's
token, the pull request shown merged, `ci.yml` started on `main`, `v0.1.0` tagged on the commit
released and marked latest, its binary starting without Rust; a run from another branch or of a
commit not on `dev` moving nothing and saying why.

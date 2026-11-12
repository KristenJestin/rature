# AGENTS.md — Rature

Instructions for any coding agent working in this repository. Read this file first.

## Overview

Rature is a small native Markdown reader for Linux, written in Rust on GPUI, that can also edit
in place. It is made for proofreading what someone else wrote, an AI agent most of all: it
follows the file as it is rewritten, shows what changed since it was last read, and lets a word
be corrected where it is read. Keyboard first, no webview. It also shows JSON, YAML, CSV, images
and code.

**The [README](README.md) is the reference for behaviour** (features, keys, config, where it
keeps things, limits); [`docs/releases.md`](docs/releases.md) for versions, CI and releases. This
file is how to work.

## Commands

```
cargo build
cargo test --locked
cargo clippy --all-targets --locked -- -D warnings
cargo fmt --check
```

The last three are CI's `verify` job (`.github/workflows/ci.yml`), on Rust 1.95, the crate's
`rust-version`.

- **Release builds run at low priority, one at a time**: `nice -n 19 cargo build --release`. The
  machine is the maintainer's own, in use while you work: no parallel builds, no stress loops.
- **`RATURE_TIMING=1`** prints the start-up timings on the standard error once the window has
  shown its first frame (`src/timing.rs`), and on a canvas each frame's cost as it is painted
  (`src/canvas_view.rs`). `tools/capture.sh` sets it and prints them: use it rather than opening
  a window (see Testing).

## Where things live

`src/` is flat, one module per file:

| File | What it is |
|---|---|
| `main.rs` | The entry: arguments (`--help`, `--version`, `mcp`), the hand-over to a running window, leaving the terminal, starting the window |
| `mcp.rs` | `rature mcp`: an MCP server on stdin and stdout, both eras (2026-07-28 stateless, 2025-11-25 with `initialize`), `canvas_read`, reading and writing only inside its allowed folders; no window |
| `mcp_edit.rs` | `canvas_edit` and `canvas_create`: a call's operations checked, then applied all or none on the disk's canvas under its lock, written once through `canvas_board` |
| `instance.rs` | One window per user: a second `rature` hands its file to the running one over a socket in the runtime folder, and exits |
| `workspace.rs` | The window: the reader, a folder's tree beside it, and the picker over them |
| `reader.rs` | The file being read: what it shows, where the reading stands, what changed, the search inside it, the block being edited |
| `render.rs` | What the reader shows, built from a file's content: GPUI elements, nothing kept |
| `document.rs` | Markdown parsed (markdown-rs) into blocks and inlines, its links and anchors |
| `live.rs` | A block's Markdown edited where it is read: its marks faint in the text, the caret, the selection, the keys |
| `table.rs` | A Markdown table edited in place, cell by cell, rows and columns added and deleted |
| `list.rs` | A Markdown list edited in place, line by line beside its markers |
| `formats.rs` | What a file is shown as: Markdown, data (JSON, YAML) as a folding tree, CSV as a table, an image, a canvas, or code |
| `canvas.rs` | JSON Canvas 1.0 (Obsidian's `.canvas`), read and written without losing anything: unknown fields, key order and node order kept; a text's revision; where a new card goes, and its default size |
| `canvas_ops.rs` | Operations on a canvas by id, replayed on the disk's canvas with a field-by-field three-way merge (the disk wins a conflict), and their inverses for undo |
| `canvas_board.rs` | A canvas being edited: pending operations, undo and redo, each write the disk's text read again and the operations replayed on it, under a lock per canvas in the runtime folder; a new canvas made under the same lock |
| `canvas_view.rs` | A canvas shown as a board, which the reader draws it on: its camera (pan, zoom around the pointer, kept per file), only the cards in sight built, at a detail that follows the zoom (the whole Markdown, a box and its title, rectangles) |
| `format.rs` | Formatting on the user's own save (`Ctrl-s`), never on a change made from outside |
| `load.rs` | Reading a file off the main thread: reading, parsing, formulas, link checks, the comparison; only the latest request's result is kept |
| `cache.rs` | Results worked out off the main thread (a diagram, a code block's colours), keyed by their source: pending, done or failed |
| `diff.rs` | "What changed": the file against the file as last read, block by block, word by word inside a changed paragraph |
| `fsio.rs` | Writes of the files Rature shows and keeps: atomic, never over another program's write, through a link to its file |
| `state.rs` | What Rature keeps between runs, in `$XDG_STATE_HOME/rature`, written by a thread of its own |
| `watch.rs` | Watchers (notify): a file through its folder, so a rename over it is seen; the open folders of a tree |
| `tree.rs` | A folder's tree beside the reader, without what `.gitignore` hides, a dot on changed Markdown files |
| `picker.rs` | The pickers: a file by a few letters, a line of the folder (ripgrep), a heading, a file or folder to open |
| `start.rs` | The start page, with nothing open: the files and folders read last, and the desktop's file chooser |
| `config.rs` | `~/.config/rature/config.toml`, every key optional |
| `highlight.rs` | Code colouring, by syntect with two-face's languages, loaded once off the main thread |
| `diagram.rs` | Mermaid diagrams drawn to SVG by merman, off the main thread; `guard` catches the renderer's panics |
| `math.rs` | Formulas drawn to SVG by RaTeX |
| `timing.rs` | The start-up timings asked for with `RATURE_TIMING` |
| `icons.rs` | The icons Rature adds to the kit's (Lucide) |
| `kit.rs` | What Rature adds to the design system until the kit has it: the link colour, a text field's focus ring |

Besides `src/`:

- `build.rs`: the version `rature --version` prints (`docs/releases.md`, "The version").
- `assets/`: the brand (`assets/brand/`, its PNGs made by `tools/brand.sh`), the fonts' licence,
  the Mermaid syntax for highlighting.
- `packaging/linux/`: the desktop entry and the icon, installed by `install.sh` and packaged by
  `tools/package.sh`.
- `examples/weather-station/`: a small invented project that covers every view (Markdown with
  links, a broken one, an alert, a table and footnotes; CSV, JSON, YAML, an image, code).
- `tools/`:
  - `capture.sh`: the window captured into a PNG, in a nested headless Weston;
  - `screenshots.sh`: the README's images made again from the example, through `capture.sh`;
  - `xkeys.py`: keys typed, clicks, the wheel and drags into `capture.sh`'s nested display;
  - `canvas-gen.sh`: canvases of any number of cards, for measuring the board;
  - `brand.sh`: the brand's PNGs from its SVGs;
  - `version.sh`, `changelog.sh`, `package.sh`: the version, the notes and the archive of a
    build (`docs/releases.md`);
  - `test-install.sh`: `install.sh` run against fake releases from a local server, in temporary
    `HOME` and XDG folders.
- `.github/workflows/`: `ci.yml`, `release.yml`, `alpha-cleanup.yml` (`docs/releases.md`).

## Rules of the code

- **Rature writes nothing next to the files it reads.**
- **Its state goes only to `$XDG_STATE_HOME/rature`**, the folder `0700`, its files `0600`.
- **Every write of a user's file goes through `fsio`**: atomic (a new file renamed over the old
  one), never over what another program wrote since the file was read.
- **Parsing and any other slow work stay off the main thread**; the main thread only puts the
  result in place.
- **Linux only.**
- Never `panic = "abort"`: `diagram::guard` relies on catching the renderer's panics.
- Code, identifiers, comments and docs are in English. Rust 2024. A comment says why, not what.
- Touch only what the task needs; match the existing style.
- A new dependency of weight is said in the pull request.

## Testing

- **Tests never use the real config, state or home**: a temporary `HOME` and temporary XDG
  folders.
- **Never open a window on the user's screen.** No test opens one (GPUI's tests use its own test
  platform). A screenshot, or a look at the running window, goes through `tools/capture.sh`: a
  nested headless Weston with its own bus, `HOME` and XDG folders, never the user's session.

## Branches and releases (non-negotiable)

See `docs/releases.md`, "Branches" and "Releasing".

- **`dev` is the integration branch and the default branch.** A branch starts from `origin/dev`,
  as `feat/<n>-<topic>` or `fix/<n>-<topic>` (`<n>` the issue), and its pull request targets
  `dev`. **These two prefixes are the only ones**, whatever the commit's type: a documentation
  change goes on `feat/<n>-<topic>` or `fix/<n>-<topic>`, and its squash keeps its `docs(...)`
  type.
- **Never commit, merge, rebase, push or force-push on `main` or `dev` directly.** On one of them,
  create a branch first.
- **Pull requests into `dev` are merged by squash only.** The title is a plain Angular subject,
  since it becomes the commit; the description ends with `Closes #<n>`.
- **Each push to a `feat/` or `fix/` branch publishes its alpha, each squash into `dev` a beta.**
  A branch's alphas are deleted with it.
- **`main` gets no squash and no merge commit.** The release pull request `dev → main` is opened
  for review, and the `Release` workflow (`release.yml`, run from `dev`), not the merge button,
  closes it by moving `main` forward to a commit of `dev`.
- **No tag is pushed by hand**: CI makes them.
- **The version is computed from the commits' types**: `feat` bumps the minor, `fix` and `perf`
  the patch, a `!` after the type or a `BREAKING CHANGE:` footer the major, each one level less
  before 1.0. So a type is chosen for what the change does. A `Release-As: X.Y.Z` footer in a
  squash's body raises the next version.

## Commits

- The Angular convention: `<type>(<scope>): <subject>`, **the scope required**.
- Types: `feat`, `fix`, `refactor`, `perf`, `style`, `test`, `docs`, `chore`, `build`, `ci`.
- Scopes, as a guide rather than a closed list: `reader`, `edit`, `changes`, `files`, `state`,
  `theme`, `ui`, `start`, `workspace`, `diagram`, `tools`, `repo`, `brand`, `images`, `readme`,
  `assets`.
- **Subjects and bodies are in English**: the release notes are made from them. The history
  before 0.1 is in French; everything from now on is in English.
- One intent per commit. No `wip` commit. Published history is never rewritten.

## Public repository

This repository is public. No private path, host, account or token goes into the code, an
issue, a pull request, a commit or an example. Examples use invented data.

## When done

Run the four checks of Commands (`cargo build`, `cargo test --locked`,
`cargo clippy --all-targets --locked -- -D warnings`, `cargo fmt --check`), plus the checks the
issue asks for, and report their real output. If something fails, say so; don't claim green.

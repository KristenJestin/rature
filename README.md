<p align="center">
  <img src="assets/brand/icon.svg" alt="Rature" width="96">
</p>

<h1 align="center">Rature</h1>

<p align="center"><em>Read what changed, and fix it where you read it.</em></p>

Rature is a small native Markdown reader for Linux that you can also edit in place. It is made
for proofreading what someone else wrote, an AI agent most of all: it follows the file as it is
rewritten, shows what changed since you last read it, and lets you correct a word right where
you read it. Keyboard first, no webview.

<table>
  <tr>
    <td width="33%"><img src="docs/images/start.png" alt="The start page: recent files and folders, and buttons to open one"></td>
    <td width="33%"><img src="docs/images/folder.png" alt="A folder: its tree on the left, its README on the right"></td>
    <td width="33%"><img src="docs/images/changed.png" alt="A file rewritten since the last read: added, changed and removed blocks"></td>
  </tr>
  <tr>
    <td align="center">Started with nothing open</td>
    <td align="center">A folder and its README</td>
    <td align="center">What changed since the last read</td>
  </tr>
</table>

## Features

**Reading**

- Tables, task lists, footnotes, YAML front matter, GitHub alerts, highlighted code with a copy
  button, Mermaid diagrams, and math, inline and display.
- Parsing, formulas, diagrams, highlighting, link checks and diffs are worked out in the
  background; only the blocks in sight are built.
- Your reading position is kept for each file.

**What changed**

- Compared block by block with the last time you read the file, or with git `HEAD` (`D`): new
  blocks in green, changed ones in yellow with their changed words, removed ones struck in red.
- The first opening takes a snapshot. Leaving the file (for another one, or by quitting) marks
  what you saw as read; `m` does it at once.
- In a folder, a dot in the tree marks the Markdown files changed since you last read them.

**Editing in place**

- Click a block, or press `i` for the block at the top, and type. The block still looks as it
  reads: a Markdown mark (`##`, `**`, backticks, a link's address) shows, faint, only where the
  caret touches it.
- Tables keep their grid: each cell is edited in place, `Tab` moves to the next one, and rows and
  columns are added or deleted from the toolbar over the table or with `Alt-Shift` and an arrow,
  `Backspace` or `Delete`.
- Lists keep their markers: `Enter` starts the next item (or leaves the list on an empty one),
  `Tab` and `Shift-Tab` nest and un-nest, `Backspace` at an item's start removes its bullet, a
  click ticks a task box.
- Saved 800 ms after you stop typing. `Ctrl-s` saves and formats (Markdown with dprint, JSON
  pretty-printed).
- Writes are atomic: a new file is renamed over the old one, its permissions kept, and a
  symbolic link is followed to its file. If another program wrote the file in the meantime, it is
  not overwritten: a Markdown block is merged into the new text, otherwise what you typed goes to
  the clipboard.

**Other formats**

- JSON and YAML as foldable trees, CSV as a table, images: read only.
- Code opens in an editor, highlighted by its extension.
- Text that is not valid UTF-8 is shown read only. Binary files and text files over 10 MB are not
  opened; `e` hands them to nvim.

**Navigation and search**

- A folder opens with its tree beside the page (left or right), without what `.gitignore`
  ignores, and shows its README.
- Relative links to files and anchors, back and forward, an outline of the headings. Broken links
  show in red. Clicking a heading copies its link (`file.md#slug`).
- A plain click on a link edits it like any text; `Ctrl`-click follows it. Outside Rature, only
  `http`, `https` and `mailto` links open.
- Search in the file (`Ctrl-f`), across the folder with ripgrep (`s`), or find a file by a few
  letters (`f`, recent files first).

**Live reload**

- Written in place or replaced by a rename, as agents do, the file is read again and you stay
  where you were.

**Themes**

- The colours of [gpui-design-system](https://github.com/KristenJestin/gpui-design-system), or,
  on [Omarchy](https://omarchy.org) (an Arch Linux and Hyprland setup), its current theme,
  followed live: a theme change cross-fades the window, and formulas and diagrams are drawn again
  in the new colours.

## Install

Requirements:

- Linux.
- A Rust toolchain, 1.95 or later.
- Optional: [ripgrep](https://github.com/BurntSushi/ripgrep) (`rg`) to search a folder, and
  `nvim` with `xdg-terminal-exec` for the `e` key.

From a clone:

```sh
./install.sh
```

It builds a release binary and installs it as `~/.local/bin/rature`, with `rt` as a link to it,
and adds Rature to the application launcher (its desktop entry and icon go to
`$XDG_DATA_HOME`). No root is needed. Run it again to update.

| Option | |
|---|---|
| `--no-rt` | install without the `rt` link |
| `--uninstall` | remove what it installed; config and state are kept |
| `--help` | show the options |

If `~/.local/bin/rt` already exists and is not Rature's link, the install stops and leaves it
alone: remove it, or use `--no-rt`.

## Usage

```sh
rature notes.md     # a file
rature docs/        # a folder: its tree and its README
rt notes.md         # the same, shorter
```

The window opens and the terminal is given back. There is one window per user: opening another
file while Rature runs sends it to that window. Started without a file (from the launcher, say),
Rature lists the recent files and folders, with buttons to the desktop's file chooser.

## Keys

**Reading**

| Keys | Action |
|---|---|
| `j` `k`, `Down` `Up` | line down, up |
| `Ctrl-d` `Ctrl-u` | half a page down, up |
| `Space`, `PageDown` | a page down |
| `Shift-Space`, `PageUp`, `Ctrl-b` | a page up |
| `gg` `G`, `Home` `End` | top, bottom |
| `Ctrl-o` `Ctrl-i`, `Alt-Left` `Alt-Right` | back, forward |
| `o` | outline |
| `Ctrl-=` (or `Ctrl-+`) `Ctrl--` `Ctrl-0` | zoom in, out, reset |
| `zM` `zR` | fold, unfold every JSON or YAML node |
| `e` | open the file in nvim, in a new terminal (`xdg-terminal-exec`) |
| `q` | quit |

**Search and open**

| Keys | Action |
|---|---|
| `Ctrl-f`, `n` `N`, `Esc` | search the file, next and previous hit, clear |
| `f`, `Ctrl-p` | find a file in the folder (with nothing open: open one) |
| `s`, `Ctrl-Shift-f` | search the folder (ripgrep) |
| `O` | open a recent file or folder, or a path (`Tab` completes it) |
| in a picker (`f` `s` `o` `O`): `Up` `Down`, `Ctrl-p` `Ctrl-n`, `Ctrl-k` `Ctrl-j`, `Esc` | previous, next, close |
| on the start page: `j` `k`, `Enter` or `l` | move, open |

**What changed**

| Keys | Action |
|---|---|
| `]c` `[c` | next, previous change |
| `m` | mark read |
| `D` | compare with the last read, or with git `HEAD` |

**Editing**

| Keys | Action |
|---|---|
| `i`, click | edit a block |
| `Esc`, `Ctrl-Enter` | stop editing |
| `Ctrl-z`, `Ctrl-Shift-z` or `Ctrl-y` | undo, redo |
| `Ctrl-s` | save and format |
| table: `Tab` `Shift-Tab` | next, previous cell |
| table: `Alt-Shift` and an arrow | add a row above or below, a column left or right |
| table: `Alt-Shift-Backspace`, `Alt-Shift-Delete` | delete the row, the column |
| list: `Tab` `Shift-Tab` | nest, un-nest the item |

**Folder tree**

| Keys | Action |
|---|---|
| `t` | show or hide the tree (or the button at the corner of the bar) |
| `Ctrl-h` `Ctrl-l` | focus the tree, the page |
| `j` `k` (or arrows) | move |
| `l`, `Enter`, `Right` | open |
| `h`, `Left` | close |
| `gg` `G` | first, last |

## Config

`~/.config/rature/config.toml` (or under `$XDG_CONFIG_HOME`). Every key is optional; these are
the defaults:

```toml
tree = "left"            # or "right": where a folder's tree stands
tree_width = 260
column = 760             # the reading column's width
font = "Geist"           # the text; Geist and Geist Mono are bundled
code_font = "Geist Mono"
font_size = 16
```

## Where it keeps things

Rature writes nothing next to the files you read. Besides its config, it keeps its state in
`$XDG_STATE_HOME/rature` (`~/.local/state/rature` by default): reading positions, recent files
and folders, and a snapshot of each file as you last read it, which "what changed" compares to.

- The folder is readable by you alone (`0700`, its files `0600`), so a copy of a private note
  stays private.
- It does not grow for ever: snapshots are kept for the 500 most recent files and 90 days at
  most, reading positions for 1000 files and 90 days.

## Limits

- Linux only: one window per user through a Unix socket, the XDG file chooser, nvim through
  `xdg-terminal-exec`.
- Editing is per block. A heading or a list item shows its marks while you edit it, so its text
  shifts by their width. `Enter` splits a block only once it is saved.
- "What changed" covers Markdown files only. A paragraph holding math is shown old and new, not
  word by word. Your own edits count as changes until you mark the file read.
- A CSV cell or a JSON or YAML value stays on one line: a cell is cut to its column, a long value
  scrolls sideways.

## Development

```sh
cargo test
cargo build --release
```

[`examples/weather-station/`](examples/weather-station/) is a small invented project that
covers every view: Markdown with links, a broken one, an alert, a table and footnotes, plus CSV,
JSON, YAML, an image and code.

[`tools/capture.sh`](tools/capture.sh) captures the window into a PNG, inside a nested headless
Weston with its own `HOME` and XDG folders, never your session:

```sh
tools/capture.sh examples/weather-station out.png
```

Environment variables tune it: `KEYS` types keys into the window first, `THEN` runs a command
afterwards (to change the file, say), `STATE` starts from a given state folder, `OMARCHY=1` uses
a copy of the current Omarchy theme, `W` and `H` set the size. It needs `weston` (with
Xwayland), `weston-screenshooter`, `dbus-daemon` and ImageMagick.

[`tools/screenshots.sh`](tools/screenshots.sh) makes the images of this README again from the
example, through `capture.sh`. It needs a release build.

## Built with

Rust and [GPUI](https://github.com/zed-industries/zed), Zed's UI framework, through
[gpui-kit](https://crates.io/crates/gpui-kit). The window is built from
[gpui-design-system](https://github.com/KristenJestin/gpui-design-system), shared with the
author's agent runner, Flottille: its colours, fonts, icons, components and motion. Markdown is
parsed by [markdown-rs](https://github.com/wooorm/markdown-rs) and drawn by Rature's own
elements; Mermaid diagrams by [Merman](https://crates.io/crates/merman) and math by
[RaTeX](https://crates.io/crates/ratex-svg), both to SVG; code highlighted by
[syntect](https://github.com/trishume/syntect) with [two-face](https://crates.io/crates/two-face);
diffs by [similar](https://crates.io/crates/similar); files followed by
[notify](https://github.com/notify-rs/notify); the file finder matched by
[nucleo](https://github.com/helix-editor/nucleo); Markdown formatted by
[dprint](https://dprint.dev).

## License

Rature is free software under the GNU Affero General Public License, version 3 only
(`AGPL-3.0-only`). See [LICENSE](LICENSE).

The default fonts, Geist and Geist Mono, are by
[The Geist Project Authors](https://github.com/vercel/geist-font), under the SIL Open Font
License 1.1. They are embedded through gpui-design-system; the licence text is kept in
[assets/fonts/OFL.txt](assets/fonts/OFL.txt).

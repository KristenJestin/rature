# Rature

A minimal Markdown reader you can also edit in place, for proofreading what an agent (or anyone)
writes. Native, keyboard first, no webview.

Linux only: one window per user through a Unix socket, `e` opening nvim through
`xdg-terminal-exec`, the XDG file chooser, and the theme followed through Omarchy's hook.

## What it does

![Started without a file](docs/images/start.png)
![A folder](docs/images/folder.png)
![What changed since the last read](docs/images/changed.png)

- **A file or a folder**: `rature file.md` or `rature folder/`. A folder shows a tree (left or
  right), ignoring what `.gitignore` ignores, and opens its README. One window: opening another
  file sends it there. Started without one, the window lists the recent files and folders, with
  buttons to the desktop's file chooser (XDG portal). `f` there, or `O` at any time, opens one by
  its path, Tab completing it.
- **No trace in the folder**: config in `~/.config/rature/config.toml`, reading positions, recent
  files and read snapshots in `$XDG_STATE_HOME/rature` (`~/.local/state/rature`), readable by you
  alone. Snapshots are kept for the 500 most recent files and 90 days at most, positions for
  1000 files.
- **Markdown**: tables, task lists, footnotes, highlighted code (copy button), Mermaid diagrams,
  math (inline and display), YAML front matter, GitHub alerts. Reading and parsing the file, its
  formulas, its link checks and "what changed" are worked out in the background, as diagrams and
  code highlighting are; only the blocks, rows and lines in sight are built.
- **Other formats**: JSON and YAML as foldable trees, CSV as a table, images, read only. Code
  opens in an editor, highlighted by extension: click in it or `i`, `Esc` to leave. Binary files
  and text files over 10 MB are refused (`e` opens them in nvim); images are shown whatever
  their size. Text that is not valid UTF-8 is shown read only. Only regular files are opened:
  no pipes or devices.
- **Links**: relative links to files and anchors, back and forward, an outline. `Ctrl`-click
  follows a link (outside Rature: `http`, `https` and `mailto` only), a plain click edits it like
  any text. Broken links are red. Clicking a heading copies its link (`file.md#slug`).
- **Search**: in the file (`Ctrl-f`), across the folder through ripgrep (`s`), files by a few letters
  (`f`, recent first).
- **Live reload**: written in place or replaced by a rename, the file is read again where you are.
- **What changed**: since the last read (or against git `HEAD`), block by block: new blocks green,
  changed ones yellow with their changed words, removed ones struck in red. The first opening
  takes the snapshot; leaving the file (another one, or quitting) marks what you saw read, `m`
  does it at once.
- **Edit in place**: click a block, or `i` for the block at the top, and you write in it where it
  is read, and it looks as it is read: a mark (`##`, `**`, backticks, a link's address) shows,
  faint, only where the caret touches what it marks. The arrows go on to the next block. A table keeps its grid: each cell is edited
  where it is, `Tab` to the next one, rows and columns added and deleted from the tools over it
  (`Alt-Shift` and an arrow, `Backspace` or `Delete`), and the table is written back as Markdown. A list keeps its bullets, numbers and task boxes:
  each line is edited beside its marker, `Enter` starts the next item, or on an empty one leaves the list,
  `Backspace` at an item's start takes its bullet away, `Tab` and `Shift-Tab` nest and un-nest it, a click ticks a box. Saved 800 ms after you stop typing, code too, whole or not at all (a new file renamed over the
  old one, its permissions kept, a link followed to its file). Written meanwhile by another
  program, the file is not overwritten: a Markdown block is merged into its new text, otherwise
  what you typed goes to the clipboard. `Ctrl-s` saves and formats (Markdown by
  dprint, JSON pretty-printed).
- **Theme**: the design system's own colours, or, on [Omarchy](https://omarchy.org) (an Arch
  Linux and Hyprland setup), its current theme, followed live: a theme change cross-fades the
  open window, formulas and diagrams drawn again in its colours.

## Config

`~/.config/rature/config.toml`, every key optional:

```toml
tree = "left"            # or "right"
tree_width = 260
column = 760             # reading width
font = "Geist"           # the text read, bundled
code_font = "Geist Mono" # code, bundled
font_size = 16
```

## Stack

Rust and GPUI through gpui-kit, as Flottille (the author's desktop app that runs coding agents).
No webview. The window is built from the components Flottille shares, [gpui-design-system](https://github.com/KristenJestin/gpui-design-system): its
colours, Geist and Geist Mono (bundled, SIL OFL), Lucide icons, buttons, rows, the search modal,
toasts, and its motion
(entrances that rise and fade in, hovers and choices that fade, rows that fold, a theme that
cross-fades). Markdown parsed by `markdown` (mdast)
and drawn by our own elements; Mermaid by Merman and math by RaTeX, both to SVG;
syntect with two-face for code; `similar` for the diff; `notify` to follow files; `nucleo` for
the file finder; dprint for Markdown formatting.

## Installing

`./install.sh` builds this clone and installs `rature` into `~/.local/bin`, with `rt` a link to
it, and adds Rature to the application launcher (entry and icon in `$XDG_DATA_HOME`). No root.
An `rt` that is not already Rature's link is left alone and the install stops (`--no-rt` installs
without the link). Run it again to update; `./install.sh --uninstall` removes what it installed. Started from the launcher, without
a file, it shows the recent files and folders.

## Using

`rature file.md` (or `rt file.md`) opens the file and gives the terminal back. With a window
already open, the file goes to it. The file is followed: written in place or replaced by a
rename, as an agent does, it is read again where you are.

| Keys | |
|---|---|
| `j` `k`, arrows | line down, up |
| `Ctrl-d` `Ctrl-u` | half a page |
| `Space` `PageDown` | a page down |
| `Shift-Space` `PageUp` `Ctrl-b` | a page up |
| `gg` `G`, `Home` `End` | top, bottom |
| `Ctrl-f`, `n` `N`, `Esc` | search the file, next and previous hit, clear |
| `f`, `Ctrl-p` | find a file (with nothing open: open one) |
| `O` | open a recent file or folder, or a path (`Tab` completes) |
| `s`, `Ctrl-Shift-f` | search the folder (ripgrep) |
| `o` | outline |
| `Ctrl-o` `Ctrl-i`, `Alt-Left` `Alt-Right` | back, forward |
| `]c` `[c` | next, previous change |
| `m` | mark read |
| `D` | compare with the last read or with git `HEAD` |
| `i`, click | edit a block; `Esc` or `Ctrl-Enter` to stop |
| editing: `Ctrl-z`, `Ctrl-Shift-z` `Ctrl-y` | undo, redo |
| `Ctrl-s` | save and format |
| `zM` `zR` | fold, unfold every JSON or YAML node |
| `Ctrl-=` (or `Ctrl-+`) `Ctrl--` `Ctrl-0` | zoom in, out, reset |
| `t` | show, hide the tree (or the button at the corner of the bar) |
| `Ctrl-h` `Ctrl-l` | to the tree, to the page |
| tree: `j` `k`, `l` `Enter`, `h`, `gg` `G` (arrows too) | move, open, close, first, last |
| `e` | the file in nvim, in the desktop's terminal (`xdg-terminal-exec`) |
| `q` | quit |

## Development

`tools/capture.sh <file-or-folder> <out.png>` captures the window in a nested headless Weston,
never the user's session, with its own `HOME` and XDG folders, and prints the start-up timing.
`KEYS` types keys into it first, `THEN` runs a command afterwards (to change the file, say),
`OMARCHY=1` uses a copy of the current Omarchy theme. `examples/weather-station/` covers every view; `tools/screenshots.sh` makes the images above from it.

First frame: about 170 ms for a file, 175 to 230 ms for a folder (release build, headless Weston).

## Limits

- Editing is per block: a heading or a list shows its marks, so its text moves by their width.
  Enter splits a block only once it is saved.
- "What changed" covers Markdown files. A paragraph holding math is shown old and new, not word
  by word. Your own edits count as changes until `m`.
- CSV cells and JSON or YAML values show on one line each: a cell is cut to its column, a long
  value scrolls sideways.
- Linux only (see the top).

## License

Rature is free software under the GNU Affero General Public License, version 3 only
(`AGPL-3.0-only`): see [LICENSE](LICENSE).

Geist and Geist Mono are by Vercel ([The Geist Project Authors](https://github.com/vercel/geist-font)),
under the SIL Open Font License 1.1
([assets/fonts/OFL.txt](assets/fonts/OFL.txt)). They are bundled, through gpui-design-system,
and are the default fonts.

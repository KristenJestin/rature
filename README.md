# Rature

A minimal Markdown reader you can also edit in place, for proofreading what an agent (or anyone)
writes. Native, keyboard first, no webview.

## Requirements

- **Open a file or a folder**: `rature file.md` or `rature folder/`. A folder shows a tree on the
  left (or right, configurable). One instance: opening another file reuses the open window.
- **No trace in the opened folder**: config and theme in `~/.config/rature`, history and read
  snapshots in `~/.local/state/rature` (XDG).
- **Read and edit in the same place**: live preview, the Markdown is rendered everywhere except
  around the cursor. No separate modes. `e` opens the file in `nvim`.
- **Full Markdown**: tables, task lists, footnotes, code highlighting, Mermaid diagrams, math,
  YAML front matter, GitHub alerts (`> [!NOTE]`).
- **Other formats**: JSON and YAML (foldable), CSV as a table, images, code with highlighting.
- **Links**: relative links between files and anchors, back and forward, a table of contents,
  broken links and missing anchors flagged. Copy a code block, yank a heading's link.
- **Search**: in the file, and across the folder through ripgrep. Fuzzy file finder, recent files.
- **Keyboard first**: vim keys, instant start-up.
- **Live reload**: the file updates while it is written from outside, without losing the reading
  position. The position is also kept per file across sessions.
- **What changed**: highlight what moved since the last read as a rendered diff, block by block
  (heading, paragraph, table row), not line by line. `]c` / `[c` jump between changes. "Read"
  means an explicit mark; optionally diff against git (`HEAD`, last commit).
- **Theme**: a base theme, and the Omarchy theme followed on every change (`theme-set.d` hook).
- **Save and format**: auto save of the user's own edits; formatting by file type (Markdown
  included) only on the user's own save, never on an external change.
- Reading width, font and zoom adjustable. Large and binary files refused cleanly.

## Stack

Rust and GPUI through gpui-kit, as Flottille. No webview.

- Markdown: an established parser (`markdown`, or `comrak`), rendered by our own GPUI elements so
  the document tree is ours (the block diff needs it).
- Mermaid: `mermaid-rs-renderer` (fallback `merman`), to SVG, drawn through resvg.
- Math: RaTeX, to SVG with `embed_glyphs`.

## Prototype

`cargo run --release -- examples/sample.md` renders a file with a Mermaid diagram, a display
formula and inline math, natively. `tools/capture.sh <file.md> <out.png>` captures the window in a
nested headless Weston, never the user's session, and prints the start-up timing
(`RATURE_TIMING=1`).

Measured on the prototype (release build, headless Weston, machine in use):

- first frame of a plain file: about 155 ms after start;
- the sample: about 200 ms, of which 40 ms of Mermaid on the main thread (the formulas take under
  a millisecond). Diagrams are to be drawn off the main thread, the text first.

## Next step

Draw diagrams in the background, then the real reader: live reload, vim keys, the folder tree.

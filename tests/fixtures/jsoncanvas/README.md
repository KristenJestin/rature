# JSON Canvas fixtures

`sample.canvas` is the `sample.canvas` of the JSON Canvas specification's repository,
copied byte for byte (tab indent, no newline at its end):

- source: https://github.com/obsidianmd/jsoncanvas/blob/main/sample.canvas
- last commit that changed it: `8ca9fdcac7292a1b44480659d891772a0fa891f7` (2024-03-24)
- copyright (c) 2024 Obsidian.md, under the MIT License, whose text is in `LICENSE` beside it.

`src/canvas.rs` reads it and writes it back, and checks the bytes are the same.

//! A Markdown table edited where it is read: its grid stays, each cell is a line edited in
//! place, and the table is written back as Markdown.

use gpui_kit::*;

use crate::live::{Caret, Edge, LiveBlock, LiveEvent};
use crate::render;
use crate::ui::palette;

actions!(table, [NextCell, PreviousCell]);

pub fn bind_keys(cx: &mut App) {
    let at = Some("TableEdit");
    cx.bind_keys([
        KeyBinding::new("tab", NextCell, at),
        KeyBinding::new("shift-tab", PreviousCell, at),
    ]);
}

pub struct TableEdit {
    /// The header first, then the rows.
    cells: Vec<Vec<Entity<LiveBlock>>>,
    /// The line under the header, kept as it was: it holds the columns' alignment.
    rule: String,
    /// The table as last given: written back as it is while no cell changed.
    source: String,
    changed: bool,
    /// The cell that takes the caret first.
    first: (usize, usize),
    focus: FocusHandle,
    _subscriptions: Vec<Subscription>,
}

impl EventEmitter<LiveEvent> for TableEdit {}

impl TableEdit {
    pub fn new(source: String, caret: Caret, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let mut lines = source.lines();
        let header = lines.next().map(cells).unwrap_or_default();
        let rule = lines.next().unwrap_or("| --- |").to_string();
        let width = header.len().max(1);
        let rows: Vec<Vec<String>> = std::iter::once(header)
            .chain(lines.map(cells))
            .map(|mut row| {
                row.resize(width, String::new());
                row
            })
            .collect();
        let last = (rows.len() - 1, width - 1);
        let first = match caret {
            Caret::End => last,
            _ => (0, 0),
        };
        let mut subscriptions = Vec::new();
        let cells = rows
            .into_iter()
            .enumerate()
            .map(|(r, row)| {
                row.into_iter()
                    .enumerate()
                    .map(|(c, text)| {
                        let at = match caret {
                            Caret::At(position) | Caret::Inside(position) => {
                                Caret::Inside(position)
                            }
                            Caret::Start if (r, c) == (0, 0) => Caret::Start,
                            Caret::End if (r, c) == last => Caret::End,
                            _ => Caret::Nowhere,
                        };
                        let cell = cx.new(|cx| LiveBlock::new(text, at, window, cx).single_line());
                        subscriptions.push(cx.subscribe_in(
                            &cell,
                            window,
                            move |this, _, event: &LiveEvent, window, cx| match event {
                                LiveEvent::Change => {
                                    this.changed = true;
                                    cx.emit(LiveEvent::Change);
                                }
                                LiveEvent::Leave(edge) => this.go(r, c, *edge, window, cx),
                                LiveEvent::Blur => {}
                            },
                        ));
                        cell
                    })
                    .collect()
            })
            .collect();
        let focus = cx.focus_handle();
        subscriptions.push(cx.on_focus_out(&focus, window, |_, _, _, cx| cx.emit(LiveEvent::Blur)));
        Self {
            cells,
            rule,
            source,
            changed: false,
            first,
            focus,
            _subscriptions: subscriptions,
        }
    }

    /// The handle of the cell that takes the caret first.
    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        let (r, c) = self.first;
        self.cells[r][c].read(cx).focus_handle().clone()
    }

    /// The table as Markdown: one line per row, the cells between pipes.
    pub fn value(&self, cx: &App) -> String {
        if !self.changed {
            return self.source.clone();
        }
        let line = |row: &Vec<Entity<LiveBlock>>| {
            let cells: Vec<&str> = row.iter().map(|cell| cell.read(cx).value()).collect();
            format!("| {} |", cells.join(" | "))
        };
        let mut lines = vec![line(&self.cells[0]), self.rule.clone()];
        lines.extend(self.cells[1..].iter().map(line));
        lines.join("\n")
    }

    /// The table as the file now holds it, formatted: its cells are the same.
    pub fn set_value(&mut self, source: String) {
        self.source = source;
        self.changed = false;
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        for cell in self.cells.iter().flatten() {
            cell.update(cx, |cell, cx| cell.close(cx));
        }
    }

    /// Moves the caret from the cell `(r, c)` past its `edge`: to the next cell, or out of the
    /// table.
    fn go(&mut self, r: usize, c: usize, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        let (rows, columns) = (self.cells.len(), self.cells[0].len());
        let (to, end) = match edge {
            Edge::Above if r > 0 => ((r - 1, c), true),
            Edge::Below if r + 1 < rows => ((r + 1, c), false),
            Edge::Before if c > 0 => ((r, c - 1), true),
            Edge::Before if r > 0 => ((r - 1, columns - 1), true),
            Edge::After if c + 1 < columns => ((r, c + 1), false),
            Edge::After if r + 1 < rows => ((r + 1, 0), false),
            _ => return cx.emit(LiveEvent::Leave(edge)),
        };
        self.cells[to.0][to.1].update(cx, |cell, cx| cell.enter(end, window, cx));
    }

    fn current(&self, window: &Window, cx: &App) -> Option<(usize, usize)> {
        self.cells.iter().enumerate().find_map(|(r, row)| {
            row.iter()
                .position(|cell| cell.read(cx).focus_handle().is_focused(window))
                .map(|c| (r, c))
        })
    }

    fn next_cell(&mut self, _: &NextCell, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((r, c)) = self.current(window, cx) {
            self.go(r, c, Edge::After, window, cx);
        }
    }

    fn previous_cell(&mut self, _: &PreviousCell, window: &mut Window, cx: &mut Context<Self>) {
        if let Some((r, c)) = self.current(window, cx) {
            self.go(r, c, Edge::Before, window, cx);
        }
    }
}

/// The cells of a table's line, their Markdown as written: split at the pipes that are neither
/// escaped nor in code.
fn cells(line: &str) -> Vec<String> {
    let line = line.trim();
    let line = line.strip_prefix('|').unwrap_or(line);
    let line = line
        .strip_suffix('|')
        .filter(|_| !line.ends_with("\\|"))
        .unwrap_or(line);
    let mut cells = Vec::new();
    let (mut cell, mut code, mut escaped) = (String::new(), false, false);
    for c in line.chars() {
        match c {
            '|' if !code && !escaped => cells.push(std::mem::take(&mut cell)),
            _ => cell.push(c),
        }
        if c == '`' {
            code = !code;
        }
        escaped = c == '\\' && !escaped;
    }
    cells.push(cell);
    cells
        .into_iter()
        .map(|cell| cell.trim().to_string())
        .collect()
}

impl Render for TableEdit {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let rows = self
            .cells
            .iter()
            .map(|row| {
                row.iter()
                    .map(|cell| cell.clone().into_any_element())
                    .collect()
            })
            .collect();
        div()
            .key_context("TableEdit")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::next_cell))
            .on_action(cx.listener(Self::previous_cell))
            .child(render::grid("table-edit".into(), &p, rows))
    }
}

#[cfg(test)]
mod tests {
    use super::cells;

    #[test]
    fn a_line_is_cut_at_its_pipes_but_not_in_code_or_escaped() {
        assert_eq!(cells("| `a|b` | c \\| d |  |"), ["`a|b`", "c \\| d", ""]);
        assert_eq!(cells("| --- | :-: |"), ["---", ":-:"]);
    }
}

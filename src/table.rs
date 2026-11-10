//! A Markdown table edited where it is read: its grid stays, each cell is a line edited in
//! place, rows and columns are added and deleted, and the table is written back as Markdown.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::document::Links;
use crate::icons;
use crate::live::{Caret, Edge, LiveBlock, LiveEvent};
use crate::render;
use gpui_design_system::{Button, Icon, Type, enter, palette, space};

actions!(
    table,
    [
        NextCell,
        PreviousCell,
        RowAbove,
        RowBelow,
        ColumnLeft,
        ColumnRight,
        DeleteRow,
        DeleteColumn
    ]
);

pub fn bind_keys(cx: &mut App) {
    let at = Some("TableEdit");
    cx.bind_keys([
        KeyBinding::new("tab", NextCell, at),
        KeyBinding::new("shift-tab", PreviousCell, at),
        KeyBinding::new("alt-shift-up", RowAbove, at),
        KeyBinding::new("alt-shift-down", RowBelow, at),
        KeyBinding::new("alt-shift-left", ColumnLeft, at),
        KeyBinding::new("alt-shift-right", ColumnRight, at),
        KeyBinding::new("alt-shift-backspace", DeleteRow, at),
        KeyBinding::new("alt-shift-delete", DeleteColumn, at),
    ]);
}

pub struct TableEdit {
    /// The header first, then the rows.
    cells: Vec<Vec<Entity<LiveBlock>>>,
    /// Each column's cell of the line under the header: `---`, `:-:`, its alignment.
    rule: Vec<String>,
    /// The table as last given: written back as it is while nothing changed.
    source: String,
    changed: bool,
    /// The cell that takes the caret first, then the one that last had it.
    at: (usize, usize),
    closing: bool,
    links: Rc<Links>,
    focus: FocusHandle,
    subscriptions: Vec<Subscription>,
}

impl EventEmitter<LiveEvent> for TableEdit {}

impl TableEdit {
    pub fn new(
        source: String,
        caret: Caret,
        links: Rc<Links>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut lines = source.lines();
        let header = lines.next().map(cells).unwrap_or_default();
        let width = header.len().max(1);
        let mut rule = lines.next().map(cells).unwrap_or_default();
        rule.resize(width, "---".into());
        let rows: Vec<Vec<String>> = std::iter::once(header)
            .chain(lines.map(cells))
            .map(|mut row| {
                row.resize(width, String::new());
                row
            })
            .collect();
        let last = (rows.len() - 1, width - 1);
        let focus = cx.focus_handle();
        let blur = cx.on_focus_out(&focus, window, |this: &mut Self, _, _, cx| {
            if !this.closing {
                cx.emit(LiveEvent::Blur)
            }
        });
        let mut this = Self {
            cells: Vec::new(),
            rule,
            source,
            changed: false,
            at: match caret {
                Caret::End => last,
                _ => (0, 0),
            },
            closing: false,
            links,
            focus,
            subscriptions: vec![blur],
        };
        this.cells = rows
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
                        this.cell(text, at, window, cx)
                    })
                    .collect()
            })
            .collect();
        this
    }

    /// A cell holding `text`, whose events lead to where it stands when they come.
    fn cell(
        &mut self,
        text: String,
        caret: Caret,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Entity<LiveBlock> {
        let links = self.links.clone();
        let cell = cx.new(|cx| LiveBlock::new(text, caret, links, window, cx).single_line());
        self.subscriptions.push(cx.subscribe_in(
            &cell,
            window,
            |this, cell, event: &LiveEvent, window, cx| match event {
                LiveEvent::Change => this.change(cx),
                LiveEvent::Leave(edge) => {
                    if let Some((r, c)) = this.position(cell) {
                        this.go(r, c, *edge, window, cx)
                    }
                }
                LiveEvent::Break(_) => {
                    if let Some((r, c)) = this.position(cell) {
                        this.go(r, c, Edge::Below, window, cx)
                    }
                }
                LiveEvent::Join | LiveEvent::Blur => {}
                LiveEvent::Follow(link) => cx.emit(LiveEvent::Follow(link.clone())),
            },
        ));
        cell
    }

    fn position(&self, cell: &Entity<LiveBlock>) -> Option<(usize, usize)> {
        self.cells
            .iter()
            .enumerate()
            .find_map(|(r, row)| row.iter().position(|other| other == cell).map(|c| (r, c)))
    }

    fn change(&mut self, cx: &mut Context<Self>) {
        self.changed = true;
        cx.emit(LiveEvent::Change);
        cx.notify();
    }

    /// The handle of the cell that takes the caret first.
    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        let (r, c) = self.at;
        self.cells[r][c].read(cx).focus_handle().clone()
    }

    /// The table as Markdown: one line per row, the cells between pipes.
    pub fn value(&self, cx: &App) -> String {
        if !self.changed {
            return self.source.clone();
        }
        let line = |cells: Vec<&str>| format!("| {} |", cells.join(" | "));
        let row = |row: &Vec<Entity<LiveBlock>>| {
            line(row.iter().map(|cell| cell.read(cx).value()).collect())
        };
        let mut lines = vec![
            row(&self.cells[0]),
            line(self.rule.iter().map(String::as_str).collect()),
        ];
        lines.extend(self.cells[1..].iter().map(row));
        lines.join("\n")
    }

    /// The table as the file now holds it, formatted: its cells are the same.
    pub fn set_value(&mut self, source: String) {
        self.source = source;
        self.changed = false;
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.closing = true;
        for cell in self.cells.iter().flatten() {
            cell.update(cx, |cell, cx| cell.close(cx));
        }
        cx.notify();
    }

    /// Gives the caret to the cell `(r, c)`, at its start or its end.
    fn enter(&mut self, (r, c): (usize, usize), end: bool, window: &mut Window, cx: &mut App) {
        self.at = (r, c);
        self.cells[r][c].update(cx, |cell, cx| cell.enter(end, window, cx));
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
        self.enter(to, end, window, cx);
    }

    /// The cell with the caret, else the one that last had it.
    fn current(&mut self, window: &Window, cx: &App) -> (usize, usize) {
        let focused = self.cells.iter().enumerate().find_map(|(r, row)| {
            row.iter()
                .position(|cell| cell.read(cx).focus_handle().is_focused(window))
                .map(|c| (r, c))
        });
        if let Some(at) = focused {
            self.at = at;
        }
        self.at
    }

    fn next_cell(&mut self, _: &NextCell, window: &mut Window, cx: &mut Context<Self>) {
        let (r, c) = self.current(window, cx);
        self.go(r, c, Edge::After, window, cx);
    }

    fn previous_cell(&mut self, _: &PreviousCell, window: &mut Window, cx: &mut Context<Self>) {
        let (r, c) = self.current(window, cx);
        self.go(r, c, Edge::Before, window, cx);
    }

    /// Adds an empty row at `index`, the header staying first, and gives it the caret.
    fn add_row(
        &mut self,
        index: usize,
        column: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let index = index.clamp(1, self.cells.len());
        let row = (0..self.cells[0].len())
            .map(|_| self.cell(String::new(), Caret::Nowhere, window, cx))
            .collect();
        self.cells.insert(index, row);
        self.enter((index, column), false, window, cx);
        self.change(cx);
    }

    /// Adds an empty column at `index` and gives the caret to its cell in the row `row`.
    fn add_column(
        &mut self,
        index: usize,
        row: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        for r in 0..self.cells.len() {
            let cell = self.cell(String::new(), Caret::Nowhere, window, cx);
            self.cells[r].insert(index, cell);
        }
        self.rule.insert(index, "---".into());
        self.enter((row, index), false, window, cx);
        self.change(cx);
    }

    fn row_above(&mut self, _: &RowAbove, window: &mut Window, cx: &mut Context<Self>) {
        let (r, c) = self.current(window, cx);
        self.add_row(r, c, window, cx);
    }

    fn row_below(&mut self, _: &RowBelow, window: &mut Window, cx: &mut Context<Self>) {
        let (r, c) = self.current(window, cx);
        self.add_row(r + 1, c, window, cx);
    }

    fn column_left(&mut self, _: &ColumnLeft, window: &mut Window, cx: &mut Context<Self>) {
        let (r, c) = self.current(window, cx);
        self.add_column(c, r, window, cx);
    }

    fn column_right(&mut self, _: &ColumnRight, window: &mut Window, cx: &mut Context<Self>) {
        let (r, c) = self.current(window, cx);
        self.add_column(c + 1, r, window, cx);
    }

    /// Deletes the row with the caret: the header stays, a table needs one.
    fn delete_row(&mut self, _: &DeleteRow, window: &mut Window, cx: &mut Context<Self>) {
        let (r, c) = self.current(window, cx);
        if r == 0 {
            return;
        }
        self.cells.remove(r);
        let r = r.min(self.cells.len() - 1);
        self.enter((r, c), false, window, cx);
        self.change(cx);
    }

    /// Deletes the column with the caret, unless it is the last one.
    fn delete_column(&mut self, _: &DeleteColumn, window: &mut Window, cx: &mut Context<Self>) {
        let (r, c) = self.current(window, cx);
        if self.cells[0].len() == 1 {
            return;
        }
        for row in &mut self.cells {
            row.remove(c);
        }
        self.rule.remove(c);
        let c = c.min(self.cells[0].len() - 1);
        self.enter((r, c), false, window, cx);
        self.change(cx);
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

/// The table's tools, floating over its top edge so the table does not move for them.
fn tools(cx: &mut Context<TableEdit>) -> impl IntoElement {
    let p = palette(cx);
    let this = cx.entity().downgrade();
    let tool = move |id: &'static str, glyph: Icon, tip: &'static str, action: Box<dyn Action>| {
        let this = this.clone();
        Button::icon(id, glyph, tip).on_click(move |_, window, cx| {
            // The cell keeps the caret: the action goes where it is.
            if let Some(this) = this.upgrade() {
                let focus = this.update(cx, |this, cx| this.focus_handle(cx));
                window.focus(&focus, cx);
            }
            window.dispatch_action(action.boxed_clone(), cx);
        })
    };
    let label = |text: &'static str| {
        div()
            .px(space::XS)
            .mono(11.)
            .text_color(p.faint)
            .child(text)
    };
    div()
        .flex()
        .items_center()
        .gap(px(2.))
        .p(px(3.))
        .rounded(px(10.))
        .border_1()
        .border_color(p.line2)
        .bg(p.card)
        .shadow_md()
        .child(label("ROW"))
        .child(tool(
            "row-above",
            icons::ROW_ABOVE,
            "Row above  Alt-Shift-↑",
            Box::new(RowAbove),
        ))
        .child(tool(
            "row-below",
            icons::ROW_BELOW,
            "Row below  Alt-Shift-↓",
            Box::new(RowBelow),
        ))
        .child(tool(
            "delete-row",
            Icon::Trash,
            "Delete the row  Alt-Shift-Backspace",
            Box::new(DeleteRow),
        ))
        .child(div().w(px(1.)).h(px(18.)).mx(space::XS).bg(p.line2))
        .child(label("COLUMN"))
        .child(tool(
            "column-left",
            icons::COLUMN_LEFT,
            "Column on the left  Alt-Shift-←",
            Box::new(ColumnLeft),
        ))
        .child(tool(
            "column-right",
            icons::COLUMN_RIGHT,
            "Column on the right  Alt-Shift-→",
            Box::new(ColumnRight),
        ))
        .child(tool(
            "delete-column",
            Icon::Trash,
            "Delete the column  Alt-Shift-Delete",
            Box::new(DeleteColumn),
        ))
}

impl Render for TableEdit {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.current(window, cx);
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
            .on_action(cx.listener(Self::row_above))
            .on_action(cx.listener(Self::row_below))
            .on_action(cx.listener(Self::column_left))
            .on_action(cx.listener(Self::column_right))
            .on_action(cx.listener(Self::delete_row))
            .on_action(cx.listener(Self::delete_column))
            .relative()
            .child(render::grid("table-edit".into(), &p, rows))
            .when(!self.closing, |d| {
                d.child(div().absolute().top(px(-46.)).right_0().child(enter(
                    "table-tools",
                    0,
                    tools(cx),
                )))
            })
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

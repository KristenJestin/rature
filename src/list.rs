//! A Markdown list edited where it is read: its bullets, numbers and task boxes stay drawn, each
//! line is edited in place beside its marker, and the list is written back as Markdown.

use std::rc::Rc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::document::Links;
use crate::live::{Caret, Edge, LiveBlock, LiveEvent};
use crate::render;
use crate::ui::{palette, space};

actions!(list, [Indent, Outdent]);

pub fn bind_keys(cx: &mut App) {
    let at = Some("ListEdit");
    cx.bind_keys([
        KeyBinding::new("tab", Indent, at),
        KeyBinding::new("shift-tab", Outdent, at),
    ]);
}

/// A line of the list as written: its indent, its marker, then its text, edited.
struct Line {
    /// Blank lines above it, kept as they were.
    blank: usize,
    indent: String,
    /// `- `, `1. `: none on a line that carries on the item above.
    bullet: Option<String>,
    /// A task's box: whether it is ticked.
    task: Option<bool>,
    cell: Entity<LiveBlock>,
}

impl Line {
    fn marker(&self) -> String {
        let task = match self.task {
            Some(true) => "[x] ",
            Some(false) => "[ ] ",
            None => "",
        };
        format!("{}{task}", self.bullet.as_deref().unwrap_or(""))
    }
}

pub struct ListEdit {
    lines: Vec<Line>,
    /// The list as last given: written back as it is while nothing changed.
    source: String,
    /// The line breaks the list ends with: written back after it.
    tail: String,
    changed: bool,
    /// The line that takes the caret first, then the one that last had it.
    at: usize,
    closing: bool,
    links: Rc<Links>,
    focus: FocusHandle,
    subscriptions: Vec<Subscription>,
}

impl EventEmitter<LiveEvent> for ListEdit {}

impl ListEdit {
    pub fn new(
        source: String,
        caret: Caret,
        links: Rc<Links>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let blur = cx.on_focus_out(&focus, window, |this: &mut Self, _, _, cx| {
            if !this.closing {
                cx.emit(LiveEvent::Blur)
            }
        });
        let mut this = Self {
            lines: Vec::new(),
            source: String::new(),
            tail: String::new(),
            changed: false,
            at: 0,
            closing: false,
            links,
            focus,
            subscriptions: vec![blur],
        };
        let mut blank = 0;
        let parsed: Vec<_> = source
            .lines()
            .filter_map(|line| {
                if line.trim().is_empty() {
                    blank += 1;
                    return None;
                }
                Some((std::mem::take(&mut blank), split(line)))
            })
            .collect();
        let last = parsed.len().saturating_sub(1);
        for (ix, (blank, (indent, bullet, task, text))) in parsed.into_iter().enumerate() {
            let at = match caret {
                Caret::At(position) | Caret::Inside(position) => Caret::Inside(position),
                Caret::Start if ix == 0 => Caret::Start,
                Caret::End if ix == last => Caret::End,
                _ => Caret::Nowhere,
            };
            let cell = this.cell(text, at, window, cx);
            this.lines.push(Line {
                blank,
                indent,
                bullet,
                task,
                cell,
            });
        }
        if let Caret::End = caret {
            this.at = last;
        }
        this.tail = source[source.trim_end_matches('\n').len()..].to_string();
        this.source = source;
        this
    }

    /// A line's text, whose events lead to where it stands when they come.
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
            |this, cell, event: &LiveEvent, window, cx| {
                let Some(ix) = this.lines.iter().position(|line| &line.cell == cell) else {
                    return;
                };
                match event {
                    LiveEvent::Change => {
                        this.relist(ix, window, cx);
                        this.change(cx)
                    }
                    LiveEvent::Leave(edge) => this.go(ix, *edge, window, cx),
                    LiveEvent::Break(at) => this.break_line(ix, *at, window, cx),
                    LiveEvent::Join => this.join(ix, window, cx),
                    LiveEvent::Blur => {}
                    LiveEvent::Follow(link) => cx.emit(LiveEvent::Follow(link.clone())),
                }
            },
        ));
        cell
    }

    fn change(&mut self, cx: &mut Context<Self>) {
        self.changed = true;
        cx.emit(LiveEvent::Change);
        cx.notify();
    }

    /// The handle of the line that takes the caret first.
    pub fn focus_handle(&self, cx: &App) -> FocusHandle {
        self.lines[self.at].cell.read(cx).focus_handle().clone()
    }

    /// The list as Markdown: each line's indent, marker and text.
    pub fn value(&self, cx: &App) -> String {
        if !self.changed {
            return self.source.clone();
        }
        self.lines
            .iter()
            .map(|line| {
                format!(
                    "{}{}{}{}",
                    "\n".repeat(line.blank),
                    line.indent,
                    line.marker(),
                    line.cell.read(cx).value()
                )
            })
            .collect::<Vec<_>>()
            .join("\n")
            + &self.tail
    }

    /// The list as the file now holds it: its lines are the same.
    pub fn set_value(&mut self, source: String) {
        self.source = source;
        self.changed = false;
    }

    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.closing = true;
        for line in &self.lines {
            line.cell.update(cx, |cell, cx| cell.close(cx));
        }
        cx.notify();
    }

    fn enter(&mut self, ix: usize, at: usize, window: &mut Window, cx: &mut App) {
        self.at = ix;
        self.lines[ix]
            .cell
            .update(cx, |cell, cx| cell.enter_at(at, window, cx));
    }

    /// Moves the caret from the line `ix` past its `edge`: to the next line, or out of the list.
    fn go(&mut self, ix: usize, edge: Edge, window: &mut Window, cx: &mut Context<Self>) {
        let (to, at) = match edge {
            Edge::Above | Edge::Before if ix > 0 => (ix - 1, usize::MAX),
            Edge::Below | Edge::After if ix + 1 < self.lines.len() => (ix + 1, 0),
            _ => return cx.emit(LiveEvent::Leave(edge)),
        };
        self.enter(to, at, window, cx);
    }

    /// Enter at `at` in the line `ix`: what follows the caret becomes a new item. An empty item
    /// is taken out of the list instead.
    fn break_line(&mut self, ix: usize, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        let line = &self.lines[ix];
        let text = line.cell.read(cx).value().to_string();
        if text.is_empty() && line.bullet.is_some() {
            return self.unlist(ix, cx);
        }
        let (head, rest) = text.split_at(at.min(text.len()));
        let (head, rest) = (head.to_string(), rest.to_string());
        let indent = line.indent.clone();
        let bullet = line.bullet.as_deref().map(next_bullet);
        let task = line.task.map(|_| false);
        self.lines[ix]
            .cell
            .update(cx, |cell, cx| cell.set_value(head, cx));
        let cell = self.cell(rest, Caret::Nowhere, window, cx);
        self.lines.insert(
            ix + 1,
            Line {
                blank: 0,
                indent,
                bullet,
                task,
                cell,
            },
        );
        self.enter(ix + 1, 0, window, cx);
        self.change(cx);
    }

    /// Backspace at the start of the line `ix`: an item loses its bullet, other text joins the
    /// line above.
    fn join(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        if self.lines[ix].bullet.is_some() {
            return self.unlist(ix, cx);
        }
        if ix == 0 {
            return;
        }
        let line = self.lines.remove(ix);
        let text = line.cell.read(cx).value().to_string();
        let above = &self.lines[ix - 1].cell;
        let at = above.read(cx).value().len();
        let joined = format!("{}{text}", above.read(cx).value());
        above.update(cx, |cell, cx| cell.set_value(joined, cx));
        self.enter(ix - 1, at, window, cx);
        self.change(cx);
    }

    /// A line out of the list that was given a bullet (`- `, `1. `): an item again.
    fn relist(&mut self, ix: usize, window: &mut Window, cx: &mut Context<Self>) {
        let line = &mut self.lines[ix];
        if line.bullet.is_some() {
            return;
        }
        let (indent, bullet, task, text) = split(line.cell.read(cx).value());
        if !indent.is_empty() || !bullet.as_ref().is_some_and(|b| b.ends_with(' ')) {
            return;
        }
        line.bullet = bullet;
        line.task = task;
        // Back in the list, it no longer stands apart from the items around it.
        if ix > 0 && self.lines[ix - 1].bullet.is_some() {
            self.lines[ix].blank = 0;
        }
        if let Some(next) = self.lines.get_mut(ix + 1)
            && next.bullet.is_some()
        {
            next.blank = 0;
        }
        let line = &self.lines[ix];
        line.cell.update(cx, |cell, cx| {
            cell.set_value(text, cx);
            cell.enter_at(0, window, cx);
        });
    }

    /// The line with the caret, else the one that last had it.
    fn current(&mut self, window: &Window, cx: &App) -> usize {
        if let Some(ix) = self
            .lines
            .iter()
            .position(|line| line.cell.read(cx).focus_handle().is_focused(window))
        {
            self.at = ix;
        }
        self.at
    }

    /// Nests the line under the item above it.
    fn indent(&mut self, _: &Indent, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.current(window, cx);
        let Some(parent) = self.lines[..ix].iter().rev().find(|l| l.bullet.is_some()) else {
            return;
        };
        let indent = format!(
            "{}{}",
            parent.indent,
            " ".repeat(parent.bullet.as_deref().map_or(0, str::len))
        );
        if self.lines[ix].indent.len() < indent.len() {
            self.lines[ix].indent = indent;
            self.change(cx);
        }
    }

    fn outdent(&mut self, _: &Outdent, window: &mut Window, cx: &mut Context<Self>) {
        let ix = self.current(window, cx);
        self.outdent_line(ix, cx);
    }

    /// Takes the line `ix` up a level: to the indent of the item it was nested in.
    fn outdent_line(&mut self, ix: usize, cx: &mut Context<Self>) {
        let indent = &self.lines[ix].indent;
        if indent.is_empty() {
            return;
        }
        let parent = self.lines[..ix]
            .iter()
            .rev()
            .find(|l| l.bullet.is_some() && l.indent.len() < indent.len())
            .map_or(String::new(), |l| l.indent.clone());
        self.lines[ix].indent = parent;
        self.change(cx);
    }

    /// Takes the item `ix` out of the list: a nested one up a level, else it becomes a paragraph
    /// of its own, the list going on after it.
    fn unlist(&mut self, ix: usize, cx: &mut Context<Self>) {
        if !self.lines[ix].indent.is_empty() {
            return self.outdent_line(ix, cx);
        }
        let line = &mut self.lines[ix];
        line.bullet = None;
        line.task = None;
        if ix > 0 {
            line.blank = line.blank.max(1);
        }
        if let Some(next) = self.lines.get_mut(ix + 1)
            && next.bullet.is_some()
        {
            next.blank = next.blank.max(1);
        }
        self.change(cx);
    }

    fn toggle(&mut self, ix: usize, cx: &mut Context<Self>) {
        if let Some(done) = &mut self.lines[ix].task {
            *done = !*done;
            self.change(cx);
        }
    }
}

/// Whether `text` starts as a list's item: a bullet or a number, and a space.
pub fn is_item(text: &str) -> bool {
    let (indent, bullet, ..) = split(text.lines().next().unwrap_or(""));
    indent.is_empty() && bullet.is_some_and(|b| b.ends_with(' '))
}

/// A line cut into its indent, bullet, task box and text.
fn split(line: &str) -> (String, Option<String>, Option<bool>, String) {
    let text = line.trim_start();
    let indent = line[..line.len() - text.len()].to_string();
    let digits = text.bytes().take_while(u8::is_ascii_digit).count();
    let width = match text.as_bytes() {
        [b'-' | b'*' | b'+', b' ', ..] => 2,
        [b'-' | b'*' | b'+'] => 1,
        bytes if (1..10).contains(&digits) && matches!(bytes.get(digits), Some(b'.' | b')')) => {
            match bytes.get(digits + 1) {
                Some(b' ') => digits + 2,
                None => digits + 1,
                _ => 0,
            }
        }
        _ => 0,
    };
    if width == 0 {
        return (indent, None, None, text.to_string());
    }
    let (bullet, text) = text.split_at(width);
    let (task, text) = match text.get(..4) {
        Some("[ ] ") => (Some(false), &text[4..]),
        Some("[x] " | "[X] ") => (Some(true), &text[4..]),
        _ => (None, text),
    };
    (indent, Some(bullet.to_string()), task, text.to_string())
}

/// The bullet of the item after one with `bullet`: the same, or the next number.
fn next_bullet(bullet: &str) -> String {
    let digits = bullet.bytes().take_while(u8::is_ascii_digit).count();
    match bullet[..digits].parse::<usize>() {
        Ok(number) => format!("{}{}", number + 1, &bullet[digits..]),
        Err(_) => bullet.to_string(),
    }
}

impl Render for ListEdit {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.current(window, cx);
        let p = palette(cx);
        let size = window.text_style().font_size.to_pixels(window.rem_size());
        // A nested list sits under its item's text, past the marker column and its gap.
        let step = size * 1.4 + rems(0.5).to_pixels(window.rem_size());
        // Each line's depth and number, from the items it is nested in, the way the list is
        // read: numbered from the first item of each level.
        let mut open: Vec<(usize, Option<usize>)> = Vec::new();
        let mut rows = Vec::with_capacity(self.lines.len());
        // A paragraph taken out of the list stands apart from it, as blocks do.
        let mut apart = false;
        for (ix, line) in self.lines.iter().enumerate() {
            let indent = line.indent.len();
            let gap = ix > 0 && line.blank > 0;
            if line.bullet.is_none() && line.indent.is_empty() && (gap || open.is_empty()) {
                open.clear();
                rows.push(
                    div()
                        .when(gap, |d| d.mt(space::MD))
                        .child(line.cell.clone()),
                );
                apart = true;
                continue;
            }
            let marker = match &line.bullet {
                Some(bullet) => {
                    let number = bullet
                        .trim_end_matches([' ', '.', ')'])
                        .parse::<usize>()
                        .ok();
                    while open.last().is_some_and(|(at, _)| *at > indent) {
                        open.pop();
                    }
                    let number = match open.last_mut() {
                        Some((at, last)) if *at == indent => {
                            *last = number.map(|n| last.map_or(n, |last| last + 1));
                            *last
                        }
                        _ => {
                            open.push((indent, number));
                            number
                        }
                    };
                    let marker =
                        render::marker(size, &p, line.task, number.is_some(), number.unwrap_or(0));
                    div()
                        .id(("list-marker", ix))
                        .when(line.task.is_some(), |d| {
                            d.cursor_pointer()
                                .on_click(cx.listener(move |this, _, _, cx| this.toggle(ix, cx)))
                        })
                        .child(marker)
                        .into_any_element()
                }
                None => div().into_any_element(),
            };
            let depth = match line.bullet {
                Some(_) => open.len() - 1,
                None => open.len().saturating_sub(1),
            };
            rows.push(
                render::bullet(size, marker, line.cell.clone())
                    .when(std::mem::take(&mut apart) && gap, |d| d.mt(space::MD))
                    .pl(step * depth as f32)
                    .when(line.task == Some(true), |d| d.text_color(p.mute)),
            );
        }
        div()
            .key_context("ListEdit")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::indent))
            .on_action(cx.listener(Self::outdent))
            .flex()
            .flex_col()
            .gap_1()
            .children(rows)
    }
}

#[cfg(test)]
mod tests {
    use super::{next_bullet, split};

    #[test]
    fn a_line_is_cut_into_its_indent_bullet_box_and_text() {
        assert_eq!(
            split("  - [x] done"),
            ("  ".into(), Some("- ".into()), Some(true), "done".into())
        );
        assert_eq!(
            split("12. twelve"),
            ("".into(), Some("12. ".into()), None, "twelve".into())
        );
        assert_eq!(split("   more"), ("   ".into(), None, None, "more".into()));
        assert_eq!(split("-dash"), ("".into(), None, None, "-dash".into()));
    }

    #[test]
    fn the_next_item_keeps_the_bullet_or_counts_on() {
        assert_eq!(next_bullet("- "), "- ");
        assert_eq!(next_bullet("9) "), "10) ");
    }
}

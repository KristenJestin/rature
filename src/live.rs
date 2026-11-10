//! A block's Markdown, edited where it is read: its marks (`##`, `**`, the backticks) stay in the
//! text, faint, and what they mark is drawn as it reads, the heading at its size, the strong text
//! bold, the code in the code font. The text is laid out by GPUI's `StyledText`; this module adds
//! the caret, the selection, the keys and the input of text.

use std::ops::Range;
use std::rc::Rc;
use std::time::{Duration, Instant};

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;
use markdown::mdast::Node;

use crate::config::Config;
use crate::document::Links;
use crate::highlight;
use crate::ui::motion::{ENTER, MODAL, Motion};
use crate::ui::palette;

actions!(
    live,
    [
        Left,
        Right,
        Up,
        Down,
        SelectLeft,
        SelectRight,
        SelectUp,
        SelectDown,
        WordLeft,
        WordRight,
        SelectWordLeft,
        SelectWordRight,
        Home,
        End,
        SelectHome,
        SelectEnd,
        First,
        Last,
        Backspace,
        Delete,
        DeleteWordLeft,
        Newline,
        SelectAll,
        Copy,
        Cut,
        Paste,
        Undo,
        Redo,
    ]
);

pub fn bind_keys(cx: &mut App) {
    let at = Some("LiveBlock");
    cx.bind_keys([
        KeyBinding::new("left", Left, at),
        KeyBinding::new("right", Right, at),
        KeyBinding::new("up", Up, at),
        KeyBinding::new("down", Down, at),
        KeyBinding::new("shift-left", SelectLeft, at),
        KeyBinding::new("shift-right", SelectRight, at),
        KeyBinding::new("shift-up", SelectUp, at),
        KeyBinding::new("shift-down", SelectDown, at),
        KeyBinding::new("ctrl-left", WordLeft, at),
        KeyBinding::new("ctrl-right", WordRight, at),
        KeyBinding::new("ctrl-shift-left", SelectWordLeft, at),
        KeyBinding::new("ctrl-shift-right", SelectWordRight, at),
        KeyBinding::new("home", Home, at),
        KeyBinding::new("end", End, at),
        KeyBinding::new("shift-home", SelectHome, at),
        KeyBinding::new("shift-end", SelectEnd, at),
        KeyBinding::new("ctrl-home", First, at),
        KeyBinding::new("ctrl-end", Last, at),
        KeyBinding::new("backspace", Backspace, at),
        KeyBinding::new("delete", Delete, at),
        KeyBinding::new("ctrl-backspace", DeleteWordLeft, at),
        KeyBinding::new("enter", Newline, at),
        KeyBinding::new("ctrl-a", SelectAll, at),
        KeyBinding::new("ctrl-c", Copy, at),
        KeyBinding::new("ctrl-x", Cut, at),
        KeyBinding::new("ctrl-v", Paste, at),
        KeyBinding::new("ctrl-z", Undo, at),
        KeyBinding::new("ctrl-shift-z", Redo, at),
        KeyBinding::new("ctrl-y", Redo, at),
    ]);
}

/// Where the caret goes when the block opens.
#[derive(Clone, Copy)]
pub enum Caret {
    Start,
    End,
    /// Where the block was clicked, in the window.
    At(Point<Pixels>),
    /// Where a table was clicked: the cell under it takes the caret, the others none.
    Inside(Point<Pixels>),
    /// No caret: a table's cell not chosen.
    Nowhere,
}

/// An edge of the block the caret was moved past.
#[derive(Clone, Copy)]
pub enum Edge {
    Above,
    Below,
    /// Past its start, by the left arrow.
    Before,
    /// Past its end, by the right arrow.
    After,
}

pub enum LiveEvent {
    Change,
    /// The caret was moved out of the block: the next one is edited.
    Leave(Edge),
    /// Enter in a single line, at this offset: the owner decides what a new line is.
    Break(usize),
    /// Backspace at the start of a single line: the owner joins it to the one before.
    Join,
    Blur,
    /// A link was ctrl-clicked: its address.
    Follow(String),
}

impl EventEmitter<LiveEvent> for LiveBlock {}

/// Typing within this of the last edit is undone with it.
const TYPING: Duration = Duration::from_millis(600);

pub struct LiveBlock {
    text: String,
    selected: Range<usize>,
    /// The caret is at the start of the selection.
    reversed: bool,
    /// Text being composed by an input method.
    marked: Option<Range<usize>>,
    /// Where vertical moves aim, kept across them.
    /// The x vertical moves aim at, and the top of the line they reached from the text's
    /// top: an index where a line wraps is drawn at the end of the line above, not where the
    /// caret went.
    goal: Option<Point<Pixels>>,
    focus: FocusHandle,
    layout: Option<TextLayout>,
    /// The marks left out of the layout: those away from the caret, so the block reads as it
    /// does when it is not edited.
    hidden: Vec<Range<usize>>,
    /// The colours of its fenced code, for the text and theme they were found for.
    colours: Option<(String, bool, Colours)>,
    /// A caret placed from the window's coordinates once the text is laid out.
    pending: Option<Caret>,
    selecting: bool,
    undo: Vec<(String, Range<usize>)>,
    redo: Vec<(String, Range<usize>)>,
    last_edit: Option<Instant>,
    /// How far the marks are shown: they fade in as the block opens, out as it closes.
    shown: Motion,
    /// A table's cell or a list's line: Enter and Backspace at its start go to its owner.
    single_line: bool,
    /// What its links are checked against: a broken one is drawn as reading draws it.
    links: Rc<Links>,
    _blur: Subscription,
}

impl LiveBlock {
    pub fn new(
        text: String,
        caret: Caret,
        links: Rc<Links>,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> Self {
        let focus = cx.focus_handle();
        let blur = cx.on_blur(&focus, window, |_, _, cx| cx.emit(LiveEvent::Blur));
        let mut shown = Motion::settled(0.);
        shown.animate(
            1.,
            ENTER,
            cx.background_executor().now(),
            cx.reduce_motion(),
        );
        Self {
            text,
            selected: 0..0,
            reversed: false,
            marked: None,
            goal: None,
            focus,
            layout: None,
            hidden: Vec::new(),
            colours: None,
            pending: Some(caret),
            selecting: false,
            undo: Vec::new(),
            redo: Vec::new(),
            last_edit: None,
            shown,
            single_line: false,
            links,
            _blur: blur,
        }
    }

    /// Fades the marks out, the way they came: the block is read again once they are gone.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.selected = self.caret()..self.caret();
        self.shown.animate(
            0.,
            MODAL,
            cx.background_executor().now(),
            cx.reduce_motion(),
        );
        cx.notify();
    }

    pub fn single_line(mut self) -> Self {
        self.single_line = true;
        self
    }

    /// Takes the caret, at its start or its end: a table's cell moved to.
    pub fn enter(&mut self, end: bool, window: &mut Window, cx: &mut Context<Self>) {
        self.enter_at(if end { self.text.len() } else { 0 }, window, cx);
    }

    /// Takes the caret at the offset `at`: where two lines were joined.
    pub fn enter_at(&mut self, at: usize, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.goal = None;
        self.move_to(at.min(self.text.len()), cx);
    }

    pub fn value(&self) -> &str {
        &self.text
    }

    pub fn set_value(&mut self, text: String, cx: &mut Context<Self>) {
        let end = text.len();
        self.text = text;
        self.selected = self.selected.start.min(end)..self.selected.end.min(end);
        cx.notify();
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    fn caret(&self) -> usize {
        if self.reversed {
            self.selected.start
        } else {
            self.selected.end
        }
    }

    fn move_to(&mut self, at: usize, cx: &mut Context<Self>) {
        self.selected = at..at;
        self.reversed = false;
        cx.notify();
    }

    fn select_to(&mut self, at: usize, cx: &mut Context<Self>) {
        if self.reversed {
            self.selected.start = at;
        } else {
            self.selected.end = at;
        }
        if self.selected.end < self.selected.start {
            self.reversed = !self.reversed;
            self.selected = self.selected.end..self.selected.start;
        }
        cx.notify();
    }

    fn previous(&self, at: usize) -> usize {
        self.text[..at]
            .char_indices()
            .next_back()
            .map_or(0, |(ix, _)| ix)
    }

    fn next(&self, at: usize) -> usize {
        self.text[at..]
            .chars()
            .next()
            .map_or(at, |c| at + c.len_utf8())
    }

    fn word_before(&self, at: usize) -> usize {
        let before = &self.text[..at];
        let trimmed = before.trim_end();
        trimmed
            .rfind(|c: char| c.is_whitespace())
            .map_or(0, |ix| ix + 1)
            .min(trimmed.len())
    }

    fn word_after(&self, at: usize) -> usize {
        let after = &self.text[at..];
        let start = after.len() - after.trim_start().len();
        after[start..]
            .find(char::is_whitespace)
            .map_or(self.text.len(), |ix| at + start + ix)
    }

    fn line_start(&self, at: usize) -> usize {
        self.text[..at].rfind('\n').map_or(0, |ix| ix + 1)
    }

    fn line_end(&self, at: usize) -> usize {
        self.text[at..]
            .find('\n')
            .map_or(self.text.len(), |ix| at + ix)
    }

    /// The index one line of the layout above or below `at`, none past the block's edge.
    fn vertical(&mut self, at: usize, down: bool) -> Option<usize> {
        let layout = self.layout.as_ref()?;
        let height = layout.line_height();
        let bounds = layout.bounds();
        let here = match self.goal {
            Some(goal) => point(goal.x, bounds.top() + goal.y),
            None => layout.position_for_index(to_shown(&self.hidden, at))?,
        };
        let top = if down {
            here.y + height
        } else {
            here.y - height
        };
        let y = top + height * 0.5;
        if y < bounds.top() || y > bounds.bottom() {
            return None;
        }
        self.goal = Some(point(here.x, top - bounds.top()));
        let shown = layout
            .index_for_position(point(here.x, y))
            .unwrap_or_else(|ix| ix);
        Some(from_shown(&self.hidden, shown))
    }

    fn step(&mut self, down: bool, select: bool, cx: &mut Context<Self>) {
        match self.vertical(self.caret(), down) {
            Some(at) if select => self.select_to(at, cx),
            Some(at) => {
                self.selected = at..at;
                self.reversed = false;
                cx.notify();
            }
            None if select => {
                let edge = if down { self.text.len() } else { 0 };
                self.select_to(edge, cx);
            }
            None => cx.emit(LiveEvent::Leave(if down {
                Edge::Below
            } else {
                Edge::Above
            })),
        }
    }

    fn left(&mut self, _: &Left, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        if !self.selected.is_empty() {
            return self.move_to(self.selected.start, cx);
        }
        if self.caret() == 0 {
            return cx.emit(LiveEvent::Leave(Edge::Before));
        }
        self.move_to(self.previous(self.caret()), cx);
    }

    fn right(&mut self, _: &Right, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        if !self.selected.is_empty() {
            return self.move_to(self.selected.end, cx);
        }
        if self.caret() == self.text.len() {
            return cx.emit(LiveEvent::Leave(Edge::After));
        }
        self.move_to(self.next(self.caret()), cx);
    }

    fn up(&mut self, _: &Up, _: &mut Window, cx: &mut Context<Self>) {
        self.step(false, false, cx);
    }

    fn down(&mut self, _: &Down, _: &mut Window, cx: &mut Context<Self>) {
        self.step(true, false, cx);
    }

    fn select_left(&mut self, _: &SelectLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.select_to(self.previous(self.caret()), cx);
    }

    fn select_right(&mut self, _: &SelectRight, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.select_to(self.next(self.caret()), cx);
    }

    fn select_up(&mut self, _: &SelectUp, _: &mut Window, cx: &mut Context<Self>) {
        self.step(false, true, cx);
    }

    fn select_down(&mut self, _: &SelectDown, _: &mut Window, cx: &mut Context<Self>) {
        self.step(true, true, cx);
    }

    fn word_left(&mut self, _: &WordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.move_to(self.word_before(self.caret()), cx);
    }

    fn word_right(&mut self, _: &WordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.move_to(self.word_after(self.caret()), cx);
    }

    fn select_word_left(&mut self, _: &SelectWordLeft, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.select_to(self.word_before(self.caret()), cx);
    }

    fn select_word_right(&mut self, _: &SelectWordRight, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.select_to(self.word_after(self.caret()), cx);
    }

    fn home(&mut self, _: &Home, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.move_to(self.line_start(self.caret()), cx);
    }

    fn end(&mut self, _: &End, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.move_to(self.line_end(self.caret()), cx);
    }

    fn select_home(&mut self, _: &SelectHome, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.select_to(self.line_start(self.caret()), cx);
    }

    fn select_end(&mut self, _: &SelectEnd, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.select_to(self.line_end(self.caret()), cx);
    }

    fn first(&mut self, _: &First, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.move_to(0, cx);
    }

    fn last(&mut self, _: &Last, _: &mut Window, cx: &mut Context<Self>) {
        self.goal = None;
        self.move_to(self.text.len(), cx);
    }

    fn select_all(&mut self, _: &SelectAll, _: &mut Window, cx: &mut Context<Self>) {
        self.selected = 0..self.text.len();
        self.reversed = false;
        cx.notify();
    }

    fn backspace(&mut self, _: &Backspace, window: &mut Window, cx: &mut Context<Self>) {
        if self.single_line && self.selected == (0..0) {
            return cx.emit(LiveEvent::Join);
        }
        if self.selected.is_empty() {
            let at = self.caret();
            self.selected = self.previous(at)..at;
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete(&mut self, _: &Delete, window: &mut Window, cx: &mut Context<Self>) {
        if self.selected.is_empty() {
            let at = self.caret();
            self.selected = at..self.next(at);
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn delete_word_left(
        &mut self,
        _: &DeleteWordLeft,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        if self.selected.is_empty() {
            let at = self.caret();
            self.selected = self.word_before(at)..at;
        }
        self.replace_text_in_range(None, "", window, cx);
    }

    fn newline(&mut self, _: &Newline, window: &mut Window, cx: &mut Context<Self>) {
        if self.single_line {
            return cx.emit(LiveEvent::Break(self.caret()));
        }
        self.replace_text_in_range(None, "\n", window, cx);
    }

    fn copy(&mut self, _: &Copy, _: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            let text = self.text[self.selected.clone()].to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(text));
        }
    }

    fn cut(&mut self, _: &Cut, window: &mut Window, cx: &mut Context<Self>) {
        if !self.selected.is_empty() {
            let text = self.text[self.selected.clone()].to_string();
            cx.write_to_clipboard(ClipboardItem::new_string(text));
            self.replace_text_in_range(None, "", window, cx);
        }
    }

    fn paste(&mut self, _: &Paste, window: &mut Window, cx: &mut Context<Self>) {
        if let Some(text) = cx.read_from_clipboard().and_then(|item| item.text()) {
            // A paste is undone alone.
            self.last_edit = None;
            self.replace_text_in_range(None, &text, window, cx);
            self.last_edit = None;
        }
    }

    fn undo(&mut self, _: &Undo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((text, selected)) = self.undo.pop() {
            let now = std::mem::replace(&mut self.text, text);
            self.redo.push((now, self.selected.clone()));
            self.selected = selected;
            self.last_edit = None;
            cx.emit(LiveEvent::Change);
            cx.notify();
        }
    }

    fn redo(&mut self, _: &Redo, _: &mut Window, cx: &mut Context<Self>) {
        if let Some((text, selected)) = self.redo.pop() {
            let now = std::mem::replace(&mut self.text, text);
            self.undo.push((now, self.selected.clone()));
            self.selected = selected;
            self.last_edit = None;
            cx.emit(LiveEvent::Change);
            cx.notify();
        }
    }

    fn index_at(&self, position: Point<Pixels>) -> usize {
        self.layout
            .as_ref()
            .map_or(0, |layout| {
                from_shown(
                    &self.hidden,
                    layout.index_for_position(position).unwrap_or_else(|ix| ix),
                )
            })
            .min(self.text.len())
    }

    fn mouse_down(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        window.focus(&self.focus, cx);
        self.goal = None;
        let at = self.index_at(event.position);
        if event.modifiers.secondary()
            && let Some(link) = link_at(&self.text, at)
        {
            return cx.emit(LiveEvent::Follow(link));
        }
        match event.click_count {
            2 => {
                self.selected = self.word_before(self.next(at))..self.word_after(at);
                self.reversed = false;
                cx.notify();
            }
            3.. => {
                self.selected = self.line_start(at)..self.line_end(at);
                self.reversed = false;
                cx.notify();
            }
            _ if event.modifiers.shift => self.select_to(at, cx),
            _ => {
                self.selecting = true;
                self.move_to(at, cx);
            }
        }
    }

    fn mouse_move(&mut self, event: &MouseMoveEvent, _: &mut Window, cx: &mut Context<Self>) {
        if self.selecting && event.dragging() {
            let at = self.index_at(event.position);
            self.select_to(at, cx);
        }
    }

    fn mouse_up(&mut self, _: &MouseUpEvent, _: &mut Window, cx: &mut Context<Self>) {
        self.selecting = false;
        cx.notify();
    }

    /// Places the caret asked for at the opening, now that the text is laid out.
    fn place(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let at = match self.pending.take() {
            Some(Caret::Start) => 0,
            Some(Caret::End) => self.text.len(),
            Some(Caret::At(position)) => self.index_at(position),
            Some(Caret::Inside(position))
                if self
                    .layout
                    .as_ref()
                    .is_some_and(|layout| layout.bounds().contains(&position)) =>
            {
                window.focus(&self.focus, cx);
                self.index_at(position)
            }
            _ => return cx.notify(),
        };
        self.move_to(at, cx);
    }

    fn utf16(&self, offset: usize) -> usize {
        self.text[..offset.min(self.text.len())]
            .chars()
            .map(char::len_utf16)
            .sum()
    }

    fn offset(&self, utf16: usize) -> usize {
        let mut count = 0;
        for (ix, c) in self.text.char_indices() {
            if count >= utf16 {
                return ix;
            }
            count += c.len_utf16();
        }
        self.text.len()
    }

    fn range_utf16(&self, range: &Range<usize>) -> Range<usize> {
        self.utf16(range.start)..self.utf16(range.end)
    }

    fn range_offsets(&self, range: &Range<usize>) -> Range<usize> {
        self.offset(range.start)..self.offset(range.end)
    }
}

impl EntityInputHandler for LiveBlock {
    fn text_for_range(
        &mut self,
        range: Range<usize>,
        adjusted: &mut Option<Range<usize>>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<String> {
        let range = self.range_offsets(&range);
        adjusted.replace(self.range_utf16(&range));
        Some(self.text[range].to_string())
    }

    fn selected_text_range(
        &mut self,
        _: bool,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<UTF16Selection> {
        Some(UTF16Selection {
            range: self.range_utf16(&self.selected),
            reversed: self.reversed,
        })
    }

    fn marked_text_range(&self, _: &mut Window, _: &mut Context<Self>) -> Option<Range<usize>> {
        self.marked.as_ref().map(|range| self.range_utf16(range))
    }

    fn unmark_text(&mut self, _: &mut Window, _: &mut Context<Self>) {
        self.marked = None;
    }

    fn replace_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|range| self.range_offsets(&range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        let now = Instant::now();
        if self.last_edit.is_none_or(|last| now - last > TYPING) {
            self.undo.push((self.text.clone(), self.selected.clone()));
            self.redo.clear();
        }
        self.last_edit = Some(now);
        self.text.replace_range(range.clone(), text);
        let at = range.start + text.len();
        self.selected = at..at;
        self.reversed = false;
        self.marked = None;
        self.goal = None;
        cx.emit(LiveEvent::Change);
        cx.notify();
    }

    fn replace_and_mark_text_in_range(
        &mut self,
        range: Option<Range<usize>>,
        text: &str,
        selected: Option<Range<usize>>,
        _: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let range = range
            .map(|range| self.range_offsets(&range))
            .or(self.marked.clone())
            .unwrap_or(self.selected.clone());
        self.text.replace_range(range.clone(), text);
        self.marked = (!text.is_empty()).then(|| range.start..range.start + text.len());
        self.selected = selected
            .map(|within| {
                let within = within.start + range.start..within.end + range.start;
                self.range_offsets(&within)
            })
            .unwrap_or(range.start + text.len()..range.start + text.len());
        cx.notify();
    }

    fn bounds_for_range(
        &mut self,
        range: Range<usize>,
        _: Bounds<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<Bounds<Pixels>> {
        let layout = self.layout.as_ref()?;
        let range = self.range_offsets(&range);
        let start = layout.position_for_index(to_shown(&self.hidden, range.start))?;
        let end = layout.position_for_index(to_shown(&self.hidden, range.end))?;
        Some(Bounds::from_corners(
            start,
            point(end.x.max(start.x), end.y + layout.line_height()),
        ))
    }

    fn character_index_for_point(
        &mut self,
        point: Point<Pixels>,
        _: &mut Window,
        _: &mut Context<Self>,
    ) -> Option<usize> {
        Some(self.utf16(self.index_at(point)))
    }
}

/// Where the offset `at` of the text falls in what is drawn of it, `hidden` left out.
fn to_shown(hidden: &[Range<usize>], at: usize) -> usize {
    hidden.iter().fold(at, |shown, range| {
        shown - range.end.min(at).saturating_sub(range.start)
    })
}

/// The offset of the text at the offset `at` of what is drawn: past the marks hidden there.
fn from_shown(hidden: &[Range<usize>], at: usize) -> usize {
    let mut out = at;
    for range in hidden {
        if range.start > out {
            break;
        }
        out += range.len();
    }
    out
}

/// The rectangles a selection covers, line by line of the layout.
fn selection(layout: &TextLayout, range: &Range<usize>) -> Vec<Bounds<Pixels>> {
    let (Some(start), Some(end)) = (
        layout.position_for_index(range.start),
        layout.position_for_index(range.end),
    ) else {
        return Vec::new();
    };
    let height = layout.line_height();
    let bounds = layout.bounds();
    if start.y == end.y {
        return vec![Bounds::from_corners(start, point(end.x, end.y + height))];
    }
    let mut out = vec![Bounds::from_corners(
        start,
        point(bounds.right(), start.y + height),
    )];
    if end.y > start.y + height {
        out.push(Bounds::from_corners(
            point(bounds.left(), start.y + height),
            point(bounds.right(), end.y),
        ));
    }
    out.push(Bounds::from_corners(
        point(bounds.left(), end.y),
        point(end.x, end.y + height),
    ));
    out
}

impl Render for LiveBlock {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let now = cx.background_executor().now();
        if self.shown.is_running(now) {
            window.request_animation_frame();
        }
        let shown = self.shown.value(now);
        let closing = self.shown.target() == 0.;
        let base = window.text_style();
        let code_font: SharedString = cx.global::<Config>().code_font.clone().into();
        let dark = crate::render::is_dark(cx);
        if self
            .colours
            .as_ref()
            .is_none_or(|(text, was_dark, _)| *text != self.text || *was_dark != dark)
        {
            let colours = code_colours(&self.text, dark);
            self.colours = Some((self.text.clone(), dark, colours));
        }
        let looks = looks(
            &self.text,
            self.colours.as_ref().map_or(&[], |(_, _, colours)| colours),
            &|link| self.links.broken(link),
        );
        // Marks show around the caret only. Closing, they go at once but for a line's leading
        // ones, which slide away; a block not yet given its caret is laid out as it is read,
        // so the click that opened it lands where it was aimed.
        let first = looks.first().map(|(range, _)| range.clone());
        let near = |zone: Option<(usize, usize)>| {
            let (start, end) = (self.selected.start, self.selected.end);
            self.focus.is_focused(window)
                && self.pending.is_none()
                && !closing
                && zone.is_some_and(|(from, to)| start <= to && end >= from)
        };
        let before = std::mem::take(&mut self.hidden);
        self.hidden = looks
            .iter()
            .filter(|(range, look)| {
                look.ink == Ink::Mark
                    && look.zone.is_some()
                    && !near(look.zone)
                    && !(closing && first.as_ref() == Some(range) && !before.contains(range))
            })
            .map(|(range, _)| range.clone())
            .collect();
        let looks: Vec<_> = looks
            .into_iter()
            .filter(|(range, _)| !self.hidden.contains(range))
            .collect();
        let shown_text: String = looks
            .iter()
            .map(|(range, _)| &self.text[range.clone()])
            .collect();
        let runs: Vec<TextRun> = looks
            .iter()
            .map(|(range, look)| {
                let mut font = base.font();
                if look.mono {
                    font.family = code_font.clone();
                }
                if look.bold {
                    font.weight = FontWeight::BOLD;
                }
                if look.italic {
                    font.style = FontStyle::Italic;
                }
                TextRun {
                    len: range.len(),
                    font,
                    color: match look.ink {
                        Ink::Text => look.colour.map_or(base.color, Hsla::from),
                        Ink::Mark => p.faint.opacity(shown),
                        Ink::Link => p.link,
                        Ink::Broken => p.fail,
                    },
                    background_color: look.shade.then_some(p.card2),
                    underline: matches!(look.ink, Ink::Link | Ink::Broken).then_some(
                        UnderlineStyle {
                            thickness: px(1.),
                            color: None,
                            wavy: false,
                        },
                    ),
                    strikethrough: look.strike.then_some(StrikethroughStyle {
                        thickness: px(1.),
                        color: None,
                    }),
                }
            })
            .collect();
        // The marks a first line starts with (`# `, `- `) push its text aside: it slides there
        // as they appear, rather than jumping.
        let prefix = looks
            .first()
            .filter(|(range, look)| {
                look.ink == Ink::Mark
                    && !self.text[range.clone()].contains('\n')
                    && range.end < self.text.len()
                    && !self.text.starts_with(['`', '~'])
            })
            .map_or(px(0.), |(range, _)| {
                let font_size = base.font_size.to_pixels(window.rem_size());
                window
                    .text_system()
                    .shape_line(
                        self.text[range.clone()].to_string().into(),
                        font_size,
                        &runs[..1],
                        None,
                    )
                    .width
            });
        let text = StyledText::new(shown_text).with_runs(runs);
        let layout = text.layout().clone();
        self.layout = Some(layout.clone());
        let entity = cx.entity();
        let focus = self.focus.clone();
        let drawn = |at| to_shown(&self.hidden, at);
        let (selected, caret) = (
            drawn(self.selected.start)..drawn(self.selected.end),
            drawn(self.caret()),
        );
        let (caret_ink, selection_ink) = (p.acc, p.run.opacity(0.28));
        let line_top = self.goal.map(|goal| goal.y);
        let placing = self.pending.is_some();
        div()
            .when(!closing, |d| d.key_context("LiveBlock"))
            .track_focus(&self.focus)
            .cursor_text()
            .on_action(cx.listener(Self::left))
            .on_action(cx.listener(Self::right))
            .on_action(cx.listener(Self::up))
            .on_action(cx.listener(Self::down))
            .on_action(cx.listener(Self::select_left))
            .on_action(cx.listener(Self::select_right))
            .on_action(cx.listener(Self::select_up))
            .on_action(cx.listener(Self::select_down))
            .on_action(cx.listener(Self::word_left))
            .on_action(cx.listener(Self::word_right))
            .on_action(cx.listener(Self::select_word_left))
            .on_action(cx.listener(Self::select_word_right))
            .on_action(cx.listener(Self::home))
            .on_action(cx.listener(Self::end))
            .on_action(cx.listener(Self::select_home))
            .on_action(cx.listener(Self::select_end))
            .on_action(cx.listener(Self::first))
            .on_action(cx.listener(Self::last))
            .on_action(cx.listener(Self::backspace))
            .on_action(cx.listener(Self::delete))
            .on_action(cx.listener(Self::delete_word_left))
            .on_action(cx.listener(Self::newline))
            .on_action(cx.listener(Self::select_all))
            .on_action(cx.listener(Self::copy))
            .on_action(cx.listener(Self::cut))
            .on_action(cx.listener(Self::paste))
            .on_action(cx.listener(Self::undo))
            .on_action(cx.listener(Self::redo))
            .on_mouse_down(MouseButton::Left, cx.listener(Self::mouse_down))
            .on_mouse_move(cx.listener(Self::mouse_move))
            .on_mouse_up(MouseButton::Left, cx.listener(Self::mouse_up))
            .relative()
            .left(-prefix * (1. - shown))
            .child(text)
            .child(
                canvas(
                    |_, _, _| {},
                    move |bounds, _, window, cx| {
                        if placing {
                            entity.update(cx, |this, cx| this.place(window, cx));
                            return;
                        }
                        if closing {
                            return;
                        }
                        window.handle_input(
                            &focus,
                            ElementInputHandler::new(bounds, entity.clone()),
                            cx,
                        );
                        // A table's other cells keep no caret.
                        if !focus.is_focused(window) {
                            return;
                        }
                        for rect in selection(&layout, &selected) {
                            window.paint_quad(fill(rect, selection_ink));
                        }
                        if selected.is_empty()
                            && let Some(mut at) = layout.position_for_index(caret)
                        {
                            // Moved down onto where a line wraps: the start of the line below.
                            if let Some(top) = line_top.map(|y| layout.bounds().top() + y)
                                && top != at.y
                            {
                                at = point(layout.bounds().left(), top);
                            }
                            let caret = Bounds::new(at, size(px(2.), layout.line_height()));
                            window.paint_quad(fill(caret, caret_ink));
                        }
                    },
                )
                .absolute()
                .top_0()
                .left_0()
                .size_full(),
            )
    }
}

#[derive(Clone, Copy, PartialEq, Default, Debug)]
enum Ink {
    #[default]
    Text,
    /// A Markdown mark: `#`, `**`, a backtick, a link's address.
    Mark,
    Link,
    /// A link to a file or an anchor that is not there.
    Broken,
}

/// How a piece of the text is drawn.
#[derive(Clone, Copy, PartialEq, Default, Debug)]
struct Look {
    bold: bool,
    italic: bool,
    mono: bool,
    strike: bool,
    /// Inline code's tinted ground.
    shade: bool,
    ink: Ink,
    /// Where the caret shows a mark: its line, or what it marks. A fence's show always.
    zone: Option<(usize, usize)>,
    /// Fenced code's colour, by its language.
    colour: Option<Rgba>,
}

/// The looks of `text`, in runs that cover it whole: its marks faint, what they mark drawn as it
/// reads.
fn looks(
    text: &str,
    colours: &[(Range<usize>, Rgba)],
    broken: &dyn Fn(&str) -> bool,
) -> Vec<(Range<usize>, Look)> {
    let mut each = vec![Look::default(); text.len()];
    for (range, colour) in colours {
        paint(&mut each, range.clone(), |l| l.colour = Some(*colour));
    }
    let mut fenced = false;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        // With its line break, so a fence's lines are one run.
        let range = at..at + line.len();
        at += line.len();
        let trimmed = line.trim_start();
        let indent = line.len() - trimmed.len();
        if trimmed.starts_with("```") || trimmed.starts_with("~~~") {
            fenced = !fenced;
            paint(&mut each, range, |l| {
                l.mono = true;
                l.ink = Ink::Mark;
            });
            continue;
        }
        if fenced {
            paint(&mut each, range, |l| l.mono = true);
            continue;
        }
        let marker = line_marker(trimmed);
        let start = range.start + indent;
        let zone = Some((range.start, range.start + line.trim_end_matches('\n').len()));
        paint(&mut each, start..start + marker, |l| {
            l.ink = Ink::Mark;
            l.zone = zone;
        });
    }
    if let Ok(tree) = markdown::to_mdast(text, &markdown::ParseOptions::gfm()) {
        inline(&tree, &mut each, broken);
    }
    let mut runs: Vec<(Range<usize>, Look)> = Vec::new();
    for (ix, look) in each.into_iter().enumerate() {
        match runs.last_mut() {
            Some((range, last)) if *last == look || !text.is_char_boundary(ix) => {
                range.end = ix + 1
            }
            _ => runs.push((ix..ix + 1, look)),
        }
    }
    runs
}

/// Pieces of code and their colours.
type Colours = Vec<(Range<usize>, Rgba)>;

/// The colours of the code fenced in `text`, read in the language its fence names.
fn code_colours(text: &str, dark: bool) -> Colours {
    let mut out = Vec::new();
    let mut open: Option<(&str, usize)> = None;
    let mut at = 0;
    for line in text.split_inclusive('\n') {
        let start = at;
        at += line.len();
        let trimmed = line.trim();
        if !(trimmed.starts_with("```") || trimmed.starts_with("~~~")) {
            continue;
        }
        let Some((lang, body)) = open.take() else {
            open = Some((&trimmed[3..], at));
            continue;
        };
        let Some(lines) = highlight::highlight(lang, &text[body..start], dark) else {
            continue;
        };
        let mut line_start = body;
        for (spans, line) in lines.iter().zip(text[body..start].split_inclusive('\n')) {
            out.extend(spans.iter().map(|span| {
                (
                    line_start + span.range.start..line_start + span.range.end,
                    span.color,
                )
            }));
            line_start += line.len();
        }
    }
    out
}

/// The length of the marks a line starts with: a heading's `#`, a quote's `>`, a list item's
/// bullet or number and its box.
fn line_marker(line: &str) -> usize {
    let mut at = 0;
    let rest = |at: usize| &line[at..];
    while rest(at).starts_with('>') {
        at += 1;
        at += rest(at).len() - rest(at).trim_start_matches(' ').len();
    }
    let hashes = rest(at).len() - rest(at).trim_start_matches('#').len();
    if (1..=6).contains(&hashes) && rest(at + hashes).starts_with(' ') {
        return at + hashes + 1;
    }
    let digits = rest(at).len()
        - rest(at)
            .trim_start_matches(|c: char| c.is_ascii_digit())
            .len();
    let bullet = if rest(at).starts_with(['-', '*', '+']) {
        1
    } else if digits > 0 && rest(at + digits).starts_with(['.', ')']) {
        digits + 1
    } else {
        0
    };
    if bullet > 0 && rest(at + bullet).starts_with(' ') {
        at += bullet + 1;
        for task in ["[ ] ", "[x] ", "[X] "] {
            if rest(at).starts_with(task) {
                at += task.len();
            }
        }
    }
    at
}

/// The address of the link `at` falls in.
fn link_at(text: &str, at: usize) -> Option<String> {
    fn find(node: &Node, at: usize) -> Option<String> {
        if let Node::Link(link) = node
            && span(node).is_some_and(|range| range.contains(&at))
        {
            return Some(link.url.clone());
        }
        node.children()?.iter().find_map(|child| find(child, at))
    }
    find(
        &markdown::to_mdast(text, &markdown::ParseOptions::gfm()).ok()?,
        at,
    )
}

fn paint(each: &mut [Look], range: Range<usize>, change: impl Fn(&mut Look)) {
    let end = range.end.min(each.len());
    for look in &mut each[range.start.min(end)..end] {
        change(look);
    }
}

fn span(node: &Node) -> Option<Range<usize>> {
    node.position().map(|p| p.start.offset..p.end.offset)
}

/// Where a node's children lie, its marks around them.
fn inner(node: &Node) -> Option<Range<usize>> {
    let children = node.children()?;
    let start = span(children.first()?)?.start;
    let end = span(children.last()?)?.end;
    Some(start..end)
}

fn marks(each: &mut [Look], node: &Node) {
    if let (Some(outer), Some(inner)) = (span(node), inner(node)) {
        let mark = |l: &mut Look| {
            l.ink = Ink::Mark;
            l.zone = Some((outer.start, outer.end));
        };
        paint(each, outer.start..inner.start, mark);
        paint(each, inner.end..outer.end, mark);
    }
}

fn inline(node: &Node, each: &mut [Look], broken: &dyn Fn(&str) -> bool) {
    match node {
        Node::Strong(_) => {
            if let Some(range) = span(node) {
                paint(each, range, |l| l.bold = true);
            }
            marks(each, node);
        }
        Node::Emphasis(_) => {
            if let Some(range) = span(node) {
                paint(each, range, |l| l.italic = true);
            }
            marks(each, node);
        }
        Node::Delete(_) => {
            if let Some(range) = span(node) {
                paint(each, range.clone(), |l| l.strike = true);
            }
            marks(each, node);
        }
        Node::Link(link) => {
            let ink = if broken(&link.url) {
                Ink::Broken
            } else {
                Ink::Link
            };
            if let Some(range) = inner(node) {
                paint(each, range, |l| l.ink = ink);
            }
            marks(each, node);
        }
        Node::InlineCode(code) => {
            if let Some(range) = span(node) {
                let ticks = (range.len() - code.value.len()) / 2;
                paint(each, range.clone(), |l| {
                    l.mono = true;
                    l.shade = true;
                });
                let mark = |l: &mut Look| {
                    l.ink = Ink::Mark;
                    l.zone = Some((range.start, range.end));
                };
                paint(each, range.start..range.start + ticks, mark);
                paint(each, range.end - ticks..range.end, mark);
            }
        }
        // Fenced code is drawn line by line.
        Node::Code(_) => {}
        _ => {}
    }
    if let Some(children) = node.children() {
        for child in children {
            inline(child, each, broken);
        }
    }
}

#[cfg(test)]
mod tests {
    // Not the glob: GPUI's own `test` attribute would come with it.
    use super::{Ink, from_shown, link_at, looks, to_shown};

    fn marked(text: &str) -> String {
        looks(text, &[], &|_| false)
            .into_iter()
            .map(|(range, look)| match look.ink {
                Ink::Mark => format!("[{}]", &text[range]),
                _ => text[range].to_string(),
            })
            .collect()
    }

    #[test]
    fn a_broken_link_is_drawn_broken() {
        let text = "[here](a.md) and [gone](b.md)";
        let inks: Vec<Ink> = looks(text, &[], &|link| link == "b.md")
            .into_iter()
            .filter(|(range, _)| ["here", "gone"].contains(&&text[range.clone()]))
            .map(|(_, look)| look.ink)
            .collect();
        assert_eq!(inks, [Ink::Link, Ink::Broken]);
    }

    #[test]
    fn a_link_is_found_under_its_text() {
        let text = "See [the plan](plan.md) now";
        assert_eq!(link_at(text, 6).as_deref(), Some("plan.md"));
        assert_eq!(link_at(text, 1), None);
    }

    #[test]
    fn offsets_skip_the_hidden_marks() {
        // `a **b** c`, its marks hidden: `a b c`.
        let hidden = [2..4, 5..7];
        assert_eq!(to_shown(&hidden, 2), 2);
        assert_eq!(to_shown(&hidden, 3), 2);
        assert_eq!(to_shown(&hidden, 4), 2);
        assert_eq!(to_shown(&hidden, 9), 5);
        assert_eq!(from_shown(&hidden, 2), 4);
        assert_eq!(from_shown(&hidden, 3), 7);
        assert_eq!(from_shown(&hidden, 1), 1);
    }

    #[test]
    fn marks_are_found_and_what_they_mark_is_drawn() {
        assert_eq!(marked("## Title"), "[## ]Title");
        assert_eq!(marked("a **b** c"), "a [**]b[**] c");
        assert_eq!(marked("- [ ] task"), "[- [ ] ]task");
        assert_eq!(
            marked("see `x` and [it](a.md)"),
            "see [`]x[`] and [[]it[](a.md)]"
        );
        let runs = looks("a **b** c", &[], &|_| false);
        assert!(
            runs.iter()
                .any(|(r, l)| l.bold && &"a **b** c"[r.clone()] == "b")
        );
        let fence = looks("```rust\nlet x;\n```", &[], &|_| false);
        assert!(fence.iter().all(|(_, l)| l.mono));
    }

    #[test]
    fn runs_cover_the_text_on_char_boundaries() {
        let text = "é **ü** `ß` ✓";
        let runs = looks(text, &[], &|_| false);
        assert_eq!(runs.first().unwrap().0.start, 0);
        assert_eq!(runs.last().unwrap().0.end, text.len());
        assert!(runs.windows(2).all(|w| w[0].0.end == w[1].0.start));
        assert!(runs.iter().all(|(r, _)| text.is_char_boundary(r.start)));
    }
}

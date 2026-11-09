use std::collections::HashMap;
use std::path::{Path, PathBuf};

use futures::StreamExt as _;
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::document::{self, Block, Inline, Item, Marks, Picture};
use crate::{diagram, theme, watch};

const PROSE_FONT: &str = "Noto Sans";
const CODE_FONT: &str = "JetBrainsMono Nerd Font";
/// The reading column: wide enough for a table, narrow enough for prose.
const COLUMN: f32 = 760.;
/// What `j` and `k` move.
const LINE: f32 = 64.;

actions!(
    reader,
    [
        LineDown,
        LineUp,
        HalfPageDown,
        HalfPageUp,
        PageDown,
        PageUp,
        Top,
        Bottom,
        Edit,
        Quit
    ]
);

pub fn bind_keys(cx: &mut App) {
    let at = Some("Reader");
    cx.bind_keys([
        KeyBinding::new("j", LineDown, at),
        KeyBinding::new("down", LineDown, at),
        KeyBinding::new("k", LineUp, at),
        KeyBinding::new("up", LineUp, at),
        KeyBinding::new("ctrl-d", HalfPageDown, at),
        KeyBinding::new("ctrl-u", HalfPageUp, at),
        KeyBinding::new("space", PageDown, at),
        KeyBinding::new("ctrl-f", PageDown, at),
        KeyBinding::new("pagedown", PageDown, at),
        KeyBinding::new("shift-space", PageUp, at),
        KeyBinding::new("ctrl-b", PageUp, at),
        KeyBinding::new("pageup", PageUp, at),
        KeyBinding::new("g g", Top, at),
        KeyBinding::new("home", Top, at),
        KeyBinding::new("shift-g", Bottom, at),
        KeyBinding::new("end", Bottom, at),
        KeyBinding::new("e", Edit, at),
        KeyBinding::new("q", Quit, at),
    ]);
}

/// A diagram being drawn, drawn, or that could not be.
enum Drawing {
    /// Being drawn; meanwhile the drawing that stood at the same place before the file changed,
    /// so the page does not jump.
    Pending(Option<Picture>),
    Done(Picture),
    Failed(String),
}

type Drawings = HashMap<String, Drawing>;

pub struct Reader {
    path: Option<PathBuf>,
    blocks: Vec<Block>,
    drawings: Drawings,
    scroll: ScrollHandle,
    focus: FocusHandle,
    /// Dropped with the reader or replaced by the next file's: the watch ends with it.
    watch: Option<(notify::RecommendedWatcher, Task<()>)>,
}

impl Reader {
    pub fn new(cx: &mut Context<Self>) -> Self {
        Self {
            path: None,
            blocks: Vec::new(),
            drawings: HashMap::new(),
            scroll: ScrollHandle::new(),
            focus: cx.focus_handle(),
            watch: None,
        }
    }

    pub fn focus_handle(&self) -> &FocusHandle {
        &self.focus
    }

    /// Shows the Markdown `source`, with no file behind it.
    pub fn show(&mut self, source: &str, cx: &mut Context<Self>) {
        self.path = None;
        self.watch = None;
        self.set_blocks(document::parse(source), cx);
    }

    /// Shows the file at `path` from its top, and follows its changes.
    pub fn open(&mut self, path: PathBuf, cx: &mut Context<Self>) {
        self.watch = match watch::file(&path) {
            Ok((watcher, mut changes)) => {
                let task = cx.spawn(async move |this, cx| {
                    while changes.next().await.is_some() {
                        // A burst of writes is read once.
                        while changes.try_recv().is_ok() {}
                        if this.update(cx, |this, cx| this.reload(cx)).is_err() {
                            break;
                        }
                    }
                });
                Some((watcher, task))
            }
            Err(error) => {
                eprintln!("rature: {}: not followed: {error}", path.display());
                None
            }
        };
        self.path = Some(path);
        self.scroll.set_offset(point(px(0.), px(0.)));
        self.reload(cx);
    }

    /// Reads the file again. The scroll offset is kept: what is above the reading position
    /// rarely moves while a document grows below it.
    fn reload(&mut self, cx: &mut Context<Self>) {
        let Some(path) = &self.path else { return };
        let blocks = match std::fs::read_to_string(path) {
            Ok(source) => document::parse(&source),
            // Between the removal and the rename of an atomic write: the next event brings it.
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => return,
            Err(error) => document::parse(&format!("# {}\n\n{error}\n", path.display())),
        };
        self.set_blocks(blocks, cx);
    }

    fn set_blocks(&mut self, blocks: Vec<Block>, cx: &mut Context<Self>) {
        let mut sources = Vec::new();
        document::diagrams(&blocks, &mut sources);
        let mut before = Vec::new();
        document::diagrams(&self.blocks, &mut before);
        let shown: Vec<Option<Picture>> = before
            .iter()
            .map(|source| match self.drawings.get(source) {
                Some(Drawing::Done(picture)) | Some(Drawing::Pending(Some(picture))) => {
                    Some(picture.clone())
                }
                _ => None,
            })
            .collect();
        self.drawings.retain(|source, _| sources.contains(source));
        for (ix, source) in sources.into_iter().enumerate() {
            if self.drawings.contains_key(&source) {
                continue;
            }
            let stand_in = shown.get(ix).cloned().flatten();
            self.drawings
                .insert(source.clone(), Drawing::Pending(stand_in));
            let drawn = cx.background_executor().spawn({
                let source = source.clone();
                async move { diagram::render(&source) }
            });
            cx.spawn(async move |this, cx| {
                let drawing = match drawn.await {
                    Ok(picture) => Drawing::Done(picture),
                    Err(error) => Drawing::Failed(error),
                };
                this.update(cx, |this, cx| {
                    // Still in the document: a reload may have dropped it meanwhile.
                    if let Some(slot) = this.drawings.get_mut(&source) {
                        *slot = drawing;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        self.blocks = blocks;
        cx.notify();
    }

    fn scroll_by(&mut self, delta: Pixels, cx: &mut Context<Self>) {
        let offset = self.scroll.offset();
        let bottom = -self.scroll.max_offset().y;
        let y = (offset.y - delta).min(px(0.)).max(bottom);
        self.scroll.set_offset(point(offset.x, y));
        cx.notify();
    }

    fn page(&self) -> Pixels {
        self.scroll.bounds().size.height
    }

    fn line_down(&mut self, _: &LineDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(px(LINE), cx);
    }
    fn line_up(&mut self, _: &LineUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(px(-LINE), cx);
    }
    fn half_page_down(&mut self, _: &HalfPageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(self.page() / 2., cx);
    }
    fn half_page_up(&mut self, _: &HalfPageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-self.page() / 2., cx);
    }
    fn page_down(&mut self, _: &PageDown, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(self.page() - px(LINE), cx);
    }
    fn page_up(&mut self, _: &PageUp, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-(self.page() - px(LINE)), cx);
    }
    fn top(&mut self, _: &Top, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(-self.scroll.offset().y, cx);
    }
    fn bottom(&mut self, _: &Bottom, _: &mut Window, cx: &mut Context<Self>) {
        self.scroll_by(self.scroll.max_offset().y + self.scroll.offset().y, cx);
    }
    fn quit(&mut self, _: &Quit, _: &mut Window, cx: &mut Context<Self>) {
        cx.quit();
    }

    /// Opens the file in nvim, in the desktop's terminal.
    fn edit(&mut self, _: &Edit, _: &mut Window, _: &mut Context<Self>) {
        let Some(path) = &self.path else { return };
        if let Err(error) = editor(path) {
            eprintln!("rature: cannot open nvim: {error}");
        }
    }
}

fn editor(path: &Path) -> std::io::Result<()> {
    std::process::Command::new("xdg-terminal-exec")
        .arg("nvim")
        .arg(path)
        .stdin(std::process::Stdio::null())
        .spawn()
        .map(drop)
}

impl Render for Reader {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("page")
            .key_context("Reader")
            .track_focus(&self.focus)
            .on_action(cx.listener(Self::line_down))
            .on_action(cx.listener(Self::line_up))
            .on_action(cx.listener(Self::half_page_down))
            .on_action(cx.listener(Self::half_page_up))
            .on_action(cx.listener(Self::page_down))
            .on_action(cx.listener(Self::page_up))
            .on_action(cx.listener(Self::top))
            .on_action(cx.listener(Self::bottom))
            .on_action(cx.listener(Self::edit))
            .on_action(cx.listener(Self::quit))
            .size_full()
            .track_scroll(&self.scroll)
            .overflow_y_scroll()
            .bg(theme::background())
            .text_color(theme::text())
            .font_family(PROSE_FONT)
            .text_size(px(16.))
            .line_height(rems(1.6))
            .child(
                div()
                    .mx_auto()
                    .max_w(px(COLUMN))
                    .px_8()
                    .py_10()
                    .flex()
                    .flex_col()
                    .gap_4()
                    .children(self.blocks.iter().map(|b| block(b, &self.drawings))),
            )
    }
}

fn block(block: &Block, drawings: &Drawings) -> AnyElement {
    match block {
        Block::Heading(level, inlines) => {
            let size = match level {
                1 => 30.,
                2 => 24.,
                3 => 20.,
                _ => 17.,
            };
            flow(inlines)
                .text_size(px(size))
                .font_weight(FontWeight::BOLD)
                .pt_2()
                .into_any_element()
        }
        Block::Paragraph(inlines) => flow(inlines).into_any_element(),
        Block::Code(text) => code(text).into_any_element(),
        Block::Picture(picture) => div()
            .flex()
            .justify_center()
            .child(image(picture))
            .into_any_element(),
        Block::Diagram(source) => match drawings.get(source) {
            Some(Drawing::Done(picture)) | Some(Drawing::Pending(Some(picture))) => {
                self::block(&Block::Picture(picture.clone()), drawings)
            }
            Some(Drawing::Failed(error)) => broken(source, error).into_any_element(),
            _ => div()
                .h(px(120.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(theme::muted())
                .child("Drawing the diagram…")
                .into_any_element(),
        },
        Block::Broken { source, error } => broken(source, error).into_any_element(),
        Block::List { ordered, items } => div()
            .flex()
            .flex_col()
            .gap_1()
            .children(
                items
                    .iter()
                    .enumerate()
                    .map(|(ix, it)| item(*ordered, ix, it, drawings)),
            )
            .into_any_element(),
        Block::Quote(blocks) => div()
            .border_l_4()
            .border_color(theme::border())
            .pl_4()
            .text_color(theme::muted())
            .flex()
            .flex_col()
            .gap_2()
            .children(blocks.iter().map(|b| self::block(b, drawings)))
            .into_any_element(),
        Block::Table(rows) => div()
            .flex()
            .flex_col()
            .border_1()
            .border_color(theme::border())
            .rounded_md()
            .children(rows.iter().enumerate().map(|(ix, row)| {
                div()
                    .flex()
                    .when(ix == 0, |row| {
                        row.bg(theme::surface()).font_weight(FontWeight::BOLD)
                    })
                    .when(ix > 0, |row| row.border_t_1().border_color(theme::border()))
                    .children(row.iter().map(|cell| flow(cell).flex_1().px_3().py_1()))
            }))
            .into_any_element(),
        Block::Rule => div().h(px(1.)).bg(theme::border()).into_any_element(),
    }
}

fn item(ordered: bool, ix: usize, item: &Item, drawings: &Drawings) -> impl IntoElement {
    let marker = match (item.checked, ordered) {
        (Some(true), _) => "☑".to_string(),
        (Some(false), _) => "☐".to_string(),
        (None, true) => format!("{}.", ix + 1),
        (None, false) => "•".to_string(),
    };
    div()
        .flex()
        .gap_2()
        .child(
            div()
                .w(px(20.))
                .flex_none()
                .text_color(theme::muted())
                .child(marker),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .children(item.blocks.iter().map(|b| block(b, drawings))),
        )
}

/// Text that wraps word by word, so a formula can sit inside a sentence.
fn flow(inlines: &[Inline]) -> Div {
    let mut row = div().flex().flex_wrap().items_center();
    for inline in inlines {
        match inline {
            Inline::Math(picture) => row = row.child(div().mr_1().child(image(picture))),
            Inline::Text(text, _) if text == "\n" => row = row.child(div().w_full()),
            // A code span is one piece: split, its background would show a gap per word.
            Inline::Text(text, marks) if marks.code => row = row.child(word_element(text, *marks)),
            Inline::Text(text, marks) => {
                for word in text.split_inclusive(char::is_whitespace) {
                    row = row.child(word_element(&word.replace('\n', " "), *marks));
                }
            }
        }
    }
    row
}

fn word_element(word: &str, marks: Marks) -> Div {
    div()
        .flex_none()
        .whitespace_nowrap()
        .when(marks.strong, |d| d.font_weight(FontWeight::BOLD))
        .when(marks.emphasis, |d| d.italic())
        .when(marks.strike, |d| d.line_through())
        .when(marks.link, |d| d.text_color(theme::accent()).underline())
        .when(marks.code, |d| {
            d.font_family(CODE_FONT)
                .text_size(px(14.))
                .bg(theme::surface())
                .px_1()
        })
        .child(word.to_string())
}

/// Source that could not be drawn: the reason, then the source as code.
fn broken(source: &str, error: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_color(rgb(0xf7768e))
                .text_size(px(13.))
                .child(error.to_string()),
        )
        .child(code(source))
}

fn code(text: &str) -> Div {
    div()
        .bg(theme::surface())
        .rounded_md()
        .p_4()
        .font_family(CODE_FONT)
        .text_size(px(14.))
        .line_height(rems(1.45))
        .flex()
        .flex_col()
        .children(
            text.lines()
                .map(|line| div().whitespace_nowrap().child(line.to_string())),
        )
}

fn image(picture: &Picture) -> Img {
    img(picture.image.clone())
        .w(px(picture.width))
        .h(px(picture.height))
        .max_w_full()
}

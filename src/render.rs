//! What the reader shows, built from a file's content: GPUI elements, nothing kept.

use std::cell::Cell;
use std::collections::HashSet;
use std::path::Path;
use std::sync::Arc;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::cache::{Cache, Slot};
use crate::config::Config;
use crate::diff::{Change, Word};
use crate::document::{Alert, Block, Inline, Item, Marks, Picture};
use crate::formats::{Kind, Node, Value};
use crate::highlight::Lines;
use crate::reader::Reader;
use crate::ui::{Icon, Palette, icon};

pub struct Ctx<'a, 'b> {
    pub theme: &'a Palette,
    pub config: &'a Config,
    pub zoom: f32,
    /// The folder of the file, against which images are found.
    pub folder: Option<&'a Path>,
    pub drawings: &'a Cache<String, Picture>,
    pub highlights: &'a Cache<(String, String), Option<Lines>>,
    /// What the search looks for, lowercased.
    pub query: Option<&'a str>,
    pub cx: &'a Context<'b, Reader>,
    ids: Cell<u64>,
}

impl<'a, 'b> Ctx<'a, 'b> {
    #[allow(clippy::too_many_arguments)]
    pub fn new(
        theme: &'a Palette,
        config: &'a Config,
        zoom: f32,
        folder: Option<&'a Path>,
        drawings: &'a Cache<String, Picture>,
        highlights: &'a Cache<(String, String), Option<Lines>>,
        query: Option<&'a str>,
        cx: &'a Context<'b, Reader>,
    ) -> Self {
        Self {
            theme,
            config,
            zoom,
            folder,
            drawings,
            highlights,
            query,
            cx,
            ids: Cell::new(0),
        }
    }

    fn id(&self, name: &'static str) -> ElementId {
        let id = self.ids.get();
        self.ids.set(id + 1);
        ElementId::NamedInteger(name.into(), id)
    }

    pub fn size(&self, scale: f32) -> Pixels {
        px(self.config.font_size * self.zoom * scale)
    }
}

/// The ink formulas are drawn in, as `#rrggbb`: the text colour of the theme being shown.
pub fn ink(cx: &App) -> String {
    hex(crate::ui::theme::target_palette(cx).text)
}

/// `color` as `#rrggbb`, for what is drawn as SVG.
pub fn hex(color: Hsla) -> String {
    let color = Rgba::from(color);
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(color.r),
        byte(color.g),
        byte(color.b)
    )
}

/// Whether the theme being shown is dark: diagrams and code take their dark colours.
pub fn is_dark(cx: &App) -> bool {
    crate::ui::theme::target_palette(cx).bg.l < 0.5
}

/// A heading's size, against the text's.
fn heading_scale(level: u8) -> f32 {
    match level {
        1 => 1.85,
        2 => 1.5,
        3 => 1.25,
        _ => 1.06,
    }
}

pub fn block(ctx: &Ctx, block: &Block) -> AnyElement {
    let t = ctx.theme;
    match block {
        Block::Heading(level, inlines) => {
            heading(ctx, *level, flow(ctx, inlines)).into_any_element()
        }
        Block::Paragraph(inlines) => flow(ctx, inlines).into_any_element(),
        Block::Code { lang, text } => {
            code(ctx, lang.as_deref().unwrap_or(""), text).into_any_element()
        }
        Block::Picture(picture) => centered(picture_element(picture)),
        Block::Diagram(source) => match ctx.drawings.get(source) {
            Some(Slot::Failed(error)) => broken(ctx, source, error).into_any_element(),
            Some(slot) if slot.shown().is_some() => {
                centered(picture_element(slot.shown().expect("checked")))
            }
            _ => div()
                .h(px(120.))
                .flex()
                .items_center()
                .justify_center()
                .text_color(t.mute)
                .child("Drawing the diagram…")
                .into_any_element(),
        },
        Block::Broken { source, error } => broken(ctx, source, error).into_any_element(),
        Block::List {
            ordered,
            start,
            items,
        } => div()
            .flex()
            .flex_col()
            .gap_1()
            .children(
                items
                    .iter()
                    .enumerate()
                    .map(|(ix, it)| item(ctx, *ordered, *start as usize + ix, it)),
            )
            .into_any_element(),
        Block::Quote(blocks) => div()
            .border_l_4()
            .border_color(t.line)
            .pl_4()
            .text_color(t.mute)
            .flex()
            .flex_col()
            .gap_2()
            .children(blocks.iter().map(|b| self::block(ctx, b)))
            .into_any_element(),
        Block::Alert(kind, blocks) => {
            let (color, label) = match kind {
                Alert::Note => (t.run, "Note"),
                Alert::Tip => (t.ok, "Tip"),
                Alert::Important => (t.acc, "Important"),
                Alert::Warning => (t.wait, "Warning"),
                Alert::Caution => (t.fail, "Caution"),
            };
            div()
                .border_l_4()
                .border_color(color)
                .bg(color.opacity(0.08))
                .rounded_r_md()
                .px_4()
                .py_2()
                .flex()
                .flex_col()
                .gap_2()
                .child(
                    div()
                        .text_color(color)
                        .font_weight(FontWeight::BOLD)
                        .child(label),
                )
                .children(blocks.iter().map(|b| self::block(ctx, b)))
                .into_any_element()
        }
        Block::Table(rows) => grid(
            ctx,
            rows.iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| flow(ctx, cell).into_any_element())
                        .collect()
                })
                .collect(),
        )
        .into_any_element(),
        Block::FrontMatter(pairs) => div()
            .text_size(ctx.size(0.85))
            .text_color(t.mute)
            .border_b_1()
            .border_color(t.line)
            .pb_2()
            .flex()
            .flex_col()
            .children(pairs.iter().map(|(key, value)| {
                div()
                    .flex()
                    .gap_3()
                    .child(div().w(px(110.)).flex_none().child(key.clone()))
                    .child(div().flex_1().text_color(t.text).child(value.clone()))
            }))
            .into_any_element(),
        Block::Footnote { label, blocks } => div()
            .text_size(ctx.size(0.88))
            .flex()
            .gap_2()
            .child(div().text_color(t.run).child(format!("{label}.")))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .children(blocks.iter().map(|b| self::block(ctx, b))),
            )
            .into_any_element(),
        Block::Image { url, alt } => match ctx.folder.map(|f| f.join(crate::document::decode(url)))
        {
            Some(path) if !url.contains("://") && path.exists() => {
                centered(img(path).max_w_full().into_any_element())
            }
            _ => div()
                .text_color(t.mute)
                .child(format!("[{alt}] {url}"))
                .into_any_element(),
        },
        Block::Rule => div().h(px(1.)).my_2().bg(t.line).into_any_element(),
    }
}

pub fn heading(ctx: &Ctx, level: u8, inner: Div) -> Div {
    inner
        .text_size(ctx.size(heading_scale(level)))
        .font_weight(FontWeight::BOLD)
        .pt(ctx.size(if level <= 2 { 0.6 } else { 0.3 }))
}

fn centered(element: AnyElement) -> AnyElement {
    div()
        .flex()
        .justify_center()
        .child(element)
        .into_any_element()
}

fn broken(ctx: &Ctx, source: &str, error: &str) -> Div {
    div()
        .flex()
        .flex_col()
        .gap_1()
        .child(
            div()
                .text_color(ctx.theme.fail)
                .text_size(ctx.size(0.8))
                .child(error.to_string()),
        )
        .child(code(ctx, "", source))
}

fn item(ctx: &Ctx, ordered: bool, number: usize, item: &Item) -> impl IntoElement {
    let t = ctx.theme;
    let box_size = ctx.size(0.95);
    let marker: AnyElement = match (item.checked, ordered) {
        (Some(done), _) => div()
            .mt(ctx.size(0.33))
            .size(box_size)
            .flex()
            .items_center()
            .justify_center()
            .rounded(px(4.))
            .border_1()
            .border_color(if done { t.ok } else { t.line2 })
            .when(done, |d| d.bg(t.ok))
            .when(done, |d| {
                d.child(icon(Icon::Check).size(ctx.size(0.7)).text_color(t.bg))
            })
            .into_any_element(),
        (None, true) => div()
            .text_color(t.faint)
            .font_family(crate::ui::typography::MONO)
            .text_size(ctx.size(0.85))
            .child(format!("{number}."))
            .into_any_element(),
        (None, false) => div()
            .mt(ctx.size(0.68))
            .size(ctx.size(0.3))
            .rounded_full()
            .bg(t.faint)
            .into_any_element(),
    };
    div()
        .flex()
        .gap_2()
        .child(
            div()
                .min_w(ctx.size(1.4))
                .flex_none()
                .flex()
                .justify_center()
                .child(marker),
        )
        .child(
            div()
                .flex_1()
                .flex()
                .flex_col()
                .gap_1()
                .when(item.checked == Some(true), |d| d.text_color(t.mute))
                .children(item.blocks.iter().map(|b| block(ctx, b))),
        )
}

/// Text that wraps word by word, so a formula can sit inside a sentence.
pub fn flow(ctx: &Ctx, inlines: &[Inline]) -> Div {
    let mut row = div().flex().flex_wrap().items_baseline();
    // Noto Sans: ascent 1.069 em, descent 0.293 em, in a line 1.6 rem high.
    let size = f32::from(ctx.size(1.));
    let below = (25.6 - 1.362 * size) / 2. + 0.293 * size;
    // A formula waits for what follows: punctuation right after it stays on its line.
    let mut formula: Option<Div> = None;
    for inline in inlines {
        match inline {
            // The row lines its pieces up by their bottom edge (margins aside): moved so the
            // formula's baseline meets the text's, where the line box ends `below` under it.
            Inline::Math(picture) => {
                row = row.children(formula.take());
                formula = Some(
                    div().flex().items_baseline().child(
                        div()
                            .relative()
                            .top(px(picture.depth - below))
                            .child(picture_element(picture)),
                    ),
                );
            }
            Inline::Text(text, _) if text == "\n" => {
                row = row.children(formula.take()).child(div().w_full())
            }
            // A code span is one piece: split, its background would show a gap per word.
            Inline::Text(text, marks) if marks.code => {
                row = row
                    .children(formula.take())
                    .child(word(ctx, text, marks, Change::Same))
            }
            Inline::Text(text, marks) => {
                for piece in text.split_inclusive(char::is_whitespace) {
                    let piece = word(ctx, &piece.replace('\n', " "), marks, Change::Same);
                    row = match formula.take() {
                        Some(f) if !text.starts_with(char::is_whitespace) => {
                            row.child(f.child(piece))
                        }
                        f => row.children(f).child(piece),
                    };
                }
            }
        }
    }
    row.children(formula)
}

/// The words of a paragraph or a heading that changed, the old ones struck.
pub fn changed_words(ctx: &Ctx, words: &[Word]) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .children(words.iter().map(|w| word(ctx, &w.text, &w.marks, w.change)))
}

fn word(ctx: &Ctx, text: &str, marks: &Marks, change: Change) -> AnyElement {
    let t = ctx.theme;
    let hit = ctx
        .query
        .is_some_and(|q| !q.is_empty() && text.to_lowercase().contains(q));
    let element = div()
        .flex_none()
        .whitespace_nowrap()
        .when(marks.strong, |d| d.font_weight(FontWeight::BOLD))
        .when(marks.emphasis, |d| d.italic())
        .when(marks.strike, |d| d.line_through())
        .when(marks.sup, |d| d.text_size(ctx.size(0.7)))
        .when(marks.code, |d| {
            d.font_family(ctx.config.code_font.clone())
                .text_size(ctx.size(0.875))
                .bg(t.card)
                .rounded_sm()
                .px_1()
        })
        .when(marks.link.is_some(), |d| {
            d.text_color(if marks.broken { t.fail } else { t.run })
                .underline()
                .cursor_pointer()
        })
        .when(change == Change::Inserted, |d| d.bg(t.ok.opacity(0.25)))
        .when(change == Change::Deleted, |d| {
            d.bg(t.fail.opacity(0.2)).line_through().text_color(t.mute)
        })
        .when(hit, |d| d.bg(t.wait.opacity(0.35)))
        .child(text.to_string());
    match &marks.link {
        Some(link) => {
            let link = link.clone();
            element
                .id(ctx.id("link"))
                .on_click(ctx.cx.listener(move |this, _: &ClickEvent, window, cx| {
                    this.follow(&link, window, cx)
                }))
                .into_any_element()
        }
        None => element.into_any_element(),
    }
}

pub fn code(ctx: &Ctx, lang: &str, text: &str) -> Div {
    let t = ctx.theme;
    let lines = ctx
        .highlights
        .get(&(lang.to_string(), text.to_string()))
        .and_then(|slot| slot.shown())
        .and_then(|lines| lines.clone());
    let group: SharedString = format!("code-{}", ctx.ids.get()).into();
    let copied = text.to_string();
    div()
        .group(group.clone())
        .relative()
        .bg(t.card)
        .rounded_md()
        .child(
            div()
                .id(ctx.id("code"))
                .overflow_x_scroll()
                .p_4()
                .font_family(ctx.config.code_font.clone())
                .text_size(ctx.size(0.875))
                .line_height(rems(1.45))
                .flex()
                .flex_col()
                .children(text.lines().enumerate().map(|(ix, line)| {
                    let spans = lines.as_ref().and_then(|lines| lines.get(ix));
                    code_line(ctx, line, spans.map(Vec::as_slice))
                })),
        )
        .child(
            div()
                .id(ctx.id("copy"))
                .absolute()
                .top_2()
                .right_2()
                .px_2()
                .rounded_sm()
                .bg(t.bg)
                .text_size(ctx.size(0.75))
                .text_color(t.mute)
                .cursor_pointer()
                .opacity(0.)
                .group_hover(group, |s| s.opacity(1.))
                .child("Copy")
                .on_click(ctx.cx.listener(move |this, _: &ClickEvent, _, cx| {
                    this.copy(copied.clone(), "Code copied", cx)
                })),
        )
}

fn code_line(ctx: &Ctx, line: &str, spans: Option<&[crate::highlight::Span]>) -> AnyElement {
    let hit = ctx
        .query
        .is_some_and(|q| !q.is_empty() && line.to_lowercase().contains(q));
    let text: SharedString = if line.is_empty() {
        " ".into()
    } else {
        line.to_string().into()
    };
    let styled = match spans {
        Some(spans) => StyledText::new(text).with_highlights(
            spans.iter().filter(|s| s.range.end <= line.len()).map(|s| {
                (
                    s.range.clone(),
                    HighlightStyle {
                        color: Some(s.color.into()),
                        font_weight: s.bold.then_some(FontWeight::BOLD),
                        font_style: s.italic.then_some(FontStyle::Italic),
                        ..Default::default()
                    },
                )
            }),
        ),
        None => StyledText::new(text),
    };
    div()
        .whitespace_nowrap()
        .when(hit, |d| d.bg(ctx.theme.wait.opacity(0.2)))
        .child(styled)
        .into_any_element()
}

fn grid(ctx: &Ctx, rows: Vec<Vec<AnyElement>>) -> Stateful<Div> {
    let t = ctx.theme;
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    div().id(ctx.id("table")).overflow_x_scroll().child(
        div()
            .min_w_full()
            .flex()
            .flex_col()
            .border_1()
            .border_color(t.line)
            .rounded_md()
            .children(rows.into_iter().enumerate().map(|(ix, row)| {
                let mut cells = row;
                cells.resize_with(columns, || div().into_any_element());
                div()
                    .flex()
                    .when(ix == 0, |row| row.bg(t.card).font_weight(FontWeight::BOLD))
                    .when(ix > 0, |row| row.border_t_1().border_color(t.line))
                    .children(
                        cells
                            .into_iter()
                            .map(|cell| div().flex_1().min_w(px(90.)).px_3().py_1().child(cell)),
                    )
            })),
    )
}

/// A CSV file: its first row is its head. Long ones stop at `LIMIT` rows, said under it.
pub fn table(ctx: &Ctx, rows: &[Vec<String>]) -> AnyElement {
    const LIMIT: usize = 2000;
    let shown = rows
        .iter()
        .take(LIMIT)
        .map(|row| {
            row.iter()
                .map(|cell| {
                    let hit = ctx
                        .query
                        .is_some_and(|q| !q.is_empty() && cell.to_lowercase().contains(q));
                    div()
                        .when(hit, |d| d.bg(ctx.theme.wait.opacity(0.35)))
                        .child(cell.clone())
                        .into_any_element()
                })
                .collect()
        })
        .collect();
    div()
        .flex()
        .flex_col()
        .gap_2()
        .child(grid(ctx, shown))
        .when(rows.len() > LIMIT, |d| {
            d.child(
                div()
                    .text_color(ctx.theme.mute)
                    .child(format!("The first {LIMIT} rows of {}.", rows.len())),
            )
        })
        .into_any_element()
}

/// JSON or YAML as a tree: a map or a list folds by a click on its row.
pub fn data(ctx: &Ctx, nodes: &[Node], folded: &HashSet<Vec<usize>>) -> AnyElement {
    let mut rows = Vec::new();
    for (ix, node) in nodes.iter().enumerate() {
        data_rows(ctx, node, vec![ix], 0, folded, &mut rows);
    }
    div()
        .font_family(ctx.config.code_font.clone())
        .text_size(ctx.size(0.875))
        .line_height(rems(1.5))
        .flex()
        .flex_col()
        .children(rows)
        .into_any_element()
}

fn data_rows(
    ctx: &Ctx,
    node: &Node,
    path: Vec<usize>,
    depth: usize,
    folded: &HashSet<Vec<usize>>,
    rows: &mut Vec<AnyElement>,
) {
    let t = ctx.theme;
    let key = node.key.as_ref().map(|k| {
        div()
            .text_color(t.run)
            .child(format!("{k}:"))
            .into_any_element()
    });
    let indent = px(16. * depth as f32);
    match &node.value {
        Value::Scalar(text, kind) => {
            let color = match kind {
                Kind::Text => t.ok,
                Kind::Number => t.wait,
                Kind::Bool => t.acc,
                Kind::Null => t.mute,
            };
            let hit = ctx.query.is_some_and(|q| {
                !q.is_empty()
                    && (text.to_lowercase().contains(q)
                        || node
                            .key
                            .as_ref()
                            .is_some_and(|k| k.to_lowercase().contains(q)))
            });
            rows.push(
                div()
                    .flex()
                    .gap_2()
                    .pl(indent + px(16.))
                    .when(hit, |d| d.bg(t.wait.opacity(0.2)))
                    .children(key)
                    .child(div().text_color(color).child(text.clone()))
                    .into_any_element(),
            );
        }
        Value::List(children) | Value::Map(children) => {
            let is_map = matches!(node.value, Value::Map(_));
            let closed = folded.contains(&path);
            let summary = match (is_map, closed) {
                (true, true) => format!("{{…}} {} keys", children.len()),
                (false, true) => format!("[…] {} items", children.len()),
                (true, false) => "{".to_string(),
                (false, false) => "[".to_string(),
            };
            let toggled = path.clone();
            rows.push(
                div()
                    .id(ctx.id("fold"))
                    .flex()
                    .gap_2()
                    .pl(indent)
                    .cursor_pointer()
                    .child(div().w(px(12.)).text_color(t.mute).child(if closed {
                        "▸"
                    } else {
                        "▾"
                    }))
                    .children(key)
                    .child(div().text_color(t.mute).child(summary))
                    .on_click(ctx.cx.listener(move |this, _: &ClickEvent, _, cx| {
                        this.toggle_fold(toggled.clone(), cx)
                    }))
                    .into_any_element(),
            );
            if !closed {
                for (ix, child) in children.iter().enumerate() {
                    let mut inner = path.clone();
                    inner.push(ix);
                    data_rows(ctx, child, inner, depth + 1, folded, rows);
                }
            }
        }
    }
}

pub fn picture_element(picture: &Picture) -> AnyElement {
    img(picture.image.clone())
        .w(px(picture.width))
        .h(px(picture.height))
        .max_w_full()
        .into_any_element()
}

/// A block as it was, struck: what "what changed" shows of a removed block.
pub fn ghost(ctx: &Ctx, block: &Block) -> AnyElement {
    let t = ctx.theme;
    marked(
        ctx,
        t.fail,
        t.fail_bg,
        div()
            .opacity(0.55)
            .line_through()
            .child(self::block(ctx, block)),
    )
}

/// A block that changed since the baseline: tinted, a bar at its left in the colour of what
/// happened to it. The text stays where it would be unmarked.
pub fn marked(ctx: &Ctx, ink: Hsla, tint: Hsla, content: impl IntoElement) -> AnyElement {
    let t = ctx.theme;
    div()
        .relative()
        .mx(px(-16.))
        .px(px(16.))
        .py(px(6.))
        .rounded(px(8.))
        .bg(tint)
        .child(
            div()
                .absolute()
                .left_0()
                .top(px(8.))
                .bottom(px(8.))
                .w(px(3.))
                .rounded_full()
                .bg(ink),
        )
        .text_color(t.text)
        .child(content)
        .into_any_element()
}

pub fn image_file(path: &Path) -> AnyElement {
    centered(img(Arc::<Path>::from(path)).max_w_full().into_any_element())
}

//! What the reader shows, built from a file's content: GPUI elements, nothing kept.

use std::cell::Cell;
use std::collections::HashSet;
use std::ops::Range;
use std::panic;
use std::path::Path;
use std::sync::Arc;

/// The narrowest a CSV column gets.
const CELL: f32 = 90.;
/// The narrowest a Markdown table's cell gets, at zoom 1.
pub const TABLE_CELL: f32 = 90.;
/// GPUI's rem: the reader leaves it as it is, a `Scaled` makes it follow the zoom.
const REM: f32 = 16.;
/// The page's line, against the text size: 1.6 rem at zoom 1 with the default 16 px text.
pub const LINE: f32 = 1.6;

use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::cache::{Cache, Slot};
use crate::config::Config;
use crate::diagram;
use crate::diff::{Change, Word};
use crate::document::{self, Alert, Block, Inline, Item, Marks, Picture};
use crate::formats::{Kind, Node, Value};
use crate::highlight::{self, Lines};
use gpui_design_system::{Icon, Palette, icon};

/// The view a page is drawn in, the reader or a canvas's board: what a click on what is drawn
/// asks of it.
pub trait Host: Sized + 'static {
    /// A link was clicked: the click on its block, which comes next, edits the block, or
    /// follows the link with Ctrl held.
    fn link_clicked(&mut self, link: String);
    /// Follows a link, Ctrl-clicked.
    fn follow(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>);
    /// Puts `text` in the clipboard, and says so with `what`.
    fn copy(&mut self, text: String, what: &str, cx: &mut Context<Self>);
}

pub struct Ctx<'a, 'b, H: Host> {
    pub theme: &'a Palette,
    pub config: &'a Config,
    /// What the page is drawn at: text, lines and the sizes given in px follow it.
    pub zoom: f32,
    pub drawn: &'a Drawn,
    /// What the search looks for, lowercased.
    pub query: Option<&'a str>,
    pub cx: &'a Context<'b, H>,
    /// The colour of links in this theme.
    pub link: Hsla,
    ids: Cell<u64>,
}

impl<'a, 'b, H: Host> Ctx<'a, 'b, H> {
    pub fn new(
        theme: &'a Palette,
        config: &'a Config,
        zoom: f32,
        drawn: &'a Drawn,
        query: Option<&'a str>,
        cx: &'a Context<'b, H>,
    ) -> Self {
        Self {
            theme,
            config,
            zoom,
            drawn,
            query,
            cx,
            link: crate::kit::link(theme),
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

    /// `v` px at zoom 1, at the page's zoom: a length the rem does not carry, so that it
    /// follows the zoom outside a `Scaled` as well as inside one.
    pub fn px(&self, v: f32) -> Pixels {
        px(v * self.zoom)
    }

    /// A line `rems_at_1` times the text size high, at the page's zoom.
    pub fn line_height(&self, rems_at_1: f32) -> Pixels {
        line_height(rems_at_1, self.config.font_size, self.zoom)
    }
}

/// What is drawn off the main thread for the blocks a view shows, by what it is drawn from.
#[derive(Default)]
pub struct Drawn {
    /// Mermaid diagrams, by their source.
    pub diagrams: Cache<String, Picture>,
    /// Code's colours, by its language and its text.
    pub highlights: Cache<(String, String), Option<Lines>>,
}

/// What a view shows that is drawn off the main thread.
#[derive(Default)]
pub struct Wanted {
    /// Diagrams' sources, in the order they are shown.
    pub diagrams: Vec<String>,
    /// Code, by its language and its text.
    pub code: Vec<(String, String)>,
}

impl Wanted {
    /// What these blocks show that is drawn off the main thread.
    pub fn of(blocks: &[Block]) -> Self {
        let mut wanted = Self::default();
        document::diagrams(blocks, &mut wanted.diagrams);
        document::code_blocks(blocks, &mut wanted.code);
        wanted
    }
}

impl Drawn {
    pub fn clear(&mut self) {
        self.diagrams.clear();
        self.highlights.clear();
    }

    /// Here is what a view shows now: what is not drawn yet is started off the main thread,
    /// the view told as each is done, and what it no longer shows is dropped. `before`, the
    /// diagrams it showed until now: a new one shows the one that stood at its place meanwhile,
    /// so the page does not jump. `of` finds this in the view.
    pub fn fill<T: 'static>(
        &mut self,
        wanted: Wanted,
        before: &[String],
        of: fn(&mut T) -> &mut Drawn,
        cx: &mut Context<T>,
    ) {
        let dark = is_dark(cx);
        let colors = diagram::Colors {
            font: cx.global::<Config>().font.clone(),
            ..diagram::Colors::of(&gpui_design_system::theme::target_palette(cx))
        };
        let shown: Vec<Option<Picture>> = before
            .iter()
            .map(|s| self.diagrams.get(s).and_then(|slot| slot.shown().cloned()))
            .collect();
        let Wanted { diagrams, code } = wanted;
        self.diagrams.retain(|source, _| diagrams.contains(source));
        for (ix, source) in diagrams.into_iter().enumerate() {
            if self.diagrams.contains_key(&source) {
                continue;
            }
            let stand_in = shown.get(ix).cloned().flatten();
            self.diagrams
                .insert(source.clone(), Slot::Pending(stand_in));
            let drawn = cx.background_executor().spawn({
                let (source, colors) = (source.clone(), colors.clone());
                async move { diagram::render(&source, &colors) }
            });
            cx.spawn(async move |this, cx| {
                let slot = match drawn.await {
                    Ok(picture) => Slot::Done(picture),
                    Err(error) => Slot::Failed(error),
                };
                this.update(cx, |this, cx| {
                    if let Some(place) = of(this).diagrams.get_mut(&source) {
                        *place = slot;
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
        // Code is shown plain meanwhile.
        self.highlights.retain(|key, _| code.contains(key));
        for key in code {
            if self.highlights.contains_key(&key) {
                continue;
            }
            self.highlights.insert(key.clone(), Slot::Pending(None));
            let coloured = cx.background_executor().spawn({
                let (lang, text) = key.clone();
                async move { highlight::highlight(&lang, &text, dark) }
            });
            cx.spawn(async move |this, cx| {
                let lines = coloured.await;
                this.update(cx, |this, cx| {
                    if let Some(place) = of(this).highlights.get_mut(&key) {
                        *place = Slot::Done(lines);
                        cx.notify();
                    }
                })
                .ok();
            })
            .detach();
        }
    }
}

/// Its child laid out and drawn with a rem `zoom` times GPUI's, so that every length in rems
/// inside it (spacings, radii, the kit's own sizes) follows the zoom: GPUI has no transform
/// for a subtree, a zoomed one is laid out again at its zoomed size. What is in px follows the
/// zoom through `Ctx`, text and lines included: the text size and line height set around the
/// child come from `Ctx::size` and `Ctx::line_height`, never in rems, or they would be zoomed
/// twice.
pub struct Scaled {
    rem: Pixels,
    child: AnyElement,
}

pub fn scaled(zoom: f32, child: impl IntoElement) -> Scaled {
    Scaled {
        rem: px(REM * zoom),
        child: child.into_any_element(),
    }
}

impl IntoElement for Scaled {
    type Element = Self;

    fn into_element(self) -> Self {
        self
    }
}

impl Element for Scaled {
    type RequestLayoutState = ();
    type PrepaintState = ();

    fn id(&self) -> Option<ElementId> {
        None
    }

    fn source_location(&self) -> Option<&'static panic::Location<'static>> {
        None
    }

    fn request_layout(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        window: &mut Window,
        cx: &mut App,
    ) -> (LayoutId, ()) {
        let child = &mut self.child;
        let layout =
            window.with_rem_size(Some(self.rem), |window| child.request_layout(window, cx));
        (layout, ())
    }

    fn prepaint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_rem_size(Some(self.rem), |window| {
            child.prepaint(window, cx);
        });
    }

    fn paint(
        &mut self,
        _: Option<&GlobalElementId>,
        _: Option<&InspectorElementId>,
        _: Bounds<Pixels>,
        _: &mut (),
        _: &mut (),
        window: &mut Window,
        cx: &mut App,
    ) {
        let child = &mut self.child;
        window.with_rem_size(Some(self.rem), |window| child.paint(window, cx));
    }
}

/// The ink formulas are drawn in, as `#rrggbb`: the text colour of the theme being shown.
pub fn ink(cx: &App) -> String {
    hex(gpui_design_system::theme::target_palette(cx).text)
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
    gpui_design_system::theme::target_palette(cx).bg.l < 0.5
}

/// A line `rems_at_1` times the text size `font_size` high, at `zoom`: as high as the text the
/// configured size and the zoom make, `rems_at_1` rems at zoom 1 with 16 px text. In px, not in
/// rems: inside a `Scaled`, whose rem already follows the zoom, a line in rems would be zoomed
/// twice. At 16 px, the same to the bit as GPUI turns rems into px: a power of two scales
/// without rounding.
pub fn line_height(rems_at_1: f32, font_size: f32, zoom: f32) -> Pixels {
    px(rems_at_1 * font_size * zoom)
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

pub fn block<H: Host>(ctx: &Ctx<H>, block: &Block) -> AnyElement {
    let t = ctx.theme;
    match block {
        Block::Heading(level, inlines) => {
            heading(ctx, *level, flow(ctx, inlines)).into_any_element()
        }
        Block::Paragraph(inlines) => flow(ctx, inlines).into_any_element(),
        Block::Code { lang, text } => {
            code(ctx, lang.as_deref().unwrap_or(""), text).into_any_element()
        }
        Block::Picture(picture) => centered(picture_element(ctx, picture)),
        Block::Diagram(source) => match ctx.drawn.diagrams.get(source) {
            Some(Slot::Failed(error)) => broken(ctx, source, error).into_any_element(),
            Some(slot) if slot.shown().is_some() => {
                centered(picture_element(ctx, slot.shown().expect("checked")))
            }
            _ => div()
                .h(ctx.px(120.))
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
            ctx.id("table"),
            ctx.theme,
            ctx.px(TABLE_CELL),
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
                    .child(div().w(ctx.px(110.)).flex_none().child(key.clone()))
                    .child(div().flex_1().text_color(t.text).child(value.clone()))
            }))
            .into_any_element(),
        Block::Footnote { label, blocks } => div()
            .text_size(ctx.size(0.88))
            .flex()
            .gap_2()
            .child(div().text_color(ctx.link).child(format!("{label}.")))
            .child(
                div()
                    .flex_1()
                    .flex()
                    .flex_col()
                    .children(blocks.iter().map(|b| self::block(ctx, b))),
            )
            .into_any_element(),
        // Found on disk when the file was read, not while it is drawn.
        Block::Image { url, alt, path } => match path {
            Some(path) => centered(img(path.clone()).max_w_full().into_any_element()),
            None => div()
                .text_color(t.mute)
                .child(format!("[{alt}] {url}"))
                .into_any_element(),
        },
        Block::Rule => div().h(px(1.)).my_2().bg(t.line).into_any_element(),
    }
}

pub fn heading<H: Host>(ctx: &Ctx<H>, level: u8, inner: Div) -> Div {
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

fn broken<H: Host>(ctx: &Ctx<H>, source: &str, error: &str) -> Div {
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

fn item<H: Host>(ctx: &Ctx<H>, ordered: bool, number: usize, item: &Item) -> impl IntoElement {
    let size = ctx.size(1.);
    bullet(
        size,
        marker(
            size,
            ctx.theme,
            &ctx.config.code_font,
            item.checked,
            ordered,
            number,
        ),
        div()
            .flex()
            .flex_col()
            .gap_1()
            .when(item.checked == Some(true), |d| d.text_color(ctx.theme.mute))
            .children(item.blocks.iter().map(|b| block(ctx, b))),
    )
}

/// A list's line: its marker in a column of its own, its text beside it. The list's editor
/// lays its lines out the same way.
pub fn bullet(size: Pixels, marker: impl IntoElement, content: impl IntoElement) -> Div {
    div()
        .flex()
        .gap_2()
        .child(
            div()
                .min_w(size * 1.4)
                .flex_none()
                .flex()
                .justify_center()
                .child(marker),
        )
        .child(div().flex_1().min_w_0().child(content))
}

/// A list item's bullet, number or task box, for text of `size`; a number in `code_font`.
pub fn marker(
    size: Pixels,
    t: &Palette,
    code_font: &str,
    checked: Option<bool>,
    ordered: bool,
    number: usize,
) -> AnyElement {
    match (checked, ordered) {
        (Some(done), _) => div()
            .mt(size * 0.33)
            .size(size * 0.95)
            .flex()
            .items_center()
            .justify_center()
            // 4 px for text 16 px high: it follows the zoom as the box does.
            .rounded(size * 0.25)
            .border_1()
            .border_color(if done { t.ok } else { t.line2 })
            .when(done, |d| d.bg(t.ok))
            .when(done, |d| {
                d.child(icon(Icon::Check).size(size * 0.7).text_color(t.bg))
            })
            .into_any_element(),
        (None, true) => div()
            .text_color(t.faint)
            .font_family(SharedString::from(code_font.to_string()))
            .text_size(size * 0.85)
            .child(format!("{number}."))
            .into_any_element(),
        (None, false) => div()
            .mt(size * 0.68)
            .size(size * 0.3)
            .rounded_full()
            .bg(t.faint)
            .into_any_element(),
    }
}

/// Text wrapped to the column: laid out whole, or word by word when a formula sits in it.
pub fn flow<H: Host>(ctx: &Ctx<H>, inlines: &[Inline]) -> Div {
    if inlines
        .iter()
        .all(|inline| matches!(inline, Inline::Text(..)))
    {
        return div().child(text_flow(ctx, inlines));
    }
    word_flow(ctx, inlines)
}

/// Text with no formula in it, laid out whole as the block's editor lays it out: clicked to
/// edit, it keeps its width to the pixel.
fn text_flow<H: Host>(ctx: &Ctx<H>, inlines: &[Inline]) -> AnyElement {
    let t = ctx.theme;
    let mut text = String::new();
    let mut pieces = Vec::new();
    for inline in inlines {
        if let Inline::Text(piece, marks) = inline {
            let start = text.len();
            if piece == "\n" {
                text.push('\n');
            } else {
                text.push_str(&piece.replace('\n', " "));
            }
            pieces.push((start..text.len(), marks));
        }
    }
    let mut each = vec![HighlightStyle::default(); text.len()];
    let mut code = Vec::new();
    let mut links = Vec::new();
    for (range, marks) in &pieces {
        let style = HighlightStyle {
            font_weight: marks.strong.then_some(FontWeight::BOLD),
            font_style: marks.emphasis.then_some(FontStyle::Italic),
            strikethrough: marks.strike.then_some(StrikethroughStyle {
                thickness: px(1.),
                color: None,
            }),
            color: marks
                .link
                .as_ref()
                .map(|_| if marks.broken { t.fail } else { ctx.link }),
            underline: marks.link.as_ref().map(|_| UnderlineStyle {
                thickness: px(1.),
                color: None,
                wavy: false,
            }),
            background_color: marks.code.then_some(t.card2),
            ..Default::default()
        };
        each[range.clone()].fill(style);
        if marks.code {
            code.push((
                range.clone(),
                SharedString::from(ctx.config.code_font.clone()),
            ));
        }
        if let Some(link) = &marks.link {
            links.push((range.clone(), link.clone()));
        }
    }
    if let Some(query) = ctx.query.filter(|q| !q.is_empty()) {
        let lower = text.to_lowercase();
        // Lowercasing can move bytes: the hits are only marked when it did not.
        if lower.len() == text.len() {
            for (at, _) in lower.match_indices(query) {
                for style in &mut each[at..at + query.len()] {
                    style.background_color = Some(t.wait.opacity(0.35));
                }
            }
        }
    }
    let mut highlights: Vec<(Range<usize>, HighlightStyle)> = Vec::new();
    for (ix, style) in each.into_iter().enumerate() {
        match highlights.last_mut() {
            Some((range, last)) if *last == style || !text.is_char_boundary(ix) => {
                range.end = ix + 1
            }
            _ => highlights.push((ix..ix + 1, style)),
        }
    }
    let styled = StyledText::new(text)
        .with_highlights(highlights)
        .with_font_family_overrides(code);
    if links.is_empty() {
        return styled.into_any_element();
    }
    let host = ctx.cx.entity().downgrade();
    let (ranges, targets): (Vec<_>, Vec<_>) = links.into_iter().unzip();
    InteractiveText::new(ctx.id("text"), styled)
        .on_click(ranges, move |ix, _, cx| {
            // The block's own click edits it, or follows this with Ctrl held.
            let link = targets[ix].to_string();
            host.update(cx, |this, _| this.link_clicked(link)).ok();
        })
        .into_any_element()
}

/// Text with formulas: laid out word by word, a formula kept on the line of what follows it.
fn word_flow<H: Host>(ctx: &Ctx<H>, inlines: &[Inline]) -> Div {
    let mut row = div().flex().flex_wrap().items_baseline();
    let size = f32::from(ctx.size(1.));
    // The page's line, as set around it: in or out of a `Scaled`, the same px.
    let line = f32::from(ctx.line_height(LINE));
    let below = formula_drop(size, line);
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
                            .top(ctx.px(picture.depth) - px(below))
                            .child(picture_element(ctx, picture)),
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

/// How far under the text's baseline its line box ends, for text `size` px high on a line
/// `line` px high: Noto Sans has an ascent of 1.069 em and a descent of 0.293 em, and the line's
/// leading is split above and below.
fn formula_drop(size: f32, line: f32) -> f32 {
    (line - 1.362 * size) / 2. + 0.293 * size
}

/// The words of a paragraph or a heading that changed, the old ones struck.
pub fn changed_words<H: Host>(ctx: &Ctx<H>, words: &[Word]) -> Div {
    div()
        .flex()
        .flex_wrap()
        .items_baseline()
        .children(words.iter().map(|w| word(ctx, &w.text, &w.marks, w.change)))
}

fn word<H: Host>(ctx: &Ctx<H>, text: &str, marks: &Marks, change: Change) -> AnyElement {
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
            d.text_color(if marks.broken { t.fail } else { ctx.link })
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
                .on_click(
                    ctx.cx
                        .listener(move |this: &mut H, event: &ClickEvent, window, cx| {
                            // A click edits, as anywhere else; ctrl-click follows.
                            if !event.modifiers().secondary() {
                                return;
                            }
                            cx.stop_propagation();
                            this.follow(&link, window, cx)
                        }),
                )
                .into_any_element()
        }
        None => element.into_any_element(),
    }
}

pub fn code<H: Host>(ctx: &Ctx<H>, lang: &str, text: &str) -> Div {
    let t = ctx.theme;
    let lines = ctx
        .drawn
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
                .line_height(ctx.line_height(1.45))
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
                .on_click(ctx.cx.listener(move |this: &mut H, _: &ClickEvent, _, cx| {
                    cx.stop_propagation();
                    this.copy(copied.clone(), "Code copied", cx)
                })),
        )
}

fn code_line<H: Host>(
    ctx: &Ctx<H>,
    line: &str,
    spans: Option<&[crate::highlight::Span]>,
) -> AnyElement {
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

/// A table: its first row the header, the others under it, no cell narrower than `cell`.
pub fn grid(id: ElementId, t: &Palette, cell: Pixels, rows: Vec<Vec<AnyElement>>) -> Stateful<Div> {
    let columns = rows.iter().map(Vec::len).max().unwrap_or(0);
    div().id(id).overflow_x_scroll().child(
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
                            .map(|inner| div().flex_1().min_w(cell).px_3().py_1().child(inner)),
                    )
            })),
    )
}

/// A row of a CSV file, `ix` of `count`, its first row the head: one line, its cells cut to
/// their column, so that every row is as high and only those shown are built. `columns` the
/// most cells a row has.
pub fn table_row<H: Host>(
    ctx: &Ctx<H>,
    row: &[String],
    ix: usize,
    count: usize,
    columns: usize,
) -> AnyElement {
    let t = ctx.theme;
    let cells = (0..columns).map(|column| {
        let cell = row.get(column).map_or("", String::as_str);
        let hit = ctx
            .query
            .is_some_and(|q| !q.is_empty() && cell.to_lowercase().contains(q));
        div()
            .flex_1()
            .min_w(px(CELL))
            .px_3()
            .py_1()
            .overflow_hidden()
            .whitespace_nowrap()
            .text_ellipsis()
            .when(hit, |d| d.bg(t.wait.opacity(0.35)))
            .child(cell.to_string())
    });
    div()
        .flex()
        .w_full()
        .min_w(px(CELL * columns as f32))
        .border_x_1()
        .border_b_1()
        .border_color(t.line)
        .when(ix == 0, |row| {
            row.border_t_1()
                .rounded_t_md()
                .bg(t.card)
                .font_weight(FontWeight::BOLD)
        })
        .when(ix + 1 == count, |row| row.rounded_b_md())
        .children(cells)
        .into_any_element()
}

/// A line of a JSON or YAML tree as shown: the node at `path` (its index among the documents,
/// then among its parents' children).
pub struct DataRow {
    pub path: Vec<usize>,
}

/// The lines a JSON or YAML tree shows, what is folded left out: worked out when the tree or
/// its folds change, not on each frame.
pub fn data_lines(nodes: &[Node], folded: &HashSet<Vec<usize>>) -> Vec<DataRow> {
    fn walk(node: &Node, path: Vec<usize>, folded: &HashSet<Vec<usize>>, rows: &mut Vec<DataRow>) {
        let open = match &node.value {
            Value::List(children) | Value::Map(children) if !folded.contains(&path) => {
                Some(children)
            }
            _ => None,
        };
        rows.push(DataRow { path: path.clone() });
        for (ix, child) in open.into_iter().flatten().enumerate() {
            let mut inner = path.clone();
            inner.push(ix);
            walk(child, inner, folded, rows);
        }
    }
    let mut rows = Vec::new();
    for (ix, node) in nodes.iter().enumerate() {
        walk(node, vec![ix], folded, &mut rows);
    }
    rows
}

/// The node at `path`, as `DataRow` has it.
fn node_at<'a>(nodes: &'a [Node], path: &[usize]) -> Option<&'a Node> {
    let (first, rest) = path.split_first()?;
    let mut node = nodes.get(*first)?;
    for ix in rest {
        node = match &node.value {
            Value::List(children) | Value::Map(children) => children.get(*ix)?,
            Value::Scalar(..) => return None,
        };
    }
    Some(node)
}

/// A line of a JSON or YAML tree: a value, or a map or a list that folds by a click. One line
/// each, in the code font, so that every line is as high.
pub fn data_row(
    ctx: &Ctx<crate::reader::Reader>,
    nodes: &[Node],
    row: &DataRow,
    folded: &HashSet<Vec<usize>>,
) -> AnyElement {
    let Some(node) = node_at(nodes, &row.path) else {
        return div().into_any_element();
    };
    let path = row.path.clone();
    let depth = path.len() - 1;
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
            div()
                .flex()
                .gap_2()
                .whitespace_nowrap()
                .pl(indent + px(16.))
                .when(hit, |d| d.bg(t.wait.opacity(0.2)))
                .children(key)
                .child(div().text_color(color).child(one_line(text)))
                .into_any_element()
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
            div()
                .id(ElementId::Name(format!("fold-{path:?}").into()))
                .flex()
                .gap_2()
                .whitespace_nowrap()
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
                .into_any_element()
        }
    }
}

/// A value on one line: its line breaks shown as `↵`, so that every line of a tree is as high.
fn one_line(text: &str) -> String {
    if text.contains('\n') {
        text.replace("\r\n", "↵").replace('\n', "↵")
    } else {
        text.to_string()
    }
}

/// A formula or a diagram, drawn for zoom 1, at the page's zoom.
pub fn picture_element<H: Host>(ctx: &Ctx<H>, picture: &Picture) -> AnyElement {
    img(picture.image.clone())
        .w(ctx.px(picture.width))
        .h(ctx.px(picture.height))
        .max_w_full()
        .into_any_element()
}

/// A block as it was, struck: what "what changed" shows of a removed block.
pub fn ghost<H: Host>(ctx: &Ctx<H>, block: &Block) -> AnyElement {
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
pub fn marked<H: Host>(
    ctx: &Ctx<H>,
    ink: Hsla,
    tint: Hsla,
    content: impl IntoElement,
) -> AnyElement {
    let t = ctx.theme;
    div()
        .relative()
        .mx(ctx.px(-16.))
        .px(ctx.px(16.))
        .py(ctx.px(6.))
        .rounded(ctx.px(8.))
        .bg(tint)
        .child(
            div()
                .absolute()
                .left_0()
                .top(ctx.px(8.))
                .bottom(ctx.px(8.))
                .w(ctx.px(3.))
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

#[cfg(test)]
mod tests {
    use super::{LINE, REM, data_lines, formula_drop, line_height, node_at, one_line};
    use crate::config::Config;
    use crate::formats::{Kind, Node, Value};
    use gpui_kit::{DefiniteLength, px, rems};
    use std::collections::HashSet;

    /// The line of 16 px text at `zoom`, where the rem is `rem`.
    fn line_under(zoom: f32, rem: f32) -> f32 {
        let line = DefiniteLength::from(line_height(LINE, 16., zoom));
        f32::from(line.to_pixels(px(16. * zoom).into(), px(rem)))
    }

    fn line_px(zoom: f32) -> f32 {
        line_under(zoom, REM)
    }

    #[test]
    fn the_line_grows_with_the_zoom_and_stays_put_at_zoom_1() {
        // At zoom 1, the line the page always had: 1.6 rem, 25.6 px.
        assert_eq!(line_height(LINE, 16., 1.), rems(1.6).to_pixels(px(REM)));
        // At the reader's zooms, the same to the bit as the line in rems it was.
        for zoom in [0.6, 1.1, 1.4641, 2.5] {
            assert_eq!(
                line_height(LINE, 16., zoom),
                rems(LINE * zoom).to_pixels(px(REM)),
                "zoom {zoom}"
            );
        }
        assert_eq!(line_px(1.), 25.6);
        // At the reader's zooms, 0.6 to 2.5, the line of 16 px text stays 1.6 times the text:
        // Noto Sans' glyphs, 1.362 em from top to bottom, fit in it.
        for zoom in [0.6, 1., 1.1, 1.4641, 2., 2.5] {
            let size = 16. * zoom;
            assert!((line_px(zoom) - 1.6 * size).abs() < 1e-3, "zoom {zoom}");
            assert!(line_px(zoom) > 1.362 * size, "zoom {zoom}");
        }
        assert!((line_px(2.5) - 64.).abs() < 1e-3);
    }

    #[test]
    fn the_line_follows_the_text_size() {
        // 24 px text: its glyphs, about 32.7 px, once stood on a 25.6 px line.
        let size = Config::parse("font_size = 24\n").unwrap().font_size;
        for zoom in [0.6, 1., 1.4641, 2.5] {
            let line = f32::from(line_height(LINE, size, zoom));
            let text = size * zoom;
            assert!((line - 1.6 * text).abs() < 1e-3, "zoom {zoom}");
            assert!(line > 1.362 * text, "zoom {zoom}");
        }
        assert!((f32::from(line_height(LINE, size, 1.)) - 38.4).abs() < 1e-3);
        // A text size of nothing reads as 1 px: the line is never empty.
        let none = Config::parse("font_size = 0\n").unwrap().font_size;
        assert_eq!(f32::from(line_height(LINE, none, 1.)), 1.6);
    }

    #[test]
    fn a_scaled_page_does_not_zoom_its_line_twice() {
        // In a `Scaled`, the rem follows the zoom: the line stays as high as outside.
        for zoom in [0.1, 0.5, 1., 2., 4.] {
            assert_eq!(line_under(zoom, REM * zoom), line_px(zoom), "zoom {zoom}");
        }
    }

    #[test]
    fn a_formula_sits_as_low_on_its_line_at_every_zoom() {
        let at_1 = formula_drop(16., line_px(1.));
        assert_eq!(at_1, (25.6 - 1.362 * 16.) / 2. + 0.293 * 16.);
        for zoom in [0.6, 1.4641, 2.5] {
            let drop = formula_drop(16. * zoom, line_px(zoom));
            assert!((drop - at_1 * zoom).abs() < 1e-3, "zoom {zoom}");
        }
    }

    fn scalar(key: Option<&str>, text: &str) -> Node {
        Node {
            key: key.map(str::to_string),
            value: Value::Scalar(text.into(), Kind::Text),
        }
    }

    #[test]
    fn a_tree_shows_a_line_per_node_its_folds_left_out() {
        // {a: [x, y], b: z}
        let tree = vec![Node {
            key: None,
            value: Value::Map(vec![
                Node {
                    key: Some("a".into()),
                    value: Value::List(vec![scalar(None, "x"), scalar(None, "y")]),
                },
                scalar(Some("b"), "z"),
            ]),
        }];
        let paths = |folded: &HashSet<Vec<usize>>| -> Vec<Vec<usize>> {
            data_lines(&tree, folded)
                .into_iter()
                .map(|r| r.path)
                .collect()
        };
        assert_eq!(
            paths(&HashSet::new()),
            [
                vec![0],
                vec![0, 0],
                vec![0, 0, 0],
                vec![0, 0, 1],
                vec![0, 1]
            ]
        );
        assert_eq!(
            paths(&HashSet::from([vec![0, 0]])),
            [vec![0], vec![0, 0], vec![0, 1]]
        );
        let y = node_at(&tree, &[0, 0, 1]).unwrap();
        assert!(matches!(&y.value, Value::Scalar(text, _) if text == "y"));
        assert!(node_at(&tree, &[0, 1, 0]).is_none());
        assert_eq!(one_line("a\nb"), "a↵b");
    }
}

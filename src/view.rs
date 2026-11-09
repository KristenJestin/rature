use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::document::{Block, Inline, Item, Marks, Picture};
use crate::theme;

const PROSE_FONT: &str = "Noto Sans";
const CODE_FONT: &str = "JetBrainsMono Nerd Font";
/// The reading column: wide enough for a table, narrow enough for prose.
const COLUMN: f32 = 760.;

pub struct Reader {
    blocks: Vec<Block>,
}

impl Reader {
    pub fn new(blocks: Vec<Block>) -> Self {
        Self { blocks }
    }
}

impl Render for Reader {
    fn render(&mut self, _: &mut Window, _: &mut Context<Self>) -> impl IntoElement {
        div()
            .id("page")
            .size_full()
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
                    .children(self.blocks.iter().map(block)),
            )
    }
}

fn block(block: &Block) -> AnyElement {
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
        Block::Broken { source, error } => div()
            .flex()
            .flex_col()
            .gap_1()
            .child(
                div()
                    .text_color(rgb(0xf7768e))
                    .text_size(px(13.))
                    .child(error.clone()),
            )
            .child(code(source))
            .into_any_element(),
        Block::List { ordered, items } => div()
            .flex()
            .flex_col()
            .gap_1()
            .children(
                items
                    .iter()
                    .enumerate()
                    .map(|(ix, it)| item(*ordered, ix, it)),
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
            .children(blocks.iter().map(self::block))
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

fn item(ordered: bool, ix: usize, item: &Item) -> impl IntoElement {
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
                .children(item.blocks.iter().map(block)),
        )
}

/// Text that wraps word by word, so a formula can sit inside a sentence.
fn flow(inlines: &[Inline]) -> Div {
    let mut row = div().flex().flex_wrap().items_center();
    for inline in inlines {
        match inline {
            Inline::Math(picture) => row = row.child(div().mr_1().child(image(picture))),
            Inline::Text(text, _) if text == "\n" => row = row.child(div().w_full()),
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

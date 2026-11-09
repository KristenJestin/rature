use ratex_layout::{LayoutOptions, layout, to_display_list, validate_layout};
use ratex_svg::{SvgOptions, render_to_svg};
use ratex_types::{color::Color, math_style::MathStyle};

use crate::diagram::{SCALE, picture};
use crate::document::Picture;

/// Pixels per em: a little above the text so a formula reads at the same weight.
const INLINE_EM: f64 = 18.0;
const DISPLAY_EM: f64 = 22.0;

pub fn render(source: &str, display: bool) -> Result<Picture, String> {
    let ast = ratex_parser::parse(source).map_err(|e| e.to_string())?;
    let style = if display {
        MathStyle::Display
    } else {
        MathStyle::Text
    };
    let color =
        Color::parse(crate::theme::TEXT_HEX).expect("the theme's text colour is a hex colour");
    let tree = layout(
        &ast,
        &LayoutOptions::default().with_style(style).with_color(color),
    );
    validate_layout(&tree)?;
    let list = to_display_list(&tree);
    let em = if display { DISPLAY_EM } else { INLINE_EM };
    let padding = if display { 4.0 } else { 0.0 };
    let options = SvgOptions {
        font_size: em * SCALE as f64,
        padding: padding * SCALE as f64,
        embed_glyphs: true,
        ..SvgOptions::default()
    };
    let svg = render_to_svg(&list, &options);
    let width = list.width * em + 2.0 * padding;
    let height = (list.height + list.depth) * em + 2.0 * padding;
    Ok(picture(svg, width as f32, height as f32))
}

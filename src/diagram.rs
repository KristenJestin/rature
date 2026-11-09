use std::sync::Arc;

use gpui_kit::{Image, ImageFormat};
use mermaid_rs_renderer::{RenderOptions, Theme};

use crate::document::Picture;
use crate::render::hex;
use crate::ui::Palette;

/// GPUI draws an SVG at its own size: drawn this many times larger and shown at its size, it
/// stays sharp on a scaled screen.
pub const SCALE: f32 = 2.0;

/// The theme's colours a diagram is drawn in, as `#rrggbb`.
#[derive(Clone)]
pub struct Colors {
    pub dark: bool,
    pub fill: String,
    pub border: String,
    pub text: String,
    pub line: String,
}

impl Colors {
    pub fn of(p: &Palette) -> Self {
        Self {
            dark: p.bg.l < 0.5,
            fill: hex(p.card),
            border: hex(p.line2),
            text: hex(p.text),
            line: hex(p.mute),
        }
    }
}

/// Draws `source` in the colours of the theme.
pub fn render(source: &str, colors: &Colors) -> Result<Picture, String> {
    let options = RenderOptions {
        theme: theme(colors),
        ..RenderOptions::default()
    };
    let size = mermaid_rs_renderer::measure(source, options.clone()).map_err(|e| e.to_string())?;
    let svg =
        mermaid_rs_renderer::render_with_options(source, options).map_err(|e| e.to_string())?;
    let scaled = svg.replacen(
        &format!("width=\"{}\" height=\"{}\"", size.width, size.height),
        &format!(
            "width=\"{}\" height=\"{}\"",
            size.width * SCALE,
            size.height * SCALE
        ),
        1,
    );
    Ok(picture(scaled, size.width, size.height))
}

pub fn picture(svg: String, width: f32, height: f32) -> Picture {
    Picture {
        image: Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())),
        width,
        height,
        depth: 0.,
    }
}

fn theme(colors: &Colors) -> Theme {
    let mut theme = if colors.dark {
        Theme::dark()
    } else {
        Theme::modern()
    };
    theme.background = "transparent".into();
    theme.font_family = "Geist, sans-serif".into();
    theme.primary_color = colors.fill.clone();
    theme.secondary_color = colors.fill.clone();
    theme.tertiary_color = colors.fill.clone();
    theme.primary_border_color = colors.border.clone();
    theme.primary_text_color = colors.text.clone();
    theme.text_color = colors.text.clone();
    theme.line_color = colors.line.clone();
    theme.edge_label_background = "transparent".into();
    theme.cluster_background = "transparent".into();
    theme.cluster_border = colors.border.clone();
    theme.sequence_actor_fill = colors.fill.clone();
    theme.sequence_actor_border = colors.border.clone();
    theme.sequence_actor_line = colors.line.clone();
    theme.sequence_note_fill = colors.fill.clone();
    theme.sequence_note_border = colors.border.clone();
    theme
}

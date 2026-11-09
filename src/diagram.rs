use std::sync::Arc;

use gpui_kit::{Image, ImageFormat};
use mermaid_rs_renderer::{RenderOptions, Theme};

use crate::document::Picture;

/// GPUI draws an SVG at its own size: drawn this many times larger and shown at its size, it
/// stays sharp on a scaled screen.
pub const SCALE: f32 = 2.0;

/// Draws `source`, in the dark or light Mermaid theme.
pub fn render(source: &str, dark: bool) -> Result<Picture, String> {
    let options = RenderOptions {
        theme: theme(dark),
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

fn theme(dark: bool) -> Theme {
    let mut theme = if dark { Theme::dark() } else { Theme::modern() };
    theme.background = "transparent".into();
    theme
}

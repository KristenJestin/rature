use std::sync::Arc;

use gpui_kit::{Image, ImageFormat};
use merman::svg::SvgPipeline;
use merman::{
    MermaidEngine, OperationControl, RenderOutput, RenderRequest, Renderer,
    SourcePresentationConfig, SvgRequest,
};

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
    pub background: String,
    pub fill: String,
    pub border: String,
    pub text: String,
    pub line: String,
}

impl Colors {
    pub fn of(p: &Palette) -> Self {
        Self {
            dark: p.bg.l < 0.5,
            background: hex(p.bg),
            fill: hex(p.card),
            border: hex(p.line2),
            text: hex(p.text),
            line: hex(p.mute),
        }
    }
}

/// Draws `source` in the colours of the theme.
pub fn render(source: &str, colors: &Colors) -> Result<Picture, String> {
    let site = SourcePresentationConfig::from_value(serde_json::json!({
        "theme": "base",
        "fontFamily": "Geist, sans-serif",
        "themeVariables": {
            "darkMode": colors.dark,
            "background": colors.background,
            "primaryColor": colors.fill,
            "secondaryColor": colors.fill,
            "tertiaryColor": colors.fill,
            "primaryBorderColor": colors.border,
            "primaryTextColor": colors.text,
            "textColor": colors.text,
            "lineColor": colors.line,
            "edgeLabelBackground": colors.background,
            "clusterBkg": "transparent",
            "clusterBorder": colors.border,
        }
    }));
    let renderer =
        Renderer::new().with_presentation_engine(MermaidEngine::new().with_site_config(site));
    // GPUI draws SVG with resvg, which shows no HTML labels: this pipeline turns them into text.
    let request = SvgRequest {
        pipeline: Some(SvgPipeline::resvg_safe()),
        ..SvgRequest::default()
    };
    let output = renderer
        .render(RenderRequest::svg(source, OperationControl::new(), request))
        .map_err(|e| e.to_string())?;
    let RenderOutput::Svg(Some(svg)) = output else {
        return Err("Not a Mermaid diagram".into());
    };
    let svg = svg.svg();
    let (width, height) = size(svg).ok_or("The diagram has no size")?;
    let scaled = svg.replacen(
        "width=\"100%\"",
        &format!("width=\"{}\" height=\"{}\"", width * SCALE, height * SCALE),
        1,
    );
    Ok(picture(scaled, width, height))
}

/// The width and height of the root `viewBox`.
fn size(svg: &str) -> Option<(f32, f32)> {
    let start = svg.find("viewBox=\"")? + "viewBox=\"".len();
    let end = start + svg[start..].find('"')?;
    let mut numbers = svg[start..end].split_whitespace().skip(2);
    Some((numbers.next()?.parse().ok()?, numbers.next()?.parse().ok()?))
}

pub fn picture(svg: String, width: f32, height: f32) -> Picture {
    Picture {
        image: Arc::new(Image::from_bytes(ImageFormat::Svg, svg.into_bytes())),
        width,
        height,
        depth: 0.,
    }
}

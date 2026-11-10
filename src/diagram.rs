use std::cell::Cell;
use std::collections::HashSet;
use std::hash::{DefaultHasher, Hash, Hasher};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, Once};
use std::time::Duration;

use gpui_kit::{Image, ImageFormat};
use merman::svg::SvgPipeline;
use merman::{
    MermaidEngine, OperationControl, RenderOutput, RenderRequest, Renderer,
    SourcePresentationConfig, SvgRequest,
};

use crate::document::Picture;
use crate::render::hex;
use gpui_design_system::Palette;

/// GPUI draws an SVG at its own size: drawn this many times larger and shown at its size, it
/// stays sharp on a scaled screen.
pub const SCALE: f32 = 2.0;

/// The theme's colours a diagram is drawn in, as `#rrggbb`.
#[derive(Clone)]
pub struct Colors {
    pub dark: bool,
    /// The font family its labels ask for: the text's own (`config.font`).
    pub font: String,
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
            font: gpui_design_system::typography::UI.into(),
            background: hex(p.bg),
            fill: hex(p.card),
            border: hex(p.line2),
            text: hex(p.text),
            line: hex(p.mute),
        }
    }
}

/// The longest diagram drawn, in bytes: Mermaid's own limit.
const LONGEST: usize = 50_000;
/// How long merman may take on a diagram. It looks at the clock between steps, so a step that
/// never ends is not cut short.
const PATIENCE: Duration = Duration::from_secs(5);

/// Draws `source` in the colours of the theme.
pub fn render(source: &str, colors: &Colors) -> Result<Picture, String> {
    if source.len() > LONGEST {
        return Err(format!(
            "The diagram is too long to draw ({} KB, at most {} KB)",
            source.len() / 1000,
            LONGEST / 1000
        ));
    }
    guard("diagram", source, || draw(source, colors))
}

fn draw(source: &str, colors: &Colors) -> Result<Picture, String> {
    let site = SourcePresentationConfig::from_value(serde_json::json!({
        "theme": "base",
        "fontFamily": format!("{}, sans-serif", colors.font),
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
        .render(RenderRequest::svg(
            source,
            OperationControl::new().with_deadline(PATIENCE),
            request,
        ))
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

thread_local! {
    /// Set while a guarded library draws: its panics are said once by `guard`, not by the hook.
    static QUIET: Cell<bool> = const { Cell::new(false) };
}

/// Runs `draw`, a library's drawing of `source`, and turns its panic into an error: a formula or
/// a diagram a library cannot handle must not take the window down. The panic is told on stderr
/// once per source, however often it is drawn again.
pub fn guard<T>(
    what: &str,
    source: &str,
    draw: impl FnOnce() -> Result<T, String>,
) -> Result<T, String> {
    static HOOK: Once = Once::new();
    HOOK.call_once(|| {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            if !QUIET.with(Cell::get) {
                previous(info);
            }
        }));
    });
    let was = QUIET.replace(true);
    let drawn = std::panic::catch_unwind(AssertUnwindSafe(draw));
    QUIET.set(was);
    drawn.unwrap_or_else(|panic| {
        let message = panic
            .downcast_ref::<&str>()
            .copied()
            .or_else(|| panic.downcast_ref::<String>().map(String::as_str))
            .unwrap_or("no reason given");
        if first_panic(what, source) {
            eprintln!("rature: a {what} could not be drawn, its library failed: {message}");
        }
        Err(format!("This {what} could not be drawn: {message}"))
    })
}

/// True the first time `source` panics.
fn first_panic(what: &str, source: &str) -> bool {
    static SEEN: Mutex<Option<HashSet<u64>>> = Mutex::new(None);
    let mut hasher = DefaultHasher::new();
    (what, source).hash(&mut hasher);
    let mut seen = SEEN.lock().unwrap_or_else(|poisoned| poisoned.into_inner());
    let seen = seen.get_or_insert_with(HashSet::new);
    if seen.len() > 1000 {
        seen.clear();
    }
    seen.insert(hasher.finish())
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

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_library_panic_becomes_an_error() {
        let drawn: Result<(), String> = guard("diagram", "graph TD; boom", || panic!("boom"));
        assert_eq!(drawn.unwrap_err(), "This diagram could not be drawn: boom");
        let drawn: Result<(), String> = guard("formula", "\\x", || panic!("{} {}", "at", 3));
        assert_eq!(drawn.unwrap_err(), "This formula could not be drawn: at 3");
        assert_eq!(guard("formula", "x", || Ok(2)), Ok(2));
    }

    #[test]
    fn a_panic_is_told_once_per_source() {
        assert!(first_panic("diagram", "once-a"));
        assert!(!first_panic("diagram", "once-a"));
        assert!(first_panic("formula", "once-a"));
    }

    #[test]
    fn a_huge_diagram_is_refused_before_drawing() {
        let colors = Colors {
            dark: false,
            font: "Geist".into(),
            background: "#ffffff".into(),
            fill: "#eeeeee".into(),
            border: "#888888".into(),
            text: "#000000".into(),
            line: "#444444".into(),
        };
        let huge = format!("graph TD\n{}", "A-->B\n".repeat(LONGEST / 6 + 1));
        let error = render(&huge, &colors).err().expect("too long");
        assert!(error.contains("too long"), "{error}");
    }
}

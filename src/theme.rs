//! The colours. Rature's own by default; on Omarchy, those of the current theme, read again each
//! time the theme changes (the `theme-set.d` hook `install.sh` writes tells the window).

use std::path::PathBuf;

use gpui_kit::{App, Global, Hsla, Rgba, rgb};
use serde::Deserialize;

#[derive(Clone)]
pub struct Theme {
    pub dark: bool,
    pub background: Rgba,
    /// Code blocks, table heads, the side panel.
    pub surface: Rgba,
    pub border: Rgba,
    pub text: Rgba,
    pub muted: Rgba,
    pub accent: Rgba,
    pub selection: Rgba,
    pub red: Rgba,
    pub green: Rgba,
    pub yellow: Rgba,
    pub blue: Rgba,
    pub magenta: Rgba,
}

impl Global for Theme {}

impl Theme {
    fn base() -> Self {
        Self {
            dark: true,
            background: rgb(0x1a1b26),
            surface: rgb(0x24283b),
            border: rgb(0x3b4261),
            text: rgb(0xc0caf5),
            muted: rgb(0x787c99),
            accent: rgb(0x7aa2f7),
            selection: rgb(0x2e3c64),
            red: rgb(0xf7768e),
            green: rgb(0x9ece6a),
            yellow: rgb(0xe0af68),
            blue: rgb(0x7aa2f7),
            magenta: rgb(0xbb9af7),
        }
    }

    /// The current Omarchy theme, or Rature's own when there is none or it does not read.
    pub fn current() -> Self {
        std::fs::read_to_string(omarchy_colors())
            .ok()
            .and_then(|text| from_omarchy(&text))
            .unwrap_or_else(Self::base)
    }

    /// The text colour as `#rrggbb`, for what is drawn outside GPUI (formulas, diagrams).
    pub fn text_hex(&self) -> String {
        hex(self.text)
    }
}

/// `color` at `alpha`: a tint over the background.
pub fn tint(color: Rgba, alpha: f32) -> Hsla {
    let mut color: Hsla = color.into();
    color.a = alpha;
    color
}

pub fn hex(color: Rgba) -> String {
    let byte = |v: f32| (v.clamp(0., 1.) * 255.).round() as u8;
    format!(
        "#{:02x}{:02x}{:02x}",
        byte(color.r),
        byte(color.g),
        byte(color.b)
    )
}

pub fn theme(cx: &App) -> &Theme {
    cx.global::<Theme>()
}

fn omarchy_colors() -> PathBuf {
    let state = std::env::var_os("XDG_STATE_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".local/state"));
    state.join("omarchy/current/theme/colors.toml")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

#[derive(Deserialize)]
struct Colors {
    mode: Option<String>,
    background: String,
    foreground: String,
    accent: String,
    lighter_background: Option<String>,
    dark_foreground: Option<String>,
    selection: Option<String>,
    muted: Option<String>,
    color8: Option<String>,
    red: Option<String>,
    green: Option<String>,
    yellow: Option<String>,
    blue: Option<String>,
    magenta: Option<String>,
    color1: Option<String>,
    color2: Option<String>,
    color3: Option<String>,
    color4: Option<String>,
    color5: Option<String>,
}

fn parse_hex(value: &str) -> Option<Rgba> {
    let digits = value.trim().strip_prefix('#')?;
    (digits.len() == 6)
        .then(|| u32::from_str_radix(digits, 16).ok())
        .flatten()
        .map(rgb)
}

fn mix(a: Rgba, b: Rgba, t: f32) -> Rgba {
    Rgba {
        r: a.r + (b.r - a.r) * t,
        g: a.g + (b.g - a.g) * t,
        b: a.b + (b.b - a.b) * t,
        a: 1.,
    }
}

fn from_omarchy(text: &str) -> Option<Theme> {
    let colors: Colors = toml::from_str(text).ok()?;
    let background = parse_hex(&colors.background)?;
    let text = parse_hex(&colors.foreground)?;
    let accent = parse_hex(&colors.accent)?;
    let pick = |a: &Option<String>, b: &Option<String>, fallback: Rgba| {
        a.as_deref()
            .or(b.as_deref())
            .and_then(parse_hex)
            .unwrap_or(fallback)
    };
    let base = Theme::base();
    let luminance = 0.2126 * background.r + 0.7152 * background.g + 0.0722 * background.b;
    let dark = colors
        .mode
        .as_deref()
        .map_or(luminance < 0.5, |m| m != "light");
    let surface = colors
        .lighter_background
        .as_deref()
        .and_then(parse_hex)
        .filter(|s| s != &background)
        .unwrap_or_else(|| mix(background, text, 0.06));
    Some(Theme {
        dark,
        background,
        surface,
        border: mix(background, text, 0.18),
        text,
        muted: pick(
            &colors.muted,
            &colors.dark_foreground,
            mix(background, text, 0.55),
        ),
        accent,
        selection: pick(
            &colors.selection,
            &colors.color8,
            mix(background, accent, 0.25),
        ),
        red: pick(&colors.red, &colors.color1, base.red),
        green: pick(&colors.green, &colors.color2, base.green),
        yellow: pick(&colors.yellow, &colors.color3, base.yellow),
        blue: pick(&colors.blue, &colors.color4, base.blue),
        magenta: pick(&colors.magenta, &colors.color5, base.magenta),
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_omarchy_theme_gives_its_colours_and_fills_the_rest() {
        let theme = from_omarchy(
            "background = \"#08090a\"\nforeground = \"#d0d6e0\"\naccent = \"#ff8226\"\ncolor1 = \"#eb5757\"\n",
        )
        .unwrap();
        assert!(theme.dark);
        assert_eq!(hex(theme.accent), "#ff8226");
        assert_eq!(hex(theme.red), "#eb5757");
        assert_eq!(theme.text_hex(), "#d0d6e0");
    }

    #[test]
    fn a_light_theme_is_light_and_a_broken_one_is_refused() {
        let light = from_omarchy(
            "background = \"#fafafa\"\nforeground = \"#222222\"\naccent = \"#0055ff\"\n",
        )
        .unwrap();
        assert!(!light.dark);
        assert!(from_omarchy("background = \"#fafafa\"\n").is_none());
        assert!(
            from_omarchy("background = \"blue\"\nforeground = \"#222222\"\naccent = \"#0055ff\"\n")
                .is_none()
        );
    }
}

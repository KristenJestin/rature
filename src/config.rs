//! `~/.config/rature/config.toml`: every key is optional.
//!
//! ```toml
//! tree = "left"        # or "right": where a folder's tree stands
//! tree_width = 260
//! column = 760         # the reading column's width
//! font = "Geist"       # the text read; Geist and Geist Mono are bundled
//! code_font = "Geist Mono"
//! font_size = 16
//! ```

use std::path::PathBuf;

use gpui_design_system::typography;
use gpui_kit::Global;
use serde::Deserialize;

#[derive(Clone, Deserialize)]
#[serde(default)]
pub struct Config {
    pub tree: Side,
    pub tree_width: f32,
    pub column: f32,
    pub font: String,
    pub code_font: String,
    pub font_size: f32,
}

#[derive(Clone, Copy, PartialEq, Eq, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Side {
    Left,
    Right,
}

impl Default for Config {
    fn default() -> Self {
        Self {
            tree: Side::Left,
            tree_width: 260.,
            column: 760.,
            // Both registered with the text system by `gpui_design_system::init`.
            font: typography::UI.into(),
            code_font: typography::MONO.into(),
            font_size: 16.,
        }
    }
}

impl Global for Config {}

impl Config {
    /// The file's, or the defaults with the reason on the standard error when it does not read.
    pub fn load() -> Self {
        let path = folder().join("config.toml");
        match std::fs::read_to_string(&path) {
            Ok(text) => Self::parse(&text).unwrap_or_else(|error| {
                eprintln!("rature: {}: {error}", path.display());
                Self::default()
            }),
            Err(_) => Self::default(),
        }
    }

    /// A file's text, its text size at least 1 px: a canvas's zoom divides by it.
    pub(crate) fn parse(text: &str) -> Result<Self, toml::de::Error> {
        let mut config: Self = toml::from_str(text)?;
        config.font_size = config.font_size.max(1.);
        Ok(config)
    }
}

fn folder() -> PathBuf {
    std::env::var_os("XDG_CONFIG_HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| home().join(".config"))
        .join("rature")
}

pub fn home() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_else(|| "/".into())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_partial_file_keeps_the_other_defaults() {
        let config: Config = toml::from_str("tree = \"right\"\nfont_size = 18\n").unwrap();
        assert!(config.tree == Side::Right);
        assert_eq!(config.font_size, 18.);
        assert_eq!(config.column, 760.);
        assert_eq!(config.font, "Geist");
    }

    #[test]
    fn a_text_size_of_nothing_is_one_pixel() {
        assert_eq!(Config::parse("font_size = 0\n").unwrap().font_size, 1.);
        assert_eq!(Config::parse("font_size = -4\n").unwrap().font_size, 1.);
        assert_eq!(Config::parse("font_size = nan\n").unwrap().font_size, 1.);
        assert_eq!(Config::parse("font_size = 15\n").unwrap().font_size, 15.);
    }

    #[test]
    fn the_fonts_can_be_overridden() {
        let config: Config = toml::from_str("font = \"Noto Sans\"\n").unwrap();
        assert_eq!(config.font, "Noto Sans");
        assert_eq!(config.code_font, "Geist Mono");
    }
}

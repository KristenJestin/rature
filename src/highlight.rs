//! Code colouring, by syntect with bat's languages (two-face). The languages load once, off the
//! main thread, the first time a code block asks for them.

use std::ops::Range;
use std::sync::{Arc, OnceLock};

use gpui_kit::{Rgba, rgba};
use syntect::easy::HighlightLines;
use syntect::highlighting::{FontStyle, Theme, ThemeSet};
use syntect::parsing::{SyntaxReference, SyntaxSet};

pub struct Span {
    pub range: Range<usize>,
    pub color: Rgba,
    pub bold: bool,
    pub italic: bool,
}

/// Per line of the code, its coloured spans.
pub type Lines = Arc<Vec<Vec<Span>>>;

struct Kit {
    syntaxes: SyntaxSet,
    dark: Theme,
    light: Theme,
}

fn kit() -> &'static Kit {
    static KIT: OnceLock<Kit> = OnceLock::new();
    KIT.get_or_init(|| {
        let mut themes = ThemeSet::load_defaults().themes;
        Kit {
            syntaxes: two_face::syntax::extra_newlines(),
            dark: themes.remove("base16-ocean.dark").expect("a default theme"),
            light: themes.remove("InspiredGitHub").expect("a default theme"),
        }
    })
}

fn syntax<'a>(kit: &'a Kit, lang: &str) -> Option<&'a SyntaxReference> {
    let lang = lang.trim();
    if lang.is_empty() {
        return None;
    }
    kit.syntaxes
        .find_syntax_by_token(lang)
        .or_else(|| kit.syntaxes.find_syntax_by_extension(&lang.to_lowercase()))
}

/// The colours of `text` read as `lang` (a fence's language or a file's extension); none when
/// the language is not known. Slow the first time: run it off the main thread.
pub fn highlight(lang: &str, text: &str, dark: bool) -> Option<Lines> {
    let kit = kit();
    let syntax = syntax(kit, lang)?;
    let mut lines = HighlightLines::new(syntax, if dark { &kit.dark } else { &kit.light });
    let mut out = Vec::new();
    for line in syntect::util::LinesWithEndings::from(text) {
        let mut spans = Vec::new();
        let mut at = 0;
        for (style, piece) in lines.highlight_line(line, &kit.syntaxes).ok()? {
            let end = at + piece.trim_end_matches('\n').len();
            if end > at {
                let c = style.foreground;
                spans.push(Span {
                    range: at..end,
                    color: rgba(u32::from_be_bytes([c.r, c.g, c.b, 0xff])),
                    bold: style.font_style.contains(FontStyle::BOLD),
                    italic: style.font_style.contains(FontStyle::ITALIC),
                });
            }
            at += piece.len();
        }
        out.push(spans);
    }
    Some(Arc::new(out))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn rust_is_coloured_and_an_unknown_language_is_not() {
        let lines = highlight("rust", "fn main() {}\nlet x = 1;\n", true).unwrap();
        assert_eq!(lines.len(), 2);
        assert!(lines[0].len() > 1);
        assert!(highlight("no-such-language", "x", true).is_none());
        assert!(highlight("toml", "a = 1\n", true).is_some());
    }
}

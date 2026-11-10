//! Code colouring, by syntect with bat's languages (two-face). The languages load once, off the
//! main thread, the first time a code block asks for them.

use std::cell::RefCell;
use std::ops::Range;
use std::sync::{Arc, OnceLock};

use gpui_kit::base::input::{
    EditorState, FoldRange, HighlightStyleResolver, InputEdit, InputHighlighter, Rope, RopeExt,
};
use gpui_kit::{
    Context, FontStyle as Slant, FontWeight, HighlightStyle, Rgba, SharedString, Window, rgba,
};
use syntect::easy::HighlightLines;
use syntect::highlighting::{
    FontStyle, HighlightIterator, HighlightState, Highlighter as Painter, Style, Theme, ThemeSet,
};
use syntect::parsing::{ParseState, ScopeStack, SyntaxReference, SyntaxSet};

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

fn span(style: Style, range: Range<usize>) -> Span {
    let c = style.foreground;
    Span {
        range,
        color: rgba(u32::from_be_bytes([c.r, c.g, c.b, 0xff])),
        bold: style.font_style.contains(FontStyle::BOLD),
        italic: style.font_style.contains(FontStyle::ITALIC),
    }
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
                spans.push(span(style, at..end));
            }
            at += piece.len();
        }
        out.push(spans);
    }
    Some(Arc::new(out))
}

/// Colours a code editor's text as code read is coloured. Lines are coloured from the top as far
/// as they are shown, and coloured again only from the first one an edit touches.
pub struct Highlighter {
    lang: String,
    painter: Option<(&'static SyntaxReference, Painter<'static>)>,
    text: Rope,
    done: RefCell<Vec<Line>>,
}

/// A line coloured: where it starts, its spans, and the state the next line starts in.
struct Line {
    start: usize,
    spans: Vec<Span>,
    after: (ParseState, HighlightState),
}

impl Highlighter {
    pub fn new(lang: &str, dark: bool) -> Self {
        let kit = kit();
        let painter = syntax(kit, lang).map(|syntax| {
            (
                syntax,
                Painter::new(if dark { &kit.dark } else { &kit.light }),
            )
        });
        Self {
            lang: lang.to_string(),
            painter,
            text: Rope::new(),
            done: RefCell::new(Vec::new()),
        }
    }

    /// Colours the lines up to `last`, from where the colouring stopped.
    fn colour_to(&self, last: usize) {
        let Some((syntax, painter)) = &self.painter else {
            return;
        };
        let mut done = self.done.borrow_mut();
        let last = last.min(self.text.lines_len().saturating_sub(1));
        while done.len() <= last {
            let ix = done.len();
            let (mut parse, mut paint) = match done.last() {
                Some(line) => line.after.clone(),
                None => (
                    ParseState::new(syntax),
                    HighlightState::new(painter, ScopeStack::new()),
                ),
            };
            // With its line break: the languages read lines that end in one.
            let text = format!("{}\n", self.text.slice_line(ix));
            let ops = parse.parse_line(&text, &kit().syntaxes).unwrap_or_default();
            let mut spans = Vec::new();
            let mut at = 0;
            for (style, piece) in HighlightIterator::new(&mut paint, &ops, &text, painter) {
                let end = at + piece.trim_end_matches('\n').len();
                if end > at {
                    spans.push(span(style, at..end));
                }
                at += piece.len();
            }
            done.push(Line {
                start: self.text.line_start_offset(ix),
                spans,
                after: (parse, paint),
            });
        }
    }
}

impl InputHighlighter for Highlighter {
    fn language(&self) -> SharedString {
        self.lang.clone().into()
    }

    fn update(
        &mut self,
        edit: Option<InputEdit>,
        text: &Rope,
        _: bool,
        _: &mut Window,
        _: &mut Context<EditorState>,
    ) {
        let kept = edit.map_or(0, |edit| {
            self.text
                .offset_to_point(edit.start_byte.min(self.text.len()))
                .row
        });
        self.done.get_mut().truncate(kept);
        self.text = text.clone();
    }

    fn styles(
        &self,
        range: &Range<usize>,
        _: &dyn HighlightStyleResolver,
    ) -> Vec<(Range<usize>, HighlightStyle)> {
        let end = range.end.min(self.text.len());
        self.colour_to(self.text.offset_to_point(end).row);
        let done = self.done.borrow();
        let mut out = Vec::new();
        let mut at = range.start;
        let first = done
            .partition_point(|line| line.start <= range.start)
            .saturating_sub(1);
        'lines: for line in &done[first.min(done.len())..] {
            for span in &line.spans {
                let (from, to) = (line.start + span.range.start, line.start + span.range.end);
                if from >= range.end {
                    break 'lines;
                }
                let (from, to) = (from.max(at), to.min(range.end));
                if from >= to {
                    continue;
                }
                if from > at {
                    out.push((at..from, HighlightStyle::default()));
                }
                let style = HighlightStyle {
                    color: Some(span.color.into()),
                    font_weight: span.bold.then_some(FontWeight::BOLD),
                    font_style: span.italic.then_some(Slant::Italic),
                    ..Default::default()
                };
                out.push((from..to, style));
                at = to;
            }
        }
        if at < range.end {
            out.push((at..range.end, HighlightStyle::default()));
        }
        out
    }

    fn fold_ranges(&self, _: &Rope) -> Vec<FoldRange> {
        Vec::new()
    }
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

    struct NoStyles;

    impl HighlightStyleResolver for NoStyles {
        fn style(&self, _: &str) -> Option<HighlightStyle> {
            None
        }
    }

    #[test]
    fn an_editor_range_is_covered_whole_in_order() {
        let mut highlighter = Highlighter::new("rust", true);
        highlighter.text = Rope::from("fn main() {}\nlet x = 1;\n");
        let runs = highlighter.styles(&(3..20), &NoStyles);
        assert_eq!(runs.first().unwrap().0.start, 3);
        assert_eq!(runs.last().unwrap().0.end, 20);
        assert!(runs.windows(2).all(|w| w[0].0.end == w[1].0.start));
        assert!(runs.iter().any(|(_, style)| style.color.is_some()));
        // Only the lines asked for are coloured.
        assert_eq!(highlighter.done.borrow().len(), 2);
        let plain = Highlighter::new("no-such-language", true);
        assert_eq!(plain.styles(&(0..0), &NoStyles).len(), 0);
    }
}

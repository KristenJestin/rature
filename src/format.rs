//! Formatting on the user's own save (`Ctrl-s`), never on a change made from outside.

use std::path::Path;

/// `text` formatted as its file's kind says: Markdown by dprint, JSON pretty-printed. Unchanged
/// for any other kind, or when the text does not parse.
pub fn format(path: &Path, text: &str) -> String {
    let ext = path
        .extension()
        .map(|e| e.to_string_lossy().to_lowercase())
        .unwrap_or_default();
    let formatted = match ext.as_str() {
        "md" | "markdown" | "mdown" => markdown(text),
        "json" => serde_json::from_str::<serde_json::Value>(text)
            .ok()
            .and_then(|value| serde_json::to_string_pretty(&value).ok())
            .map(|pretty| pretty + "\n"),
        _ => None,
    };
    formatted.unwrap_or_else(|| text.to_string())
}

fn markdown(text: &str) -> Option<String> {
    use dprint_plugin_markdown::configuration::{ConfigurationBuilder, TextWrap};
    let config = ConfigurationBuilder::new()
        .text_wrap(TextWrap::Maintain)
        .build();
    match dprint_plugin_markdown::format_text(text, &config, |_, _, _| Ok(None)) {
        Ok(Some(formatted)) => Some(formatted),
        Ok(None) => Some(text.to_string()),
        Err(_) => None,
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn markdown_and_json_are_formatted_and_the_rest_left_alone() {
        let md = format(Path::new("a.md"), "#  Title\n* one\n* two\n");
        assert_eq!(md, "# Title\n\n- one\n- two\n");
        assert_eq!(
            format(Path::new("a.json"), "{\"a\":1}"),
            "{\n  \"a\": 1\n}\n"
        );
        assert_eq!(format(Path::new("a.json"), "{bad"), "{bad");
        assert_eq!(format(Path::new("a.rs"), "fn  x(){}"), "fn  x(){}");
    }
}

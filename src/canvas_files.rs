//! The files a canvas's file cards show. A card's path is found from the canvas's folder
//! ([`crate::canvas::resolve`]); its file is read off the main thread, a Markdown file parsed
//! with its links checked from its own folder, and cut to the heading the card's `subpath`
//! names. The board keeps what was read by path, and reads a file again when the watch on its
//! folder says it changed.

use std::io::ErrorKind;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use crate::document::{Document, Heading};
use crate::formats::{self, Content, Loaded};

/// What a file card shows of its file.
#[derive(Clone)]
pub enum Preview {
    /// Not there: the card is broken.
    Missing,
    /// A folder, not a file: broken too.
    Folder,
    /// A Markdown file, parsed.
    Markdown(Arc<Document>),
    /// Any other file: its icon and its name.
    Other,
    /// A Markdown file that is not shown, and why (too large, not text, not readable).
    Unread(String),
}

impl Preview {
    /// Whether the card leads nowhere: drawn in red, as a broken link is.
    pub fn is_broken(&self) -> bool {
        matches!(self, Self::Missing | Self::Folder)
    }
}

/// Reads the file at `path` as a file card shows it, its formulas drawn in `ink`. Only a Markdown
/// file is read: of any other, its name says enough. Reads and parses: call it off the main
/// thread.
pub fn read(path: &Path, ink: &str) -> Preview {
    let gone = |kind: ErrorKind| matches!(kind, ErrorKind::NotFound | ErrorKind::NotADirectory);
    let meta = match std::fs::metadata(path) {
        Ok(meta) => meta,
        Err(error) if gone(error.kind()) => return Preview::Missing,
        Err(error) => return Preview::Unread(format!("It cannot be read: {error}.")),
    };
    if meta.is_dir() {
        return Preview::Folder;
    }
    if !formats::is_markdown(path) {
        return Preview::Other;
    }
    match formats::load(path, ink) {
        Ok(Loaded {
            content: Content::Markdown(doc),
            ..
        }) => Preview::Markdown(doc),
        Ok(Loaded {
            content: Content::Message(why),
            ..
        }) => Preview::Unread(why),
        Ok(_) => Preview::Other,
        // Removed between the two looks.
        Err(error) if gone(error.kind()) => Preview::Missing,
        Err(error) => Preview::Unread(format!("It cannot be read: {error}.")),
    }
}

/// The part of a Markdown file a file card shows.
#[derive(Clone, Debug, PartialEq, Eq)]
pub struct Section {
    /// The top-level blocks shown.
    pub blocks: Range<usize>,
    /// The anchor of the heading it starts at: where the file opens.
    pub anchor: Option<String>,
    /// The subpath names a heading the file does not have: all of it is shown.
    pub missing: bool,
}

/// What a card whose `subpath` is this shows of `doc`. `#Heading` shows that heading up to the
/// next one of its level or above, the deeper ones it holds included; `#A#B`, the heading `B`
/// within `A`'s. A heading is named by its text, by its text in another case, or by its anchor.
/// Obsidian's block, `#^id`, shows the whole file for now; so does a heading the file does not
/// have, said `missing`.
pub fn section(doc: &Document, subpath: Option<&str>) -> Section {
    let whole = Section {
        blocks: 0..doc.blocks.len(),
        anchor: None,
        missing: false,
    };
    let Some(subpath) = subpath.map(str::trim).filter(|s| !s.is_empty()) else {
        return whole;
    };
    let subpath = subpath.strip_prefix('#').unwrap_or(subpath);
    if subpath.starts_with('^') {
        return whole;
    }
    let mut blocks = whole.blocks.clone();
    let mut start = None;
    let names = subpath
        .split('#')
        .map(str::trim)
        .filter(|name| !name.is_empty() && !name.starts_with('^'));
    for name in names {
        let Some(found) = heading(doc, &blocks, name) else {
            return Section {
                missing: true,
                ..whole
            };
        };
        let end = doc
            .headings
            .iter()
            .find(|h| h.block > found.block && h.block < blocks.end && h.level <= found.level)
            .map_or(blocks.end, |h| h.block);
        blocks = found.block..end;
        start = Some(found.block);
    }
    let anchor = start.and_then(|block| {
        doc.anchors
            .iter()
            .find(|(anchor, at)| **at == block && !anchor.starts_with("fn-"))
            .map(|(anchor, _)| anchor.clone())
    });
    Section {
        blocks,
        anchor,
        missing: false,
    }
}

/// The first heading among `blocks` that `name` names: by its text, then in any case, then by
/// its anchor.
fn heading<'a>(doc: &'a Document, blocks: &Range<usize>, name: &str) -> Option<&'a Heading> {
    let inside = || doc.headings.iter().filter(|h| blocks.contains(&h.block));
    let lower = name.to_lowercase();
    inside()
        .find(|h| h.text.trim() == name)
        .or_else(|| inside().find(|h| h.text.trim().to_lowercase() == lower))
        .or_else(|| {
            let block = *doc.anchors.get(name)?;
            inside().find(|h| h.block == block)
        })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::document;
    use std::path::PathBuf;

    const INK: &str = "#000000";

    fn doc(source: &str) -> Document {
        document::parse(source, INK, None)
    }

    /// Blocks: 0 `# Garden`, 1 intro, 2 `## Beds`, 3 text, 4 `### Raised`, 5 text, 6 `## Tools`,
    /// 7 text, 8 `# Notes`, 9 text.
    const GARDEN: &str = "# Garden\n\nIntro.\n\n## Beds\n\nFour beds.\n\n### Raised\n\nTwo of them.\n\n## Tools\n\nA spade.\n\n# Notes\n\nLater.\n";

    #[test]
    fn a_heading_shows_up_to_the_next_of_its_level() {
        let doc = doc(GARDEN);
        let tools = section(&doc, Some("#Tools"));
        assert_eq!(tools.blocks, 6..8);
        assert_eq!(tools.anchor.as_deref(), Some("tools"));
        assert!(!tools.missing);
        // The last heading of its level goes up to the next one above it.
        assert_eq!(section(&doc, Some("#Garden")).blocks, 0..8);
        assert_eq!(section(&doc, Some("#Notes")).blocks, 8..10);
    }

    #[test]
    fn a_heading_followed_by_a_deeper_one_holds_it() {
        let doc = doc(GARDEN);
        let beds = section(&doc, Some("#Beds"));
        assert_eq!(beds.blocks, 2..6, "Raised and its text are in Beds");
        assert_eq!(beds.anchor.as_deref(), Some("beds"));
        // Named within another, in another case, or by its anchor.
        assert_eq!(section(&doc, Some("#Garden#Raised")).blocks, 4..6);
        assert_eq!(section(&doc, Some("#raised")).blocks, 4..6);
        let by_anchor = doc_with_punctuation();
        assert_eq!(section(&by_anchor, Some("#whats-next")).blocks, 1..3);
    }

    fn doc_with_punctuation() -> Document {
        doc("Before.\n\n## What's next?\n\nSoon.\n\n## Done\n")
    }

    #[test]
    fn a_block_shows_the_whole_file() {
        let doc = doc(GARDEN);
        let block = section(&doc, Some("#^a1b2c3"));
        assert_eq!(block.blocks, 0..10);
        assert_eq!(block.anchor, None);
        assert!(!block.missing);
        // No subpath, or an empty one: the whole file.
        assert_eq!(section(&doc, None).blocks, 0..10);
        assert_eq!(section(&doc, Some("")).blocks, 0..10);
    }

    #[test]
    fn a_heading_that_is_not_there_shows_the_whole_file_and_says_so() {
        let doc = doc(GARDEN);
        let nope = section(&doc, Some("#Orchard"));
        assert_eq!(nope.blocks, 0..10);
        assert_eq!(nope.anchor, None);
        assert!(nope.missing);
        // Within a heading that does not hold it.
        assert!(section(&doc, Some("#Notes#Raised")).missing);
    }

    fn folder(name: &str) -> PathBuf {
        let folder =
            std::env::temp_dir().join(format!("rature-canvas-files-{name}-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        folder
    }

    #[test]
    fn a_file_is_read_as_its_card_shows_it() {
        let root = folder("read");
        std::fs::create_dir_all(root.join("notes")).unwrap();
        std::fs::write(
            root.join("notes/plan.md"),
            "# Plan\n\nSee [beds](beds.md).\n",
        )
        .unwrap();
        std::fs::write(root.join("notes/beds.md"), "# Beds\n").unwrap();
        std::fs::write(root.join("data.json"), "{}").unwrap();
        let canvas_folder = root.join("boards");
        // Found from the canvas's folder, up and down again.
        let plan = crate::canvas::resolve(&canvas_folder, "../notes/plan.md");
        let Preview::Markdown(doc) = read(&plan, INK) else {
            panic!("not read as Markdown")
        };
        // Its links checked from its own folder, not the canvas's.
        let document::Block::Paragraph(inlines) = &doc.blocks[1] else {
            panic!("not a paragraph")
        };
        assert!(inlines.iter().any(|inline| matches!(
            inline,
            document::Inline::Text(_, marks) if marks.link.is_some() && !marks.broken
        )));
        let missing = crate::canvas::resolve(&canvas_folder, "plan.md");
        assert!(read(&missing, INK).is_broken());
        assert!(matches!(read(&missing, INK), Preview::Missing));
        assert!(matches!(read(&root.join("notes"), INK), Preview::Folder));
        assert!(read(&root.join("notes"), INK).is_broken());
        assert!(matches!(read(&root.join("data.json"), INK), Preview::Other));
        // Through a file, as if it were a folder: not there either.
        let through = root.join("data.json/x.md");
        assert!(matches!(read(&through, INK), Preview::Missing));
        std::fs::remove_dir_all(root).unwrap();
    }
}

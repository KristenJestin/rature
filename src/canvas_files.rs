//! The files a canvas's file cards show. A card's path is found from the canvas's folder
//! ([`crate::canvas::resolve`]); its file is read off the main thread, a Markdown file parsed
//! with its links checked from its own folder, and cut to the heading the card's `subpath`
//! names; of an image (png, jpeg, svg), its size, read from its header: GPUI decodes it as the
//! reader's images are. The board keeps what was read by path, and reads a file again when the
//! watch on its folder says it changed.

use std::io::{self, Cursor, ErrorKind, Read as _};
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use image::ImageDecoder as _;
use image::metadata::Orientation;

use crate::document::{Document, Heading};
use crate::formats::{self, Content, Loaded};
use crate::fsio;

/// What a file card shows of its file.
#[derive(Clone)]
pub enum Preview {
    /// Not there: the card is broken.
    Missing,
    /// A folder, not a file: broken too.
    Folder,
    /// A Markdown file, parsed.
    Markdown(Arc<Document>),
    /// An image: drawn in the card, its aspect kept.
    Image(Picture),
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

/// An image's size, in its own pixels, and whether it is an SVG, drawn again at each size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Picture {
    pub width: f32,
    pub height: f32,
    pub svg: bool,
}

/// Reads the file at `path` as a file card shows it, its formulas drawn in `ink`. Only a Markdown
/// file is read, and an image's header: of any other, its name says enough. Reads and parses:
/// call it off the main thread.
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
    if formats::is_card_image(path) {
        // A pipe would hold the read waiting.
        if let Some(what) = fsio::not_a_file(&meta) {
            return Preview::Unread(format!("It is {what}: only regular files are read here."));
        }
        return match picture(path) {
            Ok(picture) => Preview::Image(picture),
            Err(error) if gone(error.kind()) => Preview::Missing,
            Err(error) => Preview::Unread(format!("It cannot be read as an image: {error}.")),
        };
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

/// The most pixels a png or a jpeg a card shows has across or down: GPUI decodes the whole image,
/// four bytes a pixel, and one of 8192 by 8192 already takes 256 MiB.
pub const MOST_PIXELS: u32 = 8192;

/// How much of a png or a jpeg is read for its size: its header and the metadata before it. The
/// jpeg decoder would otherwise read the whole file.
const HEADER: u64 = 1024 * 1024;

fn invalid(error: String) -> io::Error {
    io::Error::new(ErrorKind::InvalidData, error)
}

/// The size of the image at `path`, read from its header and turned as its EXIF orientation
/// turns it, as GPUI draws it; an SVG's from its `width`, `height` and `viewBox`, parsed whole
/// but never drawn ([`svg`]). A png or a jpeg larger than [`MOST_PIXELS`] a side is refused.
pub fn picture(path: &Path) -> io::Result<Picture> {
    if formats::is_svg(path) {
        let (_, width, height) = svg(path)?;
        return Ok(Picture {
            width,
            height,
            svg: true,
        });
    }
    let mut head = Vec::new();
    std::fs::File::open(path)?
        .take(HEADER)
        .read_to_end(&mut head)?;
    let image_error = |error: image::ImageError| match error {
        image::ImageError::IoError(error) => error,
        other => invalid(other.to_string()),
    };
    let mut decoder = image::ImageReader::new(Cursor::new(head))
        .with_guessed_format()?
        .into_decoder()
        .map_err(image_error)?;
    let (width, height) = decoder.dimensions();
    if width == 0 || height == 0 {
        return Err(invalid("it has no size".into()));
    }
    if width > MOST_PIXELS || height > MOST_PIXELS {
        return Err(invalid(format!(
            "it is {width} by {height} pixels, larger than {MOST_PIXELS} a side"
        )));
    }
    let (width, height) = turned(width, height, decoder.orientation().map_err(image_error)?);
    Ok(Picture {
        width: width as f32,
        height: height as f32,
        svg: false,
    })
}

/// The size of an image `width` by `height` once its EXIF `orientation` is applied: a quarter
/// turn swaps its sides.
fn turned(width: u32, height: u32, orientation: Orientation) -> (u32, u32) {
    match orientation {
        Orientation::Rotate90
        | Orientation::Rotate270
        | Orientation::Rotate90FlipH
        | Orientation::Rotate270FlipH => (height, width),
        _ => (width, height),
    }
}

/// The SVG at `path`, read and checked: a regular file of at most [`formats::LARGEST`] bytes,
/// parsed without reading anything it names, with its size. One that names an image outside
/// itself (`<image href="other.png">`, not a `data:` one) is refused: GPUI's parser, which draws
/// it, would read that file, whatever it is (`/dev/zero`). Reads and parses: call it off the
/// main thread.
pub fn svg(path: &Path) -> io::Result<(Vec<u8>, f32, f32)> {
    fsio::regular(path)?;
    let mut bytes = Vec::new();
    std::fs::File::open(path)?
        .take(formats::LARGEST + 1)
        .read_to_end(&mut bytes)?;
    if bytes.len() as u64 > formats::LARGEST {
        return Err(invalid("it is too large".into()));
    }
    let (width, height) = svg_size(&bytes)?;
    Ok((bytes, width, height))
}

/// The size of the SVG `bytes`, parsed without reading any file it names; refused when it names
/// one.
fn svg_size(bytes: &[u8]) -> io::Result<(f32, f32)> {
    let named = Arc::new(AtomicBool::new(false));
    let mut options = usvg::Options::default();
    options.image_href_resolver.resolve_string = Box::new({
        let named = named.clone();
        move |_, _| {
            named.store(true, Ordering::Relaxed);
            None
        }
    });
    let tree =
        usvg::Tree::from_data(bytes, &options).map_err(|error| invalid(error.to_string()))?;
    if named.load(Ordering::Relaxed) {
        return Err(invalid(
            "it names an image outside itself, which is not read".into(),
        ));
    }
    let size = tree.size();
    Ok((size.width(), size.height()))
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

    #[test]
    fn an_image_is_read_as_its_size() {
        let root = folder("images");
        // The brand's mark, 512 by 512.
        let mark = Path::new(env!("CARGO_MANIFEST_DIR")).join("assets/brand/png/mark-512.png");
        std::fs::copy(mark, root.join("mark.PNG")).unwrap();
        let image = |name: &str| match read(&root.join(name), INK) {
            Preview::Image(picture) => picture,
            _ => panic!("{name} not read as an image"),
        };
        assert_eq!(
            image("mark.PNG"),
            Picture {
                width: 512.,
                height: 512.,
                svg: false
            }
        );
        std::fs::write(
            root.join("wide.svg"),
            r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 300 100"><rect width="300" height="100"/></svg>"#,
        )
        .unwrap();
        assert_eq!(
            image("wide.svg"),
            Picture {
                width: 300.,
                height: 100.,
                svg: true
            }
        );
        // Not an image after all, or not there: said, or broken.
        std::fs::write(root.join("fake.png"), "not a picture").unwrap();
        assert!(matches!(
            read(&root.join("fake.png"), INK),
            Preview::Unread(_)
        ));
        std::fs::write(root.join("fake.svg"), "<svg").unwrap();
        assert!(matches!(
            read(&root.join("fake.svg"), INK),
            Preview::Unread(_)
        ));
        assert!(matches!(
            read(&root.join("gone.jpg"), INK),
            Preview::Missing
        ));
        // A pipe is not read: it would hold the read waiting.
        let pipe = root.join("pipe.png");
        let made = std::process::Command::new("mkfifo")
            .arg(&pipe)
            .status()
            .unwrap();
        assert!(made.success());
        assert!(matches!(read(&pipe, INK), Preview::Unread(_)));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// What `read` says of a file it does not show as an image.
    fn unread(path: &Path) -> String {
        match read(path, INK) {
            Preview::Unread(why) => why,
            _ => panic!("{} read", path.display()),
        }
    }

    #[test]
    fn an_svg_that_names_a_file_is_refused_without_reading_it() {
        let root = folder("svg-href");
        // Read, this one would never end.
        let svg = r#"<svg xmlns="http://www.w3.org/2000/svg" xmlns:xlink="http://www.w3.org/1999/xlink" viewBox="0 0 10 10"><image href="/dev/zero" width="10" height="10"/><image xlink:href="/dev/zero" width="10" height="10"/></svg>"#;
        std::fs::write(root.join("zero.svg"), svg).unwrap();
        let why = unread(&root.join("zero.svg"));
        assert!(why.contains("names an image outside itself"), "{why}");
        // An image of its own, in a `data:` address, is no file.
        let own = r#"<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 10 10"><image href="data:image/png;base64,AAAA" width="10" height="10"/></svg>"#;
        std::fs::write(root.join("own.svg"), own).unwrap();
        assert!(matches!(
            read(&root.join("own.svg"), INK),
            Preview::Image(Picture { svg: true, .. })
        ));
        std::fs::remove_dir_all(root).unwrap();
    }

    /// A jpeg `width` by `height`, its EXIF orientation `orientation` (1 to 8).
    fn jpeg(width: u32, height: u32, orientation: u8) -> Vec<u8> {
        let mut encoded = Vec::new();
        let pixels = vec![128u8; (width * height * 3) as usize];
        image::codecs::jpeg::JpegEncoder::new(&mut encoded)
            .encode(&pixels, width, height, image::ExtendedColorType::Rgb8)
            .unwrap();
        // APP1: `Exif`, a little-endian TIFF header, one IFD of one entry, Orientation (0x0112),
        // a SHORT.
        let mut app1 = vec![0xFF, 0xE1, 0x00, 0x22];
        app1.extend_from_slice(b"Exif\0\0II*\0\x08\0\0\0\x01\0\x12\x01\x03\0\x01\0\0\0");
        app1.extend_from_slice(&[orientation, 0, 0, 0, 0, 0, 0, 0]);
        assert_eq!(app1.len(), 2 + 0x22);
        // Right after the start of the image.
        encoded.splice(2..2, app1);
        encoded
    }

    #[test]
    fn a_jpeg_turned_by_its_exif_orientation_is_read_turned() {
        let root = folder("exif");
        let size = |orientation: u8| {
            let path = root.join(format!("turned-{orientation}.jpg"));
            std::fs::write(&path, jpeg(4, 2, orientation)).unwrap();
            match read(&path, INK) {
                Preview::Image(picture) => (picture.width, picture.height),
                _ => panic!("orientation {orientation} not read as an image"),
            }
        };
        // A quarter turn (6: 90°, 8: 270°, 5 and 7 flipped too) swaps the sides.
        for orientation in [5, 6, 7, 8] {
            assert_eq!(size(orientation), (2., 4.), "{orientation}");
        }
        for orientation in [1, 2, 3, 4] {
            assert_eq!(size(orientation), (4., 2.), "{orientation}");
        }
        std::fs::remove_dir_all(root).unwrap();
    }

    #[test]
    fn an_image_larger_than_8192_a_side_is_refused_from_its_header() {
        use image::ImageEncoder as _;
        let root = folder("large");
        let png = |width: u32, height: u32| {
            let mut encoded = Vec::new();
            let pixels = vec![0u8; (width * height) as usize];
            image::codecs::png::PngEncoder::new(&mut encoded)
                .write_image(&pixels, width, height, image::ExtendedColorType::L8)
                .unwrap();
            encoded
        };
        std::fs::write(root.join("wide.png"), png(MOST_PIXELS + 1, 1)).unwrap();
        std::fs::write(root.join("tall.png"), png(1, MOST_PIXELS + 1)).unwrap();
        std::fs::write(root.join("most.png"), png(MOST_PIXELS, 1)).unwrap();
        let why = unread(&root.join("wide.png"));
        assert!(why.contains("8193 by 1 pixels"), "{why}");
        unread(&root.join("tall.png"));
        assert!(matches!(
            read(&root.join("most.png"), INK),
            Preview::Image(Picture { width: 8192., .. })
        ));
        std::fs::remove_dir_all(root).unwrap();
    }
}

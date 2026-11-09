use std::collections::HashMap;
use std::ops::Range;
use std::path::Path;
use std::sync::Arc;

use markdown::mdast::Node;

use crate::math;

/// A picture drawn from source (a diagram, a formula): its SVG decoded by GPUI, and the size it
/// takes on screen. The SVG is drawn at `diagram::SCALE` times that size, so it stays sharp on a
/// scaled screen.
#[derive(Clone)]
pub struct Picture {
    pub image: Arc<gpui_kit::Image>,
    pub width: f32,
    pub height: f32,
    /// How far below its bottom edge the baseline of a formula sits: 0 for anything else.
    pub depth: f32,
}

pub enum Block {
    Heading(u8, Vec<Inline>),
    Paragraph(Vec<Inline>),
    Code {
        lang: Option<String>,
        text: String,
    },
    Picture(Picture),
    /// A Mermaid diagram, by its source: drawn off the main thread, once per source.
    Diagram(String),
    /// Source that could not be drawn, shown as code with the reason.
    Broken {
        source: String,
        error: String,
    },
    List {
        ordered: bool,
        start: u32,
        items: Vec<Item>,
    },
    Quote(Vec<Block>),
    /// A GitHub alert, `> [!NOTE]` and its kin.
    Alert(Alert, Vec<Block>),
    Table(Vec<Vec<Vec<Inline>>>),
    /// The YAML at the top of the file, as its keys and values when it is that flat.
    FrontMatter(Vec<(String, String)>),
    Footnote {
        label: String,
        blocks: Vec<Block>,
    },
    Image {
        url: String,
        alt: String,
    },
    Rule,
}

#[derive(Clone, Copy, PartialEq, Eq)]
pub enum Alert {
    Note,
    Tip,
    Important,
    Warning,
    Caution,
}

pub struct Item {
    pub checked: Option<bool>,
    pub blocks: Vec<Block>,
}

#[derive(Clone)]
pub enum Inline {
    Text(String, Marks),
    Math(Picture),
}

#[derive(Clone, Default)]
pub struct Marks {
    pub strong: bool,
    pub emphasis: bool,
    pub code: bool,
    pub strike: bool,
    pub sup: bool,
    /// Where the text leads, as written.
    pub link: Option<Arc<str>>,
    /// A relative link to a file or an anchor that is not there.
    pub broken: bool,
}

pub struct Heading {
    pub level: u8,
    pub text: String,
    pub slug: String,
    pub block: usize,
}

/// A Markdown file read: its blocks, and for each top-level block where it is in the source.
pub struct Document {
    pub source: String,
    pub blocks: Vec<Block>,
    /// Byte range of each top-level block in `source`.
    pub spans: Vec<Range<usize>>,
    /// First line (from 1) of each top-level block.
    pub lines: Vec<usize>,
    /// Plain text of each top-level block, lowercased: what a search looks into.
    pub texts: Vec<String>,
    pub headings: Vec<Heading>,
    /// Heading slugs and footnote anchors (`fn-<label>`), to the top-level block they are in.
    pub anchors: HashMap<String, usize>,
}

impl Document {
    pub fn source_of(&self, block: usize) -> &str {
        &self.source[self.spans[block].clone()]
    }

    /// The top-level block at line `line`, or the last before it.
    pub fn block_at_line(&self, line: usize) -> usize {
        self.lines
            .partition_point(|&start| start <= line)
            .saturating_sub(1)
    }
}

/// Reads `source`; formulas are drawn in `ink`. Relative links are checked against `folder`.
pub fn parse(source: &str, ink: &str, folder: Option<&Path>) -> Document {
    let mut options = markdown::ParseOptions::gfm();
    options.constructs.math_flow = true;
    options.constructs.math_text = true;
    options.constructs.frontmatter = true;
    let root = markdown::to_mdast(source, &options).expect("Markdown without MDX never fails");
    let nodes = root.children().map(Vec::as_slice).unwrap_or_default();
    let reader = Reader { ink };
    let mut doc = Document {
        source: source.to_string(),
        blocks: Vec::new(),
        spans: Vec::new(),
        lines: Vec::new(),
        texts: Vec::new(),
        headings: Vec::new(),
        anchors: HashMap::new(),
    };
    let mut slugs = Slugs::default();
    for node in nodes {
        let Some(block) = reader.block(node) else {
            continue;
        };
        let index = doc.blocks.len();
        if let Block::Heading(level, inlines) = &block {
            let text = plain_inlines(inlines);
            let slug = slugs.next(&text);
            doc.anchors.insert(slug.clone(), index);
            doc.headings.push(Heading {
                level: *level,
                text,
                slug,
                block: index,
            });
        }
        if let Block::Footnote { label, .. } = &block {
            doc.anchors.insert(format!("fn-{label}"), index);
        }
        let position = node.position().expect("parsed nodes have a position");
        doc.spans.push(position.start.offset..position.end.offset);
        doc.lines.push(position.start.line);
        doc.texts.push(plain(&block).to_lowercase());
        doc.blocks.push(block);
    }
    if let Some(folder) = folder {
        let anchors: Vec<String> = doc.anchors.keys().cloned().collect();
        for block in &mut doc.blocks {
            check_links(block, folder, &anchors);
        }
    }
    doc
}

struct Reader<'a> {
    ink: &'a str,
}

impl Reader<'_> {
    fn blocks(&self, nodes: &[Node]) -> Vec<Block> {
        nodes.iter().filter_map(|node| self.block(node)).collect()
    }

    fn block(&self, node: &Node) -> Option<Block> {
        Some(match node {
            Node::Heading(h) => {
                Block::Heading(h.depth, self.inlines(&h.children, &Marks::default()))
            }
            Node::Paragraph(p) => match p.children.as_slice() {
                [Node::Image(image)] => Block::Image {
                    url: image.url.clone(),
                    alt: image.alt.clone(),
                },
                children => Block::Paragraph(self.inlines(children, &Marks::default())),
            },
            Node::Code(c) if c.lang.as_deref() == Some("mermaid") => {
                Block::Diagram(c.value.clone())
            }
            Node::Code(c) => Block::Code {
                lang: c.lang.clone(),
                text: c.value.clone(),
            },
            Node::Math(m) => match math::render(&m.value, true, self.ink) {
                Ok(picture) => Block::Picture(picture),
                Err(error) => Block::Broken {
                    source: m.value.clone(),
                    error,
                },
            },
            Node::List(l) => Block::List {
                ordered: l.ordered,
                start: l.start.unwrap_or(1),
                items: l
                    .children
                    .iter()
                    .filter_map(|child| match child {
                        Node::ListItem(item) => Some(Item {
                            checked: item.checked,
                            blocks: self.blocks(&item.children),
                        }),
                        _ => None,
                    })
                    .collect(),
            },
            Node::Blockquote(q) => match alert(&q.children) {
                Some((kind, rest)) => Block::Alert(kind, self.blocks(&rest)),
                None => Block::Quote(self.blocks(&q.children)),
            },
            Node::Table(t) => Block::Table(
                t.children
                    .iter()
                    .map(|row| {
                        row.children()
                            .into_iter()
                            .flatten()
                            .map(|cell| {
                                self.inlines(
                                    cell.children().map(Vec::as_slice).unwrap_or_default(),
                                    &Marks::default(),
                                )
                            })
                            .collect()
                    })
                    .collect(),
            ),
            Node::Yaml(y) => Block::FrontMatter(front_matter(&y.value)),
            Node::FootnoteDefinition(f) => Block::Footnote {
                label: f.label.clone().unwrap_or_else(|| f.identifier.clone()),
                blocks: self.blocks(&f.children),
            },
            Node::Html(h) => Block::Code {
                lang: Some("html".into()),
                text: h.value.clone(),
            },
            Node::ThematicBreak(_) => Block::Rule,
            _ => return None,
        })
    }

    fn inlines(&self, nodes: &[Node], marks: &Marks) -> Vec<Inline> {
        let mut out = Vec::new();
        for node in nodes {
            let with = |change: fn(&mut Marks)| {
                let mut marks = marks.clone();
                change(&mut marks);
                marks
            };
            match node {
                Node::Text(t) => out.push(Inline::Text(t.value.clone(), marks.clone())),
                Node::InlineCode(c) => {
                    out.push(Inline::Text(c.value.clone(), with(|m| m.code = true)))
                }
                Node::Strong(s) => {
                    out.extend(self.inlines(&s.children, &with(|m| m.strong = true)))
                }
                Node::Emphasis(e) => {
                    out.extend(self.inlines(&e.children, &with(|m| m.emphasis = true)))
                }
                Node::Delete(d) => {
                    out.extend(self.inlines(&d.children, &with(|m| m.strike = true)))
                }
                Node::Link(l) => {
                    let mut linked = marks.clone();
                    linked.link = Some(l.url.as_str().into());
                    out.extend(self.inlines(&l.children, &linked));
                }
                Node::Image(i) => {
                    let mut linked = marks.clone();
                    linked.link = Some(i.url.as_str().into());
                    let alt = if i.alt.is_empty() { "image" } else { &i.alt };
                    out.push(Inline::Text(format!("[{alt}]"), linked));
                }
                Node::FootnoteReference(f) => {
                    let label = f.label.clone().unwrap_or_else(|| f.identifier.clone());
                    let mut linked = marks.clone();
                    linked.sup = true;
                    linked.link = Some(format!("#fn-{label}").into());
                    out.push(Inline::Text(label, linked));
                }
                Node::Break(_) => out.push(Inline::Text("\n".into(), marks.clone())),
                Node::InlineMath(m) => match math::render(&m.value, false, self.ink) {
                    Ok(picture) => out.push(Inline::Math(picture)),
                    Err(_) => out.push(Inline::Text(m.value.clone(), with(|m| m.code = true))),
                },
                Node::Html(h) => out.push(Inline::Text(h.value.clone(), with(|m| m.code = true))),
                _ => {}
            }
        }
        out
    }
}

/// `> [!NOTE]` and its kin: the kind, and the quote without its marker.
fn alert(children: &[Node]) -> Option<(Alert, Vec<Node>)> {
    let Some(Node::Paragraph(first)) = children.first() else {
        return None;
    };
    let Some(Node::Text(text)) = first.children.first() else {
        return None;
    };
    let rest = text.value.strip_prefix("[!")?;
    let (name, after) = rest.split_once(']')?;
    let kind = match name.to_ascii_uppercase().as_str() {
        "NOTE" => Alert::Note,
        "TIP" => Alert::Tip,
        "IMPORTANT" => Alert::Important,
        "WARNING" => Alert::Warning,
        "CAUTION" => Alert::Caution,
        _ => return None,
    };
    let mut first = first.clone();
    let after = after.trim_start_matches([' ', '\n']);
    if after.is_empty() {
        first.children.remove(0);
        // The break that followed the marker.
        if matches!(first.children.first(), Some(Node::Break(_))) {
            first.children.remove(0);
        }
    } else if let Some(Node::Text(text)) = first.children.first_mut() {
        text.value = after.to_string();
    }
    let mut rest: Vec<Node> = Vec::new();
    if !first.children.is_empty() {
        rest.push(Node::Paragraph(first));
    }
    rest.extend(children[1..].iter().cloned());
    Some((kind, rest))
}

fn front_matter(yaml: &str) -> Vec<(String, String)> {
    use yaml_rust2::{Yaml, YamlLoader};
    fn scalar(value: &Yaml) -> Option<String> {
        match value {
            Yaml::String(s) | Yaml::Real(s) => Some(s.clone()),
            Yaml::Integer(i) => Some(i.to_string()),
            Yaml::Boolean(b) => Some(b.to_string()),
            Yaml::Array(items) => items
                .iter()
                .map(|item| match item {
                    Yaml::Array(_) | Yaml::Hash(_) => None,
                    item => scalar(item),
                })
                .collect::<Option<Vec<_>>>()
                .map(|parts| parts.join(", ")),
            _ => None,
        }
    }
    match YamlLoader::load_from_str(yaml)
        .ok()
        .and_then(|docs| docs.into_iter().next())
    {
        Some(Yaml::Hash(hash)) => hash
            .iter()
            .map(|(key, value)| {
                let key = scalar(key).unwrap_or_default();
                let value = scalar(value).unwrap_or_else(|| "…".into());
                (key, value)
            })
            .collect(),
        _ => vec![(String::new(), yaml.to_string())],
    }
}

/// Marks the relative links that lead nowhere: a file that is not there, or an anchor of this
/// file that no heading or footnote has.
fn check_links(block: &mut Block, folder: &Path, anchors: &[String]) {
    let check = |inlines: &mut Vec<Inline>| {
        for inline in inlines {
            if let Inline::Text(_, marks) = inline
                && let Some(link) = &marks.link
            {
                marks.broken = is_broken(link, folder, anchors);
            }
        }
    };
    match block {
        Block::Heading(_, inlines) | Block::Paragraph(inlines) => check(inlines),
        Block::Table(rows) => rows.iter_mut().flatten().for_each(check),
        Block::Quote(blocks) | Block::Alert(_, blocks) | Block::Footnote { blocks, .. } => blocks
            .iter_mut()
            .for_each(|b| check_links(b, folder, anchors)),
        Block::List { items, .. } => items
            .iter_mut()
            .flat_map(|item| item.blocks.iter_mut())
            .for_each(|b| check_links(b, folder, anchors)),
        _ => {}
    }
}

fn is_broken(link: &str, folder: &Path, anchors: &[String]) -> bool {
    if link.contains("://") || link.starts_with("mailto:") {
        return false;
    }
    let (file, anchor) = link.split_once('#').unwrap_or((link, ""));
    if file.is_empty() {
        return !anchor.is_empty() && !anchors.iter().any(|a| a == anchor);
    }
    !folder.join(decode(file)).exists()
}

/// `%20` and its kin, as a link to a file writes them.
pub fn decode(path: &str) -> String {
    let bytes = path.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        let hex = (bytes[i] == b'%' && i + 2 < bytes.len())
            .then(|| std::str::from_utf8(&bytes[i + 1..i + 3]).ok())
            .flatten()
            .and_then(|hex| u8::from_str_radix(hex, 16).ok());
        match hex {
            Some(byte) => {
                out.push(byte);
                i += 3;
            }
            None => {
                out.push(bytes[i]);
                i += 1;
            }
        }
    }
    String::from_utf8_lossy(&out).into_owned()
}

/// GitHub's anchors: lowercase, punctuation dropped, spaces as dashes, a repeat numbered.
#[derive(Default)]
struct Slugs(HashMap<String, usize>);

impl Slugs {
    fn next(&mut self, text: &str) -> String {
        let base: String = text
            .trim()
            .to_lowercase()
            .chars()
            .filter_map(|c| match c {
                ' ' => Some('-'),
                c if c.is_alphanumeric() || c == '-' || c == '_' => Some(c),
                _ => None,
            })
            .collect();
        let seen = self.0.entry(base.clone()).or_insert(0);
        let slug = if *seen == 0 {
            base
        } else {
            format!("{base}-{seen}")
        };
        *seen += 1;
        slug
    }
}

pub fn plain_inlines(inlines: &[Inline]) -> String {
    inlines
        .iter()
        .map(|inline| match inline {
            Inline::Text(text, _) => text.as_str(),
            Inline::Math(_) => "",
        })
        .collect()
}

/// A block's text, without its marks: what a search reads.
pub fn plain(block: &Block) -> String {
    let join = |parts: Vec<String>| parts.join("\n");
    match block {
        Block::Heading(_, inlines) | Block::Paragraph(inlines) => plain_inlines(inlines),
        Block::Code { text, .. } => text.clone(),
        Block::Diagram(source) | Block::Broken { source, .. } => source.clone(),
        Block::List { items, .. } => join(
            items
                .iter()
                .flat_map(|item| item.blocks.iter().map(plain))
                .collect(),
        ),
        Block::Quote(blocks) | Block::Alert(_, blocks) | Block::Footnote { blocks, .. } => {
            join(blocks.iter().map(plain).collect())
        }
        Block::Table(rows) => join(
            rows.iter()
                .map(|row| {
                    row.iter()
                        .map(|cell| plain_inlines(cell))
                        .collect::<Vec<_>>()
                        .join(" ")
                })
                .collect(),
        ),
        Block::FrontMatter(pairs) => join(pairs.iter().map(|(k, v)| format!("{k} {v}")).collect()),
        Block::Image { alt, .. } => alt.clone(),
        Block::Picture(_) | Block::Rule => String::new(),
    }
}

/// The sources of the diagrams in `blocks`, at any depth.
pub fn diagrams(blocks: &[Block], out: &mut Vec<String>) {
    for block in blocks {
        match block {
            Block::Diagram(source) => out.push(source.clone()),
            Block::Quote(inner)
            | Block::Alert(_, inner)
            | Block::Footnote { blocks: inner, .. } => diagrams(inner, out),
            Block::List { items, .. } => items.iter().for_each(|item| diagrams(&item.blocks, out)),
            _ => {}
        }
    }
}

/// The code blocks in `blocks`, at any depth, as (language, text).
pub fn code_blocks(blocks: &[Block], out: &mut Vec<(String, String)>) {
    for block in blocks {
        match block {
            Block::Code { lang, text } => {
                out.push((lang.clone().unwrap_or_default(), text.clone()))
            }
            Block::Quote(inner)
            | Block::Alert(_, inner)
            | Block::Footnote { blocks: inner, .. } => code_blocks(inner, out),
            Block::List { items, .. } => {
                items.iter().for_each(|item| code_blocks(&item.blocks, out))
            }
            _ => {}
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../examples/sample.md");
    const INK: &str = "#c0caf5";

    fn count(doc: &Document, f: fn(&Block) -> bool) -> usize {
        doc.blocks.iter().filter(|b| f(b)).count()
    }

    #[test]
    fn a_display_formula_becomes_a_picture_and_a_mermaid_block_a_diagram() {
        let doc = parse(SAMPLE, INK, None);
        assert_eq!(count(&doc, |b| matches!(b, Block::Broken { .. })), 0);
        assert_eq!(count(&doc, |b| matches!(b, Block::Picture(_))), 1);
        assert_eq!(count(&doc, |b| matches!(b, Block::Diagram(_))), 1);
    }

    #[test]
    fn inline_math_sits_in_its_paragraph() {
        let doc = parse(SAMPLE, INK, None);
        let inline = doc
            .blocks
            .iter()
            .filter_map(|b| match b {
                Block::Paragraph(inlines) => Some(inlines),
                _ => None,
            })
            .flatten()
            .filter(|i| matches!(i, Inline::Math(_)))
            .count();
        assert_eq!(inline, 2);
    }

    #[test]
    fn a_diagram_is_drawn_larger_than_it_shows() {
        let picture = crate::diagram::render(
            "flowchart LR\n A --> B",
            &crate::diagram::Colors::of(&crate::ui::Palette::base()),
        )
        .unwrap();
        let svg = std::str::from_utf8(picture.image.bytes()).unwrap();
        let drawn = format!("width=\"{}\"", picture.width * crate::diagram::SCALE);
        assert!(
            svg[..svg.find('>').unwrap()].contains(&drawn),
            "{}",
            &svg[..200]
        );
    }

    #[test]
    fn headings_get_github_anchors_and_each_block_its_source() {
        let doc = parse("# One two!\n\ntext\n\n# One two!\n", INK, None);
        let slugs: Vec<&str> = doc.headings.iter().map(|h| h.slug.as_str()).collect();
        assert_eq!(slugs, ["one-two", "one-two-1"]);
        assert_eq!(doc.source_of(1), "text");
        assert_eq!(doc.lines, [1, 3, 5]);
        assert_eq!(doc.block_at_line(4), 1);
    }

    #[test]
    fn an_alert_loses_its_marker_and_keeps_its_text() {
        let doc = parse("> [!WARNING]\n> Mind the gap.\n", INK, None);
        let Block::Alert(Alert::Warning, blocks) = &doc.blocks[0] else {
            panic!("not an alert")
        };
        assert_eq!(plain(&blocks[0]).trim(), "Mind the gap.");
    }

    #[test]
    fn front_matter_reads_as_keys_and_values() {
        let doc = parse(
            "---\ntitle: Tomatoes\ntags: [garden, summer]\n---\n\n# T\n",
            INK,
            None,
        );
        let Block::FrontMatter(pairs) = &doc.blocks[0] else {
            panic!("no front matter")
        };
        assert_eq!(pairs[0], ("title".into(), "Tomatoes".into()));
        assert_eq!(pairs[1], ("tags".into(), "garden, summer".into()));
    }

    #[test]
    fn links_to_missing_files_and_anchors_are_broken() {
        let folder = std::env::temp_dir().join(format!("rature-links-{}", std::process::id()));
        std::fs::create_dir_all(&folder).unwrap();
        std::fs::write(folder.join("there.md"), "# There\n").unwrap();
        let doc = parse(
            "# Here\n\n[a](there.md) [b](gone.md) [c](#here) [d](#nowhere) [e](https://x.org)\n",
            INK,
            Some(&folder),
        );
        let Block::Paragraph(inlines) = &doc.blocks[1] else {
            panic!("not a paragraph")
        };
        let broken: Vec<bool> = inlines
            .iter()
            .filter_map(|i| match i {
                Inline::Text(_, m) if m.link.is_some() => Some(m.broken),
                _ => None,
            })
            .collect();
        assert_eq!(broken, [false, true, false, true, false]);
        std::fs::remove_dir_all(folder).unwrap();
    }
}

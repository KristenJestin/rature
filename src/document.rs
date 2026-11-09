use markdown::mdast::Node;
use std::sync::Arc;

use crate::math;

/// A picture drawn from source (a diagram, a formula): its SVG decoded by GPUI, and the size it
/// takes on screen. The SVG is drawn at `diagram::SCALE` times that size, so it stays sharp on a
/// scaled screen.
#[derive(Clone)]
pub struct Picture {
    pub image: Arc<gpui_kit::Image>,
    pub width: f32,
    pub height: f32,
}

pub enum Block {
    Heading(u8, Vec<Inline>),
    Paragraph(Vec<Inline>),
    Code(String),
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
        items: Vec<Item>,
    },
    Quote(Vec<Block>),
    Table(Vec<Vec<Vec<Inline>>>),
    Rule,
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

#[derive(Clone, Copy, Default)]
pub struct Marks {
    pub strong: bool,
    pub emphasis: bool,
    pub code: bool,
    pub strike: bool,
    pub link: bool,
}

/// The sources of the diagrams in `blocks`, at any depth.
pub fn diagrams(blocks: &[Block], out: &mut Vec<String>) {
    for block in blocks {
        match block {
            Block::Diagram(source) => out.push(source.clone()),
            Block::Quote(inner) => diagrams(inner, out),
            Block::List { items, .. } => items.iter().for_each(|item| diagrams(&item.blocks, out)),
            _ => {}
        }
    }
}

pub fn parse(source: &str) -> Vec<Block> {
    let mut options = markdown::ParseOptions::gfm();
    options.constructs.math_flow = true;
    options.constructs.math_text = true;
    let root = markdown::to_mdast(source, &options).expect("Markdown without MDX never fails");
    blocks(root.children().map(Vec::as_slice).unwrap_or_default())
}

fn blocks(nodes: &[Node]) -> Vec<Block> {
    nodes.iter().filter_map(block).collect()
}

fn block(node: &Node) -> Option<Block> {
    Some(match node {
        Node::Heading(h) => Block::Heading(h.depth, inlines(&h.children, Marks::default())),
        Node::Paragraph(p) => Block::Paragraph(inlines(&p.children, Marks::default())),
        Node::Code(c) if c.lang.as_deref() == Some("mermaid") => Block::Diagram(c.value.clone()),
        Node::Code(c) => Block::Code(c.value.clone()),
        Node::Math(m) => match math::render(&m.value, true) {
            Ok(picture) => Block::Picture(picture),
            Err(error) => Block::Broken {
                source: m.value.clone(),
                error,
            },
        },
        Node::List(l) => Block::List {
            ordered: l.ordered,
            items: l
                .children
                .iter()
                .filter_map(|child| match child {
                    Node::ListItem(item) => Some(Item {
                        checked: item.checked,
                        blocks: blocks(&item.children),
                    }),
                    _ => None,
                })
                .collect(),
        },
        Node::Blockquote(q) => Block::Quote(blocks(&q.children)),
        Node::Table(t) => Block::Table(
            t.children
                .iter()
                .map(|row| {
                    row.children()
                        .into_iter()
                        .flatten()
                        .map(|cell| {
                            inlines(
                                cell.children().map(Vec::as_slice).unwrap_or_default(),
                                Marks::default(),
                            )
                        })
                        .collect()
                })
                .collect(),
        ),
        Node::ThematicBreak(_) => Block::Rule,
        _ => return None,
    })
}

fn inlines(nodes: &[Node], marks: Marks) -> Vec<Inline> {
    let mut out = Vec::new();
    for node in nodes {
        match node {
            Node::Text(t) => out.push(Inline::Text(t.value.clone(), marks)),
            Node::InlineCode(c) => out.push(Inline::Text(
                c.value.clone(),
                Marks {
                    code: true,
                    ..marks
                },
            )),
            Node::Strong(s) => out.extend(inlines(
                &s.children,
                Marks {
                    strong: true,
                    ..marks
                },
            )),
            Node::Emphasis(e) => out.extend(inlines(
                &e.children,
                Marks {
                    emphasis: true,
                    ..marks
                },
            )),
            Node::Delete(d) => out.extend(inlines(
                &d.children,
                Marks {
                    strike: true,
                    ..marks
                },
            )),
            Node::Link(l) => out.extend(inlines(
                &l.children,
                Marks {
                    link: true,
                    ..marks
                },
            )),
            Node::Break(_) => out.push(Inline::Text("\n".into(), marks)),
            Node::InlineMath(m) => match math::render(&m.value, false) {
                Ok(picture) => out.push(Inline::Math(picture)),
                Err(_) => out.push(Inline::Text(
                    m.value.clone(),
                    Marks {
                        code: true,
                        ..marks
                    },
                )),
            },
            _ => {}
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = include_str!("../examples/sample.md");

    #[test]
    fn a_display_formula_becomes_a_picture_and_a_mermaid_block_a_diagram() {
        let blocks = parse(SAMPLE);
        let count = |f: fn(&Block) -> bool| blocks.iter().filter(|b| f(b)).count();
        assert_eq!(count(|b| matches!(b, Block::Broken { .. })), 0);
        assert_eq!(count(|b| matches!(b, Block::Picture(_))), 1);
        assert_eq!(count(|b| matches!(b, Block::Diagram(_))), 1);
    }

    #[test]
    fn inline_math_sits_in_its_paragraph() {
        let blocks = parse(SAMPLE);
        let inline = blocks
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
        let picture = crate::diagram::render("flowchart LR\n A --> B").unwrap();
        let svg = std::str::from_utf8(picture.image.bytes()).unwrap();
        let drawn = format!("width=\"{}\"", picture.width * crate::diagram::SCALE);
        assert!(
            svg[..svg.find('>').unwrap()].contains(&drawn),
            "{}",
            &svg[..200]
        );
    }
}

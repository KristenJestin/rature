//! What changed in a canvas since it was last read (or since its last commit), as the board
//! shows it. Worked out off the main thread from [`model::compare`]: each text card whose text
//! changed with the reader's marks by block and by word, where each card was before it moved,
//! what was deleted, and where each change is, which `]c` and `[c` frame. Drawn on the board:
//! a card added outlined in green, one changed in yellow, the place a card left as a dashed
//! outline, a card or an edge deleted as its ghost in red, an edge's stroke in the colour of its
//! change; a card that arrives while the canvas is shown rises into place.
//!
//! What Rature writes itself is marked read at once ([`mark_written`]): what stays marked is
//! what others wrote.

use std::collections::HashMap;
use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use gpui_design_system::Palette;
use gpui_design_system::motion::{ENTER, RISE, ease_out_cubic};
use gpui_kit::*;

// Not `self`: `canvas` is GPUI's element, painted here.
use crate::canvas::{self as model, Canvas, Edge, EdgeChange, End, Node, NodeChange, NodeKind};
use crate::canvas_edges::{self as edges, Line};
use crate::canvas_ops::{Op, Report, Target};
use crate::canvas_view::{self, Area, Camera, CanvasFile, Card, CardKind};
use crate::diff::{self, Mark};
use crate::document::{self, Block, Document};
use crate::load::{Entrances, fingerprint};
use crate::render::{self, Ctx, Host};
use crate::state;

/// A deleted edge's line, in the canvas's pixels, as the board draws edges.
const EDGE_WIDTH: f32 = 2.;

/// How a canvas differs from the one it is compared to, ready to draw.
#[derive(Default)]
pub struct BoardChanges {
    /// The cards new or changed, by id.
    cards: HashMap<String, Marked>,
    edges: HashMap<String, EdgeChange>,
    /// Where cards were before they moved or were resized.
    places: Vec<Area>,
    /// The cards deleted, where they were.
    gone: Vec<Gone>,
    /// The edges deleted, where they were.
    gone_edges: Vec<Line>,
    /// Where each change is, top to bottom then left to right: what `]c` and `[c` frame.
    stops: Vec<Area>,
}

struct Marked {
    change: NodeChange,
    /// A text card whose text changed: its Markdown now (the card's own), as it was, and the
    /// reader's marks between the two.
    words: Option<Words>,
}

struct Words {
    now: Arc<Document>,
    before: Arc<Document>,
    changes: diff::Changes,
}

/// A card deleted: where it was, its title, and a text card's Markdown.
struct Gone {
    area: Area,
    title: String,
    doc: Option<Arc<Document>>,
}

impl BoardChanges {
    /// How `file`, as read now, differs from `before`; the Markdown of a text card as it was
    /// parsed as `file`'s are, its formulas in `ink`, its links checked from `folder`. Slow on
    /// a large canvas much changed: off the main thread.
    pub fn new(before: &Canvas, file: &CanvasFile, ink: &str, folder: Option<&Path>) -> Self {
        let now = &*file.canvas;
        let changes = model::compare(before, now);
        let docs: HashMap<&str, &Arc<Document>> = file
            .cards
            .iter()
            .filter_map(|card| match &card.kind {
                CardKind::Text(doc) if !card.id.is_empty() => Some((card.id.as_str(), doc)),
                _ => None,
            })
            .collect();
        let mut out = Self::default();
        for (id, &change) in &changes.nodes {
            let (Some(node), was) = (now.node(id), before.node(id)) else {
                continue;
            };
            let place = node.rect().map(area);
            let mut stop = place;
            let mut words = None;
            if let NodeChange::Changed { content, moved } = change
                && let Some(was) = was
            {
                let old = was.rect().map(area);
                if moved && let Some(old) = old {
                    out.places.push(old);
                    stop = Some(stop.map_or(old, |stop| stop.union(old)));
                }
                if content
                    && was.kind() == Some(NodeKind::Text)
                    && was.text() != node.text()
                    && let Some(doc) = docs.get(id.as_str())
                {
                    let old =
                        Arc::new(document::parse(was.text().unwrap_or_default(), ink, folder));
                    words = Some(Words {
                        changes: diff::compare(&old, doc),
                        now: Arc::clone(doc),
                        before: old,
                    });
                }
            }
            out.stops.extend(stop);
            out.cards.insert(id.clone(), Marked { change, words });
        }
        for id in &changes.deleted_nodes {
            let Some((node, Some(place))) = before.node(id).map(|n| (n, n.rect())) else {
                continue;
            };
            let place = area(place);
            out.stops.push(place);
            out.gone.push(gone(node, place, ink, folder));
        }
        // An edge that comes or goes with its card is part of that card's change: no stop of
        // its own.
        let with_card = |edge: &Edge, card: NodeChange| {
            [edge.source_node(), edge.target_node()]
                .into_iter()
                .flatten()
                .any(|end| changes.nodes.get(end) == Some(&card))
        };
        let rects_now = rects(now);
        for (id, &change) in &changes.edges {
            out.edges.insert(id.clone(), change);
            let Some(edge) = now.edges().iter().find(|e| e.id() == Some(id.as_str())) else {
                continue;
            };
            if !with_card(edge, NodeChange::Added) {
                out.stops.extend(ends(edge, &rects_now));
            }
        }
        let rects_before = rects(before);
        for id in &changes.deleted_edges {
            let Some(edge) = before.edges().iter().find(|e| e.id() == Some(id.as_str())) else {
                continue;
            };
            out.gone_edges.extend(line(edge, &rects_before));
            let with_deleted = [edge.source_node(), edge.target_node()]
                .into_iter()
                .flatten()
                .any(|end| changes.deleted_nodes.iter().any(|gone| gone == end));
            if !with_deleted {
                out.stops.extend(ends(edge, &rects_before));
            }
        }
        out.stops
            .sort_by(|a, b| a.y.total_cmp(&b.y).then(a.x.total_cmp(&b.x)));
        out
    }

    /// How many changes there are: the stops of `]c`.
    pub fn count(&self) -> usize {
        self.stops.len()
    }

    pub fn stops(&self) -> &[Area] {
        &self.stops
    }

    /// The border of the card `id`, as its change has it, and its background unless it keeps
    /// its own: green for a card added; yellow for one changed, its background its own when it
    /// shows the marks of its text, or when only its place or size changed.
    pub fn card_colors(&self, id: &str, p: &Palette) -> Option<(Hsla, Option<Hsla>)> {
        let marked = self.cards.get(id)?;
        Some(match marked.change {
            NodeChange::Added => (p.ok, Some(p.ok_bg)),
            NodeChange::Changed { content: true, .. } if marked.words.is_none() => {
                (p.wait, Some(p.wait_bg))
            }
            NodeChange::Changed { .. } => (p.wait, None),
        })
    }

    /// The stroke of the edge `id`, as its change has it.
    pub fn edge_color(&self, id: &str, p: &Palette) -> Option<Hsla> {
        Some(match self.edges.get(id)? {
            EdgeChange::Added => p.ok,
            EdgeChange::Changed => p.wait,
        })
    }

    /// The blocks of the text card `id`, its Markdown `doc`, with the reader's marks of what
    /// changed in its text, the blocks removed struck where they were; `None` when its text did
    /// not change, or when `doc` is not the Markdown it was compared as (being edited).
    pub fn blocks<H: Host>(
        &self,
        ctx: &Ctx<H>,
        id: &str,
        doc: &Arc<Document>,
    ) -> Option<Vec<AnyElement>> {
        let words = self.cards.get(id)?.words.as_ref()?;
        if !Arc::ptr_eq(&words.now, doc) {
            return None;
        }
        let t = ctx.theme;
        let removed = |at: usize| {
            words
                .changes
                .removed
                .iter()
                .filter(move |(before, _)| *before == at)
                .filter_map(|(_, gone)| words.before.blocks.get(*gone))
                .map(|gone| render::ghost(ctx, gone))
        };
        let mut out = Vec::new();
        for (ix, block) in doc.blocks.iter().enumerate() {
            out.extend(removed(ix));
            let mark = words.changes.marks.get(ix);
            let inner = match (block, mark) {
                (Block::Paragraph(_), Some(Mark::Changed(Some(changed)))) => {
                    render::changed_words(ctx, changed).into_any_element()
                }
                (Block::Heading(level, _), Some(Mark::Changed(Some(changed)))) => {
                    render::heading(ctx, *level, render::changed_words(ctx, changed))
                        .into_any_element()
                }
                _ => render::block(ctx, block),
            };
            out.push(match mark {
                Some(Mark::Added) => render::marked(ctx, t.ok, t.ok_bg, inner),
                Some(Mark::Changed(_)) => render::marked(ctx, t.wait, t.wait_bg, inner),
                _ => inner,
            });
        }
        out.extend(removed(doc.blocks.len()));
        Some(out)
    }

    /// What is no more where it was, in the canvas's area `seen`, to go below the cards: the
    /// places cards left, dashed; the edges and cards deleted, in red, a card's title struck, or
    /// its Markdown at the full detail (`ctx`).
    pub fn ghosts<H: Host>(
        &self,
        ctx: Option<&Ctx<H>>,
        camera: Camera,
        seen: &Area,
        p: &Palette,
    ) -> Vec<AnyElement> {
        let zoom = camera.zoom;
        let places: Vec<Area> = self
            .places
            .iter()
            .filter(|place| place.meets(seen))
            .map(|place| camera.on_screen(*place))
            .collect();
        let lines: Vec<Line> = self
            .gone_edges
            .iter()
            .filter(|line| area_of(line.bounds).meets(seen))
            .cloned()
            .collect();
        let mut out = Vec::new();
        if !places.is_empty() || !lines.is_empty() {
            let (wait, fail, clear) = (p.wait, p.fail, hsla(0., 0., 0., 0.));
            let layer = canvas(
                |_, _, _| (),
                move |bounds, (), window, _| {
                    let at = |point: edges::Point| {
                        let (x, y) = camera.to_screen(point.x, point.y);
                        bounds.origin + gpui_kit::point(px(x), px(y))
                    };
                    for line in &lines {
                        let mut stroke = PathBuilder::stroke(px((EDGE_WIDTH * zoom).max(1.)));
                        stroke.move_to(at(line.from));
                        stroke.cubic_bezier_to(at(line.to), at(line.c1), at(line.c2));
                        if let Ok(path) = stroke.build() {
                            window.paint_path(path, fail);
                        }
                        for arrow in line.from_arrow.iter().chain(&line.to_arrow) {
                            let mut fill = PathBuilder::fill();
                            fill.add_polygon(&arrow.map(at), true);
                            if let Ok(path) = fill.build() {
                                window.paint_path(path, fail);
                            }
                        }
                    }
                    for place in places {
                        window.paint_quad(quad(
                            Bounds::new(
                                bounds.origin + gpui_kit::point(px(place.x), px(place.y)),
                                size(px(place.width.max(1.)), px(place.height.max(1.))),
                            ),
                            px(8. * zoom),
                            clear,
                            px((2. * zoom).max(1.)),
                            wait,
                            BorderStyle::Dashed,
                        ));
                    }
                },
            )
            .absolute()
            .size_full();
            out.push(layer.into_any_element());
        }
        for (ix, gone) in self.gone.iter().enumerate() {
            if !gone.area.meets(seen) {
                continue;
            }
            let place = camera.on_screen(gone.area);
            let frame = div()
                .id(("gone", ix))
                .absolute()
                .left(px(place.x))
                .top(px(place.y))
                .w(px(place.width))
                .h(px(place.height))
                .overflow_hidden()
                .rounded(px(8. * zoom))
                .border(px((2. * zoom).max(1.)))
                .border_color(p.fail)
                .bg(p.fail_bg);
            let struck = div()
                .size_full()
                .opacity(0.55)
                .line_through()
                .text_color(p.text);
            let shown = match (ctx, &gone.doc) {
                (Some(ctx), Some(doc)) => frame.child(render::scaled(
                    ctx.zoom,
                    struck
                        .px_5()
                        .py_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_size(ctx.size(1.))
                        .line_height(ctx.line_height(render::LINE))
                        .children(doc.blocks.iter().map(|block| render::block(ctx, block))),
                )),
                (Some(ctx), None) => frame
                    .p(px(16. * zoom))
                    .child(struck.text_size(ctx.size(1.)).child(gone.title.clone())),
                (None, _) => frame.p(px(6.)).child(
                    struck
                        .text_size(px(13.))
                        .line_height(px(13. * 1.3))
                        .child(gone.title.clone()),
                ),
            };
            out.push(shown.into_any_element());
        }
        out
    }
}

/// A node's place and size as an area of the canvas.
fn area(rect: model::Rect) -> Area {
    Area {
        x: rect.x as f32,
        y: rect.y as f32,
        width: rect.width as f32,
        height: rect.height as f32,
    }
}

fn area_of(rect: edges::Rect) -> Area {
    Area {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

/// A deleted card as its ghost shows it.
fn gone(node: &Node, place: Area, ink: &str, folder: Option<&Path>) -> Gone {
    let named = |name: Option<&str>| name.unwrap_or_default().to_string();
    let (title, doc) = match node.kind() {
        Some(NodeKind::Text) => {
            let text = node.text().unwrap_or_default();
            let doc = document::parse(text, ink, folder);
            (canvas_view::title(&doc, text), Some(Arc::new(doc)))
        }
        Some(NodeKind::Group) => (named(node.label()), None),
        Some(NodeKind::File) => (named(node.file()), None),
        Some(NodeKind::Link) => (named(node.url()), None),
        Some(NodeKind::Other(kind)) => (kind.to_string(), None),
        None => (String::new(), None),
    };
    Gone {
        area: place,
        title,
        doc,
    }
}

/// The place and size of each node of `canvas` that has them, by id: the first of an id.
fn rects(canvas: &Canvas) -> HashMap<&str, model::Rect> {
    let mut rects = HashMap::new();
    for node in canvas.nodes() {
        if let (Some(id), Some(rect)) = (node.id(), node.rect()) {
            rects.entry(id).or_insert(rect);
        }
    }
    rects
}

/// The area holding the two cards an edge joins, those of them `rects` has.
fn ends(edge: &Edge, rects: &HashMap<&str, model::Rect>) -> Option<Area> {
    [edge.source_node(), edge.target_node()]
        .into_iter()
        .flatten()
        .filter_map(|end| rects.get(end).copied().map(area))
        .reduce(Area::union)
}

/// An edge's line between its two cards in `rects`, as the board lays it.
fn line(edge: &Edge, rects: &HashMap<&str, model::Rect>) -> Option<Line> {
    let rect = |end: Option<&str>| {
        let a = area(*rects.get(end?)?);
        Some(edges::Rect {
            x: a.x,
            y: a.y,
            width: a.width,
            height: a.height,
        })
    };
    Some(Line::new(
        rect(edge.source_node())?,
        edges::Side::of(edge.source_side()),
        rect(edge.target_node())?,
        edges::Side::of(edge.target_side()),
        edge.source_end() == End::Arrow,
        edge.target_end() == End::Arrow,
    ))
}

/// What the board keeps of the changes: those shown, the change `]c` last framed, and the
/// cards that rise into place.
pub struct OnBoard {
    changes: Option<Arc<BoardChanges>>,
    /// The change `]c` or `[c` last framed, by its place in the stops.
    at: Option<usize>,
    entrances: Entrances,
    /// The cards rising this frame, by id, and how far they rose (0 to 1).
    rising: HashMap<String, f32>,
}

impl OnBoard {
    /// None shown yet; the cards there now are simply there.
    pub fn new(now: Instant) -> Self {
        Self {
            changes: None,
            at: None,
            entrances: Entrances::quiet(now, ENTER),
            rising: HashMap::new(),
        }
    }

    pub fn set(&mut self, changes: Option<Arc<BoardChanges>>) {
        let count = changes.as_ref().map_or(0, |c| c.count());
        self.at = self.at.filter(|at| *at < count);
        self.changes = changes;
    }

    pub fn changes(&self) -> Option<&BoardChanges> {
        self.changes.as_deref()
    }

    /// The canvas was read again, from `before` to `after`: the cards it did not have rise.
    pub fn arrived(&mut self, before: &CanvasFile, after: &CanvasFile, now: Instant) {
        let prints = |file: &CanvasFile| -> Vec<u64> {
            file.cards
                .iter()
                .filter(|card| !card.id.is_empty())
                .map(|card| fingerprint(&card.id))
                .collect()
        };
        self.entrances.changed(&prints(before), &prints(after), now);
    }

    /// Which of `cards`, drawn now, rise; whether any does, and the board is drawn again.
    pub fn rise(&mut self, cards: &[&Card], now: Instant) -> bool {
        self.rising.clear();
        for card in cards.iter().filter(|card| !card.id.is_empty()) {
            if let Some(t) = self.entrances.progress(fingerprint(&card.id), now) {
                self.rising.insert(card.id.clone(), t);
            }
        }
        !self.rising.is_empty()
    }

    /// Where the card `id`, at `place` on screen, is drawn while it rises, and how opaque.
    pub fn risen(&self, id: &str, place: Area) -> (Area, f32) {
        match self.rising.get(id) {
            Some(&t) => {
                let shown = ease_out_cubic(t);
                let y = place.y + RISE * (1. - shown);
                (Area { y, ..place }, shown)
            }
            None => (place, 1.),
        }
    }

    /// The next change (or the one before), round the end: what the camera frames.
    pub fn step(&mut self, forward: bool) -> Option<Area> {
        let stops = self.changes.as_ref()?.stops();
        let count = stops.len();
        if count == 0 {
            return None;
        }
        let at = match (self.at, forward) {
            (None, true) => 0,
            (None, false) => count - 1,
            (Some(at), true) => (at + 1) % count,
            (Some(at), false) => (at + count - 1) % count,
        };
        self.at = Some(at);
        Some(stops[at])
    }
}

/// Rature's own operations, just written to the canvas at `path`: replayed on the canvas as it
/// was last read, so that what the user did there never shows as changed, while what others
/// wrote stays marked. A field they set takes Rature's value whatever the canvas read held.
/// Reads and writes the state: off the window's thread.
pub fn mark_written(path: &Path, ops: &[Op]) {
    let Some(read) = state::last_read(path) else {
        return;
    };
    let mut canvas = Canvas::read(&read);
    for op in ops {
        as_read(op, &mut canvas);
    }
    state::mark_read(path, &canvas.snapshot());
}

/// `op` done on the canvas as read: a field set takes its value there, whatever it held.
fn as_read(op: &Op, canvas: &mut Canvas) {
    let Op::Set { target, id, fields } = op else {
        return op.apply(canvas, &mut Report::default());
    };
    let entry = match target {
        Target::Node => canvas.node_mut(id).map(Node::fields_mut),
        Target::Edge => canvas
            .edges_mut()
            .iter_mut()
            .find(|edge| edge.id() == Some(id.as_str()))
            .map(Edge::fields_mut),
    };
    let Some(entry) = entry else {
        return;
    };
    for field in fields {
        match &field.value {
            Some(value) => {
                entry.insert(field.key.clone(), value.clone());
            }
            None => {
                entry.shift_remove(&field.key);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::{BoardChanges, OnBoard, as_read};
    use crate::canvas::{Canvas, NodeChange};
    use crate::canvas_ops::{Op, Report};
    use crate::canvas_view::CanvasFile;
    use crate::diff::Mark;
    use gpui_design_system::Palette;
    use std::time::Instant;

    const READ: &str = r##"{"nodes":[
        {"id":"a","type":"text","text":"# Title\n\nThe oven is high.","x":0,"y":0,"width":100,"height":50},
        {"id":"b","type":"text","text":"B","x":0,"y":300,"width":100,"height":50},
        {"id":"c","type":"text","text":"C","x":400,"y":0,"width":100,"height":50}
    ],"edges":[
        {"id":"bc","fromNode":"b","toNode":"c"}
    ]}"##;

    /// Card `a` reworded, `b` deleted with its edge, `c` moved, `d` added with an edge from `c`.
    const NOW: &str = r##"{"nodes":[
        {"id":"a","type":"text","text":"# Title\n\nThe oven is low.","x":0,"y":0,"width":100,"height":50},
        {"id":"c","type":"text","text":"C","x":420,"y":20,"width":100,"height":50},
        {"id":"d","type":"text","text":"D","x":0,"y":600,"width":100,"height":50}
    ],"edges":[
        {"id":"cd","fromNode":"c","toNode":"d"}
    ]}"##;

    fn board(before: &str, now: &str) -> BoardChanges {
        let file = CanvasFile::read(now, "#000000", None).unwrap();
        BoardChanges::new(&Canvas::read(before), &file, "#000000", None)
    }

    #[test]
    fn each_change_is_a_stop_top_to_bottom_an_edge_with_its_card_none() {
        let changes = board(READ, NOW);
        // a (y 0), c and where it was (y 0 to 70), b deleted (y 300), d added (y 600); the edges
        // come and go with d and b.
        let tops: Vec<(f32, f32)> = changes.stops().iter().map(|s| (s.y, s.x)).collect();
        assert_eq!(tops, [(0., 0.), (0., 400.), (300., 0.), (600., 0.)]);
        assert_eq!(changes.count(), 4);
        let c = changes.stops()[1];
        assert_eq!(
            (c.width, c.height),
            (120., 70.),
            "where it was and where it is"
        );
        assert_eq!(changes.places.len(), 1);
        assert_eq!(changes.gone.len(), 1);
        assert_eq!(changes.gone[0].title, "B");
        assert_eq!(changes.gone_edges.len(), 1);
        assert_eq!(changes.cards["d"].change, NodeChange::Added);
        // Nothing changed: nothing to stop at.
        assert_eq!(board(READ, READ).count(), 0);
        // An edge relabelled between two cards that stayed: a stop of its own.
        let relabelled = READ.replace(r#""toNode":"c"}"#, r#""toNode":"c","label":"x"}"#);
        assert_eq!(board(READ, &relabelled).count(), 1);
    }

    #[test]
    fn a_reworded_card_carries_the_readers_marks_by_word() {
        let changes = board(READ, NOW);
        let words = changes.cards["a"].words.as_ref().expect("its words");
        // The heading stays, the paragraph changed by a word.
        assert!(matches!(words.changes.marks[0], Mark::Same));
        let Mark::Changed(Some(changed)) = &words.changes.marks[1] else {
            panic!("not a word change")
        };
        assert!(changed.iter().any(|w| w.text.starts_with("low")));
        // Moved only: no words; its colours are a yellow border on its own background.
        assert!(changes.cards["c"].words.is_none());
        let p = Palette::own(gpui_design_system::theme::Shade::Dark);
        assert_eq!(changes.card_colors("c", &p), Some((p.wait, None)));
        assert_eq!(changes.card_colors("a", &p), Some((p.wait, None)));
        assert_eq!(changes.card_colors("d", &p), Some((p.ok, Some(p.ok_bg))));
        assert_eq!(changes.card_colors("x", &p), None);
        assert_eq!(changes.edge_color("cd", &p), Some(p.ok));
    }

    #[test]
    fn rature_s_own_writes_are_read_already() {
        // As read, then moved by the user (an operation made on what the board shows, which
        // another program changed meanwhile: `c` reworded).
        let read = Canvas::read(READ);
        let shown = Canvas::read(&READ.replace(r#""text":"C""#, r#""text":"C2""#));
        let moved = Op::move_node(&shown, "c", 450., 0.).unwrap();
        let deleted = Op::delete_node(&shown, "b").unwrap();
        let mut disk = shown.clone();
        for op in [&moved, &deleted] {
            op.apply(&mut disk, &mut Report::default());
        }
        let mut as_read_now = read.clone();
        for op in [&moved, &deleted] {
            as_read(op, &mut as_read_now);
        }
        // What the user did is read; the rewording another program did is still a change.
        let file = CanvasFile::read(&disk.snapshot(), "#000000", None).unwrap();
        let changes = BoardChanges::new(&as_read_now, &file, "#000000", None);
        assert_eq!(changes.count(), 1);
        assert_eq!(
            changes.cards["c"].change,
            NodeChange::Changed {
                content: true,
                moved: false
            }
        );
    }

    #[test]
    fn the_changes_are_gone_through_round_the_end() {
        let mut shown = OnBoard::new(Instant::now());
        assert_eq!(shown.step(true), None);
        shown.set(Some(std::sync::Arc::new(board(READ, NOW))));
        let ys: Vec<f32> = (0..5).map(|_| shown.step(true).unwrap().y).collect();
        assert_eq!(ys, [0., 0., 300., 600., 0.]);
        assert_eq!(shown.step(false).unwrap().y, 600.);
        // Fewer changes once read again: from the start.
        shown.set(Some(std::sync::Arc::new(board(READ, READ))));
        assert_eq!(shown.step(true), None);
    }
}

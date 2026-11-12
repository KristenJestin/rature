//! A canvas shown as a board: its cards where the canvas puts them, seen through a camera that
//! pans and zooms. Only the cards in sight are built, at a detail that follows the zoom: the whole
//! Markdown close up, a coloured box and its title further out, rectangles from afar. The reader
//! keeps the file, its watcher, its bar, back and forward; the board draws it.
//!
//! Cards are selected, moved, resized, nudged and deleted on it, each change an operation of
//! [`crate::canvas_ops`] done on a [`Board`] and written by it, off the window's thread. A double
//! click on empty space makes a text card there; a text card's Markdown is typed whole in a
//! [`LiveBlock`], written 800 ms after the last key.

use std::cell::RefCell;
use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::rc::Rc;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_design_system::color::mix;
use gpui_design_system::toast::Toast;
use gpui_design_system::{Palette, palette, toast};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::canvas::{self as model, Canvas, Color, End, Node, NodeKind, Problem, Rect};
use crate::canvas_board::{self, Board, Saved, Written};
use crate::canvas_edges::{self as edges, Line};
use crate::canvas_ops::{Op, Report, Target};
use crate::config::Config;
use crate::document::{self, Document, Links};
use crate::live::{Caret, LiveBlock, LiveEvent};
use crate::reader::Reader;
use crate::render::{self, Ctx, Drawn, Host, Wanted};
use crate::timing;

/// The closest and the furthest the camera goes.
pub const MIN_ZOOM: f32 = 0.1;
pub const MAX_ZOOM: f32 = 4.;
/// A step of zoom: a key, a notch of the wheel.
pub const STEP: f32 = 1.1;
/// From this zoom up, a card shows its whole Markdown.
const FULL: f32 = 0.5;
/// From this zoom up to `FULL`, a box and its title; below it, rectangles only.
const LIGHT: f32 = 0.2;
/// The light version's titles, on screen, whatever the zoom.
const TITLE: f32 = 13.;
/// How far beyond the board, on screen, cards are still built: a pan finds them ready.
const MARGIN: f32 = 200.;
/// The room left around everything when it is framed.
const ROOM: f32 = 48.;
/// How long after the last event of a gesture its light version gives way to the full one.
const SETTLE: Duration = Duration::from_millis(150);
/// A touchpad's scroll worth one step of zoom, in pixels.
const WHEEL_STEP: f32 = 50.;
/// The lines GPUI counts for a notch of the wheel, on Linux: a notch is a step of zoom.
const NOTCH: f32 = 3.;
/// A handle's side, on screen, at every zoom.
const HANDLE: f32 = 8.;
/// How far from a corner, on screen, a press takes its handle.
const REACH: f32 = 6.;
/// How far the pointer goes, on screen, before a press is a drag rather than a click.
const SLOP: f32 = 3.;
/// The smallest a card is resized to, in the canvas's pixels (unless it was smaller).
const MIN_SIZE: f32 = 40.;
/// A nudge, and one with Shift, in the canvas's pixels.
const NUDGE: f32 = 1.;
const NUDGE_FAR: f32 = 10.;
/// What an arrow pans with nothing selected, on screen: a line of the reader.
const PAN_LINE: f32 = 64.;
/// How long nudges rest before they are written, as the reader's autosave.
const NUDGE_REST: Duration = Duration::from_millis(800);
/// How long before a write kept from the canvas by another process is tried again.
const RETRY: Duration = Duration::from_secs(1);

actions!(
    canvas,
    [
        NudgeLeft,
        NudgeRight,
        NudgeUp,
        NudgeDown,
        NudgeLeftFar,
        NudgeRightFar,
        NudgeUpFar,
        NudgeDownFar,
        DeleteSelection,
        Undo,
        Redo,
    ]
);

/// The board's keys, in the reader's `Canvas` context; [`on_keys`] takes them.
pub fn bind_keys(cx: &mut App) {
    let at = Some("Canvas");
    cx.bind_keys([
        KeyBinding::new("left", NudgeLeft, at),
        KeyBinding::new("right", NudgeRight, at),
        KeyBinding::new("up", NudgeUp, at),
        KeyBinding::new("down", NudgeDown, at),
        KeyBinding::new("shift-left", NudgeLeftFar, at),
        KeyBinding::new("shift-right", NudgeRightFar, at),
        KeyBinding::new("shift-up", NudgeUpFar, at),
        KeyBinding::new("shift-down", NudgeDownFar, at),
        KeyBinding::new("delete", DeleteSelection, at),
        KeyBinding::new("ctrl-z", Undo, at),
        KeyBinding::new("ctrl-shift-z", Redo, at),
        KeyBinding::new("enter", EditCard, at),
    ]);
    // Around a card's text being typed, inside the reader's page.
    let card = Some("CanvasCard");
    cx.bind_keys([
        KeyBinding::new("escape", StopTyping, card),
        KeyBinding::new("ctrl-enter", StopTyping, card),
        KeyBinding::new("ctrl-s", WriteCard, card),
    ]);
}

actions!(
    canvas_card,
    [
        /// The text of the card selected alone is typed.
        EditCard,
        StopTyping,
        /// What was typed is written now.
        WriteCard,
    ]
);

/// The board's keys on `element`, the one that holds the focus (the reader's page): the board
/// itself is not on the way of the keys.
pub fn on_keys<E: InteractiveElement>(element: E, board: &Entity<CanvasView>) -> E {
    fn nudge<A: Action>(
        board: &Entity<CanvasView>,
        dx: f32,
        dy: f32,
    ) -> impl Fn(&A, &mut Window, &mut App) + 'static {
        let board = board.clone();
        move |_, _, cx| board.update(cx, |board, cx| board.nudge(dx, dy, cx))
    }
    let (deleted, undone, redone) = (board.clone(), board.clone(), board.clone());
    element
        .on_action(nudge::<NudgeLeft>(board, -NUDGE, 0.))
        .on_action(nudge::<NudgeRight>(board, NUDGE, 0.))
        .on_action(nudge::<NudgeUp>(board, 0., -NUDGE))
        .on_action(nudge::<NudgeDown>(board, 0., NUDGE))
        .on_action(nudge::<NudgeLeftFar>(board, -NUDGE_FAR, 0.))
        .on_action(nudge::<NudgeRightFar>(board, NUDGE_FAR, 0.))
        .on_action(nudge::<NudgeUpFar>(board, 0., -NUDGE_FAR))
        .on_action(nudge::<NudgeDownFar>(board, 0., NUDGE_FAR))
        .on_action(move |_: &DeleteSelection, _, cx| {
            deleted.update(cx, |board, cx| board.delete(cx))
        })
        .on_action(move |_: &Undo, _, cx| undone.update(cx, |board, cx| board.undo(false, cx)))
        .on_action(move |_: &Redo, _, cx| redone.update(cx, |board, cx| board.undo(true, cx)))
        .on_action(on_edit_card(board))
}

/// A place and a size, in the canvas's pixels or on screen.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Area {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

impl Area {
    /// Whether the two overlap, an edge shared counting.
    pub fn meets(&self, other: &Area) -> bool {
        self.x <= other.x + other.width
            && other.x <= self.x + self.width
            && self.y <= other.y + other.height
            && other.y <= self.y + self.height
    }

    pub fn contains(&self, x: f32, y: f32) -> bool {
        (self.x..=self.x + self.width).contains(&x) && (self.y..=self.y + self.height).contains(&y)
    }

    /// The smallest area holding both.
    pub fn union(self, other: Area) -> Area {
        let (x, y) = (self.x.min(other.x), self.y.min(other.y));
        Area {
            x,
            y,
            width: (self.x + self.width).max(other.x + other.width) - x,
            height: (self.y + self.height).max(other.y + other.height) - y,
        }
    }
}

/// Where the board looks: the canvas's point at its top left corner, and the zoom (a screen
/// pixel is `1 / zoom` of the canvas's).
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Camera {
    pub x: f32,
    pub y: f32,
    pub zoom: f32,
}

impl Camera {
    /// Everything in `all` seen on a board `width` by `height`, in its middle, with some room
    /// around; never closer than zoom 1, which a few small cards would otherwise fill the board
    /// with.
    pub fn framing(all: Area, width: f32, height: f32) -> Self {
        let room = |side: f32| (side - 2. * ROOM).max(1.);
        let zoom = (room(width) / all.width.max(1.))
            .min(room(height) / all.height.max(1.))
            .clamp(MIN_ZOOM, 1.);
        Self {
            x: all.x + all.width / 2. - width / (2. * zoom),
            y: all.y + all.height / 2. - height / (2. * zoom),
            zoom,
        }
    }

    /// The point of the board over the canvas's point (`x`, `y`).
    pub fn to_screen(self, x: f32, y: f32) -> (f32, f32) {
        ((x - self.x) * self.zoom, (y - self.y) * self.zoom)
    }

    /// The canvas's point under the point (`x`, `y`) of the board.
    pub fn to_canvas(self, x: f32, y: f32) -> (f32, f32) {
        (self.x + x / self.zoom, self.y + y / self.zoom)
    }

    /// An area of the canvas, on the board.
    pub fn on_screen(&self, area: Area) -> Area {
        let (x, y) = self.to_screen(area.x, area.y);
        Area {
            x,
            y,
            width: area.width * self.zoom,
            height: area.height * self.zoom,
        }
    }

    /// Zoomed by `factor` around the point `at` of the board, which stays over the same point of
    /// the canvas; within the zoom's limits.
    pub fn zoomed(self, factor: f32, at: (f32, f32)) -> Self {
        self.zoomed_to(self.zoom * factor, at)
    }

    /// At `zoom` around the point `at` of the board, as `zoomed`.
    pub fn zoomed_to(self, zoom: f32, at: (f32, f32)) -> Self {
        let zoom = zoom.clamp(MIN_ZOOM, MAX_ZOOM);
        let (x, y) = self.to_canvas(at.0, at.1);
        Self {
            x: x - at.0 / zoom,
            y: y - at.1 / zoom,
            zoom,
        }
    }

    /// Moved with what the board shows: the canvas follows a drag of (`dx`, `dy`) on screen.
    pub fn panned(self, dx: f32, dy: f32) -> Self {
        Self {
            x: self.x - dx / self.zoom,
            y: self.y - dy / self.zoom,
            ..self
        }
    }

    /// The part of the canvas a board `width` by `height` shows, with `margin` screen pixels
    /// around it.
    pub fn seen(&self, width: f32, height: f32, margin: f32) -> Area {
        let (x, y) = self.to_canvas(-margin, -margin);
        Area {
            x,
            y,
            width: (width + 2. * margin) / self.zoom,
            height: (height + 2. * margin) / self.zoom,
        }
    }

    /// The zoom a card's text is drawn at: its size, `font_size` at zoom 1, rounded to half a
    /// pixel, so that a gesture does not lay its glyphs out again at each of its sizes.
    pub fn text_zoom(&self, font_size: f32) -> f32 {
        let size = ((font_size * self.zoom * 2.).round() / 2.).max(0.5);
        size / font_size
    }

    /// As the state keeps it.
    pub fn kept(&self) -> [f32; 3] {
        [self.x, self.y, self.zoom]
    }

    /// As the state kept it; `None` for a camera no board could have.
    pub fn from_kept([x, y, zoom]: [f32; 3]) -> Option<Self> {
        (x.is_finite() && y.is_finite() && zoom.is_finite()).then(|| Self {
            x,
            y,
            zoom: zoom.clamp(MIN_ZOOM, MAX_ZOOM),
        })
    }
}

/// How much of each card is built.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Detail {
    /// Its whole Markdown.
    Full,
    /// A coloured box and its title.
    Light,
    /// A rectangle, all of them painted at once.
    Rects,
}

impl Detail {
    /// At `zoom`; while a gesture zooms, no closer than the light version, which keeps up.
    pub fn at(zoom: f32, gesturing: bool) -> Self {
        if zoom < LIGHT {
            Self::Rects
        } else if zoom < FULL || gesturing {
            Self::Light
        } else {
            Self::Full
        }
    }
}

/// A card's colour, as the canvas gives it.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Tint {
    /// `"1"` red to `"6"` purple.
    Preset(u8),
    Rgb([u8; 3]),
}

/// What a card is, as the board draws it.
#[derive(Clone)]
pub enum CardKind {
    Text(Arc<Document>),
    Group,
    /// A file card, drawn as its path for now.
    File,
    /// A link card, drawn as its address for now.
    Link,
    /// A type the board does not know: its name.
    Other,
}

#[derive(Clone)]
pub struct Card {
    /// The node's index in the canvas: its element's id.
    pub index: usize,
    /// The node's id, empty when it has none: what it is selected and changed by.
    pub id: String,
    pub area: Area,
    pub tint: Option<Tint>,
    pub kind: CardKind,
    /// What the light version shows: a text's first heading, else its first line; a group's
    /// label, a file's path, a link's address.
    pub title: String,
}

/// A canvas file, read off the main thread and ready to draw.
#[derive(Clone)]
pub struct CanvasFile {
    /// The canvas as the file holds it: what its edits start from.
    pub canvas: Arc<Canvas>,
    /// The cards with a place and a size, groups first, each kind in the canvas's order: a group
    /// lies below what it holds.
    pub cards: Vec<Card>,
    /// What all the cards and edges cover.
    pub all: Option<Area>,
    /// The edges between two cards that have a place and a size, in the canvas's order.
    pub edges: Vec<EdgeLine>,
}

/// An edge between two cards, as the board draws it.
#[derive(Clone, PartialEq)]
pub struct EdgeLine {
    /// The edge's index in the canvas.
    pub index: usize,
    pub line: Line,
    pub tint: Option<Tint>,
    pub label: Option<String>,
}

impl CanvasFile {
    /// Reads the text of a `.canvas` file, each text card's Markdown parsed, its formulas drawn in
    /// `ink` and its links checked from `folder`, the canvas's own. `None` when it is not JSON:
    /// it is shown as code then. A canvas otherwise wrong (a duplicate id, an edge to nowhere)
    /// is shown as far as it could be read. Slow on a large canvas: call it off the main thread.
    pub fn read(text: &str, ink: &str, folder: Option<&Path>) -> Option<Self> {
        let canvas = Canvas::read(text);
        if canvas
            .problems()
            .iter()
            .any(|problem| matches!(problem, Problem::Json(_)))
        {
            return None;
        }
        let mut cards: Vec<Card> = canvas
            .nodes()
            .iter()
            .enumerate()
            .filter_map(|(index, node)| {
                card(index, node, |text| document::parse(text, ink, folder))
            })
            .collect();
        // Stable: each kind keeps the canvas's order.
        cards.sort_by_key(|card| !matches!(card.kind, CardKind::Group));
        let edges = lines(&canvas, &node_rects(&canvas));
        // The curves too: one may bulge out of its cards.
        let all = cards
            .iter()
            .map(|card| card.area)
            .chain(edges.iter().map(|edge| area(edge.line.extent())))
            .reduce(Area::union);
        Some(Self {
            canvas: Arc::new(canvas),
            cards,
            all,
            edges,
        })
    }

    /// The edges of `canvas`, the canvas being edited, laid on the cards where they are now:
    /// moved, resized or held by the pointer; those to a card no longer there left out.
    pub fn laid_edges(&self, canvas: &Canvas) -> Vec<EdgeLine> {
        let rects: HashMap<&str, edges::Rect> = self
            .cards
            .iter()
            .filter(|card| !card.id.is_empty())
            .map(|card| {
                let a = card.area;
                let rect = edges::Rect {
                    x: a.x,
                    y: a.y,
                    width: a.width,
                    height: a.height,
                };
                (card.id.as_str(), rect)
            })
            .collect();
        lines(canvas, &rects)
    }

    /// The edge nearest the canvas's point (`x`, `y`), no further than `reach`: its place in
    /// `edges`.
    pub fn edge_at(&self, x: f32, y: f32, reach: f32) -> Option<usize> {
        let at = edges::Point::new(x, y);
        self.edges
            .iter()
            .enumerate()
            .filter(|(_, edge)| {
                let b = edge.line.bounds;
                (b.x - reach..=b.x + b.width + reach).contains(&x)
                    && (b.y - reach..=b.y + b.height + reach).contains(&y)
            })
            .map(|(i, edge)| (i, edge.line.distance(at)))
            .filter(|(_, distance)| *distance <= reach)
            .min_by(|a, b| a.1.total_cmp(&b.1))
            .map(|(i, _)| i)
    }

    /// What the text cards show that is drawn off the main thread.
    fn wanted(&self) -> Wanted {
        let mut wanted = Wanted::default();
        for card in &self.cards {
            if let CardKind::Text(doc) = &card.kind {
                document::diagrams(&doc.blocks, &mut wanted.diagrams);
                document::code_blocks(&doc.blocks, &mut wanted.code);
            }
        }
        wanted
    }
}

/// An edge's rectangle as an area of the canvas.
fn area(rect: edges::Rect) -> Area {
    Area {
        x: rect.x,
        y: rect.y,
        width: rect.width,
        height: rect.height,
    }
}

/// The place and size of each node of `canvas` that has them, by id.
fn node_rects(canvas: &Canvas) -> HashMap<&str, edges::Rect> {
    canvas
        .nodes()
        .iter()
        .filter_map(|node| {
            let rect = node.rect()?;
            let rect = edges::Rect {
                x: rect.x as f32,
                y: rect.y as f32,
                width: rect.width as f32,
                height: rect.height as f32,
            };
            Some((node.id()?, rect))
        })
        .collect()
}

/// The edges of `canvas` between two nodes of `rects`, the place and size of each by id, their
/// geometry worked out once.
fn lines(canvas: &Canvas, rects: &HashMap<&str, edges::Rect>) -> Vec<EdgeLine> {
    canvas
        .edges()
        .iter()
        .enumerate()
        .filter_map(|(index, edge)| {
            let from = *rects.get(edge.source_node()?)?;
            let to = *rects.get(edge.target_node()?)?;
            let line = Line::new(
                from,
                edges::Side::of(edge.source_side()),
                to,
                edges::Side::of(edge.target_side()),
                edge.source_end() == End::Arrow,
                edge.target_end() == End::Arrow,
            );
            let tint = match edge.color() {
                Some(Color::Preset(n)) => Some(Tint::Preset(n)),
                Some(Color::Rgb(rgb)) => Some(Tint::Rgb(rgb)),
                Some(Color::Other(_)) | None => None,
            };
            let label = edge
                .label()
                .filter(|label| !label.trim().is_empty())
                .map(str::to_string);
            Some(EdgeLine {
                index,
                line,
                tint,
                label,
            })
        })
        .collect()
}

/// The card of the node at `index` of the canvas, a text's Markdown read by `parse`; `None` for
/// a node without a place and a size.
fn card(index: usize, node: &Node, parse: impl FnOnce(&str) -> Document) -> Option<Card> {
    let rect = node.rect()?;
    let area = Area {
        x: rect.x as f32,
        y: rect.y as f32,
        width: rect.width as f32,
        height: rect.height as f32,
    };
    let tint = match node.color() {
        Some(Color::Preset(n)) => Some(Tint::Preset(n)),
        Some(Color::Rgb(rgb)) => Some(Tint::Rgb(rgb)),
        Some(Color::Other(_)) | None => None,
    };
    let named = |name: Option<&str>| name.unwrap_or_default().to_string();
    let (kind, title) = match node.kind() {
        Some(NodeKind::Text) => {
            let text = node.text().unwrap_or_default();
            let doc = parse(text);
            let title = title(&doc, text);
            (CardKind::Text(Arc::new(doc)), title)
        }
        Some(NodeKind::Group) => (CardKind::Group, named(node.label())),
        Some(NodeKind::File) => (CardKind::File, named(node.file())),
        Some(NodeKind::Link) => (CardKind::Link, named(node.url())),
        Some(NodeKind::Other(kind)) => (CardKind::Other, kind.to_string()),
        None => (CardKind::Other, String::new()),
    };
    Some(Card {
        index,
        id: node.id().unwrap_or_default().to_string(),
        area,
        tint,
        kind,
        title,
    })
}

/// A text card's title: its first heading, else its first line that says something.
fn title(doc: &Document, text: &str) -> String {
    if let Some(heading) = doc.headings.first() {
        return heading.text.clone();
    }
    text.lines()
        .map(|line| {
            line.trim_start_matches(['#', '>', '-', '*', '+', ' ', '\t'])
                .trim()
        })
        .find(|line| !line.is_empty())
        .unwrap_or_default()
        .to_string()
}

/// A card's colour in this theme. The theme has a red, a yellow and a green; orange, cyan and
/// purple, which it has not, are fixed hues (28°, 185°, 270°) at its yellow's saturation and
/// lightness, so that they sit in its shade, dark or light.
pub fn tint(p: &Palette, tint: Tint) -> Hsla {
    let fixed = |degrees: f32| hsla(degrees / 360., p.wait.s, p.wait.l, 1.);
    match tint {
        Tint::Preset(1) => p.fail,
        Tint::Preset(2) => fixed(28.),
        Tint::Preset(3) => p.wait,
        Tint::Preset(4) => p.ok,
        Tint::Preset(5) => fixed(185.),
        Tint::Preset(_) => fixed(270.),
        Tint::Rgb([r, g, b]) => Rgba {
            r: f32::from(r) / 255.,
            g: f32::from(g) / 255.,
            b: f32::from(b) / 255.,
            a: 1.,
        }
        .into(),
    }
}

/// The board of the canvas the reader shows.
pub struct CanvasView {
    file: Arc<CanvasFile>,
    reader: WeakEntity<Reader>,
    /// Unknown until the board is first laid out, unless the state kept one: everything is
    /// framed then.
    camera: Option<Camera>,
    /// Where the board was last laid out, in the window.
    bounds: Option<Bounds<Pixels>>,
    /// The diagrams and code's colours the text cards show.
    drawn: Drawn,
    /// The link under the last click, followed if Ctrl was held.
    clicked_link: Option<String>,
    /// Space is held: a drag anywhere pans.
    space: bool,
    /// Where the pointer was at the last step of a pan.
    panning: Option<Point<Pixels>>,
    /// A zoom gesture goes on: the light version is drawn until it settles.
    gesturing: bool,
    settle: Option<Task<()>>,
    /// What is being edited, and its writes.
    edits: Edits,
    /// The edge under the pointer, by its place in the file's `edges`.
    hovered: Option<usize>,
    /// The edges' shapes, by their index in the canvas, kept while the file and the zoom stay.
    paths: Rc<RefCell<HashMap<usize, Built>>>,
    /// Where each label was last drawn, in the window, by its edge's place in the file's `edges`.
    labels: Labels,
}

/// The selection, what the pointer holds, and the canvas as it is edited.
struct Edits {
    /// The canvas's file.
    path: PathBuf,
    /// The cards selected, by id, in the order they were.
    selected: Vec<String>,
    /// Cards, or a handle, held by the pointer until it is let go.
    held: Option<Held>,
    /// What a press does if it is let go before the pointer goes past [`SLOP`].
    click: Option<Click>,
    /// The canvas being edited, made at the first edit.
    board: Option<Board>,
    /// A write runs off the window's thread; `again` asks for another once it is done.
    writing: bool,
    again: bool,
    /// The file was not a valid canvas at the last write: tried again once it is read again.
    stalled: bool,
    /// A write to come: nudges resting, a lock waited for.
    later: Option<Task<()>>,
    /// The text card being typed in.
    typing: Option<Typing>,
}

/// A text card whose Markdown is typed whole, where it is read.
struct Typing {
    id: String,
    field: Entity<LiveBlock>,
    /// How many actions undo held before: those done since are one once it ends.
    mark: usize,
    /// What had the keys before, given them back when it ends.
    before: Option<FocusHandle>,
    window: AnyWindowHandle,
    _events: Subscription,
}

/// What the pointer holds.
enum Held {
    /// Cards, from where they were when pressed, moved by `by` in the canvas's pixels.
    Moving {
        from: Point<Pixels>,
        start: Vec<(String, Area)>,
        by: (f32, f32),
        going: bool,
    },
    /// A card by a corner, its area `now`.
    Resizing {
        from: Point<Pixels>,
        id: String,
        start: Area,
        corner: Corner,
        now: Area,
        going: bool,
    },
}

struct Click {
    at: Point<Pixels>,
    pick: Pick,
}

/// What a click does to the selection.
enum Pick {
    Only(String),
    Toggle(String),
    Clear,
}

impl CanvasView {
    pub fn new(
        file: Arc<CanvasFile>,
        camera: Option<Camera>,
        path: PathBuf,
        reader: WeakEntity<Reader>,
        cx: &mut Context<Self>,
    ) -> Self {
        let mut view = Self {
            file: file.clone(),
            reader,
            camera,
            bounds: None,
            drawn: Drawn::default(),
            clicked_link: None,
            space: false,
            panning: None,
            gesturing: false,
            settle: None,
            edits: Edits {
                path,
                selected: Vec::new(),
                held: None,
                click: None,
                board: None,
                writing: false,
                again: false,
                stalled: false,
                later: None,
                typing: None,
            },
            hovered: None,
            paths: Rc::default(),
            labels: Rc::default(),
        };
        view.set_file(file, cx);
        view
    }

    /// The canvas read again: the camera stays where it is. What is not written yet is replayed
    /// on it, and the cards held stay where the pointer has them.
    pub fn set_file(&mut self, file: Arc<CanvasFile>, cx: &mut Context<Self>) {
        self.drawn
            .fill(file.wanted(), &[], |this: &mut Self| &mut this.drawn, cx);
        self.paths.borrow_mut().clear();
        self.labels.borrow_mut().clear();
        self.hovered = None;
        self.file = file;
        self.read_again(cx);
        cx.notify();
    }

    pub fn camera(&self) -> Option<Camera> {
        self.camera
    }

    /// How high the board is on screen: what a page of the keys moves.
    pub fn height(&self) -> Pixels {
        self.bounds.map_or(px(0.), |bounds| bounds.size.height)
    }

    /// The colours changed: what was drawn in the old ones is drawn again with the file.
    pub fn restyle(&mut self) {
        self.drawn.clear();
    }

    /// Zooms by `factor`, or back to 1 (`None`), around the pointer when it is over the board,
    /// else around its middle.
    pub fn zoom_key(&mut self, factor: Option<f32>, window: &Window, cx: &mut Context<Self>) {
        let (Some(camera), Some(bounds)) = (self.camera, self.bounds) else {
            return;
        };
        let pointer = window.mouse_position();
        let at = if bounds.contains(&pointer) {
            local(pointer, bounds)
        } else {
            (
                f32::from(bounds.size.width) / 2.,
                f32::from(bounds.size.height) / 2.,
            )
        };
        self.camera = Some(match factor {
            Some(factor) => camera.zoomed(factor, at),
            None => camera.zoomed_to(1., at),
        });
        cx.notify();
    }

    /// Moves the canvas by (`dx`, `dy`) on screen, as a drag would.
    pub fn pan_by(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        if let Some(camera) = self.camera {
            self.camera = Some(camera.panned(dx, dy));
            cx.notify();
        }
    }

    /// Space pressed or let go: while it is held, a drag on a card pans too. A space typed in a
    /// card is not held.
    pub fn hold_space(&mut self, held: bool, cx: &mut Context<Self>) {
        if held && self.typing() {
            return;
        }
        if self.space != held {
            self.space = held;
            cx.notify();
        }
    }

    /// The board laid out at `bounds`: framed the first time, unless a camera was kept. Whether
    /// it moved or changed size, and has to be drawn again.
    fn laid_out(&mut self, bounds: Bounds<Pixels>) -> bool {
        if self.bounds == Some(bounds) {
            return false;
        }
        self.bounds = Some(bounds);
        if self.camera.is_none() {
            let (width, height) = (f32::from(bounds.size.width), f32::from(bounds.size.height));
            self.camera = Some(match self.file.all {
                Some(all) => Camera::framing(all, width, height),
                None => Camera {
                    x: -width / 2.,
                    y: -height / 2.,
                    zoom: 1.,
                },
            });
        }
        true
    }

    fn press(&mut self, event: &MouseDownEvent, window: &mut Window, cx: &mut Context<Self>) {
        if self.press_typing(event, window, cx) {
            return;
        }
        if self.space || self.press_selection(event, cx) {
            self.panning = Some(event.position);
            cx.notify();
        }
    }

    fn drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        self.drag_held(position, cx);
        let Some(from) = self.panning else {
            return;
        };
        self.panning = Some(position);
        let delta = position - from;
        self.pan_by(f32::from(delta.x), f32::from(delta.y), cx);
    }

    fn release(&mut self, cx: &mut Context<Self>) {
        self.release_held(cx);
        if self.panning.take().is_some() {
            cx.notify();
        }
    }

    /// The wheel scrolls; with Ctrl it zooms around the pointer.
    fn wheel(&mut self, event: &ScrollWheelEvent, window: &mut Window, cx: &mut Context<Self>) {
        let (Some(camera), Some(bounds)) = (self.camera, self.bounds) else {
            return;
        };
        cx.stop_propagation();
        if event.modifiers.control {
            let steps = match event.delta {
                ScrollDelta::Lines(lines) => lines.y / NOTCH,
                ScrollDelta::Pixels(pixels) => f32::from(pixels.y) / WHEEL_STEP,
            };
            let at = local(event.position, bounds);
            self.camera = Some(camera.zoomed(STEP.powf(steps), at));
            self.gesture(cx);
        } else {
            let delta = event.delta.pixel_delta(window.line_height());
            let (mut dx, mut dy) = (f32::from(delta.x), f32::from(delta.y));
            // A mouse's wheel has no other way sideways.
            if event.modifiers.shift && dx == 0. {
                (dx, dy) = (dy, 0.);
            }
            self.camera = Some(camera.panned(dx, dy));
        }
        cx.notify();
    }

    fn pinch(&mut self, event: &PinchEvent, _: &mut Window, cx: &mut Context<Self>) {
        let (Some(camera), Some(bounds)) = (self.camera, self.bounds) else {
            return;
        };
        let at = local(event.position, bounds);
        self.camera = Some(camera.zoomed(1. + event.delta, at));
        self.gesture(cx);
        cx.notify();
    }

    /// A zoom gesture goes on: the light version until it has rested for `SETTLE`.
    fn gesture(&mut self, cx: &mut Context<Self>) {
        self.gesturing = true;
        self.settle = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(SETTLE).await;
            this.update(cx, |this, cx| {
                this.gesturing = false;
                this.settle = None;
                cx.notify();
            })
            .ok();
        }));
    }

    /// A card at its whole Markdown, or a group, at the camera's zoom.
    fn full_card(&self, ctx: &Ctx<Self>, card: &Card, camera: Camera, p: &Palette) -> AnyElement {
        let area = camera.on_screen(card.area);
        let color = card.tint.map(|t| tint(p, t));
        let zoom = camera.zoom;
        let frame = placed(div().id(("card", card.index)), area)
            .rounded(px(8. * zoom))
            .border(px((2. * zoom).max(1.)));
        match &card.kind {
            CardKind::Group => group(card, area, color, px(TITLE.max(f32::from(ctx.size(1.)))), p),
            CardKind::Text(doc) => frame
                .overflow_hidden()
                .border_color(color.unwrap_or(p.line2))
                .bg(color.map_or(p.card, |c| mix(p.card, c, 0.1)))
                .child(render::scaled(
                    ctx.zoom,
                    div()
                        .size_full()
                        .px_5()
                        .py_4()
                        .flex()
                        .flex_col()
                        .gap_2()
                        .text_size(ctx.size(1.))
                        .line_height(ctx.line_height(render::LINE))
                        .map(|body| self.card_text(ctx, card, doc, body)),
                ))
                .on_click(ctx.cx.listener(|this, event: &ClickEvent, window, cx| {
                    if let Some(link) = this.clicked_link.take()
                        && event.modifiers().secondary()
                    {
                        this.follow(&link, window, cx);
                    }
                }))
                .into_any_element(),
            CardKind::File | CardKind::Link | CardKind::Other => frame
                .overflow_hidden()
                .border_color(color.unwrap_or(p.line2))
                .bg(color.map_or(p.card, |c| mix(p.card, c, 0.1)))
                .p(px(16. * zoom))
                .text_size(ctx.size(1.))
                .text_color(p.mute)
                .child(card.title.clone())
                .into_any_element(),
        }
    }

    /// A card as a coloured box and its title, at a fixed size on screen.
    fn light_card(card: &Card, camera: Camera, p: &Palette) -> AnyElement {
        let area = camera.on_screen(card.area);
        let color = card.tint.map(|t| tint(p, t));
        if let CardKind::Group = card.kind {
            return group(card, area, color, px(TITLE), p);
        }
        placed(div().id(("card", card.index)), area)
            .overflow_hidden()
            .rounded(px(4.))
            .border_1()
            .border_color(color.unwrap_or(p.line2))
            .bg(color.map_or(p.card2, |c| mix(p.card, c, 0.3)))
            .p(px(6.))
            .text_size(px(TITLE))
            .line_height(px(TITLE * 1.3))
            .text_color(p.text)
            .child(card.title.clone())
            .into_any_element()
    }

    /// Every card as a rectangle, in one element.
    fn rects(cards: &[&Card], camera: Camera, p: &Palette) -> AnyElement {
        let quads: Vec<(Area, Hsla, Hsla)> = cards
            .iter()
            .map(|card| {
                let color = card.tint.map(|t| tint(p, t));
                let area = camera.on_screen(card.area);
                match card.kind {
                    CardKind::Group => (
                        area,
                        color.map_or(p.text.opacity(0.03), |c| c.opacity(0.08)),
                        color.map_or(p.line2, |c| c.opacity(0.6)),
                    ),
                    _ => (
                        area,
                        color.map_or(p.card2, |c| mix(p.card, c, 0.4)),
                        color.unwrap_or(p.line2),
                    ),
                }
            })
            .collect();
        canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                for (area, background, border) in quads {
                    let at = Bounds::new(
                        bounds.origin + point(px(area.x), px(area.y)),
                        size(px(area.width.max(1.)), px(area.height.max(1.))),
                    );
                    window.paint_quad(quad(
                        at,
                        px(2.),
                        background,
                        px(1.),
                        border,
                        BorderStyle::Solid,
                    ));
                }
            },
        )
        .absolute()
        .size_full()
        .into_any_element()
    }
}

/// The selection, and what is done to the cards: pressed, dragged, let go, keys, writes.
impl CanvasView {
    /// How high a group's label stands above it on screen, at the detail drawn.
    fn label_line(&self, camera: Camera, cx: &App) -> f32 {
        let size = match Detail::at(camera.zoom, self.gesturing) {
            Detail::Full => {
                let font = cx.global::<Config>().font_size;
                TITLE.max(font * camera.text_zoom(font))
            }
            Detail::Light => TITLE,
            Detail::Rects => return 0.,
        };
        size * 1.4
    }

    /// A press, Space not held: a handle or cards are held, or the selection waits for the
    /// release to know whether it was a click. True when the press pans: on empty space, or on
    /// the background of a group not selected.
    fn press_selection(&mut self, event: &MouseDownEvent, cx: &mut Context<Self>) -> bool {
        let (Some(camera), Some(bounds)) = (self.camera, self.bounds) else {
            return false;
        };
        let (x, y) = local(event.position, bounds);
        let label = self.label_line(camera, cx);
        let (at, shift) = (event.position, event.modifiers.shift);
        let edits = &mut self.edits;
        edits.click = None;
        let hit = hit(&self.file.cards, camera, label, edits.alone(), x, y);
        let pick = |id| match shift {
            true => Pick::Toggle(id),
            false => Pick::Only(id),
        };
        match hit {
            Hit::Handle(id, corner) => {
                if let Some(card) = self.file.cards.iter().find(|card| card.id == id) {
                    edits.held = Some(Held::Resizing {
                        from: at,
                        id,
                        start: card.area,
                        corner,
                        now: card.area,
                        going: false,
                    });
                }
                false
            }
            Hit::Card(id) => {
                if shift {
                    if let Some(index) = edits.selected.iter().position(|s| *s == id) {
                        edits.selected.remove(index);
                        cx.notify();
                        return false;
                    }
                    edits.selected.push(id);
                } else if edits.selected.contains(&id) {
                    // A drag moves the whole selection; a click selects this card alone.
                    edits.click = Some(Click {
                        at,
                        pick: Pick::Only(id),
                    });
                } else {
                    edits.selected = vec![id];
                }
                self.hold_selection(at);
                cx.notify();
                false
            }
            Hit::Group(id) if !shift && edits.selected.contains(&id) => {
                edits.click = Some(Click {
                    at,
                    pick: Pick::Only(id),
                });
                self.hold_selection(at);
                false
            }
            Hit::Group(id) => {
                edits.click = Some(Click { at, pick: pick(id) });
                true
            }
            Hit::Empty => {
                edits.click = (!shift).then_some(Click {
                    at,
                    pick: Pick::Clear,
                });
                true
            }
        }
    }

    /// The selection held, with what its groups hold.
    fn hold_selection(&mut self, at: Point<Pixels>) {
        let start = carried(&self.file.cards, &self.edits.selected);
        self.edits.held = (!start.is_empty()).then_some(Held::Moving {
            from: at,
            start,
            by: (0., 0.),
            going: false,
        });
    }

    /// The pointer moved while pressed: past [`SLOP`], no more a click, and what it holds goes
    /// with it.
    fn drag_held(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let edits = &mut self.edits;
        if let Some(click) = &edits.click
            && far(position - click.at)
        {
            edits.click = None;
        }
        let Some(camera) = self.camera else {
            return;
        };
        let (from, going) = match &mut edits.held {
            Some(Held::Moving { from, going, .. } | Held::Resizing { from, going, .. }) => {
                (*from, going)
            }
            None => return,
        };
        let delta = position - from;
        if !*going && !far(delta) {
            return;
        }
        *going = true;
        let (dx, dy) = (
            f32::from(delta.x) / camera.zoom,
            f32::from(delta.y) / camera.zoom,
        );
        match &mut edits.held {
            Some(Held::Moving { by, .. }) => *by = (dx, dy),
            Some(Held::Resizing {
                start, corner, now, ..
            }) => *now = resized(*start, *corner, dx, dy),
            None => {}
        }
        self.show_held();
        cx.notify();
    }

    /// The pointer let go: a click selects, a drag is done and written.
    fn release_held(&mut self, cx: &mut Context<Self>) {
        let edits = &mut self.edits;
        if let Some(click) = edits.click.take() {
            match click.pick {
                Pick::Only(id) => edits.selected = vec![id],
                Pick::Toggle(id) => match edits.selected.iter().position(|s| *s == id) {
                    Some(index) => {
                        edits.selected.remove(index);
                    }
                    None => edits.selected.push(id),
                },
                Pick::Clear => edits.selected.clear(),
            }
            cx.notify();
        }
        let to: Vec<(String, Area)> = match edits.held.take() {
            Some(Held::Moving {
                start,
                by,
                going: true,
                ..
            }) => start
                .into_iter()
                .map(|(id, area)| (id, moved(area, by.0, by.1)))
                .collect(),
            Some(Held::Resizing {
                id,
                now,
                going: true,
                ..
            }) => vec![(id, now)],
            _ => return,
        };
        if self.edit(|canvas| placing(canvas, &to), cx) {
            self.save(cx);
        }
    }

    fn held_cursor(&self) -> Option<CursorStyle> {
        match &self.edits.held {
            Some(Held::Resizing { corner, .. }) => Some(match corner {
                Corner::TopLeft | Corner::BottomRight => CursorStyle::ResizeUpLeftDownRight,
                Corner::TopRight | Corner::BottomLeft => CursorStyle::ResizeUpRightDownLeft,
            }),
            _ => None,
        }
    }

    /// The selection outlined in the accent colour, and the handles of a card selected alone:
    /// painted over the cards, in one element.
    fn selection(&self, camera: Camera, p: &Palette) -> Option<AnyElement> {
        let selected = &self.edits.selected;
        if selected.is_empty() {
            return None;
        }
        // The theme's accent, or its blue when the accent is grey: as links are.
        let accent = crate::kit::link(p);
        let outlines: Vec<Area> = self
            .file
            .cards
            .iter()
            .filter(|card| selected.contains(&card.id))
            .map(|card| camera.on_screen(card.area))
            .collect();
        let handles: Vec<Area> = self
            .edits
            .alone()
            .and_then(|id| self.file.cards.iter().find(|card| card.id == id))
            .map(|card| camera.on_screen(card.area))
            .filter(|area| handles_shown(*area))
            .map(|area| Corner::ALL.map(|corner| handle(corner, area)).to_vec())
            .unwrap_or_default();
        let (fill, clear) = (p.bg, hsla(0., 0., 0., 0.));
        let element = canvas(
            |_, _, _| (),
            move |bounds, (), window, _| {
                let at = |area: Area| {
                    Bounds::new(
                        bounds.origin + point(px(area.x), px(area.y)),
                        size(px(area.width), px(area.height)),
                    )
                };
                for area in outlines {
                    let around = Area {
                        x: area.x - 2.,
                        y: area.y - 2.,
                        width: area.width + 4.,
                        height: area.height + 4.,
                    };
                    window.paint_quad(quad(
                        at(around),
                        px(4.),
                        clear,
                        px(2.),
                        accent,
                        BorderStyle::Solid,
                    ));
                }
                for area in handles {
                    window.paint_quad(quad(
                        at(area),
                        px(2.),
                        fill,
                        px(1.5),
                        accent,
                        BorderStyle::Solid,
                    ));
                }
            },
        )
        .absolute()
        .size_full();
        Some(element.into_any_element())
    }

    /// An arrow: the selection nudged by (`dx`, `dy`), written once the keys rest; with nothing
    /// selected, the board pans a line.
    fn nudge(&mut self, dx: f32, dy: f32, cx: &mut Context<Self>) {
        if self.edits.selected.is_empty() {
            let pan = |d: f32| if d == 0. { 0. } else { -d.signum() * PAN_LINE };
            self.pan_by(pan(dx), pan(dy), cx);
            return;
        }
        if self.edits.held.is_some() {
            return;
        }
        let to: Vec<(String, Area)> = carried(&self.file.cards, &self.edits.selected)
            .into_iter()
            .map(|(id, area)| (id, moved(area, dx, dy)))
            .collect();
        if self.edit(|canvas| placing(canvas, &to), cx) {
            self.save_later(NUDGE_REST, cx);
        }
    }

    /// `Delete`: the selection deleted with its edges, and written.
    fn delete(&mut self, cx: &mut Context<Self>) {
        if self.edits.selected.is_empty() || self.edits.held.is_some() {
            return;
        }
        let ids = std::mem::take(&mut self.edits.selected);
        if self.edit(|canvas| deleting(canvas, &ids), cx) {
            self.save(cx);
        }
    }

    /// Undoes the last action, or does again the last undone, and writes it. A card a deletion
    /// undone gives back, or a text, is shown as soon as its Markdown is read.
    fn undo(&mut self, redo: bool, cx: &mut Context<Self>) {
        if self.edits.held.is_some() {
            return;
        }
        let Some(board) = &mut self.edits.board else {
            return;
        };
        let done = if redo { board.redo() } else { board.undo() };
        if done {
            self.show_board();
            self.show_texts(cx);
            self.save(cx);
            cx.notify();
        }
    }

    /// Does the operations `make` makes against the canvas being edited, as one action. False
    /// when there were none.
    fn edit(&mut self, make: impl FnOnce(&Canvas) -> Vec<Op>, cx: &mut Context<Self>) -> bool {
        self.open_board(cx);
        let mut done = false;
        if let Some(board) = &mut self.edits.board {
            let mut ops = make(board.canvas());
            done = !ops.is_empty();
            // One operation alone may join the one before it: nudges one after the other.
            match ops.len() {
                0 => {}
                1 => board.edit(ops.remove(0)),
                _ => board.edit_all(ops),
            }
        }
        self.show_board();
        cx.notify();
        done
    }

    /// The canvas being edited, from the file as it was read.
    fn open_board(&mut self, cx: &mut Context<Self>) {
        if self.edits.board.is_some() {
            return;
        }
        let canvas = (*self.file.canvas).clone();
        match Board::with_canvas(&self.edits.path, canvas_board::runtime_locks(), canvas) {
            Ok(board) => self.edits.board = Some(board),
            Err(error) => toast(
                Toast::failure(format!("This canvas cannot be edited: {error}")),
                cx,
            ),
        }
    }

    /// The file read again: what is not written yet replayed on it.
    fn read_again(&mut self, cx: &mut Context<Self>) {
        let Some(board) = &mut self.edits.board else {
            self.show_held();
            return;
        };
        board.refreshed((*self.file.canvas).clone());
        self.show_board();
        self.show_texts(cx);
        self.sync_typing(cx);
        if std::mem::take(&mut self.edits.stalled) {
            self.save(cx);
        }
    }

    /// The cards where the canvas being edited has them (the file's, before any edit), those it
    /// no longer has left out; the cards held where the pointer has them.
    fn show_board(&mut self) {
        let disk = self.file.canvas.clone();
        let canvas = self.edits.board.as_ref().map_or(&*disk, Board::canvas);
        let mut areas: HashMap<&str, Area> = HashMap::new();
        for node in canvas.nodes() {
            if let (Some(id), Some(rect)) = (node.id(), node.rect()) {
                areas.entry(id).or_insert(Area {
                    x: rect.x as f32,
                    y: rect.y as f32,
                    width: rect.width as f32,
                    height: rect.height as f32,
                });
            }
        }
        let changes: Vec<(usize, Option<Area>)> = self
            .file
            .cards
            .iter()
            .enumerate()
            .filter(|(_, card)| !card.id.is_empty())
            .filter_map(|(index, card)| {
                let now = areas.get(card.id.as_str()).copied();
                (now != Some(card.area)).then_some((index, now))
            })
            .collect();
        if !changes.is_empty() {
            let file = Arc::make_mut(&mut self.file);
            let mut gone = HashSet::new();
            for (index, area) in changes {
                match area {
                    Some(area) => file.cards[index].area = area,
                    None => {
                        gone.insert(index);
                    }
                }
            }
            let mut index = 0;
            file.cards.retain(|_| {
                index += 1;
                !gone.contains(&(index - 1))
            });
        }
        self.show_held();
        self.relay_edges();
    }

    /// The cards held, where the pointer has them, whatever the file says meanwhile.
    fn show_held(&mut self) {
        let areas: Vec<(&str, Area)> = match &self.edits.held {
            Some(Held::Moving {
                start,
                by,
                going: true,
                ..
            }) => start
                .iter()
                .map(|(id, area)| (id.as_str(), moved(*area, by.0, by.1)))
                .collect(),
            Some(Held::Resizing {
                id,
                now,
                going: true,
                ..
            }) => vec![(id.as_str(), *now)],
            _ => return,
        };
        let file = Arc::make_mut(&mut self.file);
        for (id, area) in areas {
            if let Some(card) = file.cards.iter_mut().find(|card| card.id == id) {
                card.area = area;
            }
        }
        self.relay_edges();
    }

    /// Writes what is not written yet, off the window's thread: the lock may keep it waiting.
    fn save(&mut self, cx: &mut Context<Self>) {
        let edits = &mut self.edits;
        if edits.writing {
            edits.again = true;
            return;
        }
        let Some(board) = &mut edits.board else {
            return;
        };
        if board.pending().is_empty() {
            return;
        }
        let writing = board.writing();
        edits.writing = true;
        let work = cx.background_executor().spawn(async move { writing.run() });
        // Not kept by the board: a write begun ends, even if the board is let go meanwhile.
        cx.spawn(async move |this, cx| {
            let written = work.await;
            this.update(cx, |this, cx| this.saved(written, cx)).ok();
        })
        .detach();
    }

    fn save_later(&mut self, after: Duration, cx: &mut Context<Self>) {
        self.edits.later = Some(cx.spawn(async move |this, cx| {
            cx.background_executor().timer(after).await;
            this.update(cx, |this, cx| this.save(cx)).ok();
        }));
    }

    /// The board is left (another file, back, the window closed, the file no more a canvas):
    /// what is not written yet is written now, on this thread, since the window may be gone
    /// before a write off it ends. It waits for the lock as long as a write does, at most.
    /// What cannot be written is said, never dropped in silence.
    pub fn put_down(&mut self, cx: &mut Context<Self>) {
        self.end_typing(cx);
        self.edits.later = None;
        let Some(saved) = self.edits.board.as_mut().and_then(write_now) else {
            return;
        };
        match (lost(&saved), saved) {
            (Some(message), _) => toast(Toast::failure(message), cx),
            (None, Ok(Saved::Done { report, .. })) => tell(report, cx),
            (None, _) => {}
        }
        self.show_board();
        cx.notify();
    }

    /// A write done: what did not go as Rature did it is said, the file as written shown.
    fn saved(&mut self, written: io::Result<Written>, cx: &mut Context<Self>) {
        self.edits.writing = false;
        let saved = match written {
            Ok(written) => self
                .edits
                .board
                .as_mut()
                .map(|board| board.written(written)),
            Err(error) => {
                toast(
                    Toast::failure(format!("The canvas cannot be written: {error}")),
                    cx,
                );
                None
            }
        };
        match saved {
            Some(Saved::Done { report, .. }) => tell(report, cx),
            Some(Saved::Invalid(problems)) => {
                self.edits.stalled = true;
                let why = problems
                    .first()
                    .map(ToString::to_string)
                    .unwrap_or_default();
                toast(
                    Toast::failure(format!(
                        "The canvas file is not valid now ({why}): your changes wait for it to be mended"
                    )),
                    cx,
                );
            }
            Some(Saved::Busy) => self.save_later(RETRY, cx),
            None => {}
        }
        self.show_board();
        self.sync_typing(cx);
        if std::mem::take(&mut self.edits.again) {
            self.save(cx);
        }
        cx.notify();
    }
}

impl Edits {
    /// The card selected alone: the one with handles.
    fn alone(&self) -> Option<&str> {
        match self.selected.as_slice() {
            [id] => Some(id),
            _ => None,
        }
    }
}

/// What is not written yet of `board`, written now on this thread; `None` when there was
/// nothing to write.
fn write_now(board: &mut Board) -> Option<io::Result<Saved>> {
    (!board.pending().is_empty()).then(|| board.save())
}

/// What a write made as the board is left says when it could not write: what was not written
/// is lost then.
fn lost(saved: &io::Result<Saved>) -> Option<String> {
    let why = match saved {
        Ok(Saved::Done { .. }) => return None,
        Ok(Saved::Invalid(problems)) => format!(
            "The canvas file is not valid now ({})",
            problems
                .first()
                .map(ToString::to_string)
                .unwrap_or_default()
        ),
        Ok(Saved::Busy) => "Another program kept the canvas busy".into(),
        Err(error) => format!("The canvas cannot be written: {error}"),
    };
    Some(format!("{why}: your last changes to it are not written"))
}

/// What a write could not do as Rature did it, said in a toast; the text typed that the disk
/// did not take, on the clipboard.
fn tell(report: Report, cx: &mut App) {
    let Some((said, clipboard)) = told(&report) else {
        return;
    };
    if let Some(text) = clipboard {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
    }
    toast(Toast::failure(said), cx);
}

/// What [`tell`] says of `report`, and what it puts on the clipboard; `None` for a clean one.
fn told(report: &Report) -> Option<(String, Option<String>)> {
    if report.is_clean() {
        return None;
    }
    let mut said: Vec<String> = report.conflicts.iter().map(ToString::to_string).collect();
    let clipboard = (!report.clipboard.is_empty()).then(|| report.clipboard.join("\n\n"));
    if clipboard.is_some() {
        said.push("what you typed is in the clipboard".into());
    }
    Some((said.join("; "), clipboard))
}

/// How long typing rests before it is written, as the reader's autosave.
const TYPING_REST: Duration = Duration::from_millis(800);

/// Enter, through [`on_keys`]: the card selected alone, if a text, is typed in.
fn on_edit_card(board: &Entity<CanvasView>) -> impl Fn(&EditCard, &mut Window, &mut App) + 'static {
    let board = board.clone();
    move |_, window, cx| board.update(cx, |board, cx| board.edit_selected(window, cx))
}

/// Text cards made by a double click, and their Markdown typed whole where it is read.
impl CanvasView {
    /// Whether a card's text is being typed: the reader's keys are kept from it then.
    pub fn typing(&self) -> bool {
        self.edits.typing.is_some()
    }

    /// A press while a card is typed in, or a double click; true when it is taken. In the card
    /// typed in, its field takes it; anywhere else the typing ends first. A double click on a
    /// text card types in it, the caret where it was; on empty space or a group's background, it
    /// makes a text card there.
    fn press_typing(
        &mut self,
        event: &MouseDownEvent,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) -> bool {
        let (Some(camera), Some(bounds)) = (self.camera, self.bounds) else {
            return false;
        };
        if self.space {
            return false;
        }
        let (x, y) = local(event.position, bounds);
        let label = self.label_line(camera, cx);
        let hit = hit(&self.file.cards, camera, label, self.edits.alone(), x, y);
        if let Some(typing) = &self.edits.typing {
            if hit == Hit::Card(typing.id.clone()) {
                return true;
            }
            self.stop_typing(cx);
        }
        if event.click_count != 2 || event.modifiers.modified() {
            return false;
        }
        match hit {
            Hit::Card(id) if self.is_text(&id) => {
                self.type_in(id, Caret::At(event.position), window, cx)
            }
            Hit::Empty | Hit::Group(_) => {
                let (x, y) = camera.to_canvas(x, y);
                self.make_card(x, y, window, cx);
            }
            _ => return false,
        }
        // The page, which holds the keys otherwise, would take them back on this press.
        window.prevent_default();
        true
    }

    fn is_text(&self, id: &str) -> bool {
        self.file
            .cards
            .iter()
            .any(|card| card.id == id && matches!(card.kind, CardKind::Text(_)))
    }

    /// Enter: the text card selected alone is typed in, the caret at its end.
    fn edit_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        if self.edits.held.is_some() || self.typing() {
            return;
        }
        let Some(id) = self.edits.alone().map(str::to_string) else {
            return;
        };
        if self.is_text(&id) {
            self.type_in(id, Caret::End, window, cx);
        }
    }

    /// The text card `id` typed in, its text as the canvas being edited has it.
    fn type_in(&mut self, id: String, caret: Caret, window: &mut Window, cx: &mut Context<Self>) {
        self.open_board(cx);
        let Some(board) = &self.edits.board else {
            return;
        };
        let Some(text) = board.canvas().node(&id).and_then(Node::text) else {
            return;
        };
        let (text, mark) = (text.to_string(), board.actions());
        self.start_typing(id, text, caret, mark, window, cx);
    }

    /// A text card made at the canvas's point (`x`, `y`), empty, and typed in: written once the
    /// keys rest, undone at once with what is typed in it.
    fn make_card(&mut self, x: f32, y: f32, window: &mut Window, cx: &mut Context<Self>) {
        self.open_board(cx);
        let Some(board) = &self.edits.board else {
            return;
        };
        let mark = board.actions();
        let node = new_card(board.canvas(), x, y);
        let id = node.id().unwrap_or_default().to_string();
        if !self.edit(|_| vec![Op::add_node(node)], cx) {
            return;
        }
        self.show_texts(cx);
        self.save_later(TYPING_REST, cx);
        self.start_typing(id, String::new(), Caret::Start, mark, window, cx);
    }

    fn start_typing(
        &mut self,
        id: String,
        text: String,
        caret: Caret,
        mark: usize,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let folder = self.edits.path.parent().unwrap_or(Path::new("."));
        let anchors = self
            .file
            .cards
            .iter()
            .find(|card| card.id == id)
            .and_then(|card| match &card.kind {
                CardKind::Text(doc) => Some(doc.anchors.keys().cloned().collect()),
                _ => None,
            })
            .unwrap_or_default();
        let links = Rc::new(Links::new(folder.to_path_buf(), anchors));
        let field = cx.new(|cx| LiveBlock::new(text, caret, links, window, cx));
        let events = cx.subscribe_in(
            &field,
            window,
            |this: &mut Self, _, event: &LiveEvent, window, cx| match event {
                LiveEvent::Change => this.typed(cx),
                LiveEvent::Blur => this.stop_typing(cx),
                LiveEvent::Follow(link) => this.follow(link, window, cx),
                // Typed whole: nothing past its edges, no line of an owner to break or join.
                LiveEvent::Leave(_) | LiveEvent::Break(_) | LiveEvent::Join => {}
            },
        );
        let before = window.focused(cx);
        let focus = field.read(cx).focus_handle().clone();
        window.focus(&focus, cx);
        self.edits.selected = vec![id.clone()];
        self.edits.typing = Some(Typing {
            id,
            field,
            mark,
            before,
            window: window.window_handle(),
            _events: events,
        });
        self.tell_reader(cx);
        cx.notify();
    }

    /// The text typed changed: an operation sets it, joined to the one before until a write
    /// begins, which waits for the keys to rest.
    fn typed(&mut self, cx: &mut Context<Self>) {
        let Some(typing) = &self.edits.typing else {
            return;
        };
        let (id, text) = (typing.id.clone(), typing.field.read(cx).value().to_string());
        if self.edit(|canvas| typing_ops(canvas, &id, &text), cx) {
            self.save_later(TYPING_REST, cx);
        }
    }

    /// `Esc`, `Ctrl-Enter`, a press elsewhere, the keys gone elsewhere: the typing ends, and
    /// what was typed is written now.
    fn stop_typing(&mut self, cx: &mut Context<Self>) {
        if self.end_typing(cx) {
            self.save(cx);
        }
    }

    /// The typing ends, nothing written: what it did is one action to undo, the keys go back
    /// where they were unless they went elsewhere, and the card's Markdown is read again. False
    /// when no card was typed in.
    fn end_typing(&mut self, cx: &mut Context<Self>) -> bool {
        let Some(typing) = self.edits.typing.take() else {
            return false;
        };
        if let Some(board) = &mut self.edits.board {
            board.join_since(typing.mark);
        }
        let (field, before) = (typing.field.read(cx).focus_handle().clone(), typing.before);
        let window = typing.window;
        // Not while this window is being updated: it is out of reach then.
        cx.defer(move |cx| {
            window
                .update(cx, |_, window, cx| {
                    if let Some(before) = before
                        && (field.is_focused(window) || window.focused(cx).is_none())
                    {
                        window.focus(&before, cx);
                    }
                })
                .ok();
        });
        self.show_texts(cx);
        self.tell_reader(cx);
        cx.notify();
        true
    }

    /// The reader drawn again, which keeps its keys from a card typed in, or gives them back.
    fn tell_reader(&self, cx: &mut Context<Self>) {
        let reader = self.reader.clone();
        // Not while the reader is updated: it may be the one that called.
        cx.defer(move |cx| {
            reader.update(cx, |_, cx| cx.notify()).ok();
        });
    }

    /// The card typed in, as the canvas being edited has it now. Gone, the typing ends. Its text
    /// changed by another program while nothing typed waits to be written: the disk's wins, and
    /// is typed on. What waits is merged once written: the disk wins there too, and what was
    /// typed goes to the clipboard ([`tell`]); then the disk's is typed on.
    fn sync_typing(&mut self, cx: &mut Context<Self>) {
        let (Some(typing), Some(board)) = (&self.edits.typing, &self.edits.board) else {
            return;
        };
        let text = board.canvas().node(&typing.id).and_then(Node::text);
        let waits = text_waits(board.pending(), &typing.id);
        let field = typing.field.clone();
        match text.map(str::to_string) {
            None => {
                self.end_typing(cx);
            }
            Some(text) if !waits && text != field.read(cx).value() => {
                field.update(cx, |field, cx| field.set_value(text, cx));
            }
            Some(_) => {}
        }
    }

    /// The cards as the canvas being edited has them. One it has that the board does not show
    /// (made, or given back by an undo) is added; a text changed (undone) is read again, off the
    /// window's thread. The card typed in shows its field rather than its Markdown.
    fn show_texts(&mut self, cx: &mut Context<Self>) {
        let Some(board) = &self.edits.board else {
            return;
        };
        let typing = self.edits.typing.as_ref().map(|typing| typing.id.as_str());
        let (mut added, mut read) = (Vec::new(), Vec::new());
        for (index, node) in board.canvas().nodes().iter().enumerate() {
            let Some(id) = node.id() else {
                continue;
            };
            let text = node.text().unwrap_or_default();
            let stale = match self.file.cards.iter().find(|card| card.id == id) {
                Some(Card {
                    kind: CardKind::Text(doc),
                    ..
                }) => doc.source != text,
                Some(_) => false,
                // Shown empty until it is read.
                None => match card(index, node, |_| document::parse("", "", None)) {
                    Some(new) => {
                        let text = matches!(new.kind, CardKind::Text(_));
                        added.push(new);
                        text
                    }
                    None => false,
                },
            };
            if stale && !text.is_empty() && Some(id) != typing {
                read.push((id.to_string(), text.to_string()));
            }
        }
        if !added.is_empty() {
            let file = Arc::make_mut(&mut self.file);
            // An index no card has: the canvas being edited numbers its nodes otherwise.
            let mut next = file.cards.iter().map(|card| card.index + 1).max();
            for mut new in added {
                new.index = next.unwrap_or(0);
                next = Some(new.index + 1);
                // Groups below the rest, as the file has them.
                let at = match new.kind {
                    CardKind::Group => file
                        .cards
                        .iter()
                        .take_while(|card| matches!(card.kind, CardKind::Group))
                        .count(),
                    _ => file.cards.len(),
                };
                file.cards.insert(at, new);
            }
            self.relay_edges();
            cx.notify();
        }
        if read.is_empty() {
            return;
        }
        let (ink, folder) = (
            render::ink(cx),
            self.edits.path.parent().map(Path::to_path_buf),
        );
        let work = cx.background_executor().spawn(async move {
            read.into_iter()
                .map(|(id, text)| {
                    let doc = document::parse(&text, &ink, folder.as_deref());
                    let title = title(&doc, &text);
                    (id, Arc::new(doc), title)
                })
                .collect::<Vec<_>>()
        });
        cx.spawn(async move |this, cx| {
            let read = work.await;
            this.update(cx, |this, cx| this.texts_read(read, cx)).ok();
        })
        .detach();
    }

    /// Markdown read off the window's thread, put on its cards unless their text changed again
    /// meanwhile.
    fn texts_read(&mut self, read: Vec<(String, Arc<Document>, String)>, cx: &mut Context<Self>) {
        let disk = self.file.canvas.clone();
        let canvas = self.edits.board.as_ref().map_or(&*disk, Board::canvas);
        let now: Vec<(String, Arc<Document>, String)> = read
            .into_iter()
            .filter(|(id, doc, _)| canvas.node(id).and_then(Node::text) == Some(&doc.source))
            .collect();
        if now.is_empty() {
            return;
        }
        let file = Arc::make_mut(&mut self.file);
        for (id, doc, title) in now {
            if let Some(card) = file.cards.iter_mut().find(|card| card.id == id) {
                card.kind = CardKind::Text(doc);
                card.title = title;
            }
        }
        cx.notify();
    }

    fn is_typed(&self, card: &Card) -> bool {
        self.edits
            .typing
            .as_ref()
            .is_some_and(|typing| typing.id == card.id)
    }

    /// A text card's body: its Markdown read, or the field it is typed in.
    fn card_text(&self, ctx: &Ctx<Self>, card: &Card, doc: &Document, body: Div) -> Div {
        match &self.edits.typing {
            Some(typing) if typing.id == card.id => body
                .key_context("CanvasCard")
                .on_action(
                    ctx.cx
                        .listener(|this, _: &StopTyping, _, cx| this.stop_typing(cx)),
                )
                .on_action(ctx.cx.listener(|this, _: &WriteCard, _, cx| this.save(cx)))
                .child(typing.field.clone()),
            _ => body.children(doc.blocks.iter().map(|block| render::block(ctx, block))),
        }
    }

    /// The card typed in, at its whole Markdown whatever the zoom: it keeps its field, and the
    /// keys. `None` at the full detail, where it is drawn with the others.
    fn typed_card(
        &self,
        detail: Detail,
        camera: Camera,
        p: &Palette,
        config: &Config,
        cx: &mut Context<Self>,
    ) -> Option<AnyElement> {
        let typing = self.edits.typing.as_ref()?;
        if detail == Detail::Full {
            return None;
        }
        let card = self.file.cards.iter().find(|card| card.id == typing.id)?;
        let zoom = camera.text_zoom(config.font_size);
        let ctx = Ctx::new(p, config, zoom, &self.drawn, None, cx);
        Some(self.full_card(&ctx, card, camera, p))
    }
}

/// A new text card, empty, at its default size, its middle at the canvas's point (`x`, `y`):
/// where a double click asked for it.
pub fn new_card(canvas: &Canvas, x: f32, y: f32) -> Node {
    let (width, height) = model::default_size(NodeKind::Text, Some(""));
    let rect = Rect {
        x: (f64::from(x) - width / 2.).round(),
        y: (f64::from(y) - height / 2.).round(),
        width,
        height,
    };
    Node::new_text(&canvas.new_id(), "", rect)
}

/// The operation that gives the card `id` of `canvas` the text `text`: none when it has it.
fn typing_ops(canvas: &Canvas, id: &str, text: &str) -> Vec<Op> {
    Op::set_text(canvas, id, text).into_iter().collect()
}

/// Whether a text typed in the card `id` waits in `pending` to be written.
fn text_waits(pending: &[Op], id: &str) -> bool {
    pending.iter().any(|op| {
        matches!(op, Op::Set { target: Target::Node, id: set, fields }
            if set == id && fields.iter().any(|field| field.key == "text"))
    })
}

/// A corner of a card.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Corner {
    TopLeft,
    TopRight,
    BottomLeft,
    BottomRight,
}

impl Corner {
    const ALL: [Self; 4] = [
        Self::TopLeft,
        Self::TopRight,
        Self::BottomLeft,
        Self::BottomRight,
    ];

    fn left(self) -> bool {
        matches!(self, Self::TopLeft | Self::BottomLeft)
    }

    fn top(self) -> bool {
        matches!(self, Self::TopLeft | Self::TopRight)
    }

    /// Where it is on `area`.
    fn of(self, area: Area) -> (f32, f32) {
        (
            if self.left() {
                area.x
            } else {
                area.x + area.width
            },
            if self.top() {
                area.y
            } else {
                area.y + area.height
            },
        )
    }
}

/// What a press on the board is on.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Hit {
    /// A handle of the card selected alone.
    Handle(String, Corner),
    /// A card, or a group's label.
    Card(String),
    /// A group's background: a drag there pans, unless the group is selected.
    Group(String),
    Empty,
}

/// What a press at the board's point (`x`, `y`) is on: a handle of the card selected alone
/// (`alone`), the topmost card that is not a group, a group's label (`label` high on screen,
/// above it), a group's background, or nothing. A card without an id is never selected.
fn hit(cards: &[Card], camera: Camera, label: f32, alone: Option<&str>, x: f32, y: f32) -> Hit {
    if let Some(id) = alone
        && let Some(card) = cards.iter().find(|card| card.id == id)
        && let Some(corner) = handle_at(camera.on_screen(card.area), x, y)
    {
        return Hit::Handle(id.into(), corner);
    }
    let (at_x, at_y) = camera.to_canvas(x, y);
    let named = || cards.iter().rev().filter(|card| !card.id.is_empty());
    let group = |card: &&Card| matches!(card.kind, CardKind::Group);
    if let Some(card) = named()
        .filter(|card| !group(card))
        .find(|card| card.area.contains(at_x, at_y))
    {
        return Hit::Card(card.id.clone());
    }
    let labelled = |card: &&Card| {
        let area = camera.on_screen(card.area);
        let title = Area {
            y: area.y - label,
            height: label,
            ..area
        };
        label > 0. && title.contains(x, y)
    };
    if let Some(card) = named().filter(group).find(labelled) {
        return Hit::Card(card.id.clone());
    }
    named()
        .filter(group)
        .find(|card| card.area.contains(at_x, at_y))
        .map_or(Hit::Empty, |card| Hit::Group(card.id.clone()))
}

/// Whether a card this large on screen has handles: a small one is all handles otherwise, and
/// could not be moved.
fn handles_shown(area: Area) -> bool {
    area.width >= 3. * HANDLE && area.height >= 3. * HANDLE
}

/// The handle of a card at `area` on screen that the screen's point (`x`, `y`) is on.
fn handle_at(area: Area, x: f32, y: f32) -> Option<Corner> {
    if !handles_shown(area) {
        return None;
    }
    Corner::ALL.into_iter().find(|corner| {
        let (at_x, at_y) = corner.of(area);
        (x - at_x).abs() <= REACH && (y - at_y).abs() <= REACH
    })
}

/// The square of a handle, on screen.
fn handle(corner: Corner, area: Area) -> Area {
    let (x, y) = corner.of(area);
    Area {
        x: x - HANDLE / 2.,
        y: y - HANDLE / 2.,
        width: HANDLE,
        height: HANDLE,
    }
}

/// Whether the pointer went far enough from where it was pressed to drag.
fn far(delta: Point<Pixels>) -> bool {
    f32::from(delta.x).hypot(f32::from(delta.y)) > SLOP
}

/// The cards a drag of the cards `ids` moves, from where they are: them, and what lies wholly
/// inside a group among them, each once.
fn carried(cards: &[Card], ids: &[String]) -> Vec<(String, Area)> {
    let within = |inner: Area, outer: Area| {
        inner.x >= outer.x
            && inner.y >= outer.y
            && inner.x + inner.width <= outer.x + outer.width
            && inner.y + inner.height <= outer.y + outer.height
    };
    let mut seen = HashSet::new();
    let mut held = Vec::new();
    for id in ids {
        let Some(card) = cards.iter().find(|card| card.id == *id) else {
            continue;
        };
        let inside: Vec<&Card> = match card.kind {
            CardKind::Group => cards
                .iter()
                .filter(|inner| !inner.id.is_empty() && within(inner.area, card.area))
                .collect(),
            _ => Vec::new(),
        };
        for card in std::iter::once(card).chain(inside) {
            if seen.insert(card.id.as_str()) {
                held.push((card.id.clone(), card.area));
            }
        }
    }
    held
}

/// `area` moved by (`dx`, `dy`), its place rounded to the pixel as the file keeps it.
fn moved(area: Area, dx: f32, dy: f32) -> Area {
    Area {
        x: (area.x + dx).round(),
        y: (area.y + dy).round(),
        ..area
    }
}

/// `start` resized by its `corner` dragged by (`dx`, `dy`): the opposite corner stays, the
/// edges dragged rounded to the pixel, never smaller than [`MIN_SIZE`] (or than it was).
fn resized(start: Area, corner: Corner, dx: f32, dy: f32) -> Area {
    let (least_width, least_height) = (MIN_SIZE.min(start.width), MIN_SIZE.min(start.height));
    let (mut left, mut right) = (start.x, start.x + start.width);
    let (mut top, mut bottom) = (start.y, start.y + start.height);
    if corner.left() {
        left = (left + dx).round().min(right - least_width);
    } else {
        right = (right + dx).round().max(left + least_width);
    }
    if corner.top() {
        top = (top + dy).round().min(bottom - least_height);
    } else {
        bottom = (bottom + dy).round().max(top + least_height);
    }
    Area {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    }
}

/// The operations that put each node of `to` at its area, from where `canvas` has it: a move
/// when its place changes, a resize when its size does.
fn placing(canvas: &Canvas, to: &[(String, Area)]) -> Vec<Op> {
    // A coordinate left as it was keeps the file's own number, which an f32 may not hold.
    let keep = |now: f64, to: f32| {
        if now as f32 == to { now } else { f64::from(to) }
    };
    let mut ops = Vec::new();
    for (id, area) in to {
        let Some(now) = canvas.node(id).and_then(|node| node.rect()) else {
            continue;
        };
        ops.extend(Op::move_node(
            canvas,
            id,
            keep(now.x, area.x),
            keep(now.y, area.y),
        ));
        ops.extend(Op::resize_node(
            canvas,
            id,
            keep(now.width, area.width),
            keep(now.height, area.height),
        ));
    }
    ops
}

/// The operations that delete the nodes `ids` of `canvas` with their edges, each made against
/// the canvas the ones before it leave, as [`Board::edit_all`] wants them: an edge between two
/// of them is deleted once.
fn deleting(canvas: &Canvas, ids: &[String]) -> Vec<Op> {
    let mut left = canvas.clone();
    ids.iter()
        .filter_map(|id| {
            let op = Op::delete_node(&left, id)?;
            op.apply(&mut left, &mut Report::default());
            Some(op)
        })
        .collect()
}

/// `div` set at `area` of the board.
fn placed(div: Stateful<Div>, area: Area) -> Stateful<Div> {
    div.absolute()
        .left(px(area.x))
        .top(px(area.y))
        .w(px(area.width))
        .h(px(area.height))
}

/// A group: a tinted background, its label above its top left corner in `label` type.
fn group(card: &Card, area: Area, color: Option<Hsla>, label: Pixels, p: &Palette) -> AnyElement {
    let line = label * 1.4;
    div()
        .id(("card", card.index))
        .absolute()
        .left(px(area.x))
        .top(px(area.y) - line)
        .w(px(area.width))
        .h(px(area.height) + line)
        .flex()
        .flex_col()
        .child(
            div()
                .h(line)
                .flex_none()
                .overflow_hidden()
                .whitespace_nowrap()
                .text_ellipsis()
                .text_size(label)
                .line_height(line)
                .text_color(color.unwrap_or(p.mute))
                .child(card.title.clone()),
        )
        .child(
            div()
                .flex_1()
                .rounded(px(6.))
                .border_1()
                .border_color(color.map_or(p.line2, |c| c.opacity(0.6)))
                .bg(color.map_or(p.text.opacity(0.03), |c| c.opacity(0.08))),
        )
        .into_any_element()
}

/// `position`, in the window, as a point of the board at `bounds`.
fn local(position: Point<Pixels>, bounds: Bounds<Pixels>) -> (f32, f32) {
    let at = position - bounds.origin;
    (f32::from(at.x), f32::from(at.y))
}

/// An edge's line, in the canvas's pixels, and the hovered one's.
const EDGE_WIDTH: f32 = 2.;
const HOVERED_WIDTH: f32 = 3.;
/// How near the pointer an edge is hovered, on screen.
const HOVER: f32 = 6.;
/// A label's widest, and the height of the room it is centred in, in the canvas's pixels.
const LABEL_WIDTH: f32 = 260.;
const LABEL_HEIGHT: f32 = 200.;

/// Where labels are drawn, in the window, by their edge's place in the file's `edges`.
type Labels = Rc<RefCell<Vec<(usize, Bounds<Pixels>)>>>;

/// An edge's shapes, tessellated at a zoom and a width in the canvas's pixels scaled by it, from
/// the canvas's origin: a pan only moves them, it does not tessellate them again. A card moved
/// moves its edges' curves: they are tessellated again.
struct Built {
    zoom: f32,
    width: f32,
    curve: [edges::Point; 4],
    stroke: Option<gpui_kit::Path<Pixels>>,
    arrows: Option<gpui_kit::Path<Pixels>>,
}

impl Built {
    fn new(line: &Line, zoom: f32, width: f32) -> Self {
        let at = |p: edges::Point| point(px(p.x * zoom), px(p.y * zoom));
        let mut stroke = PathBuilder::stroke(px(width));
        stroke.move_to(at(line.from));
        stroke.cubic_bezier_to(at(line.to), at(line.c1), at(line.c2));
        let ends: Vec<[edges::Point; 3]> =
            line.from_arrow.into_iter().chain(line.to_arrow).collect();
        let arrows = (!ends.is_empty())
            .then(|| {
                let mut fill = PathBuilder::fill();
                for arrow in ends {
                    fill.add_polygon(&arrow.map(at), true);
                }
                fill.build().ok()
            })
            .flatten();
        Self {
            zoom,
            width,
            curve: curve(line),
            stroke: stroke.build().ok(),
            arrows,
        }
    }
}

/// What a line's shapes follow: its curve's ends and control points.
fn curve(line: &Line) -> [edges::Point; 4] {
    [line.from, line.c1, line.c2, line.to]
}

/// `path` moved by `by`.
fn shifted(path: &gpui_kit::Path<Pixels>, by: Point<Pixels>) -> gpui_kit::Path<Pixels> {
    let mut path = path.clone();
    path.bounds.origin += by;
    for vertex in &mut path.vertices {
        vertex.xy_position += by;
    }
    path
}

/// The edges: painted below the cards, their labels, the one under the pointer.
impl CanvasView {
    /// The edges in the canvas's area `seen` added to `board`, below the cards it gets next:
    /// their curves and arrows in one element, then their labels at the full detail only, each
    /// with text `font_size` at zoom 1. The hovered edge is highlighted.
    #[allow(clippy::too_many_arguments)]
    fn edges(
        &self,
        board: Stateful<Div>,
        seen: &Area,
        camera: Camera,
        detail: Detail,
        font_size: f32,
        p: &Palette,
        cx: &mut Context<Self>,
    ) -> Stateful<Div> {
        let shown: Vec<(usize, Hsla, f32)> = self
            .file
            .edges
            .iter()
            .enumerate()
            .filter(|(_, edge)| area(edge.line.bounds).meets(seen))
            .map(|(i, edge)| {
                let color = edge.tint.map_or(p.mute, |t| tint(p, t));
                if self.hovered == Some(i) {
                    (i, lighter(color), HOVERED_WIDTH)
                } else {
                    (i, color, EDGE_WIDTH)
                }
            })
            .collect();
        self.labels.borrow_mut().clear();
        let labels: Vec<AnyElement> = match detail {
            Detail::Full => {
                let size = px(camera.text_zoom(font_size) * font_size);
                shown
                    .iter()
                    .filter_map(|&(i, _, _)| {
                        let edge = &self.file.edges[i];
                        let text = edge.label.as_ref()?;
                        Some(label(i, edge, text, camera, size, p, self.labels.clone()))
                    })
                    .collect()
            }
            Detail::Light | Detail::Rects => Vec::new(),
        };
        let (file, paths, this) = (
            self.file.clone(),
            self.paths.clone(),
            cx.entity().downgrade(),
        );
        let zoom = camera.zoom;
        let layer = canvas(
            |bounds, window, _| window.insert_hitbox(bounds, HitboxBehavior::Normal),
            move |bounds, hitbox, window, _| {
                let by = bounds.origin - point(px(camera.x * zoom), px(camera.y * zoom));
                let mut paths = paths.borrow_mut();
                for (i, color, width) in shown {
                    let edge = &file.edges[i];
                    let width = (width * zoom).max(1.);
                    let built = paths
                        .entry(edge.index)
                        .or_insert_with(|| Built::new(&edge.line, zoom, width));
                    if built.zoom != zoom
                        || built.width != width
                        || built.curve != curve(&edge.line)
                    {
                        *built = Built::new(&edge.line, zoom, width);
                    }
                    for path in [&built.stroke, &built.arrows].into_iter().flatten() {
                        window.paint_path(shifted(path, by), color);
                    }
                }
                window.on_mouse_event(move |event: &MouseMoveEvent, phase, window, cx| {
                    if phase == DispatchPhase::Bubble {
                        let over = hitbox.is_hovered(window);
                        this.update(cx, |this, cx| this.hover(event.position, over, cx))
                            .ok();
                    }
                });
            },
        )
        .absolute()
        .size_full();
        board.child(layer).children(labels)
    }

    /// The edges laid again on the cards as the board shows them, edited or held by the pointer.
    fn relay_edges(&mut self) {
        let disk = self.file.canvas.clone();
        let canvas = self.edits.board.as_ref().map_or(&*disk, Board::canvas);
        let edges = self.file.laid_edges(canvas);
        if edges != self.file.edges {
            if edges.len() != self.file.edges.len() {
                self.hovered = None;
            }
            Arc::make_mut(&mut self.file).edges = edges;
        }
    }

    /// The pointer moved to `position`, `over` the board or not: the edge under it is
    /// highlighted.
    fn hover(&mut self, position: Point<Pixels>, over: bool, cx: &mut Context<Self>) {
        let hovered = if over {
            self.edge_under(position, cx)
        } else {
            None
        };
        if hovered != self.hovered {
            self.hovered = hovered;
            cx.notify();
        }
    }

    /// The edge within `HOVER` of `position` on screen, unless a card, a handle or a group's
    /// label is there, as a press finds them: they come first, and hide the edges below them.
    fn edge_under(&self, position: Point<Pixels>, cx: &App) -> Option<usize> {
        let (Some(camera), Some(bounds)) = (self.camera, self.bounds) else {
            return None;
        };
        if self.panning.is_some() || self.edits.held.is_some() {
            return None;
        }
        let (x, y) = local(position, bounds);
        let label = self.label_line(camera, cx);
        match hit(&self.file.cards, camera, label, self.edits.alone(), x, y) {
            Hit::Handle(..) | Hit::Card(_) => return None,
            Hit::Group(_) | Hit::Empty => {}
        }
        let labels = self.labels.borrow();
        if let Some(&(i, _)) = labels.iter().find(|(_, at)| at.contains(&position)) {
            return Some(i);
        }
        let (x, y) = camera.to_canvas(x, y);
        self.file.edge_at(x, y, HOVER / camera.zoom)
    }
}

/// A hovered edge's colour: its own, lighter, so that its tint still says what it says.
fn lighter(color: Hsla) -> Hsla {
    hsla(color.h, color.s, (color.l + 0.15).min(1.), color.a)
}

/// The label of the edge at `i` in the file's `edges`: a box on the board's colour at the curve's
/// middle, its text `size`; where it is drawn is pushed to `drawn`, for the pointer.
fn label(
    i: usize,
    edge: &EdgeLine,
    text: &str,
    camera: Camera,
    size: Pixels,
    p: &Palette,
    drawn: Labels,
) -> AnyElement {
    let middle = edge.line.middle();
    let (x, y) = camera.to_screen(middle.x, middle.y);
    let zoom = camera.zoom;
    let (width, height) = (LABEL_WIDTH * zoom, LABEL_HEIGHT * zoom);
    div()
        .absolute()
        .left(px(x - width / 2.))
        .top(px(y - height / 2.))
        .w(px(width))
        .h(px(height))
        .flex()
        .items_center()
        .justify_center()
        .child(
            div()
                .max_w(px(width))
                .px(px(6. * zoom))
                .py(px(2. * zoom))
                .rounded(px(4. * zoom))
                .bg(p.bg)
                .text_size(size)
                .line_height(size * 1.4)
                .text_color(p.text)
                .text_center()
                .relative()
                .child(text.to_string())
                .child(
                    canvas(
                        move |bounds, _, _| drawn.borrow_mut().push((i, bounds)),
                        |_, _, _, _| {},
                    )
                    .absolute()
                    .top_0()
                    .left_0()
                    .size_full(),
                ),
        )
        .into_any_element()
}

impl Host for CanvasView {
    fn link_clicked(&mut self, link: String) {
        self.clicked_link = Some(link);
    }

    fn follow(&mut self, link: &str, window: &mut Window, cx: &mut Context<Self>) {
        let (reader, link) = (self.reader.clone(), link.to_string());
        // The reader may open another file and let this board go: not while it is updated.
        window.defer(cx, move |window, cx| {
            reader
                .update(cx, |reader, cx| reader.follow_link(&link, window, cx))
                .ok();
        });
    }

    fn copy(&mut self, text: String, what: &str, cx: &mut Context<Self>) {
        cx.write_to_clipboard(ClipboardItem::new_string(text));
        toast(Toast::success(what.to_string()), cx);
    }
}

impl Render for CanvasView {
    fn render(&mut self, _: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let started = timing::on().then(Instant::now);
        let p = palette(cx);
        let config = cx.global::<Config>().clone();
        let this = cx.entity().downgrade();
        let watch = this.clone();
        // Where the board is: what the camera frames, and the cards it shows.
        let tracker = canvas(
            move |bounds, window, cx| {
                let moved = watch
                    .update(cx, |this, _| this.laid_out(bounds))
                    .unwrap_or(false);
                // A notify while the window draws is lost: gpui-fast marks a window to be drawn
                // again only between two draws. The next frame asks for it.
                if moved {
                    window.on_next_frame(move |_, cx| {
                        watch.update(cx, |_, cx| cx.notify()).ok();
                    });
                }
            },
            |_, _, _, _| {},
        )
        .absolute()
        .size_full();
        let mut board = div()
            .id("board")
            .relative()
            .size_full()
            .overflow_hidden()
            .bg(p.bg)
            .on_mouse_down(MouseButton::Left, cx.listener(Self::press))
            .on_scroll_wheel(cx.listener(Self::wheel))
            .on_pinch(cx.listener(Self::pinch))
            .when(self.panning.is_some(), |d| {
                d.cursor(CursorStyle::ClosedHand)
            })
            .when(self.space && self.panning.is_none(), |d| {
                d.cursor(CursorStyle::OpenHand)
            })
            .when_some(self.held_cursor(), |d, cursor| d.cursor(cursor))
            .child(tracker);
        let (Some(camera), Some(bounds)) = (self.camera, self.bounds) else {
            return board;
        };
        let seen = camera.seen(
            f32::from(bounds.size.width),
            f32::from(bounds.size.height),
            MARGIN,
        );
        let shown: Vec<&Card> = self
            .file
            .cards
            .iter()
            .filter(|card| card.area.meets(&seen))
            .collect();
        let detail = Detail::at(camera.zoom, self.gesturing);
        board = self.edges(board, &seen, camera, detail, config.font_size, &p, cx);
        board = match detail {
            Detail::Full => {
                let zoom = camera.text_zoom(config.font_size);
                let ctx = Ctx::new(&p, &config, zoom, &self.drawn, None, cx);
                board.children(
                    shown
                        .iter()
                        .map(|card| self.full_card(&ctx, card, camera, &p)),
                )
            }
            // The card typed in is drawn whole, after them.
            Detail::Light => board.children(
                shown
                    .iter()
                    .filter(|card| !self.is_typed(card))
                    .map(|card| Self::light_card(card, camera, &p)),
            ),
            Detail::Rects => {
                let shown: Vec<&Card> = shown
                    .iter()
                    .copied()
                    .filter(|card| !self.is_typed(card))
                    .collect();
                board.child(Self::rects(&shown, camera, &p))
            }
        };
        board = board.children(self.typed_card(detail, camera, &p, &config, cx));
        board = board.children(self.selection(camera, &p));
        // A pan, or cards held, go on wherever the pointer goes, over the board or not.
        if self.panning.is_some() || self.edits.held.is_some() {
            board = board.child(
                canvas(
                    |_, _, _| (),
                    move |_, (), window, _| {
                        let moved = this.clone();
                        window.on_mouse_event(move |event: &MouseMoveEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble {
                                moved
                                    .update(cx, |this, cx| this.drag(event.position, cx))
                                    .ok();
                            }
                        });
                        let released = this.clone();
                        window.on_mouse_event(move |event: &MouseUpEvent, phase, _, cx| {
                            if phase == DispatchPhase::Bubble && event.button == MouseButton::Left {
                                released.update(cx, |this, cx| this.release(cx)).ok();
                            }
                        });
                    },
                )
                .absolute()
                .size_0(),
            );
        }
        if let Some(started) = started {
            let (count, total, built) = (shown.len(), self.file.cards.len(), started.elapsed());
            // Painted last: the frame's cost on the main thread, its layout included.
            board = board.child(
                canvas(
                    |_, _, _| (),
                    move |_, (), _, _| {
                        let ms = |d: Duration| d.as_secs_f64() * 1000.;
                        eprintln!(
                            "rature: canvas frame: {count} of {total} cards, {detail:?} at zoom {:.2} from ({:.0}, {:.0}): built in {:.2} ms, painted {:.2} ms after",
                            camera.zoom,
                            camera.x,
                            camera.y,
                            ms(built),
                            ms(started.elapsed()),
                        );
                    },
                )
                .absolute()
                .size_0(),
            );
        }
        board
    }
}

#[cfg(test)]
mod tests {
    // Not the glob: GPUI's own `test` attribute would come with it.
    use super::{
        Area, Camera, CanvasFile, Corner, Detail, HANDLE, Hit, MAX_ZOOM, MIN_SIZE, MIN_ZOOM, ROOM,
        STEP, Tint, carried, deleting, handle_at, hit, moved, placing, resized, tint,
    };
    use super::{lost, write_now};
    use crate::canvas::Canvas;
    use crate::canvas_board::{Board, Saved};
    use crate::canvas_ops::{Op, Report};
    use gpui_design_system::Palette;

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    #[test]
    fn everything_is_framed_in_the_middle_never_closer_than_zoom_1() {
        let all = Area {
            x: -500.,
            y: 0.,
            width: 2000.,
            height: 500.,
        };
        let camera = Camera::framing(all, 1096., 900.);
        // The width decides: 1000 px of room for 2000 of canvas.
        assert!(close(camera.zoom, 0.5), "{camera:?}");
        let (left, top) = camera.to_screen(all.x, all.y);
        let (right, bottom) = camera.to_screen(all.x + all.width, all.y + all.height);
        assert!(
            close(left, ROOM) && close(right, 1096. - ROOM),
            "{left} {right}"
        );
        assert!(close(top, 900. - bottom), "centred: {top} {bottom}");
        // A single small card: shown at zoom 1, in the middle.
        let small = Area {
            x: 0.,
            y: 0.,
            width: 100.,
            height: 50.,
        };
        let camera = Camera::framing(small, 1000., 800.);
        assert_eq!(camera.zoom, 1.);
        assert_eq!(camera.to_screen(50., 25.), (500., 400.));
        // A huge canvas: no further than the furthest zoom.
        let huge = Area {
            width: 1e6,
            ..small
        };
        assert_eq!(Camera::framing(huge, 1000., 800.).zoom, MIN_ZOOM);
    }

    #[test]
    fn zooming_keeps_the_point_under_the_pointer_in_place() {
        let camera = Camera {
            x: 120.,
            y: -40.,
            zoom: 0.8,
        };
        let at = (300., 210.);
        let under = camera.to_canvas(at.0, at.1);
        for factor in [STEP, 1. / STEP, 3., 0.2] {
            let zoomed = camera.zoomed(factor, at);
            let (x, y) = zoomed.to_canvas(at.0, at.1);
            assert!(
                close(x, under.0) && close(y, under.1),
                "{factor}: {zoomed:?}"
            );
        }
        // Within its limits, and still around the pointer at them.
        let near = camera.zoomed(100., at);
        assert_eq!(near.zoom, MAX_ZOOM);
        let (x, y) = near.to_canvas(at.0, at.1);
        assert!(close(x, under.0) && close(y, under.1));
        assert_eq!(camera.zoomed(0.001, at).zoom, MIN_ZOOM);
        // Back to 1 around the pointer.
        let reset = camera.zoomed_to(1., at);
        assert_eq!(reset.zoom, 1.);
        let (x, _) = reset.to_canvas(at.0, at.1);
        assert!(close(x, under.0));
    }

    #[test]
    fn a_drag_moves_the_canvas_with_the_pointer() {
        let camera = Camera {
            x: 0.,
            y: 0.,
            zoom: 0.5,
        };
        let before = camera.to_screen(100., 100.);
        let after = camera.panned(30., -20.).to_screen(100., 100.);
        assert!(close(after.0 - before.0, 30.) && close(after.1 - before.1, -20.));
    }

    #[test]
    fn only_the_cards_in_sight_and_a_margin_are_built() {
        let camera = Camera {
            x: 0.,
            y: 0.,
            zoom: 0.5,
        };
        // 800 by 600 on screen, 100 around: the canvas from -200 to 1800 and -200 to 1400.
        let seen = camera.seen(800., 600., 100.);
        assert_eq!(
            seen,
            Area {
                x: -200.,
                y: -200.,
                width: 2000.,
                height: 1600.
            }
        );
        let card = |x: f32, y: f32| Area {
            x,
            y,
            width: 100.,
            height: 100.,
        };
        assert!(card(0., 0.).meets(&seen));
        assert!(card(-290., 500.).meets(&seen), "in the margin");
        assert!(card(1750., 1350.).meets(&seen), "across the corner");
        assert!(!card(-301., 0.).meets(&seen));
        assert!(!card(0., 1401.).meets(&seen));
        assert!(!card(1801., 0.).meets(&seen));
    }

    #[test]
    fn text_is_sized_to_half_a_pixel() {
        let camera = |zoom| Camera { x: 0., y: 0., zoom };
        assert_eq!(camera(1.).text_zoom(16.), 1.);
        // 16 × 0.77 = 12.32: drawn at 12.5.
        assert!(close(camera(0.77).text_zoom(16.) * 16., 12.5));
        assert!(close(camera(0.76).text_zoom(16.) * 16., 12.));
        assert!(close(camera(0.6).text_zoom(15.) * 15., 9.));
    }

    #[test]
    fn the_detail_follows_the_zoom_and_a_gesture_is_light() {
        assert_eq!(Detail::at(1., false), Detail::Full);
        assert_eq!(Detail::at(0.5, false), Detail::Full);
        assert_eq!(Detail::at(0.49, false), Detail::Light);
        assert_eq!(Detail::at(0.2, false), Detail::Light);
        assert_eq!(Detail::at(0.19, false), Detail::Rects);
        assert_eq!(Detail::at(2., true), Detail::Light);
        assert_eq!(Detail::at(0.1, true), Detail::Rects);
    }

    #[test]
    fn a_kept_camera_comes_back_within_limits() {
        let camera = Camera {
            x: 10.,
            y: -4.5,
            zoom: 0.3,
        };
        assert_eq!(Camera::from_kept(camera.kept()), Some(camera));
        assert_eq!(Camera::from_kept([0., 0., 40.]).unwrap().zoom, MAX_ZOOM);
        assert_eq!(Camera::from_kept([f32::NAN, 0., 1.]), None);
    }

    #[test]
    fn a_canvas_is_read_into_cards_groups_first() {
        let text = r##"{"nodes":[
            {"id":"a","type":"text","text":"Intro line\n\n## Plan\n\nMore.","x":0,"y":0,"width":200,"height":100,"color":"2"},
            {"id":"g","type":"group","label":"Ideas","x":-50,"y":-50,"width":500,"height":400},
            {"id":"b","type":"text","text":"- first item\n- second","x":300,"y":200,"width":100,"height":100,"color":"#ff0000"},
            {"id":"f","type":"file","file":"notes/a.md","x":0,"y":500,"width":100,"height":100},
            {"id":"n","type":"text","text":"no place"}
        ],"edges":[]}"##;
        let file = CanvasFile::read(text, "#000000", None).unwrap();
        let ids: Vec<usize> = file.cards.iter().map(|c| c.index).collect();
        // The group below the rest; the card without a place left out.
        assert_eq!(ids, [1, 0, 2, 3]);
        let titles: Vec<&str> = file.cards.iter().map(|c| c.title.as_str()).collect();
        assert_eq!(titles, ["Ideas", "Plan", "first item", "notes/a.md"]);
        assert_eq!(file.cards[1].tint, Some(Tint::Preset(2)));
        assert_eq!(file.cards[2].tint, Some(Tint::Rgb([255, 0, 0])));
        assert_eq!(
            file.all,
            Some(Area {
                x: -50.,
                y: -50.,
                width: 500.,
                height: 650.
            })
        );
        // The group's background is the group's, the card over it the card's.
        let camera = Camera {
            x: -100.,
            y: -100.,
            zoom: 1.,
        };
        let at = |x: f32, y: f32| hit(&file.cards, camera, 0., None, x + 100., y + 100.);
        assert_eq!(at(-20., -20.), Hit::Group("g".into()));
        assert_eq!(at(10., 10.), Hit::Card("a".into()));
        assert_eq!(at(-80., -80.), Hit::Empty);
    }

    /// Cards `a` and `b`, an edge between them and one from `b` to `c`, and a group `g` holding
    /// `a` alone.
    const BOARD: &str = r##"{"nodes":[
        {"id":"g","type":"group","label":"Group","x":-20,"y":-20,"width":300,"height":200},
        {"id":"a","type":"text","text":"A","x":0,"y":0,"width":200,"height":100},
        {"id":"b","type":"text","text":"B","x":400,"y":0,"width":200,"height":100},
        {"id":"c","type":"text","text":"C","x":800,"y":0,"width":200,"height":100}
    ],"edges":[
        {"id":"ab","fromNode":"a","toNode":"b"},
        {"id":"bc","fromNode":"b","toNode":"c"}
    ]}"##;

    fn board() -> (Canvas, CanvasFile) {
        (
            Canvas::read(BOARD),
            CanvasFile::read(BOARD, "#000000", None).unwrap(),
        )
    }

    fn ids(names: &[&str]) -> Vec<String> {
        names.iter().map(|name| name.to_string()).collect()
    }

    fn rect(canvas: &Canvas, id: &str) -> (f64, f64, f64, f64) {
        let r = canvas.node(id).unwrap().rect().unwrap();
        (r.x, r.y, r.width, r.height)
    }

    fn applied(canvas: &Canvas, ops: &[Op]) -> Canvas {
        let mut canvas = canvas.clone();
        let mut report = Report::default();
        for op in ops {
            op.apply(&mut canvas, &mut report);
        }
        assert!(report.is_clean());
        canvas
    }

    /// The board left: what is pending is written at once; when the file is no more a valid
    /// canvas, nothing is, and it is said.
    #[test]
    fn what_is_pending_is_written_when_the_board_is_left_or_said_lost() {
        let folder = std::env::temp_dir().join(format!("rature-view-leave-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&folder);
        std::fs::create_dir_all(&folder).unwrap();
        let path = folder.join("a.canvas");
        std::fs::write(&path, BOARD).unwrap();
        let mut board = Board::open_with_locks(&path, folder.join("locks")).unwrap();
        assert!(write_now(&mut board).is_none(), "nothing pending");
        let nudge = |board: &mut Board, x: f32| {
            let file = CanvasFile::read(BOARD, "#000000", None).unwrap();
            let to: Vec<(String, Area)> = carried(&file.cards, &ids(&["b"]))
                .into_iter()
                .map(|(id, area)| (id, moved(area, x, 0.)))
                .collect();
            let ops = placing(board.canvas(), &to);
            board.edit_all(ops);
        };
        nudge(&mut board, 1.);
        let saved = write_now(&mut board).unwrap();
        assert_eq!(lost(&saved), None);
        assert!(matches!(saved, Ok(Saved::Done { written: true, .. })));
        let read = || Canvas::read(&std::fs::read_to_string(&path).unwrap());
        assert_eq!(rect(&read(), "b").0, 401.);
        assert!(board.pending().is_empty());
        // Another program leaves two nodes with one id: not a canvas to write.
        let broken = BOARD.replace(r#""id":"c""#, r#""id":"a""#);
        std::fs::write(&path, &broken).unwrap();
        nudge(&mut board, 10.);
        let saved = write_now(&mut board).unwrap();
        assert!(matches!(saved, Ok(Saved::Invalid(_))));
        let said = lost(&saved).unwrap();
        assert!(
            said.starts_with("The canvas file is not valid now (")
                && said.ends_with("your last changes to it are not written"),
            "{said}"
        );
        assert_eq!(std::fs::read_to_string(&path).unwrap(), broken);
        let _ = std::fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_drag_moves_each_card_held_once_rounded() {
        let (canvas, file) = board();
        let to: Vec<(String, Area)> = carried(&file.cards, &ids(&["b"]))
            .into_iter()
            .map(|(id, area)| (id, moved(area, 30.4, -10.6)))
            .collect();
        let ops = placing(&canvas, &to);
        assert_eq!(ops, [Op::move_node(&canvas, "b", 430., -11.).unwrap()]);
        assert_eq!(rect(&applied(&canvas, &ops), "b"), (430., -11., 200., 100.));
        // Back where it was: nothing to write.
        let back = carried(&file.cards, &ids(&["b"]));
        assert!(placing(&canvas, &back).is_empty());
        // Only what changes: a drag straight down sets `y` alone.
        let down = [("b".to_string(), moved(back[0].1, 0., 25.))];
        match placing(&canvas, &down).as_slice() {
            [Op::Set { fields, .. }] => {
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].key, "y");
            }
            other => panic!("{other:?}"),
        }
    }

    #[test]
    fn a_group_carries_what_lies_inside_it() {
        let (canvas, file) = board();
        // `g` and `a`, once, though `a` is selected too; `b` lies outside.
        let held = carried(&file.cards, &ids(&["g", "a"]));
        let held: Vec<&str> = held.iter().map(|(id, _)| id.as_str()).collect();
        assert_eq!(held, ["g", "a"]);
        let to: Vec<(String, Area)> = carried(&file.cards, &ids(&["g"]))
            .into_iter()
            .map(|(id, area)| (id, moved(area, 100., 50.)))
            .collect();
        let moved = applied(&canvas, &placing(&canvas, &to));
        assert_eq!(rect(&moved, "g"), (80., 30., 300., 200.));
        assert_eq!(rect(&moved, "a"), (100., 50., 200., 100.));
        assert_eq!(rect(&moved, "b"), (400., 0., 200., 100.));
    }

    #[test]
    fn a_resize_from_each_corner_keeps_the_opposite_one() {
        let (canvas, file) = board();
        let start = carried(&file.cards, &ids(&["a"]))[0].1;
        // Dragged 10 right and 20 down from each corner: (x, y, width, height).
        let cases = [
            (Corner::TopLeft, (10., 20., 190., 80.), 2),
            (Corner::TopRight, (0., 20., 210., 80.), 2),
            (Corner::BottomLeft, (10., 0., 190., 120.), 2),
            (Corner::BottomRight, (0., 0., 210., 120.), 1),
        ];
        for (corner, expected, count) in cases {
            let area = resized(start, corner, 10., 20.);
            let ops = placing(&canvas, &[("a".into(), area)]);
            // A move when the place changes, and the resize.
            assert_eq!(ops.len(), count, "{corner:?}: {ops:?}");
            assert_eq!(rect(&applied(&canvas, &ops), "a"), expected, "{corner:?}");
            let right = |a: Area| (a.x + a.width, a.y + a.height);
            match corner {
                Corner::TopLeft => assert_eq!(right(area), right(start)),
                Corner::BottomRight => assert_eq!((area.x, area.y), (start.x, start.y)),
                _ => {}
            }
        }
        // Never smaller than the least size, the opposite corner still in place.
        let small = resized(start, Corner::TopLeft, 500., 500.);
        assert_eq!((small.width, small.height), (MIN_SIZE, MIN_SIZE));
        assert_eq!(
            (small.x + small.width, small.y + small.height),
            (200., 100.)
        );
    }

    #[test]
    fn a_nudge_moves_the_selection_by_one_or_ten() {
        let (canvas, file) = board();
        for (step, x) in [(1., 401.), (-10., 390.)] {
            let to: Vec<(String, Area)> = carried(&file.cards, &ids(&["b", "c"]))
                .into_iter()
                .map(|(id, area)| (id, moved(area, step, 0.)))
                .collect();
            let ops = placing(&canvas, &to);
            assert_eq!(ops.len(), 2);
            let nudged = applied(&canvas, &ops);
            assert_eq!(rect(&nudged, "b").0, x);
            assert_eq!(rect(&nudged, "c").0, x + 400.);
        }
    }

    #[test]
    fn a_deletion_takes_the_edges_with_the_cards_and_is_undone_whole() {
        let (canvas, _) = board();
        let ops = deleting(&canvas, &ids(&["a", "b"]));
        assert_eq!(ops.len(), 2);
        // The edge between them goes with the first; the second takes `bc` alone.
        match &ops[1] {
            Op::DeleteNode { edges, .. } => assert_eq!(edges.len(), 1),
            other => panic!("{other:?}"),
        }
        let deleted = applied(&canvas, &ops);
        assert!(deleted.node("a").is_none() && deleted.node("b").is_none());
        assert!(deleted.edges().is_empty());
        let inverse: Vec<Op> = ops.iter().rev().map(Op::inverse).collect();
        assert_eq!(applied(&deleted, &inverse), canvas);
    }

    #[test]
    fn the_handles_are_the_same_size_on_screen_at_every_zoom() {
        let card = Area {
            x: 100.,
            y: 50.,
            width: 400.,
            height: 300.,
        };
        for zoom in [0.25, 0.5, 1., 2., 4.] {
            let camera = Camera { x: 0., y: 0., zoom };
            let area = camera.on_screen(card);
            let (right, bottom) = (area.x + area.width, area.y + area.height);
            assert_eq!(
                handle_at(area, area.x + 4., area.y - 4.),
                Some(Corner::TopLeft)
            );
            assert_eq!(handle_at(area, right - 5., area.y), Some(Corner::TopRight));
            assert_eq!(
                handle_at(area, area.x, bottom + 6.),
                Some(Corner::BottomLeft)
            );
            assert_eq!(handle_at(area, right, bottom), Some(Corner::BottomRight));
            // Out of reach, whatever the zoom: 7 px off, or inside the card.
            assert_eq!(handle_at(area, right + 7., bottom), None, "{zoom}");
            assert_eq!(handle_at(area, area.x + 12., area.y + 12.), None, "{zoom}");
        }
        // From afar the card is too small for handles: a press on it holds it.
        let far = Camera {
            x: 0.,
            y: 0.,
            zoom: 0.05,
        };
        let area = far.on_screen(card);
        assert!(area.width < 3. * HANDLE);
        assert_eq!(handle_at(area, area.x, area.y), None);
    }

    #[test]
    fn a_press_finds_the_handle_then_the_card_then_the_group() {
        let (_, file) = board();
        let camera = Camera {
            x: -100.,
            y: -100.,
            zoom: 2.,
        };
        let screen = |x: f32, y: f32| camera.to_screen(x, y);
        let at =
            |alone: Option<&str>, (x, y): (f32, f32)| hit(&file.cards, camera, 20., alone, x, y);
        // The corner of `a`: its handle when it is selected alone, else the card.
        assert_eq!(
            at(Some("a"), screen(200., 100.)),
            Hit::Handle("a".into(), Corner::BottomRight)
        );
        assert_eq!(at(None, screen(200., 100.)), Hit::Card("a".into()));
        assert_eq!(at(Some("b"), screen(200., 100.)), Hit::Card("a".into()));
        // The group's background, and its label above it, 20 px high on screen.
        assert_eq!(at(None, screen(250., 150.)), Hit::Group("g".into()));
        let (x, y) = screen(0., -20.);
        assert_eq!(at(None, (x, y - 10.)), Hit::Card("g".into()));
        assert_eq!(at(None, (x, y - 25.)), Hit::Empty);
        assert_eq!(at(None, screen(350., 50.)), Hit::Empty);
    }

    #[test]
    fn a_file_that_is_not_json_is_not_a_canvas() {
        assert!(CanvasFile::read("{nodes", "#000000", None).is_none());
        let empty = CanvasFile::read("", "#000000", None).unwrap();
        assert!(empty.cards.is_empty() && empty.all.is_none());
    }

    #[test]
    fn the_presets_are_the_themes_colours_or_fixed_hues() {
        let p = Palette::own(gpui_design_system::theme::Shade::Dark);
        assert_eq!(tint(&p, Tint::Preset(1)), p.fail);
        assert_eq!(tint(&p, Tint::Preset(3)), p.wait);
        assert_eq!(tint(&p, Tint::Preset(4)), p.ok);
        let orange = tint(&p, Tint::Preset(2));
        assert!(close(orange.h * 360., 28.) && orange.l == p.wait.l);
        let red = tint(&p, Tint::Rgb([255, 0, 0]));
        assert!(close(red.h, 0.) && close(red.s, 1.) && close(red.l, 0.5));
    }

    #[test]
    fn the_specs_sample_has_its_edge_hovered_near_its_curve() {
        let text = include_str!("../tests/fixtures/jsoncanvas/sample.canvas");
        let file = CanvasFile::read(text, "#000000", None).unwrap();
        assert_eq!(file.edges.len(), 1);
        let line = &file.edges[0].line;
        // From the logo's right side to the links' left side, an arrow at its end only.
        assert_eq!((line.from.x, line.from.y), (-63., -400.));
        let [tip, ..] = line.to_arrow.unwrap();
        assert_eq!((tip.x, tip.y), (40., -360.));
        assert!(line.from_arrow.is_none());
        let middle = line.middle();
        assert_eq!(file.edge_at(middle.x, middle.y + 3., 6.), Some(0));
        assert_eq!(file.edge_at(middle.x, middle.y + 30., 6.), None);
    }

    #[test]
    fn edges_keep_their_label_colour_and_ends_and_need_two_placed_cards() {
        let text = r##"{"nodes":[
            {"id":"a","type":"text","text":"A","x":0,"y":0,"width":100,"height":50},
            {"id":"b","type":"text","text":"B","x":300,"y":0,"width":100,"height":50},
            {"id":"n","type":"text","text":"no place"}
        ],"edges":[
            {"id":"e1","fromNode":"a","toNode":"b","label":"goes to","color":"4","fromEnd":"arrow","toEnd":"none"},
            {"id":"e2","fromNode":"a","toNode":"n"},
            {"id":"e3","fromNode":"a","toNode":"gone"}
        ]}"##;
        let file = CanvasFile::read(text, "#000000", None).unwrap();
        assert_eq!(file.edges.len(), 1);
        let edge = &file.edges[0];
        assert_eq!(edge.index, 0);
        assert_eq!(edge.label.as_deref(), Some("goes to"));
        assert_eq!(edge.tint, Some(Tint::Preset(4)));
        assert!(edge.line.from_arrow.is_some() && edge.line.to_arrow.is_none());
        // Between the two cards, on the straight line joining them.
        assert_eq!(file.edge_at(200., 25., 6.), Some(0));
    }

    #[test]
    fn edges_follow_their_cards_moved_and_go_with_them() {
        let text = r##"{"nodes":[
            {"id":"a","type":"text","text":"A","x":0,"y":0,"width":100,"height":50},
            {"id":"b","type":"text","text":"B","x":300,"y":0,"width":100,"height":50}
        ],"edges":[{"id":"e","fromNode":"a","toNode":"b"}]}"##;
        let mut file = CanvasFile::read(text, "#000000", None).unwrap();
        let canvas = file.canvas.clone();
        assert!(file.laid_edges(&canvas) == file.edges, "nothing moved");
        let b = file.cards.iter_mut().find(|card| card.id == "b").unwrap();
        b.area.y += 300.;
        let laid = file.laid_edges(&canvas);
        let [tip, ..] = laid[0].line.to_arrow.unwrap();
        // The left side's middle of b where it is now.
        assert_eq!((tip.x, tip.y), (300., 325.));
        file.cards.retain(|card| card.id != "b");
        assert!(file.laid_edges(&canvas).is_empty());
    }

    #[test]
    fn the_framing_takes_in_a_curve_bulging_out_of_its_cards() {
        let text = r##"{"nodes":[
            {"id":"a","type":"text","text":"A","x":0,"y":0,"width":100,"height":50},
            {"id":"b","type":"text","text":"B","x":300,"y":0,"width":100,"height":50}
        ],"edges":[
            {"id":"e","fromNode":"a","fromSide":"top","toNode":"b","toSide":"top"}
        ]}"##;
        let all = CanvasFile::read(text, "#000000", None)
            .unwrap()
            .all
            .unwrap();
        // Both ends leave upwards: the curve rises above the cards, about 118 at its top.
        assert!(all.y < -100., "{all:?}");
        assert_eq!((all.x, all.width), (0., 400.));
        assert_eq!(all.y + all.height, 50.);
    }
}

#[cfg(test)]
mod typing_tests {
    use super::{Camera, CanvasFile, Hit, hit, new_card, text_waits, told, typing_ops, write_now};
    use crate::canvas::{self, Canvas, NodeKind};
    use crate::canvas_board::{Board, Saved};
    use crate::canvas_ops::{Op, Report};
    use serde_json::Value;
    use std::fs;
    use std::path::PathBuf;

    /// A card `a` whose text is `A`.
    const ONE: &str = r#"{"nodes":[{"id":"a","type":"text","text":"A","x":0,"y":0,"width":200,"height":100}],"edges":[]}"#;

    /// A folder of its own for the test `name`, holding `ONE` as `a.canvas`: the folder and the
    /// canvas's path.
    fn canvas_file(name: &str) -> (PathBuf, PathBuf) {
        let folder = std::env::temp_dir().join(format!("rature-{name}-{}", std::process::id()));
        let _ = fs::remove_dir_all(&folder);
        fs::create_dir_all(&folder).unwrap();
        let path = folder.join("a.canvas");
        fs::write(&path, ONE).unwrap();
        (folder, path)
    }

    fn on_disk(path: &PathBuf) -> Canvas {
        Canvas::read(&fs::read_to_string(path).unwrap())
    }

    /// What each key gives the canvas being edited, as the field's text after it.
    fn type_keys(board: &mut Board, id: &str, texts: &[&str]) {
        for text in texts {
            for op in typing_ops(board.canvas(), id, text) {
                board.edit(op);
            }
        }
    }

    #[test]
    fn a_double_click_on_empty_space_makes_a_card_at_the_pointer() {
        let file = CanvasFile::read(ONE, "#000000", None).unwrap();
        let camera = Camera {
            x: -100.,
            y: -50.,
            zoom: 0.5,
        };
        // The board's point (400, 300) is the canvas's (700, 550): nothing there.
        let (x, y) = (400., 300.);
        assert_eq!(hit(&file.cards, camera, 0., None, x, y), Hit::Empty);
        let (at_x, at_y) = camera.to_canvas(x, y);
        assert_eq!((at_x, at_y), (700., 550.));
        let node = new_card(&file.canvas, at_x, at_y);
        let (width, height) = canvas::default_size(NodeKind::Text, Some(""));
        let rect = node.rect().unwrap();
        assert_eq!((rect.width, rect.height), (width, height));
        // Its middle under the pointer, on whole pixels.
        assert_eq!(
            (rect.x + width / 2., rect.y + height / 2.),
            (700., 550.),
            "{rect:?}"
        );
        assert_eq!((rect.x.fract(), rect.y.fract()), (0., 0.));
        assert_eq!(node.kind(), Some(NodeKind::Text));
        assert_eq!(node.text(), Some(""));
        let id = node.id().unwrap().to_string();
        assert_eq!(id.len(), 16);
        assert_ne!(id, "a");
        // Added, it is what a press there finds.
        let mut made = (*file.canvas).clone();
        let mut report = Report::default();
        Op::add_node(node).apply(&mut made, &mut report);
        assert!(report.is_clean());
        let made = CanvasFile::read(&made.write().unwrap(), "#000000", None).unwrap();
        assert_eq!(hit(&made.cards, camera, 0., None, x, y), Hit::Card(id));
    }

    #[test]
    fn typing_a_text_is_one_set_text_operation_and_a_card_made_one_action() {
        let (folder, path) = canvas_file("view-typing");
        let mut board = Board::open_with_locks(&path, folder.join("locks")).unwrap();
        type_keys(&mut board, "a", &["A ", "A b", "A bc", "A bc"]);
        match board.pending() {
            [Op::Set { id, fields, .. }] => {
                assert_eq!(id, "a");
                assert_eq!(fields.len(), 1);
                assert_eq!(fields[0].key, "text");
                assert_eq!(fields[0].base, Some(Value::from("A")));
                assert_eq!(fields[0].value, Some(Value::from("A bc")));
            }
            other => panic!("{other:?}"),
        }
        assert!(text_waits(board.pending(), "a"));
        let saved = write_now(&mut board).unwrap().unwrap();
        assert!(matches!(saved, Saved::Done { written: true, report } if report.is_clean()));
        assert_eq!(on_disk(&path).node("a").unwrap().text(), Some("A bc"));
        assert!(!text_waits(board.pending(), "a"));
        // A card made and typed in, the keys resting between: written twice, undone at once.
        let mark = board.actions();
        let node = new_card(board.canvas(), 0., 300.);
        let id = node.id().unwrap().to_string();
        board.edit(Op::add_node(node));
        type_keys(&mut board, &id, &["x"]);
        write_now(&mut board).unwrap().unwrap();
        type_keys(&mut board, &id, &["xy"]);
        board.join_since(mark);
        write_now(&mut board).unwrap().unwrap();
        assert_eq!(on_disk(&path).node(&id).unwrap().text(), Some("xy"));
        assert!(board.undo());
        assert!(board.canvas().node(&id).is_none());
        assert_eq!(
            board.canvas().node("a").unwrap().text(),
            Some("A bc"),
            "the edit before is an action of its own"
        );
        write_now(&mut board).unwrap().unwrap();
        assert!(on_disk(&path).node(&id).is_none());
        assert!(board.redo());
        assert_eq!(board.canvas().node(&id).unwrap().text(), Some("xy"));
        let _ = fs::remove_dir_all(&folder);
    }

    #[test]
    fn a_text_changed_on_disk_while_typing_wins_and_what_was_typed_goes_to_the_clipboard() {
        let (folder, path) = canvas_file("view-typing-conflict");
        let mut board = Board::open_with_locks(&path, folder.join("locks")).unwrap();
        type_keys(&mut board, "a", &["A t", "A typed"]);
        // Another program rewrites the card meanwhile; the file is read again.
        let theirs = ONE.replace(r#""text":"A""#, r#""text":"A theirs""#);
        fs::write(&path, &theirs).unwrap();
        board.refresh(&theirs);
        assert_eq!(board.canvas().node("a").unwrap().text(), Some("A theirs"));
        assert!(
            text_waits(board.pending(), "a"),
            "what was typed still waits"
        );
        let Saved::Done { report, .. } = write_now(&mut board).unwrap().unwrap() else {
            panic!("not written");
        };
        let (said, clipboard) = told(&report).unwrap();
        assert_eq!(clipboard.as_deref(), Some("A typed"));
        assert!(
            said.starts_with("a was changed in the file too")
                && said.ends_with("what you typed is in the clipboard"),
            "{said}"
        );
        assert_eq!(on_disk(&path).node("a").unwrap().text(), Some("A theirs"));
        // Nothing waits: the field is given the disk's text, typed on from there.
        assert!(!text_waits(board.pending(), "a"));
        assert_eq!(board.canvas().node("a").unwrap().text(), Some("A theirs"));
        assert_eq!(told(&Report::default()), None);
        let _ = fs::remove_dir_all(&folder);
    }
}

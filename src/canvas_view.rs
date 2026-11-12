//! A canvas shown as a board: its cards where the canvas puts them, seen through a camera that
//! pans and zooms. Only the cards in sight are built, at a detail that follows the zoom: the whole
//! Markdown close up, a coloured box and its title further out, rectangles from afar. The reader
//! keeps the file, its watcher, its bar, back and forward; the board draws it.
//!
//! Cards are selected, moved, resized, nudged and deleted on it, each change an operation of
//! [`crate::canvas_ops`] done on a [`Board`] and written by it, off the window's thread.

use std::collections::{HashMap, HashSet};
use std::io;
use std::path::{Path, PathBuf};
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_design_system::color::mix;
use gpui_design_system::toast::Toast;
use gpui_design_system::{Palette, palette, toast};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::canvas::{Canvas, Color, NodeKind, Problem};
use crate::canvas_board::{self, Board, Saved, Written};
use crate::canvas_ops::{Op, Report};
use crate::config::Config;
use crate::document::{self, Document};
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
    ]);
}

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
    /// What all the cards cover.
    pub all: Option<Area>,
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
                        let doc = document::parse(text, ink, folder);
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
            })
            .collect();
        // Stable: each kind keeps the canvas's order.
        cards.sort_by_key(|card| !matches!(card.kind, CardKind::Group));
        let all = cards.iter().map(|card| card.area).reduce(Area::union);
        Some(Self {
            canvas: Arc::new(canvas),
            cards,
            all,
        })
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
            },
        };
        view.set_file(file, cx);
        view
    }

    /// The canvas read again: the camera stays where it is. What is not written yet is replayed
    /// on it, and the cards held stay where the pointer has them.
    pub fn set_file(&mut self, file: Arc<CanvasFile>, cx: &mut Context<Self>) {
        self.drawn
            .fill(file.wanted(), &[], |this: &mut Self| &mut this.drawn, cx);
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

    /// Space pressed or let go: while it is held, a drag on a card pans too.
    pub fn hold_space(&mut self, held: bool, cx: &mut Context<Self>) {
        if self.space != held {
            self.space = held;
            cx.notify();
        }
    }

    /// The board laid out at `bounds`: framed the first time, unless a camera was kept.
    fn laid_out(&mut self, bounds: Bounds<Pixels>, cx: &mut Context<Self>) {
        if self.bounds == Some(bounds) {
            return;
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
        cx.notify();
    }

    fn press(&mut self, event: &MouseDownEvent, _: &mut Window, cx: &mut Context<Self>) {
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
                        .children(doc.blocks.iter().map(|block| render::block(ctx, block))),
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
    /// undone gives back is shown once the file is read again, as written.
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
    if report.is_clean() {
        return;
    }
    let mut said: Vec<String> = report.conflicts.iter().map(ToString::to_string).collect();
    if !report.clipboard.is_empty() {
        cx.write_to_clipboard(ClipboardItem::new_string(report.clipboard.join("\n\n")));
        said.push("what you typed is in the clipboard".into());
    }
    toast(Toast::failure(said.join("; ")), cx);
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
            move |bounds, _, cx| {
                watch.update(cx, |this, cx| this.laid_out(bounds, cx)).ok();
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
            Detail::Light => {
                board.children(shown.iter().map(|card| Self::light_card(card, camera, &p)))
            }
            Detail::Rects => board.child(Self::rects(&shown, camera, &p)),
        };
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
}

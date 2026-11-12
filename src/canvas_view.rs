//! A canvas shown as a board: its cards where the canvas puts them, seen through a camera that
//! pans and zooms. Only the cards in sight are built, at a detail that follows the zoom: the whole
//! Markdown close up, a coloured box and its title further out, rectangles from afar. The reader
//! keeps the file, its watcher, its bar, back and forward; the board draws it.

use std::path::Path;
use std::sync::Arc;
use std::time::{Duration, Instant};

use gpui_design_system::color::mix;
use gpui_design_system::toast::Toast;
use gpui_design_system::{Palette, palette, toast};
use gpui_kit::prelude::FluentBuilder as _;
use gpui_kit::*;

use crate::canvas::{Canvas, Color, NodeKind, Problem};
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

pub struct Card {
    /// The node's index in the canvas: its element's id.
    pub index: usize,
    pub area: Area,
    pub tint: Option<Tint>,
    pub kind: CardKind,
    /// What the light version shows: a text's first heading, else its first line; a group's
    /// label, a file's path, a link's address.
    pub title: String,
}

/// A canvas file, read off the main thread and ready to draw.
pub struct CanvasFile {
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
        Some(Self { cards, all })
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

    /// The topmost card under the canvas's point (`x`, `y`) that is not a group: what a press
    /// there is on, rather than on empty space.
    pub fn card_at(&self, x: f32, y: f32) -> Option<&Card> {
        self.cards
            .iter()
            .rev()
            .filter(|card| !matches!(card.kind, CardKind::Group))
            .find(|card| card.area.contains(x, y))
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
}

impl CanvasView {
    pub fn new(
        file: Arc<CanvasFile>,
        camera: Option<Camera>,
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
        };
        view.set_file(file, cx);
        view
    }

    /// The canvas read again: the camera stays where it is.
    pub fn set_file(&mut self, file: Arc<CanvasFile>, cx: &mut Context<Self>) {
        self.drawn
            .fill(file.wanted(), &[], |this: &mut Self| &mut this.drawn, cx);
        self.file = file;
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
        let (Some(camera), Some(bounds)) = (self.camera, self.bounds) else {
            return;
        };
        let (x, y) = local(event.position, bounds);
        let (x, y) = camera.to_canvas(x, y);
        if self.space || self.file.card_at(x, y).is_none() {
            self.panning = Some(event.position);
            cx.notify();
        }
    }

    fn drag(&mut self, position: Point<Pixels>, cx: &mut Context<Self>) {
        let Some(from) = self.panning else {
            return;
        };
        self.panning = Some(position);
        let delta = position - from;
        self.pan_by(f32::from(delta.x), f32::from(delta.y), cx);
    }

    fn release(&mut self, cx: &mut Context<Self>) {
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
        // A pan goes on wherever the pointer goes, over the board or not.
        if self.panning.is_some() {
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
    use super::{Area, Camera, CanvasFile, Detail, MAX_ZOOM, MIN_ZOOM, ROOM, STEP, Tint, tint};
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
        // The group is empty space; the card over it is not.
        assert!(file.card_at(-20., -20.).is_none());
        assert_eq!(file.card_at(10., 10.).map(|c| c.index), Some(0));
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

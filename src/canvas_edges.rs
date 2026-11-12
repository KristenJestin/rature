//! The geometry of a canvas's edges, free of GPUI: where an edge leaves and reaches its cards,
//! the cubic Bézier curve it follows, the arrows at its ends, and how far a point is from it.
//! In the canvas's pixels; the board scales it by its camera.

use crate::canvas;

/// The segments a curve is sampled in, for the pointer's distance to it.
pub const SEGMENTS: usize = 16;
/// An arrow, from its base to its tip, in the canvas's pixels: drawn with a line 2 wide.
pub const ARROW_LENGTH: f32 = 12.;
/// An arrow's base, across.
pub const ARROW_WIDTH: f32 = 10.;
/// How far a control point leaves its end, as a part of the gap between the ends...
const BEND: f32 = 0.5;
/// ...no nearer than this, so that two close cards still get a curve leaving each side
/// squarely...
const MIN_BEND: f32 = 24.;
/// ...and no further than this, so that two far cards are not joined by a wide loop.
const MAX_BEND: f32 = 160.;

#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Point {
    pub x: f32,
    pub y: f32,
}

impl Point {
    pub fn new(x: f32, y: f32) -> Self {
        Self { x, y }
    }

    fn plus(self, other: Point, times: f32) -> Point {
        Point::new(self.x + other.x * times, self.y + other.y * times)
    }

    fn minus(self, other: Point) -> Point {
        Point::new(self.x - other.x, self.y - other.y)
    }

    fn length(self) -> f32 {
        self.x.hypot(self.y)
    }

    pub fn distance(self, other: Point) -> f32 {
        self.minus(other).length()
    }
}

/// A card's place and size.
#[derive(Clone, Copy, Debug, PartialEq)]
pub struct Rect {
    pub x: f32,
    pub y: f32,
    pub width: f32,
    pub height: f32,
}

/// The side of a card an edge leaves or reaches.
#[derive(Clone, Copy, Debug, PartialEq, Eq)]
pub enum Side {
    Top,
    Right,
    Bottom,
    Left,
}

impl Side {
    const ALL: [Side; 4] = [Side::Top, Side::Right, Side::Bottom, Side::Left];

    /// As the canvas names it; a side it does not know is left out.
    pub fn of(side: Option<canvas::Side<'_>>) -> Option<Self> {
        match side? {
            canvas::Side::Top => Some(Side::Top),
            canvas::Side::Right => Some(Side::Right),
            canvas::Side::Bottom => Some(Side::Bottom),
            canvas::Side::Left => Some(Side::Left),
            canvas::Side::Other(_) => None,
        }
    }

    /// The middle of this side of `rect`: where an edge is attached.
    pub fn middle(self, rect: Rect) -> Point {
        let (x, y) = (rect.x + rect.width / 2., rect.y + rect.height / 2.);
        match self {
            Side::Top => Point::new(x, rect.y),
            Side::Right => Point::new(rect.x + rect.width, y),
            Side::Bottom => Point::new(x, rect.y + rect.height),
            Side::Left => Point::new(rect.x, y),
        }
    }

    /// The way out of the card through this side, of length 1.
    pub fn normal(self) -> Point {
        match self {
            Side::Top => Point::new(0., -1.),
            Side::Right => Point::new(1., 0.),
            Side::Bottom => Point::new(0., 1.),
            Side::Left => Point::new(-1., 0.),
        }
    }
}

/// The sides an edge from `from` to `to` is attached to: those given, and for one left out
/// the one facing the other card, of the pair of side middles nearest each other. An edge from a
/// card to itself never leaves and reaches the same side.
pub fn sides(from: Rect, from_side: Option<Side>, to: Rect, to_side: Option<Side>) -> (Side, Side) {
    let candidates = |given: Option<Side>| match given {
        Some(side) => vec![side],
        None => Side::ALL.to_vec(),
    };
    let same = from == to;
    let given = from_side.is_some() && to_side.is_some();
    let mut best: Option<(Side, Side, f32)> = None;
    for a in candidates(from_side) {
        for b in candidates(to_side) {
            if same && a == b && !given {
                continue;
            }
            let gap = a.middle(from).distance(b.middle(to));
            if best.is_none_or(|(_, _, nearest)| gap < nearest) {
                best = Some((a, b, gap));
            }
        }
    }
    // Never empty: a side left out leaves three others.
    let (a, b, _) = best.unwrap_or((Side::Right, Side::Left, 0.));
    (a, b)
}

/// An edge as it is drawn: its curve, from the base of the arrow at its start (or the side's
/// middle when it has none) to the base of the one at its end, and its arrows, tip first.
#[derive(Clone, Debug, PartialEq)]
pub struct Line {
    pub from: Point,
    /// The control point leaving `from`.
    pub c1: Point,
    /// The control point reaching `to`.
    pub c2: Point,
    pub to: Point,
    pub from_arrow: Option<[Point; 3]>,
    pub to_arrow: Option<[Point; 3]>,
    /// The curve at `SEGMENTS + 1` points, after the tip of the arrow at its start and before the
    /// one at its end: what the pointer's distance is measured to.
    samples: Vec<Point>,
    /// What the curve and its arrows cover: the curve lies inside its control points.
    pub bounds: Rect,
}

impl Line {
    /// The edge from the card `from` to the card `to`, by the sides given (`None` for those left
    /// out), with an arrow at each end that has one.
    pub fn new(
        from: Rect,
        from_side: Option<Side>,
        to: Rect,
        to_side: Option<Side>,
        from_arrow: bool,
        to_arrow: bool,
    ) -> Self {
        let (from_side, to_side) = sides(from, from_side, to, to_side);
        let (start, end) = (from_side.middle(from), to_side.middle(to));
        let bend = (start.distance(end) * BEND).clamp(MIN_BEND, MAX_BEND);
        let (out, into) = (from_side.normal(), to_side.normal());
        let (mut from, mut c1) = (start, start.plus(out, bend));
        let (mut to, mut c2) = (end, end.plus(into, bend));
        // The curve stops at an arrow's base, its control point moved as far: the curve's last
        // tangent stays along the side's normal, which the arrow follows.
        let from_arrow = from_arrow.then(|| {
            from = start.plus(out, ARROW_LENGTH);
            c1 = c1.plus(out, ARROW_LENGTH);
            arrow(start, out)
        });
        let to_arrow = to_arrow.then(|| {
            to = end.plus(into, ARROW_LENGTH);
            c2 = c2.plus(into, ARROW_LENGTH);
            arrow(end, into)
        });
        let mut line = Self {
            from,
            c1,
            c2,
            to,
            from_arrow,
            to_arrow,
            samples: Vec::new(),
            bounds: Rect {
                x: 0.,
                y: 0.,
                width: 0.,
                height: 0.,
            },
        };
        let mut samples = Vec::with_capacity(SEGMENTS + 3);
        samples.extend(from_arrow.map(|[tip, ..]| tip));
        samples.extend((0..=SEGMENTS).map(|i| line.point(i as f32 / SEGMENTS as f32)));
        samples.extend(to_arrow.map(|[tip, ..]| tip));
        let corners = [from, c1, c2, to]
            .into_iter()
            .chain(from_arrow.into_iter().flatten())
            .chain(to_arrow.into_iter().flatten());
        line.bounds = cover(corners);
        line.samples = samples;
        line
    }

    /// The curve's point at `t`, from 0 at its start to 1 at its end.
    pub fn point(&self, t: f32) -> Point {
        let u = 1. - t;
        let (a, b, c, d) = (u * u * u, 3. * u * u * t, 3. * u * t * t, t * t * t);
        Point::new(
            a * self.from.x + b * self.c1.x + c * self.c2.x + d * self.to.x,
            a * self.from.y + b * self.c1.y + c * self.c2.y + d * self.to.y,
        )
    }

    /// The curve's direction at `t`, not of length 1.
    #[cfg(test)]
    pub fn tangent(&self, t: f32) -> Point {
        let u = 1. - t;
        let (a, b, c) = (3. * u * u, 6. * u * t, 3. * t * t);
        let d1 = self.c1.minus(self.from);
        let d2 = self.c2.minus(self.c1);
        let d3 = self.to.minus(self.c2);
        Point::new(
            a * d1.x + b * d2.x + c * d3.x,
            a * d1.y + b * d2.y + c * d3.y,
        )
    }

    /// What the curve and its arrows cover: tighter than `bounds`, as the curve does not reach
    /// its control points.
    pub fn extent(&self) -> Rect {
        let arrows = self.from_arrow.into_iter().chain(self.to_arrow).flatten();
        cover(self.samples.iter().copied().chain(arrows))
    }

    /// Where its label goes: the curve's middle.
    pub fn middle(&self) -> Point {
        self.point(0.5)
    }

    /// The points the curve is sampled at, arrows' tips included.
    #[cfg(test)]
    pub fn samples(&self) -> &[Point] {
        &self.samples
    }

    /// How far `point` is from the edge, measured to its samples joined by straight segments.
    pub fn distance(&self, point: Point) -> f32 {
        self.samples
            .windows(2)
            .map(|pair| to_segment(point, pair[0], pair[1]))
            .fold(f32::INFINITY, f32::min)
    }
}

/// An arrow whose tip is at `tip`, its base `ARROW_LENGTH` away along `out`: it points against
/// `out`, into the card.
fn arrow(tip: Point, out: Point) -> [Point; 3] {
    let base = tip.plus(out, ARROW_LENGTH);
    let across = Point::new(-out.y, out.x);
    [
        tip,
        base.plus(across, ARROW_WIDTH / 2.),
        base.plus(across, -ARROW_WIDTH / 2.),
    ]
}

/// The smallest rectangle holding all of `points`.
fn cover(points: impl Iterator<Item = Point>) -> Rect {
    let (mut left, mut top, mut right, mut bottom) = (
        f32::INFINITY,
        f32::INFINITY,
        f32::NEG_INFINITY,
        f32::NEG_INFINITY,
    );
    for p in points {
        (left, top) = (left.min(p.x), top.min(p.y));
        (right, bottom) = (right.max(p.x), bottom.max(p.y));
    }
    Rect {
        x: left,
        y: top,
        width: right - left,
        height: bottom - top,
    }
}

/// The distance from `p` to the segment from `a` to `b`.
fn to_segment(p: Point, a: Point, b: Point) -> f32 {
    let ab = b.minus(a);
    let squared = ab.x * ab.x + ab.y * ab.y;
    let t = if squared > 0. {
        (((p.x - a.x) * ab.x + (p.y - a.y) * ab.y) / squared).clamp(0., 1.)
    } else {
        0.
    };
    p.distance(a.plus(ab, t))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn card(x: f32, y: f32) -> Rect {
        Rect {
            x,
            y,
            width: 100.,
            height: 60.,
        }
    }

    fn close(a: f32, b: f32) -> bool {
        (a - b).abs() < 1e-3
    }

    fn unit(p: Point) -> Point {
        let length = p.length();
        Point::new(p.x / length, p.y / length)
    }

    #[test]
    fn a_side_left_out_faces_the_other_card() {
        let a = card(0., 0.);
        // To the right, then below, then to the left and a little above.
        assert_eq!(
            sides(a, None, card(300., 20.), None),
            (Side::Right, Side::Left)
        );
        assert_eq!(
            sides(a, None, card(10., 200.), None),
            (Side::Bottom, Side::Top)
        );
        assert_eq!(
            sides(a, None, card(-400., -30.), None),
            (Side::Left, Side::Right)
        );
        // One side given: the other faces it.
        assert_eq!(
            sides(a, Some(Side::Bottom), card(300., 20.), None),
            (Side::Bottom, Side::Left)
        );
        assert_eq!(
            sides(a, None, card(300., 20.), Some(Side::Top)),
            (Side::Right, Side::Top)
        );
        // Both given: kept, whatever faces what.
        assert_eq!(
            sides(a, Some(Side::Left), card(300., 20.), Some(Side::Right)),
            (Side::Left, Side::Right)
        );
        // A card to itself: never the same side twice.
        let (from, to) = sides(a, None, a, None);
        assert_ne!(from, to);
        // A side the canvas does not know is left out.
        assert_eq!(Side::of(Some(canvas::Side::Other("middle"))), None);
        assert_eq!(Side::of(Some(canvas::Side::Top)), Some(Side::Top));
    }

    #[test]
    fn the_curve_leaves_and_reaches_along_the_sides_normals() {
        let line = Line::new(
            card(0., 0.),
            None,
            card(300., 200.),
            Some(Side::Top),
            false,
            false,
        );
        assert_eq!(line.from, Point::new(100., 30.));
        assert_eq!(line.to, Point::new(350., 200.));
        let (start, end) = (unit(line.tangent(0.)), unit(line.tangent(1.)));
        assert!(close(start.x, 1.) && close(start.y, 0.), "{start:?}");
        // Into the card's top: downwards.
        assert!(close(end.x, 0.) && close(end.y, 1.), "{end:?}");
        // The control points' distance follows the gap, within bounds.
        let near = Line::new(card(0., 0.), None, card(110., 0.), None, false, false);
        assert!(close(near.c1.x - near.from.x, MIN_BEND));
        let far = Line::new(card(0., 0.), None, card(5000., 0.), None, false, false);
        assert!(close(far.c1.x - far.from.x, MAX_BEND));
        let mid = Line::new(card(0., 0.), None, card(300., 0.), None, false, false);
        assert!(close(mid.c1.x - mid.from.x, 200. * BEND));
    }

    #[test]
    fn an_arrow_points_into_its_card_along_the_last_tangent() {
        let line = Line::new(
            card(0., 0.),
            Some(Side::Bottom),
            card(300., 200.),
            Some(Side::Left),
            false,
            true,
        );
        let [tip, a, b] = line.to_arrow.unwrap();
        assert_eq!(tip, Side::Left.middle(card(300., 200.)));
        let base = Point::new((a.x + b.x) / 2., (a.y + b.y) / 2.);
        // From its base to its tip: rightwards, into the card's left side...
        let along = unit(tip.minus(base));
        assert!(close(along.x, 1.) && close(along.y, 0.), "{along:?}");
        assert!(close(tip.distance(base), ARROW_LENGTH));
        assert!(close(a.distance(b), ARROW_WIDTH));
        // ...as the curve goes where it stops.
        let last = unit(line.tangent(1.));
        assert!(close(last.x, along.x) && close(last.y, along.y), "{last:?}");
        assert!(line.from_arrow.is_none());
    }

    #[test]
    fn the_curve_ends_at_its_arrows_base() {
        let (a, b) = (card(0., 0.), card(400., 300.));
        let line = Line::new(a, None, b, None, true, true);
        for (arrow, end) in [(line.from_arrow, line.from), (line.to_arrow, line.to)] {
            let [_, left, right] = arrow.unwrap();
            let base = Point::new((left.x + right.x) / 2., (left.y + right.y) / 2.);
            assert!(
                close(end.x, base.x) && close(end.y, base.y),
                "{end:?} {base:?}"
            );
        }
        // Without an arrow, the curve goes to the side's middle.
        let plain = Line::new(a, None, b, None, false, false);
        assert_eq!(plain.from, Side::Right.middle(a));
        assert_eq!(plain.to, Side::Left.middle(b));
        // The curve is sampled in `SEGMENTS`, the arrows' tips besides.
        assert_eq!(plain.samples().len(), SEGMENTS + 1);
        assert_eq!(line.samples().len(), SEGMENTS + 3);
        assert_eq!(line.samples()[0], Side::Right.middle(a));
    }

    #[test]
    fn the_distance_to_the_pointer_is_measured_to_the_curve() {
        // Two cards side by side: the curve is the straight line between them.
        let line = Line::new(card(0., 0.), None, card(300., 0.), None, false, true);
        assert!(close(line.distance(Point::new(200., 30.)), 0.));
        assert!(close(line.distance(Point::new(200., 36.)), 6.));
        assert!(close(line.distance(Point::new(200., 20.)), 10.));
        // Beyond an end, to that end; the arrow's tip counts.
        assert!(close(line.distance(Point::new(90., 30.)), 10.));
        assert!(close(line.distance(Point::new(299., 30.)), 0.));
        // A bent curve: its middle is on it, a point off it is not.
        let bent = Line::new(
            card(0., 0.),
            None,
            card(300., 300.),
            Some(Side::Top),
            false,
            false,
        );
        assert!(bent.distance(bent.middle()) < 0.5);
        assert!(bent.distance(Point::new(100., 300.)) > 50.);
        // Its extent follows the curve, within its bounds.
        let (e, b) = (bent.extent(), bent.bounds);
        assert!(e.x >= b.x && e.y >= b.y && e.width <= b.width && e.height <= b.height);
        assert!(
            close(e.x, bent.from.x) && close(e.y + e.height, bent.to.y),
            "{e:?}"
        );
        assert!(bent.samples().iter().all(|p| {
            p.x >= b.x - 1e-3
                && p.y >= b.y - 1e-3
                && p.x <= b.x + b.width + 1e-3
                && p.y <= b.y + b.height + 1e-3
        }));
    }
}

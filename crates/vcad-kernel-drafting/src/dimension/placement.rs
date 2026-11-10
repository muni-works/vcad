//! Collision-aware placement for annotations dropped at a point (datum
//! feature symbols, feature control frames).
//!
//! An annotation placed exactly where the user clicked lands on dimension
//! text and part geometry. [`place_clear`] keeps it there when that spot is
//! free, and otherwise slides it outward on a ring search until its box
//! clears every obstacle, drawing a leader back to the anchor.

use super::render::{RenderedDimension, RenderedText, TextAlignment};
use crate::types::Point2D;

/// Axis-aligned box `(min, max)`.
pub type Box2 = (Point2D, Point2D);

/// Something an annotation must not overlap.
#[derive(Debug, Clone, Copy)]
pub enum Obstacle {
    /// A filled region (text extent, view geometry bounds).
    Rect(Box2),
    /// A stroke (dimension line, extension line, leader).
    Segment(Point2D, Point2D),
}

/// Approximate extent of a text label (glyph advance ≈ 0.6 × height).
pub fn text_box(t: &RenderedText) -> Box2 {
    let w = 0.6 * t.height * t.text.chars().count().max(1) as f64;
    let h = t.height;
    let (fx, fy) = match t.alignment {
        TextAlignment::TopLeft => (0.0, -1.0),
        TextAlignment::TopCenter => (-0.5, -1.0),
        TextAlignment::TopRight => (-1.0, -1.0),
        TextAlignment::MiddleLeft => (0.0, -0.5),
        TextAlignment::MiddleCenter => (-0.5, -0.5),
        TextAlignment::MiddleRight => (-1.0, -0.5),
        TextAlignment::BottomLeft => (0.0, 0.0),
        TextAlignment::BottomCenter => (-0.5, 0.0),
        TextAlignment::BottomRight => (-1.0, 0.0),
    };
    // Corners of the unrotated box relative to the anchor, then rotated.
    let (s, c) = t.rotation.sin_cos();
    let corners = [
        (fx * w, fy * h),
        ((fx + 1.0) * w, fy * h),
        (fx * w, (fy + 1.0) * h),
        ((fx + 1.0) * w, (fy + 1.0) * h),
    ];
    let mut lo = Point2D::new(f64::INFINITY, f64::INFINITY);
    let mut hi = Point2D::new(f64::NEG_INFINITY, f64::NEG_INFINITY);
    for (x, y) in corners {
        let p = Point2D::new(t.position.x + x * c - y * s, t.position.y + x * s + y * c);
        lo = Point2D::new(lo.x.min(p.x), lo.y.min(p.y));
        hi = Point2D::new(hi.x.max(p.x), hi.y.max(p.y));
    }
    (lo, hi)
}

/// Box of every primitive of `rd`, with text extents (not just anchors).
pub fn annotation_box(rd: &RenderedDimension) -> Option<Box2> {
    let mut b = rd.bounds()?;
    for t in &rd.texts {
        let (lo, hi) = text_box(t);
        b.0 = Point2D::new(b.0.x.min(lo.x), b.0.y.min(lo.y));
        b.1 = Point2D::new(b.1.x.max(hi.x), b.1.y.max(hi.y));
    }
    Some(b)
}

/// Obstacles contributed by an already-placed annotation: its strokes and
/// its text boxes.
pub fn obstacles_of(rd: &RenderedDimension) -> Vec<Obstacle> {
    let mut out: Vec<Obstacle> = rd
        .lines
        .iter()
        .map(|(a, b)| Obstacle::Segment(*a, *b))
        .collect();
    out.extend(rd.texts.iter().map(|t| Obstacle::Rect(text_box(t))));
    for a in &rd.arcs {
        let r = a.radius;
        out.push(Obstacle::Rect((
            Point2D::new(a.center.x - r, a.center.y - r),
            Point2D::new(a.center.x + r, a.center.y + r),
        )));
    }
    out
}

/// `rd` moved by `(dx, dy)`.
pub fn translated(rd: &RenderedDimension, dx: f64, dy: f64) -> RenderedDimension {
    let m = |p: Point2D| Point2D::new(p.x + dx, p.y + dy);
    let mut out = rd.clone();
    for l in &mut out.lines {
        *l = (m(l.0), m(l.1));
    }
    for a in &mut out.arcs {
        a.center = m(a.center);
    }
    for a in &mut out.arrows {
        a.tip = m(a.tip);
    }
    for t in &mut out.texts {
        t.position = m(t.position);
    }
    out
}

fn boxes_overlap(a: Box2, b: Box2) -> bool {
    a.0.x < b.1.x && b.0.x < a.1.x && a.0.y < b.1.y && b.0.y < a.1.y
}

fn segment_hits_box(p: Point2D, q: Point2D, b: Box2) -> bool {
    // Liang–Barsky clip of the segment against the box.
    let (mut t0, mut t1) = (0.0f64, 1.0f64);
    let d = Point2D::new(q.x - p.x, q.y - p.y);
    for (den, num) in [
        (-d.x, p.x - b.0.x),
        (d.x, b.1.x - p.x),
        (-d.y, p.y - b.0.y),
        (d.y, b.1.y - p.y),
    ] {
        if den.abs() < 1e-12 {
            if num < 0.0 {
                return false;
            }
        } else {
            let t = num / den;
            if den < 0.0 {
                t0 = t0.max(t);
            } else {
                t1 = t1.min(t);
            }
            if t0 > t1 {
                return false;
            }
        }
    }
    true
}

fn collides(b: Box2, obstacles: &[Obstacle]) -> bool {
    obstacles.iter().any(|o| match *o {
        Obstacle::Rect(r) => boxes_overlap(b, r),
        Obstacle::Segment(p, q) => segment_hits_box(p, q, b),
    })
}

fn inflate(b: Box2, g: f64) -> Box2 {
    (
        Point2D::new(b.0.x - g, b.0.y - g),
        Point2D::new(b.1.x + g, b.1.y + g),
    )
}

/// Place `item` (rendered at `anchor`) clear of `obstacles`.
///
/// Returns `item` unchanged when its box (inflated by `gap`) is already
/// clear. Otherwise searches rings of increasing radius around the anchor
/// (up, right, left, down and the diagonals, in that order) for the nearest
/// clear spot, moves the item there and adds a leader line from the anchor
/// to the closest point of the moved box. Falls back to the farthest
/// candidate if nothing within the search radius is clear.
pub fn place_clear(
    item: &RenderedDimension,
    anchor: Point2D,
    obstacles: &[Obstacle],
    gap: f64,
) -> RenderedDimension {
    let Some(b) = annotation_box(item) else {
        return item.clone();
    };
    if !collides(inflate(b, gap), obstacles) {
        return item.clone();
    }
    let step = ((b.1.y - b.0.y).max(b.1.x - b.0.x) * 0.5).max(3.0);
    const DIRS: [(f64, f64); 8] = [
        (0.0, 1.0),
        (1.0, 0.0),
        (-1.0, 0.0),
        (0.0, -1.0),
        (1.0, 1.0),
        (-1.0, 1.0),
        (1.0, -1.0),
        (-1.0, -1.0),
    ];
    let mut last = (0.0, 0.0);
    'search: for ring in 1..=24 {
        let r = ring as f64 * step;
        for (dx, dy) in DIRS {
            let n = (dx * dx + dy * dy).sqrt();
            let off = (dx / n * r, dy / n * r);
            last = off;
            let moved = (
                Point2D::new(b.0.x + off.0, b.0.y + off.1),
                Point2D::new(b.1.x + off.0, b.1.y + off.1),
            );
            if !collides(inflate(moved, gap), obstacles) {
                break 'search;
            }
        }
    }
    let mut out = translated(item, last.0, last.1);
    if let Some(nb) = annotation_box(&out) {
        let near = Point2D::new(
            anchor.x.clamp(nb.0.x, nb.1.x),
            anchor.y.clamp(nb.0.y, nb.1.y),
        );
        out.lines.push((anchor, near));
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    fn label(at: Point2D) -> RenderedDimension {
        let mut rd = RenderedDimension::new();
        rd.texts.push(RenderedText::new(at, "A", 4.0));
        rd.lines.push((
            Point2D::new(at.x - 3.0, at.y - 3.0),
            Point2D::new(at.x + 3.0, at.y + 3.0),
        ));
        rd
    }

    #[test]
    fn clear_spot_is_kept() {
        let item = label(Point2D::new(50.0, 50.0));
        let obs = [Obstacle::Rect((
            Point2D::new(0.0, 0.0),
            Point2D::new(10.0, 10.0),
        ))];
        let out = place_clear(&item, Point2D::new(50.0, 50.0), &obs, 1.0);
        assert_eq!(out.lines.len(), item.lines.len(), "no leader when clear");
        assert_eq!(out.texts[0].position.x, 50.0);
    }

    #[test]
    fn colliding_item_moves_clear_with_leader() {
        let at = Point2D::new(20.0, 20.0);
        let item = label(at);
        let dim_text = RenderedText::new(at, "40.00", 3.5);
        let obs = vec![
            Obstacle::Rect(text_box(&dim_text)),
            Obstacle::Rect((Point2D::new(0.0, 0.0), Point2D::new(40.0, 25.0))),
            Obstacle::Segment(Point2D::new(-5.0, 30.0), Point2D::new(45.0, 30.0)),
        ];
        let out = place_clear(&item, at, &obs, 1.0);
        let b = annotation_box(&translated(&item, 0.0, 0.0)).unwrap();
        let nb = annotation_box(&RenderedDimension {
            lines: out.lines[..item.lines.len()].to_vec(),
            texts: out.texts.clone(),
            ..Default::default()
        })
        .unwrap();
        assert!(!collides(nb, &obs), "still overlapping: {nb:?}");
        assert!(nb != b, "item did not move");
        assert_eq!(out.lines.len(), item.lines.len() + 1, "leader added");
        assert_eq!(out.lines.last().unwrap().0, at, "leader starts at anchor");
    }

    #[test]
    fn rotated_text_box_is_tall() {
        let t = RenderedText::new(Point2D::new(0.0, 0.0), "12345", 2.0)
            .with_rotation(std::f64::consts::FRAC_PI_2);
        let (lo, hi) = text_box(&t);
        assert!(hi.y - lo.y > hi.x - lo.x);
    }
}

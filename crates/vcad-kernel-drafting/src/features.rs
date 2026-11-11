//! Feature recognition on projected views: circular features (holes, bosses,
//! shafts seen end-on) recovered from the tessellated edge loops, plus the
//! annotations that hang off them — center marks and `N× ⌀D` callouts.
//!
//! Works on any [`ProjectedView`], so it covers parametric parts, frozen
//! meshes and imported STEP alike: a circular edge seen along its axis
//! projects to a closed polygon whose vertices all sit on one circle.

use std::collections::{BTreeMap, HashMap, HashSet};

use crate::dimension::{RenderedDimension, RenderedText, TextAlignment};
use crate::types::{Point2D, ProjectedView};

/// A circle recovered from a projected view (view mm).
#[derive(Debug, Clone, Copy, PartialEq)]
pub struct CircleFeature {
    /// Center.
    pub center: Point2D,
    /// Radius.
    pub radius: f64,
}

impl CircleFeature {
    /// Diameter.
    pub fn diameter(&self) -> f64 {
        self.radius * 2.0
    }
}

/// Group of equal-diameter circles, for an `N× ⌀D` callout.
#[derive(Debug, Clone, PartialEq)]
pub struct CircleGroup {
    /// Shared diameter (view mm).
    pub diameter: f64,
    /// Members, sorted left-to-right then bottom-to-top.
    pub circles: Vec<CircleFeature>,
}

/// Minimum polygon vertex count accepted as a circle (a hex nut is not a hole).
const MIN_VERTICES: usize = 8;
/// Max radial deviation, as a fraction of the radius.
const ROUNDNESS: f64 = 0.03;

/// Recover circles from the closed edge loops of `view`.
///
/// Edges are welded on a grid of `1e-4 ×` the view size; overlapping
/// visible/hidden copies (a through-hole's two rims) collapse to one. Only
/// loops whose vertices all have degree 2 are considered — a rim on a flat
/// face connects to nothing else, while a silhouette that merely *contains*
/// arc segments is rejected. Concentric duplicates are merged.
pub fn detect_circles(view: &ProjectedView) -> Vec<CircleFeature> {
    let b = view.bounds;
    if !b.is_valid() {
        return Vec::new();
    }
    let size = b.width().max(b.height()).max(1e-9);
    let q = size * 1e-4;
    // Weld: a point joins an existing vertex in its own or a neighboring
    // grid cell within `q` (plain rounding would split two copies of one
    // vertex that straddle a cell boundary and break the loop).
    let mut pos: HashMap<(i64, i64), Point2D> = HashMap::new();
    let mut weld = |p: Point2D| -> (i64, i64) {
        let k = ((p.x / q).round() as i64, (p.y / q).round() as i64);
        for dx in -1..=1 {
            for dy in -1..=1 {
                let n = (k.0 + dx, k.1 + dy);
                if pos.get(&n).is_some_and(|o| o.distance(&p) <= q) {
                    return n;
                }
            }
        }
        pos.insert(k, p);
        k
    };
    let mut segs: HashSet<((i64, i64), (i64, i64))> = HashSet::new();
    for e in &view.edges {
        let (a, c) = (weld(e.start), weld(e.end));
        if a == c {
            continue;
        }
        segs.insert(if a < c { (a, c) } else { (c, a) });
    }
    let mut adj: HashMap<(i64, i64), Vec<(i64, i64)>> = HashMap::new();
    for (a, c) in &segs {
        adj.entry(*a).or_default().push(*c);
        adj.entry(*c).or_default().push(*a);
    }

    // Trace turn-consistent loops: from every short segment, follow the
    // neighbor whose turn best matches the previous turn (an arc turns by a
    // constant angle per segment), so a rim survives a junction where a
    // silhouette or seam edge touches it.
    let ang = |a: (i64, i64), b: (i64, i64)| {
        let (pa, pb) = (pos[&a], pos[&b]);
        (pb.y - pa.y).atan2(pb.x - pa.x)
    };
    let wrap = |mut t: f64| {
        while t > std::f64::consts::PI {
            t -= std::f64::consts::TAU;
        }
        while t < -std::f64::consts::PI {
            t += std::f64::consts::TAU;
        }
        t
    };
    let max_turn = std::f64::consts::FRAC_PI_4;
    let mut used: HashSet<((i64, i64), (i64, i64))> = HashSet::new();
    let mut found: Vec<CircleFeature> = Vec::new();
    let mut seeds: Vec<_> = segs.iter().copied().collect();
    seeds.sort_unstable();
    for (s0, s1) in seeds {
        if used.contains(&(s0, s1)) {
            continue;
        }
        let mut lp = vec![s0];
        let (mut prev, mut cur) = (s0, s1);
        let mut last_turn: Option<f64> = None;
        let mut closed = false;
        while lp.len() <= 4096 {
            if cur == s0 {
                closed = true;
                break;
            }
            lp.push(cur);
            let heading = ang(prev, cur);
            let best = adj[&cur]
                .iter()
                .filter(|n| **n != prev)
                .map(|n| (*n, wrap(ang(cur, *n) - heading)))
                .filter(|(_, t)| {
                    t.abs() <= max_turn && (t.abs() < 1e-6 || last_turn.is_none_or(|l| l * t > 0.0))
                })
                .min_by(|a, b| {
                    let key = |t: f64| last_turn.map_or(t.abs(), |l| (t - l).abs());
                    key(a.1)
                        .partial_cmp(&key(b.1))
                        .unwrap_or(std::cmp::Ordering::Equal)
                });
            let Some((next, turn)) = best else { break };
            if turn.abs() >= 1e-6 {
                last_turn = Some(turn);
            }
            prev = cur;
            cur = next;
        }
        if !closed || lp.len() < MIN_VERTICES {
            continue;
        }
        for w in 0..lp.len() {
            let (a, c) = (lp[w], lp[(w + 1) % lp.len()]);
            used.insert(if a < c { (a, c) } else { (c, a) });
        }
        let pts: Vec<Point2D> = lp.iter().map(|k| pos[k]).collect();
        if let Some(c) = fit_circle(&pts) {
            if c.radius > q * 10.0 {
                found.push(c);
            }
        }
    }

    // Merge concentric duplicates of the same radius.
    let mut out: Vec<CircleFeature> = Vec::new();
    for c in found {
        let dup = out.iter().any(|o| {
            o.center.distance(&c.center) < o.radius.max(c.radius) * 0.02
                && (o.radius - c.radius).abs() < o.radius.max(c.radius) * 0.02
        });
        if !dup {
            out.push(c);
        }
    }
    out.sort_by(|a, b| {
        (a.center.x, a.center.y, a.radius)
            .partial_cmp(&(b.center.x, b.center.y, b.radius))
            .unwrap_or(std::cmp::Ordering::Equal)
    });
    out
}

/// Fit a circle through polygon vertices: centroid of the vertices, mean
/// distance as radius; rejected unless every vertex is within [`ROUNDNESS`]
/// and the vertices are spread around the full turn (a closed loop of a
/// regular polygon is).
fn fit_circle(pts: &[Point2D]) -> Option<CircleFeature> {
    let n = pts.len() as f64;
    let cx = pts.iter().map(|p| p.x).sum::<f64>() / n;
    let cy = pts.iter().map(|p| p.y).sum::<f64>() / n;
    let center = Point2D::new(cx, cy);
    let r = pts.iter().map(|p| p.distance(&center)).sum::<f64>() / n;
    if r <= 0.0 {
        return None;
    }
    if pts
        .iter()
        .any(|p| (p.distance(&center) - r).abs() > r * ROUNDNESS)
    {
        return None;
    }
    // No segment may span more than 60° (rules out a square's corners on a
    // circle, which pass the radial test).
    let max_step = pts
        .iter()
        .zip(pts.iter().cycle().skip(1))
        .map(|(a, b)| a.distance(b) / r)
        .fold(0.0, f64::max);
    if max_step > 1.0 {
        return None;
    }
    Some(CircleFeature { center, radius: r })
}

/// Group circles by diameter (rounded to 0.01 mm).
pub fn group_circles(circles: &[CircleFeature]) -> Vec<CircleGroup> {
    let mut by: BTreeMap<i64, Vec<CircleFeature>> = BTreeMap::new();
    for c in circles {
        by.entry((c.diameter() * 100.0).round() as i64)
            .or_default()
            .push(*c);
    }
    by.into_iter()
        .map(|(d, circles)| CircleGroup {
            diameter: d as f64 / 100.0,
            circles,
        })
        .collect()
}

/// Format a diameter callout: `⌀6` or `4× ⌀6.5`.
pub fn diameter_text(group: &CircleGroup) -> String {
    let d = format_mm(group.diameter);
    if group.circles.len() > 1 {
        format!("{}× ⌀{d}", group.circles.len())
    } else {
        format!("⌀{d}")
    }
}

/// Up to two decimals, trailing zeros trimmed.
pub fn format_mm(v: f64) -> String {
    let s = format!("{v:.2}");
    let s = s.trim_end_matches('0').trim_end_matches('.');
    if s == "-0" {
        "0".into()
    } else {
        s.to_string()
    }
}

/// Center mark for a circle drawn at sheet position `c` with sheet radius
/// `r`: a short cross at the center plus dash-dot extensions 3 mm past the
/// rim (ASME Y14.5 style; draw with [`crate::LineClass::Center`]).
pub fn center_mark(c: Point2D, r: f64) -> RenderedDimension {
    let mut rd = RenderedDimension::new();
    let arm = (r * 0.25).clamp(0.75, 2.0);
    let gap = arm * 0.5;
    let ext = r + 3.0;
    rd.add_line(Point2D::new(c.x - arm, c.y), Point2D::new(c.x + arm, c.y));
    rd.add_line(Point2D::new(c.x, c.y - arm), Point2D::new(c.x, c.y + arm));
    if r > 2.0 * arm {
        for s in [-1.0, 1.0] {
            rd.add_line(
                Point2D::new(c.x + s * (arm + gap), c.y),
                Point2D::new(c.x + s * ext, c.y),
            );
            rd.add_line(
                Point2D::new(c.x, c.y + s * (arm + gap)),
                Point2D::new(c.x, c.y + s * ext),
            );
        }
    }
    rd
}

/// Leader callout from the rim of a circle (sheet `c`, `r`) up-right at 45°
/// to a horizontal shoulder carrying `text` (sheet mm).
pub fn leader_callout(c: Point2D, r: f64, text: &str, length: f64) -> RenderedDimension {
    leader_callout_at(c, r, text, length, std::f64::consts::FRAC_PI_4)
}

/// [`leader_callout`] leaving the rim at `angle` (radians); the shoulder and
/// text run right for angles on the right half, left otherwise.
pub fn leader_callout_at(
    c: Point2D,
    r: f64,
    text: &str,
    length: f64,
    angle: f64,
) -> RenderedDimension {
    let mut rd = RenderedDimension::new();
    let rim = Point2D::new(c.x + r * angle.cos(), c.y + r * angle.sin());
    let knee = Point2D::new(rim.x + length * angle.cos(), rim.y + length * angle.sin());
    let dir = if angle.cos() >= 0.0 { 1.0 } else { -1.0 };
    let shoulder = Point2D::new(knee.x + 3.0 * dir, knee.y);
    rd.add_line(rim, knee);
    rd.add_line(knee, shoulder);
    rd.add_arrow(crate::dimension::RenderedArrow::new(
        rim,
        angle + std::f64::consts::PI,
        crate::dimension::ArrowType::ClosedFilled,
        2.5,
    ));
    let align = if dir > 0.0 {
        TextAlignment::MiddleLeft
    } else {
        TextAlignment::MiddleRight
    };
    rd.add_text(
        RenderedText::new(Point2D::new(shoulder.x + 0.8 * dir, shoulder.y), text, 3.0)
            .with_alignment(align),
    );
    rd
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::types::{ProjectedEdge, ViewDirection, Visibility};
    use crate::{EdgeType, ProjectedView};

    fn polygon(view: &mut ProjectedView, c: Point2D, r: f64, n: usize, vis: Visibility) {
        for i in 0..n {
            let a0 = i as f64 / n as f64 * std::f64::consts::TAU;
            let a1 = (i + 1) as f64 / n as f64 * std::f64::consts::TAU;
            view.add_edge(ProjectedEdge::new(
                Point2D::new(c.x + r * a0.cos(), c.y + r * a0.sin()),
                Point2D::new(c.x + r * a1.cos(), c.y + r * a1.sin()),
                vis,
                EdgeType::Sharp,
                0.0,
            ));
        }
    }

    fn plate() -> ProjectedView {
        let mut v = ProjectedView::new(ViewDirection::Front);
        polygon(
            &mut v,
            Point2D::new(50.0, 50.0),
            70.0,
            4,
            Visibility::Visible,
        ); // square outline
        polygon(
            &mut v,
            Point2D::new(20.0, 20.0),
            3.0,
            32,
            Visibility::Visible,
        );
        polygon(
            &mut v,
            Point2D::new(20.0, 20.0),
            3.0,
            32,
            Visibility::Hidden,
        ); // far rim
        polygon(
            &mut v,
            Point2D::new(80.0, 20.0),
            3.0,
            32,
            Visibility::Visible,
        );
        polygon(
            &mut v,
            Point2D::new(50.0, 70.0),
            8.0,
            48,
            Visibility::Visible,
        );
        v
    }

    #[test]
    fn weld_survives_cell_boundaries() {
        // Copies of each rim vertex jittered across rounding boundaries.
        let mut v = ProjectedView::new(ViewDirection::Front);
        polygon(
            &mut v,
            Point2D::new(50.0, 50.0),
            70.0,
            4,
            Visibility::Visible,
        );
        let q = 70.0 * 2f64.sqrt() * 1e-4;
        let (c, r, n) = (Point2D::new(0.5 * q, 0.5 * q), 4.0, 64);
        for i in 0..n {
            let p = |k: usize, j: f64| {
                let a = k as f64 / n as f64 * std::f64::consts::TAU;
                Point2D::new(c.x + r * a.cos() + j, c.y + r * a.sin() + j)
            };
            v.add_edge(ProjectedEdge::new(
                p(i, 1e-9),
                p(i + 1, -1e-9),
                Visibility::Visible,
                EdgeType::Sharp,
                0.0,
            ));
        }
        assert!(detect_circles(&v)
            .iter()
            .any(|c| (c.radius - 4.0).abs() < 0.01));
    }

    #[test]
    fn rim_with_a_junction_still_found() {
        let mut v = plate();
        // A line ending on the rim of the right-hand hole (degree-3 vertex).
        v.add_edge(ProjectedEdge::new(
            Point2D::new(83.0, 20.0),
            Point2D::new(95.0, 20.0),
            Visibility::Visible,
            EdgeType::Sharp,
            0.0,
        ));
        assert_eq!(detect_circles(&v).len(), 3);
    }

    #[test]
    fn finds_holes_not_outline() {
        let cs = detect_circles(&plate());
        assert_eq!(cs.len(), 3, "{cs:?}");
        assert!((cs[0].radius - 3.0).abs() < 0.05);
        assert!((cs[2].center.x - 80.0).abs() < 1e-6);
    }

    #[test]
    fn groups_and_formats() {
        let groups = group_circles(&detect_circles(&plate()));
        let texts: Vec<String> = groups.iter().map(diameter_text).collect();
        assert_eq!(texts.len(), 2);
        assert!(texts[0].starts_with("2× ⌀"), "{texts:?}");
        assert_eq!(format_mm(6.0), "6");
        assert_eq!(format_mm(6.5), "6.5");
        assert_eq!(format_mm(6.126), "6.13");
    }

    #[test]
    fn center_mark_spans_past_rim() {
        let rd = center_mark(Point2D::new(0.0, 0.0), 10.0);
        let (lo, hi) = rd.bounds().unwrap();
        assert!((hi.x - 13.0).abs() < 1e-9 && (lo.y + 13.0).abs() < 1e-9);
    }
}

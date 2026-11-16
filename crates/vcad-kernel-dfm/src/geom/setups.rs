//! Setup planning for axis-aligned milling: which tool axes (±X/±Y/±Z) reach
//! which parts of the solid, and the smallest set of setups that reaches
//! everything that can be reached at all.
//!
//! A sample (a point on a face with its outward normal) is reachable from
//! axis `d` — the direction from the part toward the spindle — when the face
//! does not look away from `d` and a ray from just off the surface along `d`
//! escapes the solid. Walls parallel to `d` count: the flank of the cutter
//! reaches them (a Z bore is cut from +Z), which is why the test is line of
//! sight rather than `normal · d > 0`.

use vcad_kernel_math::{Point3, Vec3};

use super::probe::{Probe, Regions};

/// A tool axis: the direction from the part toward the spindle.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub enum Axis {
    /// +Z (the default top setup).
    PosZ,
    /// −Z (flipped).
    NegZ,
    /// +X.
    PosX,
    /// −X.
    NegX,
    /// +Y.
    PosY,
    /// −Y.
    NegY,
}

impl Axis {
    /// All six, in tie-break preference order (+Z first).
    pub const ALL: [Axis; 6] = [
        Axis::PosZ,
        Axis::NegZ,
        Axis::PosX,
        Axis::NegX,
        Axis::PosY,
        Axis::NegY,
    ];

    /// Unit direction toward the spindle.
    pub fn dir(self) -> Vec3 {
        match self {
            Axis::PosZ => Vec3::new(0.0, 0.0, 1.0),
            Axis::NegZ => Vec3::new(0.0, 0.0, -1.0),
            Axis::PosX => Vec3::new(1.0, 0.0, 0.0),
            Axis::NegX => Vec3::new(-1.0, 0.0, 0.0),
            Axis::PosY => Vec3::new(0.0, 1.0, 0.0),
            Axis::NegY => Vec3::new(0.0, -1.0, 0.0),
        }
    }

    /// `"+Z"`, `"−Y"`, …
    pub fn label(self) -> &'static str {
        match self {
            Axis::PosZ => "+Z",
            Axis::NegZ => "−Z",
            Axis::PosX => "+X",
            Axis::NegX => "−X",
            Axis::PosY => "+Y",
            Axis::NegY => "−Y",
        }
    }

    /// Parse `"+z"`, `"-Y"`, `"z"` (= +Z), `"−x"`.
    pub fn parse(s: &str) -> Option<Axis> {
        let t = s.trim().to_ascii_lowercase().replace('−', "-");
        Some(match t.as_str() {
            "+z" | "z" => Axis::PosZ,
            "-z" => Axis::NegZ,
            "+x" | "x" => Axis::PosX,
            "-x" => Axis::NegX,
            "+y" | "y" => Axis::PosY,
            "-y" => Axis::NegY,
            _ => return None,
        })
    }

    fn bit(self) -> u8 {
        1 << (self as u8)
    }
}

/// Reachability of one region.
#[derive(Debug, Clone)]
pub struct RegionReach {
    /// Region id.
    pub region: usize,
    /// Area reachable from no allowed axis, mm².
    pub unreachable_area: f64,
    /// A point in the unreachable part (the largest unreachable sample).
    pub unreachable_anchor: Option<Point3>,
    /// Faces of the region with unreachable samples.
    pub unreachable_faces: Vec<usize>,
}

/// Result of the setup analysis.
#[derive(Debug, Clone)]
pub struct SetupPlan {
    /// Chosen setups, in machining order (preference order).
    pub setups: Vec<Axis>,
    /// Per chosen setup: area it adds that earlier setups didn't reach, mm².
    pub added_area: Vec<f64>,
    /// Per chosen setup: faces it adds (reached by none of the earlier ones).
    pub added_faces: Vec<Vec<usize>>,
    /// Per region with any unreachable area.
    pub unreachable: Vec<RegionReach>,
    /// Total sampled area, mm².
    pub total_area: f64,
    /// Area of inside-out (folded) boolean debris skipped, mm².
    pub inverted_area: f64,
}

/// Samples per face. Triangulated B-reps have one triangle per face; an
/// analytic face is judged on its largest triangles.
const SAMPLES_PER_FACE: usize = 24;

/// How far off the surface a reachability ray starts, mm.
pub const WALL_OFFSET_MM: f64 = 0.05;

/// Plan setups over `axes`. Samples smaller than `min_sample_area` (mm²) are
/// tessellation debris and don't get a vote.
pub fn plan(probe: &Probe, regions: &Regions, axes: &[Axis], min_sample_area: f64) -> SetupPlan {
    // Off-surface step. Rays along a wall run parallel to it, and on faceted
    // geometry the next segment of the same hole can be faceted at a
    // different phase or count (boolean rims are sag-adaptive), its chords
    // up to one chord-sag (~0.05 mm at R10/32) inside this one's. Step off
    // by more than that; nothing narrower than 0.1 mm is millable anyway.
    let eps = WALL_OFFSET_MM.max(probe.scale * 1e-4);
    let mut masks: Vec<(u8, usize, f64, Point3)> = Vec::new(); // (mask, face, area, point)
    let mut inverted_area = 0.0;
    for f in 0..probe.face_tris.len() {
        if regions.region(f).is_none() || regions.sliver.get(f).copied().unwrap_or(true) {
            continue;
        }
        for s in probe.samples(f, SAMPLES_PER_FACE) {
            if s.area < min_sample_area {
                continue;
            }
            if probe.inverted(&s, eps) {
                inverted_area += s.area;
                continue;
            }
            let mut mask = 0u8;
            for &ax in axes {
                let d = ax.dir();
                // Looking away from the spindle: an undercut from this side.
                if s.normal.dot(d) < -0.02 {
                    continue;
                }
                let o = s.point + s.normal * eps;
                if !probe.occluded(o, d, 0.0) {
                    mask |= ax.bit();
                }
            }
            masks.push((mask, f, s.area, s.point));
        }
    }
    let total_area: f64 = masks.iter().map(|m| m.2).sum();
    // Distinct masks of reachable samples; a subset of axes covers a sample
    // when it shares a bit with its mask.
    let mut distinct: Vec<u8> = masks.iter().map(|m| m.0).filter(|&m| m != 0).collect();
    distinct.sort_unstable();
    distinct.dedup();

    // Smallest covering subset; ties go to the subset whose axes come first
    // in preference order (+Z, −Z, +X, −X, +Y, −Y).
    let allowed: u8 = axes.iter().fold(0, |a, ax| a | ax.bit());
    let mut best: Option<(u32, u32, u8)> = None; // (count, rank, subset)
    for subset in 1u8..64 {
        if subset & !allowed != 0 {
            continue;
        }
        if !distinct.iter().all(|&m| m & subset != 0) {
            continue;
        }
        let count = subset.count_ones();
        // Lexicographic preference: a subset holding +Z beats any without
        // it, and so on down the list. Lower rank wins.
        let rank: u32 = 63
            - (0..6)
                .filter(|b| subset & (1 << b) != 0)
                .map(|b| 1u32 << (5 - b))
                .sum::<u32>();
        let better = match best {
            None => true,
            Some((c, r, _)) => (count, rank) < (c, r),
        };
        if better {
            best = Some((count, rank, subset));
        }
    }
    let subset = if distinct.is_empty() {
        0
    } else {
        best.map(|b| b.2).unwrap_or(0)
    };
    let setups: Vec<Axis> = Axis::ALL
        .into_iter()
        .filter(|a| subset & a.bit() != 0)
        .collect();

    let mut added_area = vec![0.0; setups.len()];
    let mut added_faces: Vec<std::collections::BTreeSet<usize>> =
        vec![Default::default(); setups.len()];
    let mut unreach: std::collections::BTreeMap<usize, RegionReach> = Default::default();
    let mut largest: std::collections::HashMap<usize, f64> = Default::default();
    for &(mask, face, area, point) in &masks {
        if let Some(i) = setups.iter().position(|a| mask & a.bit() != 0) {
            added_area[i] += area;
            added_faces[i].insert(face);
            continue;
        }
        let Some(region) = regions.region(face) else {
            continue;
        };
        let r = unreach.entry(region).or_insert_with(|| RegionReach {
            region,
            unreachable_area: 0.0,
            unreachable_anchor: None,
            unreachable_faces: Vec::new(),
        });
        r.unreachable_area += area;
        if !r.unreachable_faces.contains(&face) {
            r.unreachable_faces.push(face);
        }
        let l = largest.entry(region).or_insert(0.0);
        if area > *l {
            *l = area;
            r.unreachable_anchor = Some(point);
        }
    }
    let mut unreachable: Vec<RegionReach> = unreach.into_values().collect();
    for r in &mut unreachable {
        r.unreachable_faces.sort_unstable();
    }
    SetupPlan {
        setups,
        added_area,
        added_faces: added_faces
            .into_iter()
            .map(|s| s.into_iter().collect())
            .collect(),
        unreachable,
        total_area,
        inverted_area,
    }
}

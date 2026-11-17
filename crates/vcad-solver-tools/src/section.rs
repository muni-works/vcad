//! Drawing data for a solved device in its meridional section: the
//! potential grid (downsampled for a heatmap), equipotential polylines
//! (marching squares on the full-resolution solve), the wire cross
//! sections and recorded deuteron tracks.
//!
//! Frame: kernel millimetres, the device axis on world Z through the
//! origin, the section plane y = 0. Grid data is r ≥ 0 (axisymmetric);
//! tracks carry a **signed** r (= x), so a recirculating ion visibly
//! crosses the axis instead of folding back on itself.

use std::collections::HashMap;

use serde::Serialize;
use vcad_kernel_particle as pk;

/// Downsampled potential grid, row-major `i * nz + j` (r index outer),
/// node (i, j) at r = i·dr_mm, z = −z_half_mm + j·dz_mm.
#[derive(Debug, Clone, Serialize)]
pub struct PotentialGrid {
    /// Radial nodes.
    pub nr: usize,
    /// Axial nodes.
    pub nz: usize,
    /// Radial spacing, mm.
    pub dr_mm: f64,
    /// Axial spacing, mm.
    pub dz_mm: f64,
    /// Minimum potential, V.
    pub min_v: f64,
    /// Maximum potential, V.
    pub max_v: f64,
    /// Potential per node, V (rounded to 0.1 V).
    pub values: Vec<f64>,
}

/// One equipotential level.
#[derive(Debug, Clone, Serialize)]
pub struct Equipotential {
    /// Potential, V.
    pub level_v: f64,
    /// Polylines, each a list of `[r_mm, z_mm]` (r ≥ 0).
    pub polylines: Vec<Vec<[f64; 2]>>,
}

/// A wire ring's cross section in the section plane.
#[derive(Debug, Clone, Serialize)]
pub struct WireSection {
    /// Ring radius (the cross section's r), mm.
    pub r_mm: f64,
    /// Ring z, mm.
    pub z_mm: f64,
    /// Wire radius, mm.
    pub radius_mm: f64,
    /// Bias, V.
    pub potential_v: f64,
}

/// One recorded deuteron track.
#[derive(Debug, Clone, Serialize)]
pub struct Track {
    /// "wire" | "wall" | "survived" (censored at the pass budget).
    pub fate: &'static str,
    /// Wire ring index when `fate == "wire"`.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub wire: Option<usize>,
    /// Core passes.
    pub passes: u32,
    /// Path, `[r_signed_mm, z_mm]` (r_signed = sign(x)·√(x²+y²)).
    pub points: Vec<[f64; 2]>,
}

/// The whole section payload.
#[derive(Debug, Clone, Serialize)]
pub struct Section {
    /// Human-readable frame statement.
    pub frame: &'static str,
    /// Chamber inner radius, mm.
    pub r_max_mm: f64,
    /// Chamber half height, mm.
    pub z_half_mm: f64,
    /// Potential heatmap.
    pub potential: PotentialGrid,
    /// Equipotential polylines.
    pub equipotentials: Vec<Equipotential>,
    /// Wires.
    pub wires: Vec<WireSection>,
    /// Tracks.
    pub tracks: Vec<Track>,
    /// Launch-shell radius, mm (where the ensemble starts at rest).
    pub launch_shell_mm: f64,
    /// Core radius used for pass counting, mm.
    pub core_radius_mm: f64,
}

/// The frame statement shipped with every section.
pub const FRAME: &str =
    "kernel mm; device axis = world Z through the origin; section plane y = 0 (x = signed r)";

fn round(v: f64, q: f64) -> f64 {
    (v / q).round() * q
}

/// Fate label + wire index.
pub fn fate_label(f: pk::trace::Fate) -> (&'static str, Option<usize>) {
    match f {
        pk::trace::Fate::Wire(k) => ("wire", Some(k)),
        pk::trace::Fate::Wall => ("wall", None),
        pk::trace::Fate::Survived => ("survived", None),
    }
}

/// Recorded path → section track.
pub fn track(p: &pk::trace::TracePath) -> Track {
    let (fate, wire) = fate_label(p.outcome.fate);
    Track {
        fate,
        wire,
        passes: p.outcome.core_passes,
        points: p
            .points_mm
            .iter()
            .map(|q| {
                let r = q[0].hypot(q[1]).copysign(q[0]);
                [round(r, 0.01), round(q[2], 0.01)]
            })
            .collect(),
    }
}

/// Downsample the solve to at most `max_nodes` along its longer axis.
pub fn potential_grid(sol: &pk::poisson::Solution, max_nodes: usize) -> PotentialGrid {
    let longest = sol.nr.max(sol.nz);
    let stride = longest.div_ceil(max_nodes.max(8)).max(1);
    let nr = (sol.nr - 1) / stride + 1;
    let nz = (sol.nz - 1) / stride + 1;
    let mut values = Vec::with_capacity(nr * nz);
    let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
    for i in 0..nr {
        for j in 0..nz {
            let v = sol.phi[(i * stride) * sol.nz + j * stride];
            lo = lo.min(v);
            hi = hi.max(v);
            values.push(round(v, 0.1));
        }
    }
    PotentialGrid {
        nr,
        nz,
        dr_mm: sol.dr * 1e3 * stride as f64,
        dz_mm: sol.dz * 1e3 * stride as f64,
        min_v: lo,
        max_v: hi,
        values,
    }
}

/// `n` evenly spaced interior levels between the solve's min and max.
pub fn levels(sol: &pk::poisson::Solution, n: usize) -> Vec<f64> {
    let lo = sol.phi.iter().copied().fold(f64::INFINITY, f64::min);
    let hi = sol.phi.iter().copied().fold(f64::NEG_INFINITY, f64::max);
    if hi <= lo || hi.is_nan() || lo.is_nan() {
        return Vec::new();
    }
    (1..=n)
        .map(|k| lo + (hi - lo) * k as f64 / (n + 1) as f64)
        .collect()
}

/// Edge key: (i, j, dir) — dir 0 = the r-edge from node (i,j) to (i+1,j),
/// dir 1 = the z-edge from (i,j) to (i,j+1).
type EdgeKey = (u32, u32, u8);

/// Marching squares for one level on an `nr × nz` grid (row-major
/// `i * nz + j`), spacing (dr, dz) in mm, origin (0, z0). Returns joined
/// polylines.
pub fn contour(
    phi: &[f64],
    nr: usize,
    nz: usize,
    dr: f64,
    dz: f64,
    z0: f64,
    level: f64,
) -> Vec<Vec<[f64; 2]>> {
    let at = |i: usize, j: usize| phi[i * nz + j];
    let mut points: HashMap<EdgeKey, [f64; 2]> = HashMap::new();
    let mut segs: Vec<(EdgeKey, EdgeKey)> = Vec::new();
    let mut point = |k: EdgeKey| -> EdgeKey {
        points.entry(k).or_insert_with(|| {
            let (i, j, d) = (k.0 as usize, k.1 as usize, k.2);
            let (i2, j2) = if d == 0 { (i + 1, j) } else { (i, j + 1) };
            let (a, b) = (at(i, j), at(i2, j2));
            let t = if (b - a).abs() > 1e-300 {
                ((level - a) / (b - a)).clamp(0.0, 1.0)
            } else {
                0.5
            };
            let (r1, z1) = (i as f64 * dr, z0 + j as f64 * dz);
            let (r2, z2) = (i2 as f64 * dr, z0 + j2 as f64 * dz);
            [r1 + t * (r2 - r1), z1 + t * (z2 - z1)]
        });
        k
    };
    for i in 0..nr - 1 {
        for j in 0..nz - 1 {
            let (a, b, c, d) = (at(i, j), at(i + 1, j), at(i + 1, j + 1), at(i, j + 1));
            let case = (a > level) as u8
                | ((b > level) as u8) << 1
                | ((c > level) as u8) << 2
                | ((d > level) as u8) << 3;
            if case == 0 || case == 15 {
                continue;
            }
            let (iu, ju) = (i as u32, j as u32);
            let e0 = (iu, ju, 0u8);
            let e1 = (iu + 1, ju, 1u8);
            let e2 = (iu, ju + 1, 0u8);
            let e3 = (iu, ju, 1u8);
            let center_above = (a + b + c + d) * 0.25 > level;
            let pairs: &[(EdgeKey, EdgeKey)] = match case {
                1 | 14 => &[(e3, e0)],
                2 | 13 => &[(e0, e1)],
                3 | 12 => &[(e3, e1)],
                4 | 11 => &[(e1, e2)],
                6 | 9 => &[(e0, e2)],
                7 | 8 => &[(e3, e2)],
                5 => {
                    if center_above {
                        &[(e0, e1), (e2, e3)]
                    } else {
                        &[(e3, e0), (e1, e2)]
                    }
                }
                10 => {
                    if center_above {
                        &[(e3, e0), (e1, e2)]
                    } else {
                        &[(e0, e1), (e2, e3)]
                    }
                }
                _ => &[],
            };
            for &(p, q) in pairs {
                segs.push((point(p), point(q)));
            }
        }
    }
    // Chain segments through shared edge points.
    let mut adj: HashMap<EdgeKey, Vec<usize>> = HashMap::new();
    for (s, (p, q)) in segs.iter().enumerate() {
        adj.entry(*p).or_default().push(s);
        adj.entry(*q).or_default().push(s);
    }
    let mut used = vec![false; segs.len()];
    let other = |s: usize, k: EdgeKey| if segs[s].0 == k { segs[s].1 } else { segs[s].0 };
    let walk = |used: &mut Vec<bool>, start: usize, from: EdgeKey| -> Vec<EdgeKey> {
        let mut chain = vec![from];
        let mut cur = start;
        let mut key = from;
        loop {
            used[cur] = true;
            key = other(cur, key);
            chain.push(key);
            match adj[&key].iter().copied().find(|&n| !used[n]) {
                Some(n) => cur = n,
                None => break,
            }
        }
        chain
    };
    let mut out = Vec::new();
    for s in 0..segs.len() {
        if used[s] {
            continue;
        }
        let k0 = segs[s].0;
        let forward = walk(&mut used, s, k0);
        let back_start = adj[&k0].iter().copied().find(|&n| !used[n]);
        let mut keys: Vec<EdgeKey> = match back_start {
            Some(b) => {
                let mut back = walk(&mut used, b, k0);
                back.reverse();
                back.pop(); // k0 again
                back
            }
            None => Vec::new(),
        };
        keys.extend(forward);
        out.push(
            keys.iter()
                .map(|k| {
                    let p = points[k];
                    [round(p[0], 0.01), round(p[1], 0.01)]
                })
                .collect(),
        );
    }
    out
}

/// Equipotentials for `n` levels over the full-resolution solve.
pub fn equipotentials(sol: &pk::poisson::Solution, n: usize) -> Vec<Equipotential> {
    let (dr, dz, z0) = (sol.dr * 1e3, sol.dz * 1e3, -sol.z_half * 1e3);
    levels(sol, n)
        .into_iter()
        .map(|level| Equipotential {
            level_v: round(level, 0.1),
            polylines: contour(&sol.phi, sol.nr, sol.nz, dr, dz, z0, level),
        })
        .collect()
}

/// Wire cross sections of a device.
pub fn wires(device: &pk::device::Device) -> Vec<WireSection> {
    device
        .rings
        .iter()
        .map(|r| WireSection {
            r_mm: r.ring_radius_mm,
            z_mm: r.z_mm,
            radius_mm: r.wire_radius_mm,
            potential_v: r.potential_v,
        })
        .collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn contour_of_a_linear_ramp_is_one_straight_line() {
        // φ = r on a 5×4 grid: the 1.5 level is the vertical line r = 1.5.
        let (nr, nz) = (5, 4);
        let phi: Vec<f64> = (0..nr)
            .flat_map(|i| (0..nz).map(move |_| i as f64))
            .collect();
        let lines = contour(&phi, nr, nz, 1.0, 1.0, 0.0, 1.5);
        assert_eq!(lines.len(), 1, "one joined polyline");
        let l = &lines[0];
        assert_eq!(l.len(), nz, "one point per z row");
        for p in l {
            assert!((p[0] - 1.5).abs() < 1e-9);
        }
        let mut zs: Vec<f64> = l.iter().map(|p| p[1]).collect();
        zs.sort_by(f64::total_cmp);
        assert_eq!(zs, vec![0.0, 1.0, 2.0, 3.0]);
    }

    #[test]
    fn contour_around_a_peak_closes() {
        // A single raised node in the middle: the level ring is closed
        // (first point == last point after joining).
        let (nr, nz) = (5, 5);
        let mut phi = vec![0.0; nr * nz];
        phi[2 * nz + 2] = 1.0;
        let lines = contour(&phi, nr, nz, 1.0, 1.0, 0.0, 0.5);
        assert_eq!(lines.len(), 1);
        let l = &lines[0];
        assert_eq!(l.first(), l.last(), "closed ring");
        assert_eq!(l.len(), 5, "diamond: 4 edge points + closure");
    }
}

//! Rotary turning on a mill's 4th axis ("endmill turning").
//!
//! No lathe? A 4th axis (A rotates about machine X) can still make round
//! parts: chuck the bar along X, put a **spinning endmill** on the rotary
//! centreline (Y = 0), and let A supply the feed. The cutting speed comes
//! from the endmill, so A only has to turn at feed rate — a few rpm — which
//! is what a harmonic-drive or worm 4th axis can do. This is the strategy for
//! the Makera Carvera Air's 4th-axis module.
//!
//! Geometry. With the endmill vertical and on the centreline, the cutter at
//! tip height `z` above the axis leaves radius `z + lift(dx)` at axial offset
//! `dx` (`lift = 0` for a flat endmill, `rb − √(rb² − dx²)` for a ball), once
//! A has turned a full revolution. So rotary turning is exactly a 1D drop
//! cutter on the profile `R(x)`; paths are helices (X advances one pitch per
//! revolution of A).
//!
//! Machine facts this module relies on (Carvera community firmware docs,
//! "4th Axis Feed Rate", firmware 2.1.0c): `G93` inverse-time feed is
//! supported on A moves, and the C1/CA1 harmonic-drive axis tops out near
//! 6.6 rpm. Stock firmware instead derives A speed from an estimated radius
//! (Y/Z work coordinates, with a +30 mm diameter pad), which makes `G94`
//! feeds on X–A moves ambiguous — so this post uses `G93` for every cutting
//! move. Whether A is unlimited (continuous) on a given machine is **not**
//! documented; [`ATravel`] must be stated by the caller.

use super::geom::P2;
use super::profile::TurnProfile;
use super::program::TurnStock;
use super::TurnError;
use serde::{Deserialize, Serialize};
use std::fmt::Write as _;

/// Tip-height jump treated as a wall (gets a full spring revolution), mm.
const SPRING_JUMP: f64 = 0.05;

/// Endmill used for rotary turning.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum Endmill {
    /// Flat (square) endmill.
    Flat {
        /// Diameter, mm.
        diameter: f64,
    },
    /// Ball endmill.
    Ball {
        /// Diameter, mm.
        diameter: f64,
    },
}

impl Endmill {
    /// Half the diameter.
    pub fn half(&self) -> f64 {
        match *self {
            Endmill::Flat { diameter } | Endmill::Ball { diameter } => diameter / 2.0,
        }
    }

    /// Height of the cutting surface above the tip at axial offset `dx`
    /// (`∞` outside the footprint).
    pub fn lift(&self, dx: f64) -> f64 {
        let h = self.half();
        if dx.abs() > h + 1e-12 {
            return f64::INFINITY;
        }
        match *self {
            Endmill::Flat { .. } => 0.0,
            Endmill::Ball { .. } => h - (h * h - dx * dx).max(0.0).sqrt(),
        }
    }
}

/// Rotary-axis travel.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum ATravel {
    /// A may turn without limit.
    Continuous,
    /// A must stay within `[0, max_deg]`.
    Limited {
        /// Largest A, degrees.
        max_deg: f64,
    },
}

/// The 4th axis a rotary program runs on.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RotaryMachine {
    /// Largest A rate, deg/min.
    pub a_max_deg_per_min: f64,
    /// A travel.
    pub a_travel: ATravel,
}

impl RotaryMachine {
    /// Carvera Air 4th-axis module on community firmware ≥ 2.1.0c: A capped
    /// at 6.6 rpm (2376 deg/min, the community docs' figure for the
    /// harmonic-drive axis at Makera's settings). Travel must be stated.
    pub fn carvera_air(a_travel: ATravel) -> Self {
        Self {
            a_max_deg_per_min: 6.6 * 360.0,
            a_travel,
        }
    }
}

/// Rotary turning parameters.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RotaryParams {
    /// Endmill spindle speed, rpm.
    pub spindle_rpm: f64,
    /// Target surface feed, mm/min (A is slowed further if it would exceed
    /// the machine's A rate).
    pub surface_feed: f64,
    /// Radial depth per roughing level, mm.
    pub stepdown: f64,
    /// Axial advance per A revolution while roughing, mm.
    pub rough_pitch: f64,
    /// Axial advance per A revolution on the finish pass, mm.
    pub finish_pitch: f64,
    /// Radial stock left by roughing, mm.
    pub allowance: f64,
    /// Rapid clearance above stock, mm.
    pub clearance: f64,
    /// Radius of the neck left at the chuck end (saw it off), mm.
    pub neck_radius: f64,
}

/// One move of a rotary program, machine coordinates (Y = 0 throughout).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum RotaryMove {
    /// G0.
    Rapid {
        /// X (along the part axis).
        x: f64,
        /// Z (tip height above the axis).
        z: f64,
        /// A, degrees.
        a: f64,
    },
    /// G1 under G93.
    Feed {
        /// X.
        x: f64,
        /// Z.
        z: f64,
        /// A, degrees.
        a: f64,
    },
}

/// A planned rotary-turning program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RotaryProgram {
    /// Job name.
    pub name: String,
    /// Cutter.
    pub endmill: Endmill,
    /// Machine.
    pub machine: RotaryMachine,
    /// Parameters.
    pub params: RotaryParams,
    /// Moves.
    pub moves: Vec<RotaryMove>,
}

/// The radius the cutter must not go under, as a piecewise-linear function
/// of x: part OD, a neck band at the chuck end, zero beyond the face, stock
/// radius on the chuck side of the band.
struct Requirement {
    segs: Vec<(P2, P2)>, // [x, r], a.x ≤ b.x
}

impl Requirement {
    fn new(profile: &TurnProfile, stock: &TurnStock, neck: f64, band: f64) -> Self {
        let zmin = profile.z_min();
        let mut pts: Vec<P2> = Vec::new();
        let far = 1e4;
        pts.push([zmin - far, stock.radius()]);
        pts.push([zmin - band, stock.radius()]);
        pts.push([zmin - band, neck]);
        pts.push([zmin, neck]);
        // Outer radius along the part, sampled at every profile vertex.
        let mut zs: Vec<f64> = profile.polygon(5e-4).iter().map(|p| p[0]).collect();
        zs.sort_by(f64::total_cmp);
        zs.dedup_by(|a, b| (*a - *b).abs() < 1e-9);
        for &z in &zs {
            let lo = profile.radii_at(z - 1e-7).map_or(0.0, |v| v.1);
            let hi = profile.radii_at(z + 1e-7).map_or(0.0, |v| v.1);
            pts.push([z, lo]);
            if (hi - lo).abs() > 1e-9 {
                pts.push([z, hi]);
            }
        }
        pts.push([0.0, 0.0]);
        pts.push([far, 0.0]);
        let segs = pts
            .windows(2)
            .filter(|w| w[1][0] >= w[0][0] - 1e-12)
            .map(|w| (w[0], w[1]))
            .collect();
        Requirement { segs }
    }

    /// Lowest tip height for a cutter centred anywhere in `[x − extra,
    /// x + extra]` (conservative: lift taken at `|dx| − extra`).
    fn ztip(&self, cutter: &Endmill, x: f64, extra: f64) -> f64 {
        let hw = cutter.half() + extra;
        let (lo, hi) = (x - hw, x + hw);
        let f = |xx: f64, r: f64| {
            let dx = ((xx - x).abs() - extra).max(0.0);
            r - cutter.lift(dx)
        };
        let mut best = f64::NEG_INFINITY;
        for &(a, b) in &self.segs {
            if b[0] < lo || a[0] > hi {
                continue;
            }
            if (b[0] - a[0]).abs() < 1e-12 {
                best = best.max(f(a[0], a[1].max(b[1])));
                continue;
            }
            let m = (b[1] - a[1]) / (b[0] - a[0]);
            let r_at = |xx: f64| a[1] + m * (xx - a[0]);
            let (c0, c1) = (a[0].max(lo), b[0].min(hi));
            best = best.max(f(c0, r_at(c0))).max(f(c1, r_at(c1)));
            if let Endmill::Ball { .. } = cutter {
                let rb = cutter.half();
                // Stationary point of r(x+dx) − lift(dx) on each side.
                let d = m * rb / (1.0 + m * m).sqrt();
                for xx in [x + extra + d, x - extra + d] {
                    if xx > c0 && xx < c1 {
                        best = best.max(f(xx, r_at(xx)));
                    }
                }
            }
        }
        best
    }

    fn breakpoints(&self) -> Vec<f64> {
        self.segs.iter().flat_map(|(a, b)| [a[0], b[0]]).collect()
    }
}

/// Plan rotary turning of the OD of `profile` (finished face at X0, part
/// toward −X, rotary centreline at Y0 Z0).
pub fn plan_rotary(
    name: impl Into<String>,
    profile: &TurnProfile,
    stock: &TurnStock,
    endmill: Endmill,
    machine: RotaryMachine,
    p: RotaryParams,
) -> Result<RotaryProgram, TurnError> {
    if profile.z_max().abs() > 1e-6 {
        return Err(TurnError::BadProfile(
            "finished face must be at z = 0 (use face_at_zero())".into(),
        ));
    }
    if profile.max_radius() > stock.radius() + 1e-9 {
        return Err(TurnError::BadStock("part larger than stock".into()));
    }
    if profile.has_bore() {
        return Err(TurnError::NotTurnable(
            "rotary turning reaches the OD only: drill/bore the hole on the lathe or as a \
             separate end-on op"
                .into(),
        ));
    }
    for (v, n) in [
        (p.spindle_rpm, "spindle rpm"),
        (p.surface_feed, "surface feed"),
        (p.stepdown, "stepdown"),
        (p.rough_pitch, "rough pitch"),
        (p.finish_pitch, "finish pitch"),
        (p.clearance, "clearance"),
    ] {
        if v.is_nan() || v <= 0.0 {
            return Err(TurnError::BadParams(format!("{n} must be > 0")));
        }
    }
    if p.rough_pitch > 2.0 * endmill.half() * 0.9 {
        return Err(TurnError::BadParams(format!(
            "rough pitch {} leaves ridges: keep it under 90 % of the cutter diameter",
            p.rough_pitch
        )));
    }
    let hw = endmill.half();
    let band = 2.0 * hw + 0.5;
    if profile.z_min() - band < stock.z_chuck() + 1.0 {
        return Err(TurnError::BadStock(
            "not enough stickout for the part plus a cutter-width neck band".into(),
        ));
    }
    let req = Requirement::new(profile, stock, p.neck_radius, band);
    let x_start = stock.z_face() + hw + p.clearance;
    let x_end = profile.z_min() - band + hw;
    let safe = stock.radius() + p.clearance;
    let mut moves: Vec<RotaryMove> = Vec::new();
    let mut a = 0.0f64;
    moves.push(RotaryMove::Rapid {
        x: x_start,
        z: safe,
        a,
    });

    // Sample grid: fine, plus wall-aligned breakpoints.
    let step = 0.01;
    let mut xs: Vec<f64> = Vec::new();
    let n = ((x_start - x_end) / step).ceil() as usize;
    for i in 0..=n {
        xs.push(x_start - (x_start - x_end) * i as f64 / n as f64);
    }
    for b in req.breakpoints() {
        for x in [b + hw + 1e-4, b - hw - 1e-4] {
            if x < x_start && x > x_end {
                xs.push(x);
            }
        }
    }
    xs.sort_by(|a, b| b.total_cmp(a));
    xs.dedup_by(|a, b| (*a - *b).abs() < 1e-7);
    // Per-step safe height (max over the step) and per-point heights.
    let step_z = |allow: f64| -> Vec<f64> {
        xs.windows(2)
            .map(|w| req.ztip(&endmill, 0.5 * (w[0] + w[1]), 0.5 * (w[0] - w[1])) + allow)
            .collect()
    };

    // Roughing levels.
    let sz_r = step_z(p.allowance);
    let zmin_r = sz_r.iter().cloned().fold(f64::INFINITY, f64::min).max(0.0);
    let top = stock.radius();
    if top > zmin_r + 1e-9 {
        let nl = ((top - zmin_r) / p.stepdown).ceil() as usize;
        for k in 1..=nl {
            let zl = top - (top - zmin_r) * k as f64 / nl as f64;
            // Runs of consecutive steps that are free at this level.
            let mut runs: Vec<(usize, usize)> = Vec::new();
            let mut i = 0;
            while i < sz_r.len() {
                if sz_r[i] <= zl + 1e-9 {
                    let s = i;
                    while i < sz_r.len() && sz_r[i] <= zl + 1e-9 {
                        i += 1;
                    }
                    runs.push((s, i)); // steps s..i → points s..=i
                } else {
                    i += 1;
                }
            }
            for (s, e) in runs {
                let (hi, lo) = (xs[s], xs[e]);
                if hi - lo < 1e-3 {
                    continue;
                }
                moves.push(RotaryMove::Rapid { x: hi, z: safe, a });
                if s == 0 {
                    moves.push(RotaryMove::Rapid { x: hi, z: zl, a });
                } else {
                    moves.push(RotaryMove::Feed { x: hi, z: zl, a });
                    a += 360.0;
                    moves.push(RotaryMove::Feed { x: hi, z: zl, a });
                }
                a += 360.0 * (hi - lo) / p.rough_pitch;
                moves.push(RotaryMove::Feed { x: lo, z: zl, a });
                a += 360.0;
                moves.push(RotaryMove::Feed { x: lo, z: zl, a });
                moves.push(RotaryMove::Rapid { x: lo, z: safe, a });
            }
        }
    }

    // Finish helix: a staircase of steps, each at the highest tip height
    // its own x-interval needs, with vertical Z changes at step boundaries
    // (a vertical move at a boundary is above both neighbours' needs).
    let sz_f = step_z(0.0);
    moves.push(RotaryMove::Rapid {
        x: xs[0],
        z: safe,
        a,
    });
    moves.push(RotaryMove::Rapid {
        x: xs[0],
        z: sz_f[0].max(0.0),
        a,
    });
    for i in 0..sz_f.len() {
        let zi = sz_f[i].max(0.0);
        a += 360.0 * (xs[i] - xs[i + 1]) / p.finish_pitch;
        moves.push(RotaryMove::Feed {
            x: xs[i + 1],
            z: zi,
            a,
        });
        if let Some(&zn) = sz_f.get(i + 1) {
            let zn = zn.max(0.0);
            // At a wall (a real jump, not a taper's step), turn one full
            // revolution at the lower height so the footprint's edge has
            // swept every angle there — a helix alone leaves a thread.
            let wall = (zn - zi).abs() > SPRING_JUMP;
            if wall && zn > zi {
                a += 360.0;
                moves.push(RotaryMove::Feed {
                    x: xs[i + 1],
                    z: zi,
                    a,
                });
            }
            if (zn - zi).abs() > 1e-9 {
                moves.push(RotaryMove::Feed {
                    x: xs[i + 1],
                    z: zn,
                    a,
                });
            }
            if wall && zn < zi {
                a += 360.0;
                moves.push(RotaryMove::Feed {
                    x: xs[i + 1],
                    z: zn,
                    a,
                });
            }
        }
    }
    a += 360.0;
    let last = *xs.last().unwrap_or(&x_end);
    moves.push(RotaryMove::Feed {
        x: last,
        z: sz_f[sz_f.len() - 1].max(0.0),
        a,
    });
    moves.push(RotaryMove::Rapid {
        x: last,
        z: safe,
        a,
    });
    moves.push(RotaryMove::Rapid {
        x: x_start,
        z: safe,
        a,
    });
    if let ATravel::Limited { max_deg } = machine.a_travel {
        if a > max_deg {
            return Err(TurnError::NotTurnable(format!(
                "the program turns A to {a:.0}°, past the stated {max_deg}° travel; rotary \
                 turning needs a continuous A"
            )));
        }
    }
    Ok(RotaryProgram {
        name: name.into(),
        endmill,
        machine,
        params: p,
        moves,
    })
}

impl RotaryProgram {
    /// Duration of one feed move, minutes: surface length at the target
    /// feed, slowed to the A limit.
    fn move_minutes(&self, from: [f64; 3], to: [f64; 3]) -> f64 {
        let r = 0.5 * (from[1] + to[1]).max(0.0);
        let da = (to[2] - from[2]).abs();
        let surf =
            ((to[0] - from[0]).powi(2) + (to[1] - from[1]).powi(2) + (r * da.to_radians()).powi(2))
                .sqrt();
        (surf / self.params.surface_feed).max(da / self.machine.a_max_deg_per_min * 1.01)
    }

    /// Estimated cycle time, seconds (rapids at `rapid_mm_min`, A rapids at
    /// the A limit).
    pub fn estimated_time(&self, rapid_mm_min: f64) -> f64 {
        let mut t = 0.0;
        let mut pos: Option<[f64; 3]> = None;
        for m in &self.moves {
            let (to, feed) = match *m {
                RotaryMove::Rapid { x, z, a } => ([x, z, a], false),
                RotaryMove::Feed { x, z, a } => ([x, z, a], true),
            };
            if let Some(p) = pos {
                t += if feed {
                    self.move_minutes(p, to)
                } else {
                    let d = ((to[0] - p[0]).powi(2) + (to[1] - p[1]).powi(2)).sqrt();
                    (d / rapid_mm_min).max((to[2] - p[2]).abs() / self.machine.a_max_deg_per_min)
                } * 60.0;
            }
            pos = Some(to);
        }
        t
    }

    /// Post for a Carvera (Smoothieware-derived) 4th axis: `G93` on every
    /// cutting move, A in degrees, Y pinned to 0.
    pub fn post_carvera(&self) -> String {
        let mut o = String::new();
        let _ = writeln!(o, "(vcad rotary turning: {})", self.name);
        let _ = writeln!(
            o,
            "(post: carvera 4th axis, community firmware >= 2.1.0c for G93 on A moves)"
        );
        let _ = writeln!(
            o,
            "(work zero: X0 = finished face, Y0 Z0 = rotary centreline; A in degrees)"
        );
        let _ = writeln!(o, "(cutter: {:?})", self.endmill);
        let _ = writeln!(o, "G21 G90 G17 G54");
        let _ = writeln!(o, "G94");
        let _ = writeln!(o, "M3 S{:.0}", self.params.spindle_rpm);
        let mut pos: Option<[f64; 3]> = None;
        let mut g93 = false;
        for m in &self.moves {
            match *m {
                RotaryMove::Rapid { x, z, a } => {
                    if g93 {
                        let _ = writeln!(o, "G94");
                        g93 = false;
                    }
                    match pos {
                        // Lift first, then travel, so a rapid never sweeps low.
                        Some(p) if z > p[1] => {
                            let _ = writeln!(o, "G0 Z{z:.4}");
                            let _ = writeln!(o, "G0 X{x:.4} Y0 A{a:.3}");
                        }
                        Some(_) => {
                            let _ = writeln!(o, "G0 X{x:.4} Y0 A{a:.3}");
                            let _ = writeln!(o, "G0 Z{z:.4}");
                        }
                        None => {
                            let _ = writeln!(o, "G0 Z{z:.4}");
                            let _ = writeln!(o, "G0 X{x:.4} Y0 A{a:.3}");
                        }
                    }
                    pos = Some([x, z, a]);
                }
                RotaryMove::Feed { x, z, a } => {
                    if !g93 {
                        let _ = writeln!(o, "G93");
                        g93 = true;
                    }
                    let from = pos.unwrap_or([x, z, a]);
                    let mins = self.move_minutes(from, [x, z, a]).max(1e-6);
                    let _ = writeln!(o, "G1 X{x:.4} Z{z:.4} A{a:.3} F{:.4}", 1.0 / mins);
                    pos = Some([x, z, a]);
                }
            }
        }
        let _ = writeln!(o, "G94");
        let _ = writeln!(o, "M5");
        let _ = writeln!(o, "M30");
        o
    }
}

/// Rotary oracle settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct RotaryVerifyOptions {
    /// Allowed deviation, mm.
    pub tolerance: f64,
    /// Axial column width, mm.
    pub column: f64,
}

impl Default for RotaryVerifyOptions {
    fn default() -> Self {
        Self {
            tolerance: 0.01,
            column: 0.02,
        }
    }
}

/// Rotary oracle answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RotaryVerdict {
    /// All checks passed.
    pub ok: bool,
    /// Failures (first few).
    pub findings: Vec<String>,
    /// Deepest gouge, mm.
    pub max_gouge: f64,
    /// Worst material left on the finished OD, mm.
    pub max_excess: f64,
    /// What the answer is resolved to.
    pub resolution: String,
}

/// Verify posted rotary G-code: replay X/Z/A, sweep the endmill's
/// footprint over an (x, θ) grid of the part, and check gouge, finish,
/// rapids into stock, Y = 0, spindle, and the A-rate limit.
pub fn verify_rotary_gcode(
    gcode: &str,
    profile: &TurnProfile,
    stock: &TurnStock,
    endmill: Endmill,
    machine: &RotaryMachine,
    opts: &RotaryVerifyOptions,
) -> RotaryVerdict {
    let tol = opts.tolerance;
    let h = opts.column;
    let mut findings: Vec<String> = Vec::new();
    let zmin = profile.z_min();
    let hw = endmill.half();
    let x0 = stock.z_chuck();
    let x1 = stock.z_face();
    let ncol = ((x1 - x0) / h).ceil().max(1.0) as usize;
    let nth = 360usize;
    let col_x = |i: usize| x0 + (i as f64 + 0.5) * h;
    let part_r: Vec<Option<f64>> = (0..ncol)
        .map(|i| {
            let x = col_x(i);
            if x > 0.0 {
                Some(0.0)
            } else if x >= zmin {
                Some(profile.radii_at(x).map_or(0.0, |v| v.1))
            } else {
                None
            }
        })
        .collect();
    let mut rtop = vec![stock.radius(); ncol * nth];
    let cosh = (0.5f64).to_radians().cos();
    let mut max_gouge = 0.0f64;
    let mut pos: Option<[f64; 3]> = None;
    let mut rapid = true;
    let mut g93 = false;
    let mut spindle = false;
    let (mut metric, mut absolute) = (false, false);
    for (li, raw) in gcode.lines().enumerate() {
        let line = li + 1;
        let s: String = {
            let mut out = String::new();
            let mut d = 0;
            for c in raw.chars() {
                match c {
                    '(' => d += 1,
                    ')' => d -= 1,
                    c if d == 0 => out.push(c),
                    _ => {}
                }
            }
            out.trim().to_ascii_uppercase()
        };
        if s.is_empty() {
            continue;
        }
        let mut w: Vec<(char, f64)> = Vec::new();
        let mut ok = true;
        for tok in s.split_whitespace() {
            let mut ch = tok.chars();
            let c = ch.next().unwrap_or(' ');
            match ch.as_str().parse::<f64>() {
                Ok(v) if c.is_ascii_alphabetic() => w.push((c, v)),
                _ => {
                    findings.push(format!("line {line}: cannot read '{tok}'"));
                    ok = false;
                }
            }
        }
        if !ok {
            continue;
        }
        let (mut x, mut y, mut z, mut a, mut f) = (None, None, None, None, None);
        for &(c, v) in &w {
            let iv = v.round() as i64;
            match c {
                'G' => match iv {
                    0 => rapid = true,
                    1 => rapid = false,
                    93 => g93 = true,
                    94 => g93 = false,
                    17 | 54 => {}
                    21 => metric = true,
                    90 => absolute = true,
                    g => findings.push(format!("line {line}: G{g} cannot be replayed")),
                },
                'M' => match iv {
                    3 => spindle = true,
                    5 => spindle = false,
                    30 | 2 => {}
                    m => findings.push(format!("line {line}: M{m} cannot be replayed")),
                },
                'X' => x = Some(v),
                'Y' => y = Some(v),
                'Z' => z = Some(v),
                'A' => a = Some(v),
                'F' => f = Some(v),
                'S' => {}
                c => findings.push(format!("line {line}: word {c} cannot be replayed")),
            }
        }
        if let Some(yv) = y {
            if yv.abs() > 1e-6 {
                findings.push(format!(
                    "line {line}: Y{yv} — the cutter must stay on the rotary centreline"
                ));
            }
        }
        if x.is_none() && z.is_none() && a.is_none() {
            continue;
        }
        if !metric || !absolute {
            findings.push(format!("line {line}: motion before G21 G90"));
            continue;
        }
        let prev = pos;
        let base = prev.unwrap_or([f64::NAN; 3]);
        let to = [
            x.unwrap_or(base[0]),
            z.unwrap_or(base[1]),
            a.unwrap_or(if base[2].is_nan() { 0.0 } else { base[2] }),
        ];
        pos = Some(to);
        let from = prev.unwrap_or([f64::NAN; 3]);
        if from.iter().chain(to.iter()).any(|v| v.is_nan()) {
            // Positioning from an unknown machine position: only rapids may.
            if !rapid {
                findings.push(format!("line {line}: feed from an unknown position"));
            }
            continue;
        }
        if !rapid {
            if !spindle {
                findings.push(format!("line {line}: feed with the spindle stopped"));
            }
            if !g93 {
                findings.push(format!(
                    "line {line}: X/A feed without G93 — feed on a mixed linear/rotary move \
                     is ambiguous on this firmware"
                ));
            }
            match f {
                Some(fv) if fv > 0.0 => {
                    let rate = (to[2] - from[2]).abs() * fv;
                    if rate > machine.a_max_deg_per_min * 1.02 {
                        findings.push(format!(
                            "line {line}: A at {rate:.0} deg/min exceeds the {:.0} limit",
                            machine.a_max_deg_per_min
                        ));
                    }
                }
                _ => findings.push(format!("line {line}: G93 move without F")),
            }
        }
        // Sweep.
        let da = (to[2] - from[2]).abs();
        let dx = (to[0] - from[0]).abs();
        let dz = (to[1] - from[1]).abs();
        let n = ((da / 1.0).max(dx / (0.5 * h)).max(dz / 0.005).ceil() as usize).max(1);
        for k in 0..=n {
            let t = k as f64 / n as f64;
            let px = from[0] + (to[0] - from[0]) * t;
            let pz = from[1] + (to[1] - from[1]) * t;
            let pa = from[2] + (to[2] - from[2]) * t;
            let th = (pa.rem_euclid(360.0).floor() as usize).min(nth - 1);
            let ia = (((px - hw - x0) / h).floor().max(0.0)) as usize;
            let ib = ((((px + hw - x0) / h).ceil()).max(0.0) as usize).min(ncol);
            for i in ia.min(ncol)..ib {
                let lift = endmill.lift(col_x(i) - px);
                if !lift.is_finite() {
                    continue;
                }
                let ze = pz + lift;
                if rapid {
                    let top = (0..nth)
                        .map(|j| rtop[i * nth + j])
                        .fold(f64::NEG_INFINITY, f64::max);
                    if ze < top - tol {
                        findings.push(format!(
                            "line {line}: rapid into stock at X{:.3} Z{ze:.3}",
                            col_x(i)
                        ));
                        break;
                    }
                    continue;
                }
                if let Some(r) = part_r[i] {
                    if col_x(i) <= 0.0 && ze < r - tol {
                        max_gouge = max_gouge.max(r - ze);
                    }
                }
                let cell = &mut rtop[i * nth + th];
                *cell = cell.min(ze.max(0.0) / cosh);
            }
        }
    }
    if max_gouge > tol {
        findings.push(format!("gouge {max_gouge:.4} mm into the part"));
    }
    let mut max_excess = 0.0f64;
    let mut worst = None;
    for (i, pr) in part_r.iter().enumerate() {
        let Some(r) = pr else { continue };
        for j in 0..nth {
            let e = rtop[i * nth + j] - r;
            if e > max_excess {
                max_excess = e;
                worst = Some((col_x(i), j));
            }
        }
    }
    if max_excess > tol {
        let (xw, tw) = worst.unwrap_or((0.0, 0));
        findings.push(format!(
            "material left: {max_excess:.4} mm at X{xw:.3}, A≈{tw}°"
        ));
    }
    if findings.len() > 20 {
        findings.truncate(20);
    }
    RotaryVerdict {
        ok: findings.is_empty(),
        findings,
        max_gouge,
        max_excess,
        resolution: format!(
            "tolerance {tol} mm; axial columns {h} mm; A bins 1° (leaves at most \
             r·(1/cos 0.5° − 1) unaccounted, ≈ 0.0005 mm at r = 12)"
        ),
    }
}

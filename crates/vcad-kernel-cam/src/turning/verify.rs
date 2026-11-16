//! Verify a posted lathe program before it cuts (fail closed).
//!
//! The oracle reads the **G-code text** — not the planner's moves — so the
//! post's diameter scaling, tool-offset handling and modal state are all
//! under test. It shares no geometry with the generator (no envelope, no
//! offsets): every move sweeps the tool's real shape `core ⊕ disk(rn)` and
//! is measured against the part's full axial section.
//!
//! Checks, all of which must pass:
//!
//! 1. **Readable.** Every word is one the reader can replay; anything else
//!    (arcs, G41/G42, inch, incremental, unknown words) is refused.
//! 2. **Machine state.** No feed move with the spindle stopped or without a
//!    feed; no motion without a tool; `G96` must carry an rpm ceiling.
//! 3. **No gouge.** For each move (rapids included) the swept core hull
//!    stays ≥ `rn − tolerance` from the part — exact for straight moves.
//! 4. **No crash.** Rapids and holders never enter remaining stock (a
//!    raster of the in-process stock, `cell` resolution).
//! 5. **Finished.** Every machined boundary point (OD, faces, bore, and the
//!    parted face) lies within `tolerance` of some feed sweep, and no stock
//!    farther than `tolerance + cell` from the part survives.

use super::geom::{self, P2};
use super::post::XMode;
use super::profile::{SegRole, TurnProfile};
use super::program::TurnStock;
use super::tool::LatheTool;
use serde::{Deserialize, Serialize};

/// Oracle settings.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct VerifyOptions {
    /// Allowed deviation from the profile (gouge and material left), mm.
    pub tolerance: f64,
    /// Raster cell for the in-process stock (crash / leftover), mm.
    pub cell: f64,
    /// Spacing of finish samples along the profile, mm.
    pub sample: f64,
    /// Chord tolerance for the part's arcs, mm.
    pub chord: f64,
    /// X scaling when the program carries no G7/G8 (Grbl dialect).
    pub x_mode: XMode,
    /// Require the chuck-end face to be finished (the program parts off).
    pub expect_parted: bool,
    /// Accept the fillet a nose radius leaves in a **sharp inside corner**
    /// of the profile: surface samples within this distance of a concave
    /// corner vertex are exempt from the finish check (gouge checks still
    /// apply). 0 = strict (default): model the corner radius instead.
    pub inside_corner_allowance: f64,
}

impl Default for VerifyOptions {
    fn default() -> Self {
        Self {
            tolerance: 0.01,
            cell: 0.05,
            sample: 0.05,
            chord: 0.0005,
            x_mode: XMode::Radius,
            expect_parted: true,
            inside_corner_allowance: 0.0,
        }
    }
}

/// One reason a program fails.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum Finding {
    /// A line the reader cannot replay.
    Unreadable {
        /// 1-based line.
        line: usize,
        /// Why.
        msg: String,
    },
    /// Machine-state violation (spindle, feed, tool, CSS ceiling).
    State {
        /// 1-based line.
        line: usize,
        /// Why.
        msg: String,
    },
    /// The tool enters the part.
    Gouge {
        /// 1-based line.
        line: usize,
        /// Penetration, mm.
        depth: f64,
        /// Move end (nose centre), `[z, x]`.
        at: P2,
        /// Rapid move?
        rapid: bool,
    },
    /// A rapid or a holder enters remaining stock.
    Crash {
        /// 1-based line.
        line: usize,
        /// Stock cell hit, `[z, r]`.
        at: P2,
        /// What hit it.
        what: String,
    },
    /// A machined surface point no feed sweep reaches within tolerance.
    Unfinished {
        /// Surface point `[z, r]`.
        at: P2,
        /// Distance from the nearest sweep, mm.
        excess: f64,
        /// Which surface.
        role: SegRole,
    },
    /// Stock left away from the part.
    MaterialLeft {
        /// First cell found, `[z, r]`.
        at: P2,
        /// Cells left.
        cells: usize,
    },
}

/// The oracle's answer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnVerdict {
    /// All checks passed.
    pub ok: bool,
    /// Failures (truncated to the first few of each kind).
    pub findings: Vec<Finding>,
    /// Deepest penetration measured, mm (0 if none).
    pub max_gouge: f64,
    /// Largest distance from a machined surface point to a feed sweep, mm.
    pub max_excess: f64,
    /// Motion lines replayed.
    pub moves: usize,
    /// What the answer is resolved to: tolerance, raster cell, chord.
    pub resolution: String,
}

#[derive(Debug, Clone, Copy)]
struct Move {
    tool: usize,
    from: P2,
    to: P2,
    rapid: bool,
    line: usize,
}

struct Parsed {
    moves: Vec<Move>,
    findings: Vec<Finding>,
}

fn parse(gcode: &str, tools: &[LatheTool], opts: &VerifyOptions) -> Parsed {
    let mut findings = Vec::new();
    let mut moves = Vec::new();
    let mut x_mode = opts.x_mode;
    // A header declaring the X scaling must agree with the caller.
    for l in gcode.lines() {
        if let Some(rest) = l.trim().strip_prefix("(vcad-x-mode:") {
            let declared = if rest.contains("diameter") {
                XMode::Diameter
            } else {
                XMode::Radius
            };
            if declared != opts.x_mode {
                findings.push(Finding::State {
                    line: 0,
                    msg: format!(
                        "program declares X = {declared:?}, verifier was told {:?}",
                        opts.x_mode
                    ),
                });
            }
        }
    }
    let mut rapid_mode: Option<bool> = None;
    let mut pos: Option<P2> = None; // programmed point
    let mut tool: Option<usize> = None;
    let mut spindle_on = false;
    let mut s_word = 0.0f64;
    let mut css = false;
    let mut feed: Option<f64> = None;
    let mut metric = false;
    let mut absolute = false;

    for (i, raw) in gcode.lines().enumerate() {
        let line = i + 1;
        // Strip comments.
        let mut s = String::new();
        let mut depth = 0;
        for ch in raw.chars() {
            match ch {
                '(' => depth += 1,
                ')' => depth -= 1,
                ';' if depth == 0 => break,
                c if depth == 0 => s.push(c),
                _ => {}
            }
        }
        let s = s.trim().to_ascii_uppercase();
        if s.is_empty() {
            continue;
        }
        // Tokenise.
        let mut words: Vec<(char, f64)> = Vec::new();
        let b = s.as_bytes();
        let mut k = 0;
        let mut bad = None;
        while k < b.len() {
            let c = b[k] as char;
            if c.is_whitespace() {
                k += 1;
                continue;
            }
            if !c.is_ascii_alphabetic() {
                bad = Some(format!("unexpected '{c}'"));
                break;
            }
            let st = k + 1;
            let mut e = st;
            while e < b.len() && ((b[e] as char).is_ascii_digit() || b"+-.".contains(&b[e])) {
                e += 1;
            }
            match s[st..e].parse::<f64>() {
                Ok(v) => words.push((c, v)),
                Err(_) => {
                    bad = Some(format!("bad number after '{c}'"));
                    break;
                }
            }
            k = e;
        }
        if let Some(msg) = bad {
            findings.push(Finding::Unreadable { line, msg });
            continue;
        }
        let mut x: Option<f64> = None;
        let mut z: Option<f64> = None;
        let mut m6 = false;
        let mut t_word: Option<u32> = None;
        let mut d_word: Option<f64> = None;
        let mut g96 = false;
        let mut dwell = false;
        let mut line_bad = false;
        for &(c, v) in &words {
            let iv = v.round() as i64;
            let int = (v - v.round()).abs() < 1e-9;
            match c {
                'G' if int => match iv {
                    0 => rapid_mode = Some(true),
                    1 => rapid_mode = Some(false),
                    4 => dwell = true,
                    7 => x_mode = XMode::Diameter,
                    8 => x_mode = XMode::Radius,
                    18 | 40 | 43 | 49 | 54 | 80 => {}
                    21 => metric = true,
                    90 => absolute = true,
                    94 | 95 => {}
                    96 => {
                        g96 = true;
                        css = true;
                    }
                    97 => css = false,
                    20 => {
                        findings.push(Finding::Unreadable {
                            line,
                            msg: "inch mode (G20) refused".into(),
                        });
                        line_bad = true;
                    }
                    91 => {
                        findings.push(Finding::Unreadable {
                            line,
                            msg: "incremental mode (G91) refused".into(),
                        });
                        line_bad = true;
                    }
                    g => {
                        findings.push(Finding::Unreadable {
                            line,
                            msg: format!("G{g} cannot be replayed"),
                        });
                        line_bad = true;
                    }
                },
                'M' if int => match iv {
                    3 | 4 => spindle_on = true,
                    5 => spindle_on = false,
                    6 => m6 = true,
                    0 | 1 | 2 | 30 | 8 | 9 => {}
                    m => {
                        findings.push(Finding::Unreadable {
                            line,
                            msg: format!("M{m} cannot be replayed"),
                        });
                        line_bad = true;
                    }
                },
                'X' => x = Some(v),
                'Z' => z = Some(v),
                'F' => feed = Some(v),
                'S' => s_word = v,
                'D' => d_word = Some(v),
                'T' if int && v >= 0.0 => t_word = Some(iv as u32),
                'N' | 'P' => {}
                c => {
                    findings.push(Finding::Unreadable {
                        line,
                        msg: format!("word '{c}{v}' cannot be replayed"),
                    });
                    line_bad = true;
                }
            }
        }
        if line_bad {
            continue;
        }
        if g96 && d_word.is_none_or(|d| d <= 0.0) {
            findings.push(Finding::State {
                line,
                msg: "G96 without an rpm ceiling (D): CSS at the axis is unbounded".into(),
            });
        }
        let _ = m6;
        if let Some(t) = t_word {
            match tools.iter().position(|tl| tl.number == t) {
                Some(ix) => tool = Some(ix),
                None => {
                    findings.push(Finding::State {
                        line,
                        msg: format!("T{t} is not in the tool list"),
                    });
                    tool = None;
                }
            }
        }
        if dwell || (x.is_none() && z.is_none()) {
            continue;
        }
        if !metric || !absolute {
            findings.push(Finding::State {
                line,
                msg: "motion before G21 G90 are established".into(),
            });
            continue;
        }
        let Some(rapid) = rapid_mode else {
            findings.push(Finding::State {
                line,
                msg: "motion with no G0/G1 mode".into(),
            });
            continue;
        };
        let Some(ti) = tool else {
            findings.push(Finding::State {
                line,
                msg: "motion with no tool selected".into(),
            });
            continue;
        };
        let xs = match x_mode {
            XMode::Diameter => 0.5,
            XMode::Radius => 1.0,
        };
        let prev = pos;
        let target = [
            z.or(prev.map(|p| p[0])).unwrap_or(f64::NAN),
            x.map(|v| v * xs).or(prev.map(|p| p[1])).unwrap_or(f64::NAN),
        ];
        if target[0].is_nan() || target[1].is_nan() {
            findings.push(Finding::State {
                line,
                msg: "first motion must give both X and Z".into(),
            });
            continue;
        }
        if !rapid {
            if !spindle_on || (!css && s_word <= 0.0) {
                findings.push(Finding::State {
                    line,
                    msg: "feed move with the spindle stopped".into(),
                });
            }
            if feed.is_none_or(|f| f <= 0.0) {
                findings.push(Finding::State {
                    line,
                    msg: "feed move without a feed rate".into(),
                });
            }
        }
        let t = &tools[ti];
        let to_c = t.nose_center(target);
        match prev {
            None if !rapid => findings.push(Finding::State {
                line,
                msg: "first motion is a feed from an unknown position".into(),
            }),
            None => moves.push(Move {
                tool: ti,
                from: to_c,
                to: to_c,
                rapid,
                line,
            }),
            Some(p) => moves.push(Move {
                tool: ti,
                from: t.nose_center(p),
                to: to_c,
                rapid,
                line,
            }),
        }
        pos = Some(target);
    }
    Parsed { moves, findings }
}

/// Distance from a convex polygon to the part region (0 on overlap), and
/// the penetration estimate when overlapping.
fn hull_vs_part(hull: &[P2], edges: &[(P2, P2)], bbs: &[[f64; 4]], reach: f64) -> (f64, f64) {
    let hb = geom::bbox(hull);
    let near: Vec<usize> = (0..edges.len())
        .filter(|&i| {
            let b = bbs[i];
            b[0] <= hb[2] + reach
                && b[2] >= hb[0] - reach
                && b[1] <= hb[3] + reach
                && b[3] >= hb[1] - reach
        })
        .collect();
    let hedges = geom::poly_edges(hull);
    let mut overlap = false;
    let mut pen = 0.0f64;
    for &v in hull {
        if geom::in_region(edges, v) {
            overlap = true;
            let d = near
                .iter()
                .map(|&i| geom::point_seg_dist(v, edges[i].0, edges[i].1))
                .fold(f64::INFINITY, f64::min);
            pen = pen.max(if d.is_finite() { d } else { reach });
        }
    }
    for &i in &near {
        let (a, b) = edges[i];
        if hull.len() >= 3 && geom::in_convex(hull, a) {
            overlap = true;
            let d = hedges
                .iter()
                .map(|&(p, q)| geom::point_seg_dist(a, p, q))
                .fold(f64::INFINITY, f64::min);
            pen = pen.max(d);
        }
        if !overlap
            && hedges
                .iter()
                .any(|&(p, q)| geom::segs_intersect(p, q, a, b))
        {
            overlap = true;
        }
    }
    if overlap {
        return (0.0, pen);
    }
    let mut d = f64::INFINITY;
    for &i in &near {
        let (a, b) = edges[i];
        for &(p, q) in &hedges {
            d = d.min(geom::seg_seg_dist(p, q, a, b));
        }
    }
    (d, 0.0)
}

/// Verify posted G-code against a profile and stock.
pub fn verify_gcode(
    gcode: &str,
    tools: &[LatheTool],
    stock: &TurnStock,
    profile: &TurnProfile,
    opts: &VerifyOptions,
) -> TurnVerdict {
    let parsed = parse(gcode, tools, opts);
    let mut findings = parsed.findings;
    let moves = parsed.moves;
    let tol = opts.tolerance;
    let edges = profile.full_section_edges(opts.chord);
    let bbs: Vec<[f64; 4]> = edges.iter().map(|(a, b)| geom::bbox(&[*a, *b])).collect();

    // Sweeps.
    struct Sweep {
        hull: Vec<P2>,
        bb: [f64; 4],
        rn: f64,
        rapid: bool,
        holder: Option<Vec<P2>>,
        line: usize,
        end: P2,
    }
    let sweeps: Vec<Sweep> = moves
        .iter()
        .map(|m| {
            let t = &tools[m.tool];
            let mut pts = t.core_at(m.from);
            pts.extend(t.core_at(m.to));
            let hull = geom::convex_hull(&pts);
            let holder = match (t.holder_at(m.from), t.holder_at(m.to)) {
                (Some(mut a), Some(b)) => {
                    a.extend(b);
                    Some(geom::convex_hull(&a))
                }
                _ => None,
            };
            let bb = geom::bbox(&hull);
            Sweep {
                hull,
                bb,
                rn: t.nose_radius,
                rapid: m.rapid,
                holder,
                line: m.line,
                end: m.to,
            }
        })
        .collect();

    // 3. Gouge (and holder contact with the part).
    let mut max_gouge = 0.0f64;
    let mut n_gouge = 0;
    for s in &sweeps {
        let (d, pen) = hull_vs_part(&s.hull, &edges, &bbs, s.rn + tol + 1.0);
        let depth = if d == 0.0 { s.rn + pen } else { s.rn - d };
        if depth > tol {
            max_gouge = max_gouge.max(depth);
            if n_gouge < 8 {
                findings.push(Finding::Gouge {
                    line: s.line,
                    depth,
                    at: s.end,
                    rapid: s.rapid,
                });
            }
            n_gouge += 1;
        }
        if let Some(h) = &s.holder {
            let (d, _) = hull_vs_part(h, &edges, &bbs, tol + 1.0);
            if d <= 0.0 {
                findings.push(Finding::Crash {
                    line: s.line,
                    at: s.end,
                    what: "holder touches the part".into(),
                });
            }
        }
    }

    // 4. Raster of the in-process stock.
    let h = opts.cell;
    let z0 = stock.z_chuck();
    let z1 = stock.z_face();
    let r_out = stock.radius();
    let r_in = stock.bore_diameter / 2.0;
    let nz = ((z1 - z0) / h).ceil().max(1.0) as usize;
    let nr = (r_out / h).ceil().max(1.0) as usize;
    let cz = |i: usize| z0 + (i as f64 + 0.5) * h;
    let cr = |j: usize| (j as f64 + 0.5) * h;
    let mut present: Vec<bool> = (0..nz * nr)
        .map(|k| {
            let r = cr(k % nr);
            r >= r_in && r <= r_out
        })
        .collect();
    let mut n_crash = 0;
    let range = |lo: f64, hi: f64, o: f64, n: usize| {
        let a = (((lo - o) / h).floor().max(0.0)) as usize;
        let b = ((((hi - o) / h).ceil()).max(0.0) as usize).min(n);
        (a.min(n), b)
    };
    for s in &sweeps {
        let reach = s.rn;
        let (ia, ib) = range(s.bb[0] - reach, s.bb[2] + reach, z0, nz);
        let rmax = s.bb[1].abs().max(s.bb[3].abs()) + reach;
        let rmin = if s.bb[1] <= 0.0 && s.bb[3] >= 0.0 {
            0.0
        } else {
            s.bb[1].abs().min(s.bb[3].abs()) - reach
        };
        let (ja, jb) = range(rmin, rmax, 0.0, nr);
        let mut crashed = false;
        for i in ia..ib {
            for j in ja..jb {
                let k = i * nr + j;
                if !present[k] {
                    continue;
                }
                let q = [cz(i), cr(j)];
                let qm = [q[0], -q[1]];
                let d =
                    geom::point_convex_dist(&s.hull, q).min(geom::point_convex_dist(&s.hull, qm));
                if s.rapid {
                    if d < s.rn - h && !crashed {
                        crashed = true;
                        if n_crash < 8 {
                            findings.push(Finding::Crash {
                                line: s.line,
                                at: q,
                                what: "rapid into stock".into(),
                            });
                        }
                        n_crash += 1;
                    }
                } else if d <= s.rn {
                    present[k] = false;
                }
            }
        }
        if let Some(hp) = &s.holder {
            let hb = geom::bbox(hp);
            let (ia, ib) = range(hb[0], hb[2], z0, nz);
            let mut hit = false;
            for i in ia..ib {
                for j in 0..nr {
                    if hit {
                        break;
                    }
                    let k = i * nr + j;
                    if !present[k] {
                        continue;
                    }
                    for q in [[cz(i), cr(j)], [cz(i), -cr(j)]] {
                        if geom::in_convex(hp, q)
                            && geom::poly_edges(hp)
                                .iter()
                                .all(|&(a, b)| geom::point_seg_dist(q, a, b) > h)
                        {
                            hit = true;
                            findings.push(Finding::Crash {
                                line: s.line,
                                at: q,
                                what: "holder enters stock".into(),
                            });
                            break;
                        }
                    }
                }
            }
        }
    }

    // 5a. Leftover stock away from the part.
    let corners = concave_corners(profile);
    let zlim = profile.z_min();
    let mut left = 0usize;
    let mut first_left: Option<P2> = None;
    for i in 0..nz {
        let z = cz(i);
        if z < zlim {
            continue;
        }
        for j in 0..nr {
            if !present[i * nr + j] {
                continue;
            }
            let q = [z, cr(j)];
            if geom::in_region(&edges, q) {
                continue;
            }
            let d = edges
                .iter()
                .map(|&(a, b)| geom::point_seg_dist(q, a, b))
                .fold(f64::INFINITY, f64::min);
            let in_corner = opts.inside_corner_allowance > 0.0
                && corners
                    .iter()
                    .any(|c| geom::dist(*c, q) <= opts.inside_corner_allowance);
            if d > tol + 0.71 * h && !in_corner {
                left += 1;
                first_left.get_or_insert(q);
            }
        }
    }
    if left > 0 {
        findings.push(Finding::MaterialLeft {
            at: first_left.unwrap_or([0.0, 0.0]),
            cells: left,
        });
    }

    // 5b. Every machined surface point within tolerance of a feed sweep.
    let feeds: Vec<&Sweep> = sweeps.iter().filter(|s| !s.rapid).collect();
    let mut exempt = 0usize;
    let mut max_excess = 0.0f64;
    let mut n_unf = 0;
    for (seg, role) in profile.classified() {
        if role == SegRole::Axis || (role == SegRole::LeftFace && !opts.expect_parted) {
            continue;
        }
        let pl = seg.polyline(opts.chord);
        for w in pl.windows(2) {
            let len = geom::dist(w[0], w[1]);
            let n = ((len / opts.sample).ceil() as usize).max(1);
            for k in 0..=n {
                let q = geom::add(
                    w[0],
                    geom::scale(geom::sub(w[1], w[0]), k as f64 / n as f64),
                );
                let mut best = f64::INFINITY;
                for s in &feeds {
                    let pad = s.rn + tol + best.min(1.0);
                    if q[0] < s.bb[0] - pad
                        || q[0] > s.bb[2] + pad
                        || q[1] < s.bb[1] - pad
                        || q[1] > s.bb[3] + pad
                    {
                        continue;
                    }
                    let d = geom::point_convex_dist(&s.hull, q) - s.rn;
                    best = best.min(d);
                }
                let excess = best.max(0.0);
                if excess > tol
                    && opts.inside_corner_allowance > 0.0
                    && corners
                        .iter()
                        .any(|c| geom::dist(*c, q) <= opts.inside_corner_allowance)
                {
                    exempt += 1;
                    continue;
                }
                max_excess = max_excess.max(excess);
                if excess > tol {
                    if n_unf < 8 {
                        findings.push(Finding::Unfinished {
                            at: q,
                            excess,
                            role,
                        });
                    }
                    n_unf += 1;
                }
            }
        }
    }

    if moves.iter().all(|m| m.rapid) {
        findings.push(Finding::State {
            line: 0,
            msg: "no feed moves: nothing is cut".into(),
        });
    }
    TurnVerdict {
        ok: findings.is_empty(),
        findings,
        max_gouge,
        max_excess,
        moves: moves.len(),
        resolution: format!(
            "tolerance {tol} mm; stock raster {h} mm; surface samples every {} mm; part arcs \
             chorded at {} mm{}",
            opts.sample,
            opts.chord,
            if exempt > 0 {
                format!(
                    "; {exempt} samples within {} mm of {} sharp inside corners exempted \
                     (nose-radius fillet accepted)",
                    opts.inside_corner_allowance,
                    corners.len()
                )
            } else {
                String::new()
            }
        ),
    }
}

/// Concave (inside) corner vertices of the profile: where the CCW boundary
/// turns right between two non-tangent segments, away from the axis.
fn concave_corners(profile: &TurnProfile) -> Vec<P2> {
    let segs = profile.segments();
    let n = segs.len();
    let mut out = Vec::new();
    for i in 0..n {
        let s0 = &segs[i];
        let s1 = &segs[(i + 1) % n];
        let v = s0.end();
        if v[1] < 1e-6 {
            continue;
        }
        let d0 = geom::sub(v, s0.point_at(0.999));
        let d1 = geom::sub(s1.point_at(0.001), v);
        let c = geom::cross(d0, d1) / (geom::norm(d0) * geom::norm(d1)).max(1e-15);
        if c < -1e-3 {
            out.push(v);
        }
    }
    out
}

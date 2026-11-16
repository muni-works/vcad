//! CNC 3-axis milling checks.
//!
//! Every check runs on one shared [`Probe`] (a single tessellation + triangle
//! BVH) and reports per [`Regions`] region — a feature a person would point
//! at — rather than per B-rep face. That matters most on triangulated
//! boolean output, where one bore is hundreds of planar faces: the old
//! per-face pass reported each facet separately, under ids that collided
//! because they were derived from a surface-domain midpoint every facet on
//! a plane shared.
//!
//! Tool access is a setup plan, not a per-face verdict: the smallest set of
//! axis-aligned setups (±X/±Y/±Z) that reaches every reachable face is
//! reported once, and only regions reachable from none of them are errors.

use std::collections::{BTreeMap, BTreeSet};

use vcad_kernel_cost::Process;
use vcad_kernel_geom::{CylinderSurface, Plane, SurfaceKind};
use vcad_kernel_math::{Point3, Vec3};
use vcad_kernel_primitives::BRepSolid;

use crate::geom::probe::{Probe, Regions};
use crate::geom::provenance::ProvenanceMap;
use crate::geom::setups::{self, Axis};
use crate::issue::{DfmFix, DfmIssue, DfmSeverity, RuleResult};
use crate::rules::RulePack;

/// Largest dihedral angle merged between planar faces of one region. Above
/// a 32-gon's 11.25° facet step, well below any designed edge.
const SMOOTH_DEG: f64 = 20.0;

/// Run CNC checks.
pub fn run(
    brep: &BRepSolid,
    provenance: Option<&ProvenanceMap>,
    pack: &RulePack,
    issues: &mut Vec<DfmIssue>,
    rule_results: &mut Vec<RuleResult>,
) {
    let process = Process::Cnc3Axis;
    let probe = Probe::new(brep);
    if probe.is_empty() {
        return;
    }
    let sliver_area = pack
        .rule("tool_inaccessibility")
        .map(|r| r.num("sliver_area_mm2", 1e-3))
        .unwrap_or(1e-3);
    let regions = Regions::new(&probe, brep, SMOOTH_DEG, sliver_area);
    let origin = |faces: &[usize]| provenance.and_then(|p| faces.iter().find_map(|&f| p.get(f)));

    if let Some(rule) = pack.rule("internal_radius_too_small") {
        let min_radius = rule.num("min_internal_radius_mm", 3.0);
        let mut flagged = 0usize;
        for corner in internal_radii(brep, &probe, &regions) {
            if corner.radius_mm >= min_radius {
                continue;
            }
            flagged += 1;
            let mut issue = DfmIssue::new(
                "cnc.internal_radius_too_small",
                rule.severity_enum(),
                process,
                format!(
                    "Internal radius R{:.2} mm — smaller than min cutter R{:.2} mm",
                    corner.radius_mm, min_radius
                ),
                corner.anchor,
                corner.radius_mm,
                min_radius,
                "mm",
            )
            .with_key(&format!("f{}", corner.faces[0]))
            .with_explanation(
                "Internal radii smaller than the cutter radius can't be machined. \
                 Raise the fillet, switch to EDM, or accept a corner-relief slot.",
            )
            .with_faces(corner.faces.clone());
            if let Some(node) = provenance.and_then(|p| p.get(corner.faces[0])) {
                issue = issue.with_origin(node);
                issue = issue.with_fix(DfmFix::SetParam {
                    node,
                    path: "radius".to_string(),
                    value: serde_json::json!(min_radius),
                });
            } else {
                issue = issue.with_fix(DfmFix::Manual {
                    description: format!("Raise this radius to ≥ {:.2} mm.", min_radius),
                });
            }
            issues.push(issue);
        }
        rule_results.push(RuleResult {
            rule: "cnc.internal_radius_too_small".into(),
            label: "Internal corner radius".into(),
            passed: flagged == 0,
            violation_count: flagged,
            summary: format!(
                "{flagged} internal corner radi{} below R{min_radius:.2} mm (drilled holes excluded)",
                if flagged == 1 { "us" } else { "i" }
            ),
            affordances: Vec::new(),
        });
    }

    if let Some(rule) = pack.rule("thin_wall") {
        let min_wall = rule.num("min_wall_mm", 1.0);
        let artifact = rule.num("artifact_tol_mm", 0.05);
        let report = thin_walls(brep, &probe, &regions, min_wall, artifact);
        for w in &report.walls {
            let faces: Vec<usize> = w.faces.iter().copied().collect();
            let mut issue = DfmIssue::new(
                "cnc.thin_wall",
                rule.severity_enum(),
                process,
                format!(
                    "Wall {:.2} mm — below CNC minimum {:.2} mm",
                    w.thickness_mm, min_wall
                ),
                w.anchor,
                w.thickness_mm,
                min_wall,
                "mm",
            )
            .with_key(&format!("f{}-f{}", w.key.0, w.key.1))
            .with_explanation(
                "Thin walls chatter and deflect under cutter load. Thicken \
                 the wall, switch to sheet metal, or accept the workholding cost.",
            )
            .with_faces(faces.clone())
            .with_fix(DfmFix::Manual {
                description: format!("Thicken to at least {:.2} mm.", min_wall),
            });
            if let Some(node) = origin(&faces) {
                issue = issue.with_origin(node);
            }
            issues.push(issue);
        }
        let summary = match report.walls.iter().map(|w| w.thickness_mm).reduce(f64::min) {
            Some(t) => format!(
                "{} wall{} below {min_wall:.2} mm (thinnest {t:.2} mm)",
                report.walls.len(),
                if report.walls.len() == 1 { "" } else { "s" }
            ),
            None => format!("no wall below {min_wall:.2} mm"),
        };
        rule_results.push(RuleResult {
            rule: "cnc.thin_wall".into(),
            label: "Minimum wall".into(),
            passed: report.walls.is_empty(),
            violation_count: report.walls.len(),
            summary: if report.artifacts > 0 {
                format!(
                    "{summary}; ignored {} sub-{artifact} mm tessellation artifact sample{}",
                    report.artifacts,
                    if report.artifacts == 1 { "" } else { "s" }
                )
            } else {
                summary
            },
            affordances: Vec::new(),
        });
    }

    if let Some(rule) = pack.rule("tool_inaccessibility") {
        let mut axes: Vec<Axis> = rule
            .strings("axes")
            .iter()
            .filter_map(|s| Axis::parse(s))
            .collect();
        if axes.is_empty() {
            axes = Axis::ALL.to_vec();
        }
        let max_setups = rule.num("max_setups", 3.0).max(1.0) as usize;
        let min_unreachable = rule.num("min_unreachable_area_mm2", 0.05);
        let plan = setups::plan(&probe, &regions, &axes, sliver_area);

        let labels: Vec<&str> = plan.setups.iter().map(|a| a.label()).collect();
        let n = plan.setups.len();
        if n > 1 {
            let extra: Vec<usize> = plan.added_faces[1..].concat();
            let anchor = extra
                .first()
                .and_then(|&f| probe.face_centroid(f))
                .unwrap_or_else(|| Point3::new(0.0, 0.0, 0.0));
            let breakdown: Vec<String> = plan
                .setups
                .iter()
                .zip(&plan.added_area)
                .zip(&plan.added_faces)
                .map(|((a, area), faces)| {
                    let regs: BTreeSet<usize> =
                        faces.iter().filter_map(|&f| regions.region(f)).collect();
                    format!(
                        "{} reaches {:.0} mm² ({} feature{})",
                        a.label(),
                        area,
                        regs.len(),
                        if regs.len() == 1 { "" } else { "s" }
                    )
                })
                .collect();
            let severity = if n <= max_setups {
                DfmSeverity::Info
            } else {
                DfmSeverity::Warning
            };
            let mut issue = DfmIssue::new(
                "cnc.setups",
                severity,
                process,
                format!("Needs {n} setups: {}", labels.join(", ")),
                anchor,
                n as f64,
                max_setups as f64,
                "setups",
            )
            .with_key("plan")
            .with_explanation(format!(
                "Fewest axis-aligned setups that reach every reachable face: {}. \
                 The listed faces are the ones the first setup can't reach. \
                 Each extra setup is a re-fixture and a datum transfer; fold \
                 features onto fewer sides to save them. Reach is zero-radius \
                 line of sight with no flute-length limit, so treat the count \
                 as a lower bound.",
                breakdown.join("; ")
            ))
            .with_faces(extra.clone())
            .with_fix(DfmFix::Manual {
                description: format!(
                    "Plan {n} setups ({}), or move features so fewer sides need cutting.",
                    labels.join(", ")
                ),
            });
            if let Some(node) = origin(&extra) {
                issue = issue.with_origin(node);
            }
            issues.push(issue);
        }

        let mut unreachable = 0usize;
        for r in &plan.unreachable {
            if r.unreachable_area < min_unreachable {
                continue;
            }
            unreachable += 1;
            let anchor = r
                .unreachable_anchor
                .unwrap_or_else(|| Point3::new(0.0, 0.0, 0.0));
            let first = regions.faces[r.region][0];
            let allowed: Vec<&str> = axes.iter().map(|a| a.label()).collect();
            let mut issue = DfmIssue::new(
                "cnc.tool_inaccessibility",
                rule.severity_enum(),
                process,
                format!(
                    "{:.1} mm² not reachable from any setup ({})",
                    r.unreachable_area,
                    allowed.join(" ")
                ),
                anchor,
                r.unreachable_area,
                0.0,
                "mm2",
            )
            .with_key(&format!("f{first}"))
            .with_explanation(
                "No axis-aligned tool direction has line of sight to this \
                 surface — an undercut or an angled feature. Re-orient it onto \
                 a principal axis, use an undercut / T-slot cutter, or move to \
                 3+2 or 5-axis.",
            )
            .with_faces(r.unreachable_faces.clone())
            .with_fix(DfmFix::Manual {
                description: "Align the feature with a principal axis, or plan a \
                              3+2 / 5-axis operation."
                    .to_string(),
            });
            if let Some(node) = origin(&r.unreachable_faces) {
                issue = issue.with_origin(node);
            }
            issues.push(issue);
        }
        rule_results.push(RuleResult {
            rule: "cnc.setups".into(),
            label: "Setups".into(),
            passed: n <= max_setups,
            violation_count: n.saturating_sub(max_setups),
            summary: format!(
                "{n} setup{}: {}",
                if n == 1 { "" } else { "s" },
                labels.join(", ")
            ),
            affordances: Vec::new(),
        });
        rule_results.push(RuleResult {
            rule: "cnc.tool_inaccessibility".into(),
            label: "Tool access".into(),
            passed: unreachable == 0,
            violation_count: unreachable,
            summary: {
                let head = if unreachable == 0 {
                    "every face is reachable from the planned setups".to_string()
                } else {
                    format!("{unreachable} feature(s) reachable from no setup")
                };
                if plan.inverted_area > 0.0 {
                    format!(
                        "{head}; skipped {:.2} mm² of inside-out faces (boolean debris)",
                        plan.inverted_area
                    )
                } else {
                    head
                }
            },
            affordances: Vec::new(),
        });
    }
}

/// A concave, partial cylindrical region: an internal corner radius.
struct Corner {
    faces: Vec<usize>,
    radius_mm: f64,
    anchor: Point3,
}

/// Concave cylinder regions spanning under 180° — corner fillets. Full holes
/// (≥ 180° of arc) are drilled or bored, not cut by an end mill's corner, and
/// convex cylinders (bosses, outside rounds) are no internal radius at all.
fn internal_radii(brep: &BRepSolid, probe: &Probe, regions: &Regions) -> Vec<Corner> {
    let mut out = Vec::new();
    let faces: Vec<_> = brep.topology.faces.iter().map(|(_, f)| f).collect();
    for members in &regions.faces {
        let Some(cyl) = members.iter().find_map(|&f| {
            let s = brep.geometry.surfaces.get(faces.get(f)?.surface_index)?;
            (s.surface_type() == SurfaceKind::Cylinder)
                .then(|| s.as_any().downcast_ref::<CylinderSurface>().cloned())
                .flatten()
        }) else {
            continue;
        };
        let axis = Vec3::new(
            cyl.axis.as_ref().x,
            cyl.axis.as_ref().y,
            cyl.axis.as_ref().z,
        );
        let rx = Vec3::new(
            cyl.ref_dir.as_ref().x,
            cyl.ref_dir.as_ref().y,
            cyl.ref_dir.as_ref().z,
        );
        let ry = axis.cross(rx);
        let c = Point3::new(cyl.center.x, cyl.center.y, cyl.center.z);
        let (mut concave, mut total) = (0.0, 0.0);
        let mut angles = Vec::new();
        let mut anchor = None;
        for &f in members {
            for s in probe.samples(f, 32) {
                let d = s.point - c;
                let radial = d - axis * d.dot(axis);
                if radial.norm() < 1e-9 {
                    continue;
                }
                total += s.area;
                if s.normal.dot(radial) < 0.0 {
                    concave += s.area;
                }
                angles.push(radial.dot(ry).atan2(radial.dot(rx)));
                anchor.get_or_insert(s.point);
            }
        }
        if total <= 0.0 || concave < 0.5 * total {
            continue;
        }
        angles.sort_by(f64::total_cmp);
        let max_gap = angles
            .windows(2)
            .map(|w| w[1] - w[0])
            .chain(
                angles
                    .first()
                    .zip(angles.last())
                    .map(|(a, b)| a + std::f64::consts::TAU - b),
            )
            .fold(0.0_f64, f64::max);
        if std::f64::consts::TAU - max_gap >= std::f64::consts::PI {
            continue; // a hole
        }
        out.push(Corner {
            faces: members.clone(),
            radius_mm: cyl.radius,
            anchor: anchor.unwrap_or(c),
        });
    }
    out
}

/// One thin wall between two regions.
struct Wall {
    /// (lower, higher) first-face index of the two regions — the stable key.
    key: (usize, usize),
    faces: BTreeSet<usize>,
    thickness_mm: f64,
    anchor: Point3,
}

struct WallReport {
    walls: Vec<Wall>,
    artifacts: usize,
}

/// Thin walls, one per pair of opposing regions.
///
/// Each sample casts inward along its normal. A hit counts as a wall only
/// when it lands on a different, non-adjacent face whose surface faces
/// back (a wedge corner where two faces meet is not a wall), and only when
/// it is thicker than `artifact` — anything thinner is sliver / folded
/// boolean debris below the tessellation tolerance, not a wall. When both
/// sides are planes the thickness is the exact plane-to-plane distance from
/// the B-rep, not the facet distance.
fn thin_walls(
    brep: &BRepSolid,
    probe: &Probe,
    regions: &Regions,
    min_wall: f64,
    artifact: f64,
) -> WallReport {
    let eps = (probe.scale * 1e-5).max(1e-4);
    let planes: Vec<Option<(Point3, Vec3)>> = brep
        .topology
        .faces
        .iter()
        .map(|(_, f)| {
            let s = brep.geometry.surfaces.get(f.surface_index)?;
            let p = s.as_any().downcast_ref::<Plane>()?;
            let n = p.normal_dir.as_ref();
            Some((
                Point3::new(p.origin.x, p.origin.y, p.origin.z),
                Vec3::new(n.x, n.y, n.z),
            ))
        })
        .collect();
    let mut walls: BTreeMap<(usize, usize), Wall> = BTreeMap::new();
    let mut artifacts = 0usize;
    let offset = crate::geom::setups::WALL_OFFSET_MM.max(probe.scale * 1e-4);
    for f in 0..probe.face_tris.len() {
        let Some(ra) = regions.region(f) else {
            continue;
        };
        if regions.sliver[f] {
            continue;
        }
        for s in probe.samples(f, 24) {
            let inward = -s.normal;
            let o = s.point + inward * eps;
            let Some(hit) = probe.first_hit(o, inward, 0.0, min_wall) else {
                continue;
            };
            if probe.inverted(&s, offset) {
                artifacts += 1;
                continue;
            }
            let g = probe.tri_face[hit.tri as usize];
            let Some(rb) = regions.region(g) else {
                continue;
            };
            // The same face, or one touching it, is a wedge corner, not a
            // wall. Judged per face, not per region: a rib whose two sides
            // join over a faceted round top is one region, and still thin.
            if g == f || probe.adjacency[f].contains(&g) {
                continue;
            }
            if probe.normals[hit.tri as usize].dot(s.normal) > -0.5 {
                continue; // not an opposing surface
            }
            let mut t = hit.t + eps;
            if let (Some(Some((pa, na))), Some(Some((pb, nb)))) = (planes.get(f), planes.get(g)) {
                if na.dot(*nb).abs() > 1.0 - 1e-9 {
                    t = (*pb - *pa).dot(*na).abs();
                }
            }
            if t < artifact {
                artifacts += 1;
                continue;
            }
            if t >= min_wall {
                continue;
            }
            let key = {
                let (a, b) = (regions.faces[ra][0], regions.faces[rb][0]);
                (a.min(b), a.max(b))
            };
            let w = walls.entry(key).or_insert_with(|| Wall {
                key,
                faces: BTreeSet::new(),
                thickness_mm: f64::INFINITY,
                anchor: s.point,
            });
            w.faces.insert(f);
            w.faces.insert(g);
            if t < w.thickness_mm {
                w.thickness_mm = t;
                w.anchor = s.point;
            }
        }
    }
    WallReport {
        walls: walls.into_values().collect(),
        artifacts,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::rules::RulePack;
    use vcad_kernel_primitives::make_cube;

    fn check(brep: &BRepSolid) -> (Vec<DfmIssue>, Vec<RuleResult>) {
        let pack = RulePack::default_for(Process::Cnc3Axis);
        let (mut issues, mut results) = (Vec::new(), Vec::new());
        run(brep, None, &pack, &mut issues, &mut results);
        (issues, results)
    }

    #[test]
    fn plain_block_is_one_setup_and_clean() {
        let (issues, results) = check(&make_cube(40.0, 30.0, 20.0));
        // −Z face needs a flip unless −Y/±X side-mill it as a wall; either
        // way a block is two setups at most and nothing is unreachable.
        assert!(
            issues
                .iter()
                .all(|i| i.rule != "cnc.tool_inaccessibility" && i.rule != "cnc.thin_wall"),
            "{issues:#?}"
        );
        let setups = results.iter().find(|r| r.rule == "cnc.setups").unwrap();
        assert!(
            setups.summary.starts_with("2 setups: +Z"),
            "{}",
            setups.summary
        );
    }

    #[test]
    fn thin_plate_reports_one_exact_wall() {
        let (issues, _) = check(&make_cube(40.0, 30.0, 0.5));
        let thin: Vec<_> = issues
            .iter()
            .filter(|i| i.rule == "cnc.thin_wall")
            .collect();
        assert_eq!(thin.len(), 1, "{thin:#?}");
        // Exact plane distance, not facet distance: the FFI's fix resolver
        // matches `measured` against the primitive dimension.
        assert!(
            (thin[0].measured - 0.5).abs() < 1e-9,
            "{}",
            thin[0].measured
        );
        assert_eq!(thin[0].face_indices.len(), 2);
    }
}

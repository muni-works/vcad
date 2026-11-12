//! A blend that declines must say so.
//!
//! `Solid::fillet` and friends are fail-soft internally: rather than emit
//! a cracked or inverted shell they keep the input. For a long time that
//! decision was invisible — the caller got a valid solid back and no
//! indication anything had failed, so a part could reach a fabricator
//! with square edges where the design called for radii. These tests pin
//! the contract that replaced it:
//!
//! 1. The default entry points are fail-closed — a decline is an `Err`.
//! 2. The reason is specific enough to act on, not a bare failure.
//! 3. Multi-edge blends report per edge, not as one boolean.
//! 4. The lenient variants still exist, but you have to ask for them.

use vcad_kernel::vcad_kernel_fillet::{BlendKey, BlendSection, EdgeQuery};
use vcad_kernel::vcad_kernel_math::{Point3, Vec3};
use vcad_kernel::{BlendError, Solid};

fn cube() -> Solid {
    Solid::cube(10.0, 10.0, 10.0)
}

fn fillet_key(size: f64) -> BlendKey {
    BlendKey {
        t: 0.0,
        section: BlendSection { size, shape: 1.0 },
    }
}

/// The headline case: a radius the geometry cannot host is an error, and
/// emphatically *not* a quiet identity operation.
#[test]
fn a_radius_too_large_for_the_feature_is_an_error_not_a_noop() {
    // r >= 5 on a 10 mm cube: opposing blends meet at the midplane. The
    // up-front size bound refuses with the number, so a caller (or an
    // agent) can act on it rather than guessing. (Oversized radii used to
    // reach the post-flight gate and say only "invalid geometry".)
    for r in [5.0, 6.0, 12.0, 20.0] {
        let err = cube()
            .fillet(r)
            .expect_err("r>=5 cannot fit a 10mm cube — this must not silently succeed");
        assert!(
            matches!(err, BlendError::SizeExceedsLimit { limit, .. } if (limit - 5.0).abs() < 1e-9),
            "r={r}: {err:?}"
        );
        let msg = err.to_string();
        assert!(
            msg.contains("fillet radius") && msg.contains("5.0000 mm limit"),
            "reason should be actionable, got: {msg}"
        );
    }
}

/// The same call through the lenient door: geometry comes back untouched,
/// but the report records the decline. This is the *old* behavior — now
/// only reachable by asking for it explicitly.
#[test]
fn the_lenient_variant_returns_the_input_but_records_why() {
    let input = cube();
    let (solid, report) = input.fillet_lenient(5.0);

    assert!(report.declined(), "report must record the decline");
    assert_eq!(report.applied, 0);
    assert_eq!(
        report.declined,
        Some(BlendError::SizeExceedsLimit {
            what: "fillet radius",
            size: 5.0,
            limit: 5.0
        })
    );

    // Unchanged: same volume, same face count as the input.
    assert!((solid.volume() - input.volume()).abs() < 1e-9);
    assert_eq!(
        solid.as_brep().unwrap().topology.faces.len(),
        input.as_brep().unwrap().topology.faces.len()
    );
}

/// A radius that *does* fit still works, and reports full application.
#[test]
fn a_feasible_radius_succeeds_and_reports_every_edge_applied() {
    let (result, report) = cube().fillet_reported(1.0);
    let filleted = result.expect("r=1 fits a 10mm cube");

    assert!(!report.declined());
    assert!(
        !report.is_partial(),
        "a 12-edge cube fillet is all-or-nothing"
    );
    assert_eq!(report.requested, 12, "a cube has 12 edges");
    assert_eq!(report.applied, report.requested);
    assert!(filleted.volume() < cube().volume());
}

/// Every distinct decline carries a distinct, specific reason — the point
/// of the exercise is that "it failed" is not an acceptable answer.
#[test]
fn each_decline_names_its_own_cause() {
    // Mesh-backed: no topology to blend.
    let mesh_solid = Solid::sphere(5.0, 8).union(&Solid::sphere(5.0, 8).translate(1.0, 0.0, 0.0));
    if mesh_solid.as_brep().is_none() {
        assert_eq!(mesh_solid.fillet(0.5).unwrap_err(), BlendError::NotBRep);
    }

    // Empty solid.
    assert_eq!(
        Solid::empty().fillet(1.0).unwrap_err(),
        BlendError::EmptySolid
    );
    assert_eq!(
        Solid::empty().chamfer(1.0).unwrap_err(),
        BlendError::EmptySolid
    );
    assert_eq!(
        Solid::empty().shell(1.0).unwrap_err(),
        BlendError::EmptySolid
    );

    // No keys in the blend profile.
    assert_eq!(
        cube().edge_blend(&EdgeQuery::All, &[]).unwrap_err(),
        BlendError::NoKeys
    );

    // A query that matches no edge blends nothing — and says so, instead
    // of handing back a solid the caller believes was blended.
    assert_eq!(
        cube()
            .edge_blend(
                &EdgeQuery::Endpoints {
                    a: Point3::new(99.0, 99.0, 99.0),
                    b: Point3::new(99.0, 99.0, 90.0),
                },
                &[fillet_key(1.0)],
            )
            .unwrap_err(),
        BlendError::NoTargetEdges
    );

    // A body with a bore has faces with inner loops; the rebuild would
    // fill the bore back in.
    let bored = Solid::cube(40.0, 40.0, 10.0)
        .difference(&Solid::cylinder(6.0, 30.0, 24).translate(20.0, 20.0, -10.0));
    match bored.fillet(1.0).unwrap_err() {
        BlendError::InnerLoops { faces } => assert!(faces > 0),
        other => panic!("expected an inner-loop refusal, got {other:?}"),
    }
}

/// Partial success on a multi-edge fillet is reportable *per edge*, with
/// a reason attached to each skipped one.
#[test]
fn a_multi_edge_fillet_reports_per_edge_not_as_one_boolean() {
    use vcad_kernel_sketch::{SketchProfile, SketchSegment};

    // An arc-extruded profile takes the curved per-edge path, where the
    // kernel blends cap rims and leaves some seams sharp.
    let p2 = vcad_kernel::vcad_kernel_math::Point2::new;
    let segments = vec![
        SketchSegment::Line {
            start: p2(0.0, 0.0),
            end: p2(30.0, 0.0),
        },
        SketchSegment::Arc {
            start: p2(30.0, 0.0),
            end: p2(30.0, 20.0),
            center: p2(30.0, 10.0),
            ccw: true,
        },
        SketchSegment::Line {
            start: p2(30.0, 20.0),
            end: p2(0.0, 20.0),
        },
        SketchSegment::Line {
            start: p2(0.0, 20.0),
            end: p2(0.0, 0.0),
        },
    ];
    let profile = SketchProfile::new(
        Point3::new(0.0, 0.0, 0.0),
        Vec3::new(1.0, 0.0, 0.0),
        Vec3::new(0.0, 1.0, 0.0),
        segments,
    )
    .expect("valid profile");
    let extruded = Solid::extrude(profile, Vec3::new(0.0, 0.0, 15.0)).expect("extrude ok");

    let (_result, report) = extruded.fillet_reported(1.0);

    assert!(
        report.requested > 1,
        "expected a multi-edge target set, got {}",
        report.requested
    );
    assert_eq!(
        report.edges.len(),
        report.requested,
        "every targeted edge must appear in the report"
    );

    // Whatever the split, the accounting is consistent and every skipped
    // edge carries a reason string rather than a bare flag.
    let applied = report.edges.iter().filter(|e| e.skipped.is_none()).count();
    assert_eq!(
        applied, report.applied,
        "report.applied must match the rows"
    );
    for edge in report.skipped() {
        let reason = edge.skipped.as_deref().unwrap();
        assert!(!reason.is_empty(), "a skipped edge must say why");
        assert!(
            edge.endpoints.is_some(),
            "a skipped edge should be locatable in the model"
        );
    }
}

/// `edge_blend_named` was already fail-closed on the name; it stays that
/// way now that its error type widened to `BlendError`.
#[test]
fn a_named_edge_that_cannot_resolve_still_fails_closed() {
    let err = cube()
        .edge_blend_named("cube:top", "cube:nope", &[fillet_key(1.0)])
        .expect_err("an unresolvable name must not fall back to a guessed edge");
    assert!(
        matches!(err, BlendError::NamedEdge(_)),
        "expected a named-edge error, got {err:?}"
    );
}

/// `shell` reports when it had to abandon the analytic offset for the
/// coarse mesh approximation — a quality degradation that used to be
/// completely invisible.
#[test]
fn shell_reports_a_fallback_to_the_mesh_offset() {
    // The analytic path handles a plain box.
    let (ok, degraded) = cube().shell_reported(2.0);
    assert!(ok.is_ok());
    assert!(
        degraded.is_none(),
        "a plain box should shell analytically, got fallback: {degraded:?}"
    );
}

/// Bodies with concave inside-corner edges — a boss unioned onto a block
/// (whose result also carries coplanar seam faces), an L-shaped extrude —
/// used to be refused by whole-part fillet/chamfer (and before that, in
/// the Mac app, blanked the viewport). They now blend: convex edges are
/// trimmed, concave edges gain material, and the shell stays watertight.
#[test]
fn whole_part_blend_on_a_concave_body_rounds_inside_corners() {
    use vcad_kernel::vcad_kernel_math::Point2;
    use vcad_kernel::vcad_kernel_sketch::{SketchProfile, SketchSegment};

    let base = Solid::cube(30.0, 97.0, 25.0);
    let boss = Solid::cube(30.0, 30.0, 30.0).translate(0.0, 0.0, 25.0);
    let union = base.union(&boss);

    let pts = [
        (0.0, 0.0),
        (97.0, 0.0),
        (97.0, 25.0),
        (30.0, 25.0),
        (30.0, 55.0),
        (0.0, 55.0),
    ];
    let segs = (0..pts.len())
        .map(|i| {
            let (a, b) = (pts[i], pts[(i + 1) % pts.len()]);
            SketchSegment::Line {
                start: Point2::new(a.0, a.1),
                end: Point2::new(b.0, b.1),
            }
        })
        .collect();
    let profile = SketchProfile::new(Point3::origin(), Vec3::y(), Vec3::z(), segs).unwrap();
    let l_prism = Solid::extrude(profile, Vec3::new(30.0, 0.0, 0.0)).unwrap();

    // Same body either way: 30×97×25 block + 30×30×30 boss.
    let v_in = 30.0 * 97.0 * 25.0 + 30.0f64.powi(3);
    for (name, body) in [("union", &union), ("L-prism", &l_prism)] {
        assert!(
            (body.volume() - v_in).abs() < 1e-6 * v_in,
            "{name} input volume"
        );
        for (op, r) in [("fillet", body.fillet(2.0)), ("chamfer", body.chamfer(2.0))] {
            let out = r.unwrap_or_else(|e| panic!("{op} on {name}: {e}"));
            let mesh = out.to_mesh(32);
            assert!(
                mesh.boundary_edges().is_empty(),
                "{op} on {name}: {} open edges",
                mesh.boundary_edges().len()
            );
            let v = out.volume();
            // Twelve convex edges per box minus the shared ones lose
            // material; the two 30 mm inside edges gain a sliver back.
            assert!(
                v < v_in && v > v_in * 0.97,
                "{op} on {name}: volume {v} vs {v_in}"
            );
            // Both concave edges are rounded: the inside corner line
            // y=30, z=25 is no longer on the surface — material fills it.
            let bb = out.bounding_box();
            assert!(
                (bb.1[2] - 55.0).abs() < 1e-9,
                "{op} on {name}: top stays at z=55"
            );
        }
    }
    // A chamfer cuts r²/2 off each convex edge, a fillet only r²(1−π/4),
    // so the chamfered body is the lighter one.
    let (f, c) = (
        union.fillet(2.0).unwrap().volume(),
        union.chamfer(2.0).unwrap().volume(),
    );
    assert!(c < f, "chamfer {c} should remove more than fillet {f}");

    // The convex block alone still blends.
    assert!(Solid::cube(30.0, 97.0, 25.0).fillet(2.0).is_ok());
    assert!(Solid::cube(30.0, 97.0, 25.0).chamfer(2.0).is_ok());
}

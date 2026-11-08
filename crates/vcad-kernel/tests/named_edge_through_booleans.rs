//! Named edges on boolean results: face names survive the boolean (split
//! faces keep their lineage on every fragment), the resolver finds the edge
//! between two named faces on the result, and anything it cannot single
//! out fails closed with a clear message.

use std::f64::consts::PI;

use vcad_kernel::vcad_kernel_fillet::{BlendKey, BlendSection};
use vcad_kernel::vcad_kernel_math::{Point3, Vec3};
use vcad_kernel::vcad_kernel_naming::EdgeHint;
use vcad_kernel::{BlendError, NamedEdgeError, Solid};

fn keys(shape: f64) -> [BlendKey; 1] {
    [BlendKey {
        t: 0.0,
        section: BlendSection { size: 2.0, shape },
    }]
}

fn scoped(mut s: Solid, scope: &str) -> Solid {
    s.set_name_scope(scope);
    s
}

/// The dogfood repro: Block 40×30×10 minus a through hole r=4 at its centre.
fn block_minus_hole() -> Solid {
    let block = scoped(Solid::cube(40.0, 30.0, 10.0), "n1");
    let hole = scoped(Solid::cylinder(4.0, 20.0, 0), "n2").translate(20.0, 15.0, -5.0);
    block.difference(&hole)
}

/// Block 40×30×10 with a slot (x 15..25, full depth in y, z 5..10) cut
/// across its top: the top face splits into two fragments, the front face's
/// top edge into two edges.
fn slotted_block() -> Solid {
    let block = scoped(Solid::cube(40.0, 30.0, 10.0), "n1");
    let slot = scoped(Solid::cube(10.0, 32.0, 6.0), "n2").translate(15.0, -1.0, 5.0);
    block.difference(&slot)
}

/// Fillet volume removed along a straight edge of length `l`, r = 2.
fn fillet_loss(l: f64) -> f64 {
    (1.0 - PI / 4.0) * 4.0 * l
}

#[test]
fn fillet_on_unsplit_edge_of_difference() {
    let part = block_minus_hole();
    let v0 = part.volume();
    assert!((v0 - 11497.5).abs() < 1.0, "base volume {v0}");
    let (a, b) = part
        .resolve_named_edge("n1:front", "n1:right")
        .expect("the x=40/y=0 edge survives the boolean");
    for p in [a, b] {
        assert!(p.x == 40.0 && p.y == 0.0, "{p:?}");
    }
    let filleted = part
        .edge_blend_named("n1:front", "n1:right", &keys(1.0))
        .expect("fillet applies");
    let loss = v0 - filleted.volume();
    assert!((loss - fillet_loss(10.0)).abs() < 0.05, "removed {loss}");
}

#[test]
fn chamfer_on_unsplit_edge_of_difference() {
    let part = block_minus_hole();
    let v0 = part.volume();
    let chamfered = part
        .edge_blend_named("n1:front", "n1:right", &keys(0.0))
        .expect("chamfer applies");
    // Leg 2 × 2, 10 mm long: ½·2·2·10 = 20 mm³.
    let loss = v0 - chamfered.volume();
    assert!((loss - 20.0).abs() < 0.05, "removed {loss}");
}

#[test]
fn fillet_on_edge_of_union() {
    let block = scoped(Solid::cube(40.0, 30.0, 10.0), "n1");
    let boss = scoped(Solid::cube(10.0, 10.0, 5.0), "n2").translate(15.0, 10.0, 10.0);
    let part = block.union(&boss);
    let v0 = part.volume();
    assert!((v0 - 12500.0).abs() < 1e-6, "union volume {v0}");
    // Block's own edge, and the boss's top edge — each scoped to its operand.
    let f = part
        .edge_blend_named("n1:front", "n1:right", &keys(1.0))
        .expect("block edge on the union resolves");
    assert!(((v0 - f.volume()) - fillet_loss(10.0)).abs() < 0.05);
    let (a, b) = part
        .resolve_named_edge("n2:top", "n2:right")
        .expect("boss edge on the union resolves");
    for p in [a, b] {
        assert!(p.x == 25.0 && p.z == 15.0, "{p:?}");
    }
}

/// A reference authored against the unsplit face (`n1:top`) resolves to
/// the one fragment that still meets `n1:right`; a reference captured on
/// the result names that fragment exactly.
#[test]
fn split_face_resolves_to_the_adjacent_fragment() {
    let part = slotted_block();
    let names = part.names().expect("names survive");
    let tops: Vec<String> = names
        .faces
        .values()
        .map(|n| n.to_string())
        .filter(|n| n.starts_with("n1:top"))
        .collect();
    assert_eq!(
        tops.len(),
        2,
        "top face splits into two fragments: {tops:?}"
    );

    let (a, b) = part
        .resolve_named_edge("n1:top", "n1:right")
        .expect("lineage picks the right-hand fragment's edge");
    for p in [a, b] {
        assert!(p.x == 40.0 && p.z == 10.0, "{p:?}");
    }
    let v0 = part.volume();
    let f = part
        .edge_blend_named("n1:top", "n1:right", &keys(1.0))
        .expect("fillet on the split face's edge applies");
    assert!(((v0 - f.volume()) - fillet_loss(30.0)).abs() < 0.1);

    // The exact fragment name a pick on the result captures also resolves.
    let brep = part.as_brep().unwrap();
    let edge = brep
        .topology
        .edges
        .iter()
        .map(|(id, _)| id)
        .find(|&e| {
            vcad_kernel::vcad_kernel_naming::edge_endpoints(brep, e)
                .is_some_and(|(p, q)| p.x == 40.0 && q.x == 40.0 && p.z == 10.0 && q.z == 10.0)
        })
        .unwrap();
    let r = vcad_kernel::vcad_kernel_naming::EdgeRef::capture(brep, names, edge).unwrap();
    assert!(r.face_a.to_string().starts_with("n1:") && r.face_b.to_string().starts_with("n1:"));
    let (a, _) = part
        .resolve_named_edge(&r.face_a.to_string(), &r.face_b.to_string())
        .expect("captured fragment names resolve");
    assert_eq!(a.x, 40.0);
}

/// Top and front meet along two edges once the slot cuts between them.
/// Names alone must fail closed with a clear message; a pick-time hint
/// singles out the intended edge; a hint matching neither stays ambiguous.
#[test]
fn ambiguous_reference_fails_closed() {
    let part = slotted_block();
    let err = part
        .resolve_named_edge("n1:top", "n1:front")
        .expect_err("two candidate edges must not resolve");
    assert_eq!(err, NamedEdgeError::Ambiguous { count: 2 });
    let msg = err.to_string();
    assert!(
        msg.contains("ambiguous") && msg.contains("2 candidate"),
        "{msg}"
    );
    let blend = part.edge_blend_named("n1:top", "n1:front", &keys(1.0));
    assert!(
        matches!(
            blend,
            Err(BlendError::NamedEdge(NamedEdgeError::Ambiguous { .. }))
        ),
        "{blend:?}"
    );

    let right_half = EdgeHint {
        midpoint: Point3::new(32.5, 0.0, 10.0),
        direction: Vec3::new(1.0, 0.0, 0.0),
        length: 15.0,
    };
    let (a, b) = part
        .resolve_named_edge_hinted("n1:top", "n1:front", Some(right_half))
        .expect("the hint singles out x 25..40");
    assert!(a.x.min(b.x) == 25.0 && a.x.max(b.x) == 40.0, "{a:?} {b:?}");

    let nowhere = EdgeHint {
        midpoint: Point3::new(20.0, 0.0, 10.0),
        direction: Vec3::new(1.0, 0.0, 0.0),
        length: 40.0,
    };
    assert!(matches!(
        part.resolve_named_edge_hinted("n1:top", "n1:front", Some(nowhere)),
        Err(NamedEdgeError::Ambiguous { count: 2 })
    ));
}

/// Names that no longer meet fall back to the hint; without one, the error
/// says which face is missing.
#[test]
fn lost_reference_reports_why_and_uses_the_hint() {
    let part = block_minus_hole();
    let err = part
        .resolve_named_edge("n1:front", "n9:nope")
        .expect_err("unknown face");
    assert!(err.to_string().contains("no face named 'n9:nope'"), "{err}");
    let hint = EdgeHint {
        midpoint: Point3::new(40.0, 0.0, 5.0),
        direction: Vec3::new(0.0, 0.0, 1.0),
        length: 10.0,
    };
    let (a, b) = part
        .resolve_named_edge_hinted("n1:front", "n9:nope", Some(hint))
        .expect("the hint matches exactly one edge");
    assert!(a.x == 40.0 && b.x == 40.0 && a.y == 0.0);
}

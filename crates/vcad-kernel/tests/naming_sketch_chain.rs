//! Persistent naming for sketch-built solids: extrude seeds names from its
//! sketch segments, and one-input modifiers (fillet, chamfer, shell) keep the
//! names of the faces they don't replace. Without this, a fillet on an
//! extruded plate had no names at all and every downstream reference was
//! lost on the first rebuild.

use vcad_kernel::vcad_kernel_math::{Dir3, Point2, Point3, Vec3};
use vcad_kernel::vcad_kernel_sketch::{SketchProfile, SketchSegment};
use vcad_kernel::Solid;

fn plate(w: f64, d: f64) -> SketchProfile {
    let p = Point2::new;
    let line = |a, b| SketchSegment::Line { start: a, end: b };
    SketchProfile {
        origin: Point3::new(0.0, 0.0, 0.0),
        x_dir: Dir3::new_normalize(Vec3::new(1.0, 0.0, 0.0)),
        y_dir: Dir3::new_normalize(Vec3::new(0.0, 1.0, 0.0)),
        normal: Dir3::new_normalize(Vec3::new(0.0, 0.0, 1.0)),
        segments: vec![
            line(p(0.0, 0.0), p(w, 0.0)),
            line(p(w, 0.0), p(w, d)),
            line(p(w, d), p(0.0, d)),
            line(p(0.0, d), p(0.0, 0.0)),
        ],
    }
}

fn names(s: &Solid) -> Vec<String> {
    let mut v: Vec<String> = s
        .names()
        .expect("solid should carry names")
        .faces
        .values()
        .map(|n| n.to_string())
        .collect();
    v.sort();
    v
}

#[test]
fn extrude_names_survive_chamfer_and_shell() {
    let solid = Solid::extrude(plate(80.0, 50.0), Vec3::new(0.0, 0.0, 8.0)).unwrap();
    let base = names(&solid);
    assert_eq!(base.len(), 6, "{base:?}");

    let chamfered = solid.chamfer(1.0).expect("chamfer");
    let n = names(&chamfered);
    // Every original face is trimmed, not replaced: all six names survive;
    // the 12 chamfer faces are new and stay anonymous.
    for name in &base {
        assert!(n.contains(name), "chamfer lost {name}: {n:?}");
    }

    let shelled = solid.shell(1.0).expect("shell");
    let n = names(&shelled);
    for name in &base {
        assert!(n.contains(name), "shell lost {name}: {n:?}");
    }
}

#[test]
fn edge_between_extruded_faces_resolves_after_a_dimension_edit() {
    // The edge between the top cap and wall side.1 (the x = w wall) —
    // the reference a fillet on that edge would store.
    for w in [60.0, 80.0, 120.0] {
        let solid = Solid::extrude(plate(w, 50.0), Vec3::new(0.0, 0.0, 8.0)).unwrap();
        let (a, b) = solid
            .resolve_named_edge("extrude:end", "extrude:side.1")
            .unwrap_or_else(|e| panic!("w={w}: {e:?}"));
        for p in [a, b] {
            assert!((p.x - w).abs() < 1e-9 && (p.z - 8.0).abs() < 1e-9, "w={w}: {p:?}");
        }
    }
}

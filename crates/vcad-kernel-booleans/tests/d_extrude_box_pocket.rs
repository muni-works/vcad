//! A box pocket cut into the curved wall of an arc-extruded D profile
//! (K1 wrist_interface.loon, minimized). The extrude stores the arc as an
//! 8-point chain shared by wall and caps; the boolean rebuilt the split
//! wall's rims on the dense canonical grid, orphaning the cap chains — 112
//! unpaired half-edges and visible tearing. `freeze_arc_chains` now moves
//! both sides onto the canonical grid before the pipeline runs.

use std::collections::HashMap;

use vcad_kernel_booleans::{boolean_op, BooleanOp, BooleanResult};
use vcad_kernel_math::{Point2, Point3, Vec3};
use vcad_kernel_primitives::{make_cube, BRepSolid};
use vcad_kernel_sketch::{extrude, SketchProfile, SketchSegment};
use vcad_kernel_tessellate::tessellate_brep;

fn d_half() -> BRepSolid {
    let segments = vec![
        SketchSegment::Line {
            start: Point2::new(0.0, -13.7),
            end: Point2::new(0.0, 13.7),
        },
        SketchSegment::Arc {
            start: Point2::new(0.0, 13.7),
            end: Point2::new(0.0, -13.7),
            center: Point2::new(0.0, 0.0),
            ccw: true,
        },
    ];
    let profile = SketchProfile::new(Point3::new(0.0, 0.0, -0.3), Vec3::x(), Vec3::y(), segments)
        .expect("valid profile");
    extrude(&profile, Vec3::new(0.0, 0.0, 31.7)).expect("extrude D")
}

fn translated(mut b: BRepSolid, d: Vec3) -> BRepSolid {
    let t = vcad_kernel_math::Transform::translation(d.x, d.y, d.z);
    for (_id, v) in &mut b.topology.vertices {
        v.point = t.apply_point(&v.point);
    }
    for s in &mut b.geometry.surfaces {
        *s = s.transform(&t);
    }
    b
}

/// (directed edges with no reverse partner, signed volume)
fn mesh_stats(b: &BRepSolid) -> (usize, f64) {
    mesh_stats_at(b, 64)
}

fn mesh_stats_at(b: &BRepSolid, segments: u32) -> (usize, f64) {
    let mesh = tessellate_brep(b, segments);
    let p = |i: u32| {
        let k = i as usize * 3;
        [
            mesh.vertices[k] as f64,
            mesh.vertices[k + 1] as f64,
            mesh.vertices[k + 2] as f64,
        ]
    };
    let key = |a: [f64; 3]| a.map(|c| (c * 1e4).round() as i64);
    let mut e: HashMap<([i64; 3], [i64; 3]), i32> = HashMap::new();
    let mut vol = 0.0;
    for t in mesh.indices.chunks(3) {
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        for (x, y) in [(a, b), (b, c), (c, a)] {
            *e.entry((key(x), key(y))).or_default() += 1;
        }
        vol += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]))
            / 6.0;
    }
    let open = e
        .iter()
        .filter(|((x, y), n)| e.get(&(*y, *x)) != Some(n))
        .count();
    (open, vol)
}

#[test]
fn box_pocket_in_arc_extruded_wall_is_watertight() {
    let d = d_half();
    let pocket = translated(make_cube(11.6, 6.4, 6.4), Vec3::new(-15.6, -3.2, 3.33));
    let BooleanResult::BRep(solid) =
        boolean_op(&d, &pocket, BooleanOp::Difference, 64).expect("boolean");
    let (open, vol) = mesh_stats(&solid);
    // D volume ≈ ½π·13.7²·31.7 = 9345.6; pocket ∩ D = 6.4·6.4·(13.7 cos-chord) ≈ 6.4²·9.33 ≈ 382
    assert_eq!(open, 0, "pocketed D must tessellate watertight");
    assert!((vol - (9345.6 - 382.0)).abs() < 40.0, "vol {vol}");
}

/// The bare D must tessellate closed at every segment count. Its curved
/// wall is a two-chain strip whose rims are the extrude's 8-point arc
/// chains, which the planar caps render verbatim; at 128 segments the wall
/// used to be resampled on the cylinder grid instead (the ruled path read
/// the wall's own vertical ruling as an end-column insertion), leaving
/// 1 mm-sag lunes against both caps — 144 open edges. K1 wrist_adapter's
/// first union handed exactly that open operand to the mesh fallback.
#[test]
fn bare_arc_extrude_tessellates_closed_at_any_segment_count() {
    let d = d_half();
    let (r, h) = (13.7_f64, 31.7);
    let exact = 0.5 * std::f64::consts::PI * r * r * h;
    for segments in [8, 16, 32, 64, 128, 256] {
        let (open, vol) = mesh_stats_at(&d, segments);
        assert_eq!(open, 0, "{segments} segments: {open} open edges");
        // The arc is drawn as chords of at most the extrude's own 22.5°
        // chain step and at most the tessellation step: an inscribed
        // polygon whose area is sin(α)/α of the sector's.
        let alpha = (std::f64::consts::PI / 8.0).min(2.0 * std::f64::consts::PI / segments as f64);
        let lo = exact * alpha.sin() / alpha;
        assert!(
            vol >= lo - 0.5 && vol <= exact + 0.5,
            "{segments} segments: vol {vol:.2} outside [{lo:.2}, {exact:.2}]"
        );
    }
}

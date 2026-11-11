//! Names for solids built from a sketch profile (extrude).
//!
//! Primitive seeding ([`crate::seed_names`]) orders same-tag faces by
//! centroid, which is stable across rebuilds of the *same* primitive but not
//! across a sketch edit: move one segment and every centroid shifts. Fusion's
//! answer — and ours — is to name each wall after the sketch entity that swept
//! it. A wall is `side.<i>` where `i` is the index of the outer-loop segment
//! that generated it, an interior wall is `hole.<h>.<j>` (loop `h`, segment
//! `j`), and the caps are `start` (on the sketch plane) and `end` (at the far
//! end of the extrusion). Changing a dimension keeps every index, so every
//! name, and downstream references resolve by name.
//!
//! Matching is geometric, like [`crate::propagate_boolean`]: a line segment
//! sweeps a plane, an arc sweeps a cylinder, and each result face is matched
//! to the segment whose swept surface carries it. Fail-closed: a face no
//! segment explains, or one two segments explain equally well, stays
//! anonymous rather than taking a guessed name.

use std::collections::HashMap;

use vcad_kernel_geom::{CylinderSurface, Plane, SurfaceKind};
use vcad_kernel_math::{Point3, Vec3};
use vcad_kernel_primitives::BRepSolid;
use vcad_kernel_sketch::{SketchProfile, SketchSegment};
use vcad_kernel_topo::FaceId;

use crate::{face_centroid, quantize, FaceName, NameMap};

/// Geometric tolerance for "this face lies on that swept surface" (mm).
const TOL: f64 = 1e-6;

/// The surface one profile segment sweeps when extruded along `dir`.
enum Swept {
    /// A line sweeps a plane: a point on it, its unit normal, and the
    /// segment's 3D endpoints (to tell collinear segments apart).
    Plane { a: Point3, b: Point3, normal: Vec3 },
    /// An arc sweeps a cylinder about `center` along the extrusion axis.
    Cylinder {
        center: Point3,
        radius: f64,
        /// Start and end angles in the sketch plane, and direction.
        start: f64,
        sweep: f64,
    },
}

/// Seed face names for a solid produced by extruding `profile` (and optional
/// interior `holes`, in the profile's 2D frame) along `direction`.
///
/// Returns names only for faces it can attribute unambiguously; see the
/// module docs for the scheme. `scope` is the provisional scope (`extrude`),
/// rewritten by DAG evaluators to the node id like every other seeded name.
pub fn seed_extrude_names(
    brep: &BRepSolid,
    profile: &SketchProfile,
    holes: &[Vec<SketchSegment>],
    direction: Vec3,
    scope: &str,
) -> NameMap {
    let len = direction.norm();
    let mut map = NameMap::new();
    if len < TOL {
        return map;
    }
    let axis = direction * (1.0 / len);
    let to3 = |p: vcad_kernel_math::Point2| -> Point3 {
        profile.origin + profile.x_dir.into_inner() * p.x + profile.y_dir.into_inner() * p.y
    };

    // (tag, path prefix, swept surface) for every segment of every loop.
    let mut walls: Vec<(&str, Vec<u32>, Swept)> = Vec::new();
    let mut push_loop = |tag: &'static str, prefix: Vec<u32>, segs: &[SketchSegment]| {
        for (i, seg) in segs.iter().enumerate() {
            let mut path = prefix.clone();
            path.push(i as u32);
            walls.push((tag, path, swept(seg, &to3, axis)));
        }
    };
    push_loop("side", Vec::new(), &profile.segments);
    for (h, hole) in holes.iter().enumerate() {
        push_loop("hole", vec![h as u32], hole);
    }

    let topo = &brep.topology;
    let shell = &topo.shells[topo.solids[brep.solid_id].outer_shell];
    // name → faces carrying it, so split walls get deterministic ordinals.
    let mut groups: HashMap<FaceName, Vec<([i64; 3], FaceId)>> = HashMap::new();
    for &face_id in &shell.faces {
        let face = &topo.faces[face_id];
        let surface = brep.geometry.surfaces[face.surface_index].as_ref();
        let c = face_centroid(brep, face_id);
        let name = match surface.surface_type() {
            SurfaceKind::Plane => {
                let Some(p) = surface.as_any().downcast_ref::<Plane>() else { continue };
                let n = p.normal_dir.into_inner();
                if n.dot(axis).abs() > 1.0 - TOL {
                    let h = (c - profile.origin).dot(axis);
                    if h.abs() < TOL * len.max(1.0) {
                        Some(FaceName::new(scope, "start"))
                    } else if (h - len).abs() < TOL * len.max(1.0) {
                        Some(FaceName::new(scope, "end"))
                    } else {
                        None
                    }
                } else {
                    // A line sweeps exactly this plane; an arc the sketch
                    // kernel faceted (interior hole loops are polygonized)
                    // sweeps chord planes whose normal points radially and
                    // whose centroid sits just inside the circle.
                    unique_wall(&walls, scope, |w| match w {
                        Swept::Plane { a, b, normal } => {
                            normal.dot(n).abs() > 1.0 - TOL
                                && (p.origin - *a).dot(*normal).abs() < TOL
                                && within_segment(c, *a, *b)
                        }
                        Swept::Cylinder { center, radius, start, sweep } => {
                            let d = c - *center;
                            let radial = d - axis * d.dot(axis);
                            let r = radial.norm();
                            r > radius * 0.9
                                && r < radius + TOL
                                && (radial * (1.0 / r)).dot(n).abs() > 0.99
                                && within_arc(c, *center, profile, *start, *sweep)
                        }
                    })
                }
            }
            SurfaceKind::Cylinder => {
                let Some(cy) = surface.as_any().downcast_ref::<CylinderSurface>() else { continue };
                let ax = cy.axis.into_inner();
                unique_wall(&walls, scope, |w| match w {
                    Swept::Cylinder { center, radius, start, sweep } => {
                        let d = cy.center - *center;
                        ax.dot(axis).abs() > 1.0 - TOL
                            && (cy.radius - radius).abs() < TOL
                            && (d - axis * d.dot(axis)).norm() < TOL
                            && within_arc(c, *center, profile, *start, *sweep)
                    }
                    Swept::Plane { .. } => false,
                })
            }
            _ => None,
        };
        if let Some(name) = name {
            groups.entry(name).or_default().push((quantize(c), face_id));
        }
    }

    for (name, mut faces) in groups {
        faces.sort();
        let solo = faces.len() == 1;
        for (k, (_, id)) in faces.into_iter().enumerate() {
            let mut n = name.clone();
            if !solo {
                n.path.push(k as u32);
            }
            map.faces.insert(id, n);
        }
    }
    map
}

/// The one wall `hit` accepts, as a name — `None` for zero or several.
fn unique_wall(
    walls: &[(&str, Vec<u32>, Swept)],
    scope: &str,
    hit: impl Fn(&Swept) -> bool,
) -> Option<FaceName> {
    let mut found = walls.iter().filter(|(_, _, w)| hit(w));
    let (tag, path, _) = found.next()?;
    if found.next().is_some() {
        return None;
    }
    let mut name = FaceName::new(scope, *tag);
    name.path = path.clone();
    Some(name)
}

fn swept(
    seg: &SketchSegment,
    to3: &impl Fn(vcad_kernel_math::Point2) -> Point3,
    axis: Vec3,
) -> Swept {
    match seg {
        SketchSegment::Line { start, end } => {
            let (a, b) = (to3(*start), to3(*end));
            let n = (b - a).cross(axis);
            let normal = if n.norm() > 0.0 { n * (1.0 / n.norm()) } else { n };
            Swept::Plane { a, b, normal }
        }
        SketchSegment::Arc { start, end, center, ccw } => {
            let ang = |p: vcad_kernel_math::Point2| (p.y - center.y).atan2(p.x - center.x);
            let (s, e) = (ang(*start), ang(*end));
            let tau = std::f64::consts::TAU;
            let mut sweep = if *ccw { e - s } else { s - e };
            sweep = sweep.rem_euclid(tau);
            if sweep < 1e-12 {
                sweep = tau; // start == end: a full circle
            }
            let radius = ((start.x - center.x).powi(2) + (start.y - center.y).powi(2)).sqrt();
            let signed = if *ccw { sweep } else { -sweep };
            Swept::Cylinder { center: to3(*center), radius, start: s, sweep: signed }
        }
    }
}

/// Does `c`, projected onto the segment's line, fall inside `[a, b]`?
/// Splits collinear segments, which sweep the same plane.
fn within_segment(c: Point3, a: Point3, b: Point3) -> bool {
    let ab = b - a;
    let l2 = ab.dot(ab);
    if l2 < TOL * TOL {
        return false;
    }
    let t = (c - a).dot(ab) / l2;
    (-1e-6..=1.0 + 1e-6).contains(&t)
}

/// Does `c` lie within the arc's angular span, seen from its centre?
/// The vertex-average centroid of a cylindrical patch sits on the chord of
/// that patch, so its angle is the patch's mid-angle — inside the arc.
fn within_arc(c: Point3, center: Point3, profile: &SketchProfile, start: f64, sweep: f64) -> bool {
    if sweep.abs() >= std::f64::consts::TAU - 1e-9 {
        return true;
    }
    let d = c - center;
    let (x, y) = (d.dot(profile.x_dir.into_inner()), d.dot(profile.y_dir.into_inner()));
    if x * x + y * y < TOL * TOL {
        return false;
    }
    let a = y.atan2(x);
    let rel = if sweep > 0.0 { a - start } else { start - a };
    rel.rem_euclid(std::f64::consts::TAU) <= sweep.abs() + 1e-9
}

#[cfg(test)]
mod tests {
    use super::*;
    use vcad_kernel_math::{Dir3, Point2};

    fn plate(w: f64, d: f64) -> SketchProfile {
        let p = |x, y| Point2::new(x, y);
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

    fn names_of(brep: &BRepSolid, map: &NameMap) -> Vec<String> {
        let shell = &brep.topology.shells[brep.topology.solids[brep.solid_id].outer_shell];
        let mut v: Vec<String> = shell
            .faces
            .iter()
            .map(|f| map.faces.get(f).map(|n| n.to_string()).unwrap_or_else(|| "?".into()))
            .collect();
        v.sort();
        v
    }

    /// The outward normal of the face named `name`, as a stand-in for "which
    /// wall is it" that does not depend on the naming under test.
    fn normal_of(brep: &BRepSolid, map: &NameMap, name: &str) -> Vec3 {
        let id = map.face_named(&FaceName::parse(name).unwrap()).expect(name);
        let s = brep.geometry.surfaces[brep.topology.faces[id].surface_index].as_ref();
        s.as_any().downcast_ref::<Plane>().unwrap().normal_dir.into_inner()
    }

    #[test]
    fn plate_walls_are_named_by_segment_and_caps_by_end() {
        let prof = plate(80.0, 50.0);
        let dir = Vec3::new(0.0, 0.0, 8.0);
        let brep = vcad_kernel_sketch::extrude(&prof, dir).unwrap();
        let map = seed_extrude_names(&brep, &prof, &[], dir, "extrude");
        assert_eq!(
            names_of(&brep, &map),
            [
                "extrude:end",
                "extrude:side.0",
                "extrude:side.1",
                "extrude:side.2",
                "extrude:side.3",
                "extrude:start"
            ]
        );
        // side.0 is the wall swept by the y = 0 segment.
        assert!(normal_of(&brep, &map, "extrude:side.0").y.abs() > 0.99);
        assert!(normal_of(&brep, &map, "extrude:side.1").x.abs() > 0.99);
    }

    #[test]
    fn names_follow_segments_through_a_dimension_edit() {
        // Widening the plate moves every centroid; a centroid-ordered scheme
        // would reshuffle the walls. Segment-indexed names must not.
        let dir = Vec3::new(0.0, 0.0, 8.0);
        for w in [30.0, 80.0, 200.0] {
            let prof = plate(w, 50.0);
            let brep = vcad_kernel_sketch::extrude(&prof, dir).unwrap();
            let map = seed_extrude_names(&brep, &prof, &[], dir, "e");
            assert!(normal_of(&brep, &map, "e:side.1").x > 0.99, "w={w}");
            assert!(normal_of(&brep, &map, "e:side.3").x < -0.99, "w={w}");
        }
    }

    #[test]
    fn circular_hole_wall_is_named_by_loop_and_segment() {
        let prof = plate(80.0, 50.0);
        // A circle as two half arcs (the sketch kernel's closed-loop form).
        let arc = |a: (f64, f64), b: (f64, f64)| SketchSegment::Arc {
            start: Point2::new(a.0, a.1),
            end: Point2::new(b.0, b.1),
            center: Point2::new(40.0, 25.0),
            ccw: true,
        };
        let hole = vec![arc((45.0, 25.0), (35.0, 25.0)), arc((35.0, 25.0), (45.0, 25.0))];
        let dir = Vec3::new(0.0, 0.0, 8.0);
        let brep = vcad_kernel_sketch::extrude_with_holes(&prof, &[hole.clone()], dir)
            .expect("extrude with a circular hole");
        let map = seed_extrude_names(&brep, &prof, &[hole], dir, "e");
        let names = names_of(&brep, &map);
        assert!(!names.iter().any(|n| n == "?"), "every face named: {names:?}");
        assert!(names.iter().any(|n| n.starts_with("e:hole.0.0")), "{names:?}");
        assert!(names.iter().any(|n| n.starts_with("e:hole.0.1")), "{names:?}");
    }
}

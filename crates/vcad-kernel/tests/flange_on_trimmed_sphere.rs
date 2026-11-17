//! A flange disc unioned onto a sphere that a box has already trimmed.
//!
//! The spherical-cube vacuum chamber body is `sphere ∩ box`, with a flange
//! disc on each of the six flats whose back face sits 1 mm BEHIND the box
//! plane. The disc's back plane therefore cuts the sphere in a circle that
//! rings the hole the box plane left (or, for the flat whose trim circle
//! happens to be the patch's outer loop, hugs that loop and rings the other
//! five holes).
//!
//! The sphere clip declined both: it could punch a hole in a patch only when
//! every existing hole stayed on the patch's side. The whole-sphere fallback
//! then threw the patch's holes away, the pipeline returned the two operands
//! concatenated, and the volume referee let that through as `Analytic` —
//! seven overlapping closed shells, +1% volume, the box planes left inside as
//! membranes. With that fixed, the sixth flange still failed: the 1 mm band
//! is ~0.5% of the sphere, the classifier's 256 sample directions all missed
//! it, and it was classified by its own hole's centre.

use std::collections::HashMap;
use std::f64::consts::PI;

use vcad_kernel::Solid;
use vcad_kernel_tessellate::TriangleMesh;

/// Chamber body dimensions (mm): outer sphere, box half-width, flange back
/// plane, flange radius and thickness.
#[derive(Clone, Copy)]
struct Dims {
    name: &'static str,
    r_out: f64,
    e: f64,
    h: f64,
    r_fl: f64,
    t: f64,
}

/// `vacuum.chamber.spherical-cube` at DN63 / DN100 / DN160 (see
/// `vcad_parts::vacuum::chambers::chamber_geom`).
fn dims() -> [Dims; 3] {
    let make = |name, od: f64, bore: f64, t: f64| {
        let r_i = 106.68 * od / 151.6;
        let h = (od / 2.0 + 3.0).max((r_i * r_i - bore * bore / 4.0).sqrt() + 1.0);
        Dims {
            name,
            r_out: (h * h + (od / 2.0 - 3.0).powi(2)).sqrt(),
            e: h + 1.0,
            h,
            r_fl: od / 2.0,
            t,
        }
    };
    [
        make("DN63", 113.5, 66.0, 17.5),
        make("DN100", 151.6, 100.0, 19.8),
        make("DN160", 202.4, 150.0, 22.2),
    ]
}

fn body(d: &Dims) -> Solid {
    let c = Solid::cube(2.0 * d.e, 2.0 * d.e, 2.0 * d.e).translate(-d.e, -d.e, -d.e);
    Solid::sphere(d.r_out, 0).intersection(&c)
}

const AXES: [(&str, [f64; 3], [f64; 3]); 6] = [
    ("+z", [0.0, 0.0, 0.0], [0.0, 0.0, 1.0]),
    ("-z", [180.0, 0.0, 0.0], [0.0, 0.0, -1.0]),
    ("+x", [0.0, 90.0, 0.0], [1.0, 0.0, 0.0]),
    ("-x", [0.0, -90.0, 0.0], [-1.0, 0.0, 0.0]),
    ("+y", [-90.0, 0.0, 0.0], [0.0, 1.0, 0.0]),
    ("-y", [90.0, 0.0, 0.0], [0.0, -1.0, 0.0]),
];

fn flange(d: &Dims, axis: usize) -> Solid {
    let (_, rot, dir) = AXES[axis];
    Solid::cylinder(d.r_fl, d.t, 0)
        .rotate(rot[0], rot[1], rot[2])
        .translate(dir[0] * d.h, dir[1] * d.h, dir[2] * d.h)
}

struct Census {
    volume: f64,
    boundary: usize,
    non_manifold: usize,
    misoriented: usize,
    components: usize,
}

fn census(m: &TriangleMesh) -> Census {
    let p = |i: u32| {
        let k = 3 * i as usize;
        [
            m.vertices[k] as f64,
            m.vertices[k + 1] as f64,
            m.vertices[k + 2] as f64,
        ]
    };
    let key = |i: u32| p(i).map(|c| (c * 1e4).round() as i64);
    let mut ids: HashMap<[i64; 3], usize> = HashMap::new();
    let mut parent: Vec<usize> = Vec::new();
    fn find(parent: &mut [usize], mut i: usize) -> usize {
        while parent[i] != i {
            parent[i] = parent[parent[i]];
            i = parent[i];
        }
        i
    }
    let mut edges: HashMap<(usize, usize), Vec<bool>> = HashMap::new();
    let mut volume = 0.0;
    for t in m.indices.chunks(3) {
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        volume += (a[0] * (b[1] * c[2] - b[2] * c[1]) - a[1] * (b[0] * c[2] - b[2] * c[0])
            + a[2] * (b[0] * c[1] - b[1] * c[0]))
            / 6.0;
        let v: Vec<usize> = t
            .iter()
            .map(|&i| {
                let n = ids.len();
                let id = *ids.entry(key(i)).or_insert(n);
                if id == parent.len() {
                    parent.push(id);
                }
                id
            })
            .collect();
        for i in 0..3 {
            let (a, b) = (v[i], v[(i + 1) % 3]);
            if a == b {
                continue;
            }
            let (ra, rb) = (find(&mut parent, a), find(&mut parent, b));
            parent[ra] = rb;
            edges.entry((a.min(b), a.max(b))).or_default().push(a < b);
        }
    }
    let (mut boundary, mut non_manifold, mut misoriented) = (0, 0, 0);
    for uses in edges.values() {
        match uses.len() {
            1 => boundary += 1,
            2 => misoriented += usize::from(uses[0] == uses[1]),
            _ => non_manifold += 1,
        }
    }
    let mut roots: Vec<usize> = (0..parent.len()).map(|i| find(&mut parent, i)).collect();
    roots.sort_unstable();
    roots.dedup();
    Census {
        volume,
        boundary,
        non_manifold,
        misoriented,
        components: roots.len(),
    }
}

fn assert_clean(what: &str, s: &Solid) -> Census {
    assert_eq!(
        s.fidelity().as_str(),
        "analytic",
        "{what}: {:?}",
        s.degradations()
            .iter()
            .map(|e| e.to_string())
            .collect::<Vec<_>>()
    );
    assert!(s.degradations().is_empty(), "{what}: degraded");
    let c = census(&s.to_mesh(32));
    assert_eq!(
        (c.boundary, c.non_manifold, c.misoriented),
        (0, 0, 0),
        "{what}: boundary / non-manifold / misoriented edges"
    );
    // Two closed shells that overlap pass every edge check; they are not
    // one solid.
    assert_eq!(c.components, 1, "{what}: mesh components");
    c
}

/// Planar faces of `s` lying on the box plane `axis = ±e`. After the flange
/// is fused on, that plane is buried inside the flange: a face left there
/// is an interior membrane.
fn faces_on_box_plane(s: &Solid, d: &Dims, axis: usize) -> usize {
    let (_, _, dir) = AXES[axis];
    let b = s.brep().expect("brep");
    b.topology
        .faces
        .iter()
        .filter(|(_, f)| {
            let vs = b.topology.loop_vertices(f.outer_loop);
            vs.len() >= 3
                && vs.iter().all(|v| {
                    let p = b.topology.vertices[*v].point;
                    (p.x * dir[0] + p.y * dir[1] + p.z * dir[2] - d.e).abs() < 1e-6
                })
        })
        .count()
}

/// Volume the sphere has between the planes at `h` and `e`: what body and
/// flange share.
fn slab(d: &Dims) -> f64 {
    PI * (d.r_out * d.r_out * (d.e - d.h) - (d.e.powi(3) - d.h.powi(3)) / 3.0)
}

#[test]
fn one_flange_fuses_onto_a_box_trimmed_sphere_from_every_side() {
    for d in dims() {
        let body = body(&d);
        let body_volume = assert_clean(&format!("{} body", d.name), &body).volume;
        for axis in 0..6 {
            let what = format!("{} flange {}", d.name, AXES[axis].0);
            let fl = flange(&d, axis);
            let fl_volume = census(&fl.to_mesh(32)).volume;
            let fused = body.union(&fl);
            let c = assert_clean(&what, &fused);
            assert_eq!(
                faces_on_box_plane(&fused, &d, axis),
                0,
                "{what}: the box plane survives inside the flange"
            );
            // The operands share the 1 mm slab. Concatenating them (the old
            // result) removes none of it.
            let shared = body_volume + fl_volume - c.volume;
            assert!(
                shared > 0.5 * slab(&d) && shared < 1.5 * slab(&d),
                "{what}: union removed {shared:.0} mm³ of overlap, the shared slab is {:.0}",
                slab(&d)
            );
        }
    }
}

#[test]
fn six_flanges_fuse_into_one_chamber_body() {
    for d in dims() {
        let mut s = body(&d);
        for axis in 0..6 {
            s = s.union(&flange(&d, axis));
        }
        let c = assert_clean(&format!("{} body + 6 flanges", d.name), &s);
        for axis in 0..6 {
            assert_eq!(
                faces_on_box_plane(&s, &d, axis),
                0,
                "{}: box plane {} survives as a membrane",
                d.name,
                AXES[axis].0
            );
        }
        let b = s.brep().unwrap();
        let spheres = b
            .topology
            .faces
            .iter()
            .filter(|(_, f)| {
                b.geometry.surfaces[f.surface_index].surface_type()
                    == vcad_kernel_geom::SurfaceKind::Sphere
            })
            .count();
        assert_eq!(spheres, 1, "{}: one outer sphere face", d.name);
        // Closed form: the sphere less its six caps beyond the box, plus
        // each flange disc less the slab it shares with the sphere.
        let cap = PI * (d.r_out - d.e).powi(2) * (2.0 * d.r_out + d.e) / 3.0;
        let exact = 4.0 / 3.0 * PI * d.r_out.powi(3) - 6.0 * cap
            + 6.0 * (PI * d.r_fl * d.r_fl * d.t - slab(&d));
        // The mesh sphere is inscribed, so the mesh reads low (−0.22% on
        // DN63); six un-fused slabs read +1.0%.
        let err = (c.volume - exact) / exact;
        assert!(
            (-0.006..0.001).contains(&err),
            "{}: body volume {:.0} vs closed form {exact:.0} ({:+.3}%)",
            d.name,
            c.volume,
            err * 100.0
        );
    }
}

//! An emissive medium against the radiative-transfer answer.
//!
//! A uniform slab seen against a black sky reads `ε·L` (emission only) or
//! `(ε/κ)(1 − e^{−κL})` (with absorption); a wall halfway through hides the
//! far half. These drive the real integrator (`Tracer::radiance`) over the
//! real splat cloud `MediumCloud` builds, so they test the calibration and
//! the occlusion together.

use std::sync::Arc;

use kosm_render::pathtrace::Object;
use kosm_render::{Bvh, Point3, Ray, TriMesh, Vec3};
use vcad_kernel_raytrace::medium::{lattice_gain, EmissiveMedium, MediumCloud, SIGMA_OVER_H};
use vcad_kernel_raytrace::pathtrace::{Environment, PathTraceOptions, Pbr, Tracer};

/// A closed axis-aligned box as triangles.
fn boxmesh(min: [f64; 3], max: [f64; 3]) -> TriMesh {
    let p = |x: usize, y: usize, z: usize| {
        Point3::new(
            if x == 0 { min[0] } else { max[0] },
            if y == 0 { min[1] } else { max[1] },
            if z == 0 { min[2] } else { max[2] },
        )
    };
    let mut pos = Vec::new();
    for z in 0..2 {
        for y in 0..2 {
            for x in 0..2 {
                pos.push(p(x, y, z));
            }
        }
    }
    let quads: [[u32; 4]; 6] = [
        [0, 2, 3, 1],
        [4, 5, 7, 6],
        [0, 1, 5, 4],
        [2, 6, 7, 3],
        [0, 4, 6, 2],
        [1, 3, 7, 5],
    ];
    let mut idx = Vec::new();
    for q in quads {
        idx.extend_from_slice(&[q[0], q[1], q[2], q[0], q[2], q[3]]);
    }
    let normals = pos
        .iter()
        .map(|q| Vec3::new(q.x, q.y, q.z).normalize())
        .collect();
    TriMesh::new(pos, normals, &idx)
}

const EPS: [f32; 3] = [1.0, 0.5, 0.25];

fn slab_cloud(l: f64, absorption: f32, h: f64) -> Arc<Bvh<kosm_render::Splats>> {
    let slab = Bvh::build(boxmesh([-20.0, -20.0, 0.0], [20.0, 20.0, l]));
    let mut cloud = MediumCloud::new();
    let n = cloud.add(
        &slab,
        &EmissiveMedium {
            emission: EPS,
            absorption,
            falloff: 0.0,
        },
        h,
    );
    assert!(n > 0);
    assert!(!cloud.clamped());
    cloud.build().unwrap()
}

fn scene(
    splats: Arc<Bvh<kosm_render::Splats>>,
    objects: Vec<kosm_render::pathtrace::Object<TriMesh>>,
) -> kosm_render::pathtrace::Scene<TriMesh> {
    kosm_render::pathtrace::Scene {
        objects,
        lights: Vec::new(),
        env: Environment::constant([0.0; 3]),
        sun: None,
        ground: None,
        splats: Some(splats),
    }
}

fn radiance(
    s: &kosm_render::pathtrace::Scene<TriMesh>,
    origin: [f64; 3],
    dir: [f64; 3],
) -> [f32; 3] {
    let t = Tracer::new(s, PathTraceOptions::default());
    t.radiance(
        Ray::new(
            Point3::new(origin[0], origin[1], origin[2]),
            Vec3::new(dir[0], dir[1], dir[2]),
        ),
        1,
    )
}

fn assert_close(got: [f32; 3], want: [f64; 3], rel: f64, what: &str) {
    for c in 0..3 {
        let err = (got[c] as f64 - want[c]).abs() / want[c];
        assert!(
            err < rel,
            "{what}: channel {c} got {} want {} (rel err {err:.4})",
            got[c],
            want[c]
        );
    }
}

/// Several ray positions inside one lattice cell, so the check covers the
/// worst-case alignment rather than one lucky line.
const OFFSETS: [[f64; 2]; 4] = [[0.0, 0.0], [0.13, 0.31], [0.25, 0.25], [0.41, 0.07]];

#[test]
fn emission_only_slab_reads_eps_times_length() {
    let h = 0.5;
    for l in [2.0, 4.0, 8.0] {
        let s = scene(slab_cloud(l, 0.0, h), Vec::new());
        let m = EmissiveMedium {
            emission: EPS,
            absorption: 0.0,
            falloff: 0.0,
        };
        for o in OFFSETS {
            let got = radiance(&s, [o[0], o[1], l + 10.0], [0.0, 0.0, -1.0]);
            assert_close(got, m.slab_radiance(l), 0.01, &format!("L={l} at {o:?}"));
        }
    }
}

#[test]
fn absorbing_slab_follows_beer_lambert_emission() {
    let (h, l, kappa) = (0.5, 4.0, 0.2f32);
    assert!(kappa as f64 / lattice_gain(h) < 0.05);
    let s = scene(slab_cloud(l, kappa, h), Vec::new());
    let m = EmissiveMedium {
        emission: EPS,
        absorption: kappa,
        falloff: 0.0,
    };
    let want = m.slab_radiance(l);
    // Optically meaningful: well below the emission-only ε·L.
    assert!(want[0] < 0.8 * EPS[0] as f64 * l);
    for o in OFFSETS {
        let got = radiance(&s, [o[0], o[1], l + 10.0], [0.0, 0.0, -1.0]);
        assert_close(got, want, 0.02, &format!("κ={kappa} at {o:?}"));
    }
}

#[test]
fn oblique_ray_sees_the_longer_chord() {
    let (h, l) = (0.5, 4.0);
    let s = scene(slab_cloud(l, 0.0, h), Vec::new());
    let c = std::f64::consts::FRAC_1_SQRT_2;
    let m = EmissiveMedium {
        emission: EPS,
        absorption: 0.0,
        falloff: 0.0,
    };
    let want = m.slab_radiance(l / c);
    // Start above the slab, aimed at its centre at 45°.
    let got = radiance(&s, [-10.0, 0.1, l / 2.0 + 10.0], [c, 0.0, -c]);
    assert_close(got, want, 0.015, "45° chord");
}

#[test]
fn a_wall_inside_the_medium_hides_the_far_half() {
    let (h, l) = (0.5, 4.0);
    // A black, non-emissive plate at mid-depth, larger than the ray footprint.
    let wall = Object::new(
        Arc::new(Bvh::build(boxmesh(
            [-5.0, -5.0, l / 2.0 - 1e-3],
            [5.0, 5.0, l / 2.0],
        ))),
        Pbr {
            base_color: [0.0; 3],
            roughness: 1.0,
            ..Default::default()
        },
    );
    let s = scene(slab_cloud(l, 0.0, h), vec![wall]);
    let m = EmissiveMedium {
        emission: EPS,
        absorption: 0.0,
        falloff: 0.0,
    };
    // From above: only the near half (z ∈ [L/2, L]) is visible.
    let got = radiance(&s, [0.13, 0.31, l + 10.0], [0.0, 0.0, -1.0]);
    assert_close(
        got,
        m.slab_radiance(l / 2.0),
        0.02,
        "wall hides the far half",
    );
    // Beside the wall, the full slab is seen.
    let got = radiance(&s, [10.13, 0.31, l + 10.0], [0.0, 0.0, -1.0]);
    assert_close(got, m.slab_radiance(l), 0.01, "beside the wall");
}

#[test]
fn falloff_softens_the_edge_and_dims_the_total() {
    let (h, l) = (0.25, 6.0);
    let slab = Bvh::build(boxmesh([-20.0, -20.0, 0.0], [20.0, 20.0, l]));
    let soft = EmissiveMedium {
        emission: EPS,
        absorption: 0.0,
        falloff: 1.5,
    };
    let mut cloud = MediumCloud::new();
    cloud.add(&slab, &soft, h);
    let s = scene(cloud.build().unwrap(), Vec::new());
    let got = radiance(&s, [0.13, 0.31, l + 10.0], [0.0, 0.0, -1.0]);
    // Smoothstep over 1.5 mm at each face integrates to half the ramp, so the
    // slab reads 1.5 mm thinner (to lattice resolution).
    let m = EmissiveMedium {
        falloff: 0.0,
        ..soft
    };
    assert_close(got, m.slab_radiance(l - 1.5), 0.03, "soft-edged slab");
    let _ = SIGMA_OVER_H;
}

/// A slab described by its signed depth (`add_field`) reads the same as the
/// meshed slab (`add`): same lattice, same calibration, same answer.
#[test]
fn field_slab_matches_the_meshed_slab() {
    let (h, l) = (0.5, 4.0);
    let m = EmissiveMedium {
        emission: EPS,
        absorption: 0.0,
        falloff: 0.0,
    };
    let mut cloud = MediumCloud::new();
    let n = cloud.add_field(
        [-20.0, -20.0, 0.0],
        [20.0, 20.0, l],
        |p| p[2].min(l - p[2]),
        &m,
        h,
    );
    assert!(n > 0);
    let s = scene(cloud.build().unwrap(), Vec::new());
    for o in OFFSETS {
        let got = radiance(&s, [o[0], o[1], l + 10.0], [0.0, 0.0, -1.0]);
        assert_close(
            got,
            m.slab_radiance(l),
            0.01,
            &format!("field slab at {o:?}"),
        );
    }
    // A cloned static cloud extended per frame keeps the static medium.
    let mut base = MediumCloud::new();
    base.add_field([-1.0; 3], [1.0; 3], |p| 1.0 - p[0].abs(), &m, h);
    let mut frame = base.clone();
    let before = frame.len();
    frame.add_field([5.0; 3], [6.0; 3], |_| 1.0, &m, h);
    assert!(frame.len() > before && base.len() == before);
}

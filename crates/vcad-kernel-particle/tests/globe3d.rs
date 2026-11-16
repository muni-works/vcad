//! Validation of the 3D thin-wire field solver ([`bem`]) and the
//! geometry-agnostic tracer on it, against the 2D solver and closed forms.

use vcad_kernel_particle::bem::{launch_fibonacci, solve, BemOptions};
use vcad_kernel_particle::constants::EPSILON_0;
use vcad_kernel_particle::device::Device;
use vcad_kernel_particle::fom;
use vcad_kernel_particle::poisson::{solve_with_chamber, ChamberShape, SolveOptions};
use vcad_kernel_particle::trace::{Fate, TraceOptions, DEUTERON};
use vcad_kernel_particle::wire3d::{Chamber3d, Device3d, GreatCircleLayout};

const K_E: f64 = 1.0 / (4.0 * std::f64::consts::PI * EPSILON_0);

/// The axisymmetric special case: a ring stack in a grounded sphere must
/// give the same potential from the 3D BEM and the 2D SOR solver (with
/// the 2D solver's spherical staircase chamber). The 2D wire mask and
/// staircase wall make the 2D solver first-order in h, so the claim is
/// convergence: halving h shrinks the gap, and the Richardson-extrapolated
/// 2D potential (2·φ_h/2 − φ_h) agrees with 3D to 1% of |V|.
#[test]
fn ring_stack_matches_the_2d_solver() {
    let v = -10_000.0;
    let rings = Device::classic_fusor(60.0, 20.0, 4, 0.75, v);
    let d3 = Device3d::from_rings(&rings, Chamber3d::Sphere { radius_mm: 60.0 });
    let f3 = solve(&d3, &BemOptions::default()).unwrap();
    let coarse = solve_with_chamber(
        &rings,
        ChamberShape::Sphere,
        121,
        241,
        &SolveOptions::default(),
    )
    .unwrap();
    let fine = solve_with_chamber(
        &rings,
        ChamberShape::Sphere,
        241,
        481,
        &SolveOptions::default(),
    )
    .unwrap();
    for &(r_mm, z_mm) in &[
        (0.0, 0.0),
        (5.0, 3.0),
        (10.0, -8.0),
        (0.0, 14.0),
        (15.0, 0.0),
        (26.0, 0.0),
        (30.0, 25.0),
        (0.0, -40.0),
        (45.0, 10.0),
    ] {
        let (r, z) = (r_mm * 1e-3, z_mm * 1e-3);
        let p3 = f3.potential([r, 0.0, z]);
        // And it must be axisymmetric: same value at another azimuth.
        let az = 1.1_f64;
        let p3b = f3.potential([r * az.cos(), r * az.sin(), z]);
        assert!(
            (p3 - p3b).abs() < 1e-3 * v.abs(),
            "not axisymmetric at ({r_mm},{z_mm}): {p3} vs {p3b}"
        );
        let e_coarse = (coarse.potential_at(r, z) - p3) / v.abs();
        let e_fine = (fine.potential_at(r, z) - p3) / v.abs();
        assert!(
            e_fine.abs() <= e_coarse.abs() + 1e-3,
            "2D not converging toward 3D at ({r_mm},{z_mm}): {e_coarse} -> {e_fine}"
        );
        assert!(
            e_fine.abs() < 0.025,
            "({r_mm},{z_mm}): 2D(h=0.25) gap {e_fine}"
        );
        let extrapolated = 2.0 * e_fine - e_coarse;
        assert!(
            extrapolated.abs() < 0.01,
            "({r_mm},{z_mm}): extrapolated 2D vs 3D gap {extrapolated}"
        );
    }
}

/// A point charge in a grounded sphere, built as a tiny wire ring (its
/// image is exact): the far field inside the chamber matches
/// q(1/|x−y| − (R/|y|)/|x−y*|).
#[test]
fn small_ring_in_sphere_matches_the_point_charge_image() {
    let y = [0.0, 0.0, 0.020];
    let mut d = Device3d::from_rings(
        &Device::classic_fusor(60.0, 1.0, 1, 0.1, -100.0),
        Chamber3d::Sphere { radius_mm: 60.0 },
    );
    // Move the ring to z = 20 mm.
    if let vcad_kernel_particle::wire3d::WirePath::Circle { center_mm, .. } = &mut d.wires[0].path {
        *center_mm = [0.0, 0.0, 20.0];
    }
    let f = solve(&d, &BemOptions::default()).unwrap();
    let q = f.total_charge_c();
    let r = 0.060;
    let ystar = [0.0, 0.0, r * r / y[2]];
    let dist = |a: [f64; 3], b: [f64; 3]| {
        ((a[0] - b[0]).powi(2) + (a[1] - b[1]).powi(2) + (a[2] - b[2]).powi(2)).sqrt()
    };
    for x in [
        [0.01, 0.0, -0.02],
        [0.03, 0.02, 0.0],
        [0.0, 0.0, -0.05],
        [0.0, 0.04, 0.03],
    ] {
        let want = K_E * q * (1.0 / dist(x, y) - (r / y[2]) / dist(x, ystar));
        let got = f.potential(x);
        assert!(
            (got - want).abs() < 2e-3 * want.abs(),
            "at {x:?}: got {got}, want {want}"
        );
    }
}

/// A dense globe converges to a conducting sphere: outside the grid the
/// potential approaches the concentric-sphere solution and the charge
/// approaches the concentric capacitor's, monotonically with wire count.
#[test]
fn dense_globe_converges_to_concentric_spheres() {
    let (big_r, r_g, v) = (0.076, 0.019, -1000.0);
    let q_sphere = 4.0 * std::f64::consts::PI * EPSILON_0 * v / (1.0 / r_g - 1.0 / big_r);
    let phi_exact = |r: f64| v * (1.0 / r - 1.0 / big_r) / (1.0 / r_g - 1.0 / big_r);
    let mut prev_err = f64::INFINITY;
    let mut prev_q_err = f64::INFINITY;
    for (n_lat, n_long) in [(3, 4), (7, 10), (11, 16)] {
        let d = Device3d::lat_long_globe(76.0, 19.0, n_lat, n_long, 0.4, v);
        let f = solve(&d, &BemOptions::default()).unwrap();
        let mut err = 0.0_f64;
        for dir in vcad_kernel_particle::wire3d::fibonacci_sphere(40) {
            for r in [0.030, 0.045, 0.065] {
                let p = [dir[0] * r, dir[1] * r, dir[2] * r];
                err = err.max((f.potential(p) - phi_exact(r)).abs() / v.abs());
            }
        }
        let q_err = (f.total_charge_c() - q_sphere).abs() / q_sphere.abs();
        assert!(
            err < prev_err,
            "({n_lat},{n_long}): exterior error {err} did not fall"
        );
        assert!(
            q_err < prev_q_err,
            "({n_lat},{n_long}): charge error {q_err} did not fall"
        );
        prev_err = err;
        prev_q_err = q_err;
    }
    assert!(prev_err < 0.03, "dense globe exterior error {prev_err}");
    assert!(prev_q_err < 0.06, "dense globe charge error {prev_q_err}");
}

fn six_great_circles(v: f64) -> Device3d {
    Device3d::great_circle_globe(
        76.0,
        19.0,
        &GreatCircleLayout::Icosidodecahedral6.normals(),
        1.0,
        v,
    )
}

/// Geometric transparency of the 6-great-circle grid: Fibonacci ray count
/// vs the closed form (6 bands of solid-angle fraction a/R, minus 30
/// rhombic overlaps at 63.4° crossings), and vs ballistic tracing (field
/// negligible next to the launch energy), which exercises the tracer's
/// capsule hit test end to end.
#[test]
fn six_great_circle_geometric_vs_traced_transparency() {
    let (a, r) = (1.0_f64, 19.0_f64);
    let alpha = (a / r).asin();
    let theta = (1.0 / 5f64.sqrt()).acos();
    let blocked =
        6.0 * a / r - 30.0 * (2.0 * alpha).powi(2) / (theta.sin() * 4.0 * std::f64::consts::PI);
    let t_closed = 1.0 - blocked;

    let dev = six_great_circles(-1.0);
    let t_rays = dev.geometric_transparency(20_000);
    assert!(
        (t_rays - t_closed).abs() < 0.01,
        "ray count {t_rays} vs closed form {t_closed}"
    );

    // Ballistic: 1 V grid, 100 keV deuterons fired straight at the center
    // from the launch shell. First-pass survival = geometric transparency.
    let f = solve(&dev, &BemOptions::default()).unwrap();
    let opts = TraceOptions {
        max_passes: 1,
        ..TraceOptions::default()
    };
    let tracer = f.tracer(f.default_step_length_m(), opts);
    let r_shell = tracer.shell_radius_m();
    let speed = (2.0 * 100e3 * DEUTERON.charge_c / DEUTERON.mass_kg).sqrt();
    let dirs = vcad_kernel_particle::wire3d::fibonacci_sphere(1500);
    let survived = dirs
        .iter()
        .filter(|d| {
            let pos = [d[0] * r_shell, d[1] * r_shell, d[2] * r_shell];
            let vel = [-d[0] * speed, -d[1] * speed, -d[2] * speed];
            tracer.trace_from(DEUTERON, pos, vel).fate == Fate::Survived
        })
        .count() as f64
        / dirs.len() as f64;
    assert!(
        (survived - t_rays).abs() < 0.03,
        "ballistic traced transparency {survived} vs geometric {t_rays}"
    );
}

/// Ions in the −30 kV 6-great-circle hobby grid: energy is conserved by
/// the 3D tracer (conservative analytic field), ions recirculate, and the
/// traced per-pass transparency is a sane, reported number (field lensing
/// makes it differ from geometric; see `examples/globe_vs_rings.rs`).
#[test]
fn globe_tracing_conserves_energy_and_recirculates() {
    let f = solve(&six_great_circles(-30_000.0), &BemOptions::default()).unwrap();
    let opts = TraceOptions {
        max_passes: 6,
        ..TraceOptions::default()
    };
    let tracer = f.tracer(f.default_step_length_m(), opts);
    let out = launch_fibonacci(&tracer, DEUTERON, 24);
    let st = fom::stats(&out);
    assert!(st.mean_passes > 1.0, "ions must recirculate: {st:?}");
    assert!(
        st.mean_energy_drift_rel < 0.01,
        "mean energy drift {}",
        st.mean_energy_drift_rel
    );
    assert!(out
        .iter()
        .all(|o| !matches!(o.fate, Fate::Wire(k) if k >= 6)));
}

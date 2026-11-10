//! Shell of a lofted solid: the Mac-app defect where a ruled loft between a
//! 40×30 and a centered 20×15 rectangle, shelled at t = 2 with the top
//! open, evaluated to no geometry. Root cause: the loft's start cap carried
//! a plane whose normal pointed into the solid, so the shell offset the
//! bottom face outward and the fail-closed post-flight rejected the result.

use vcad_kernel::{FaceRef, Solid};
use vcad_kernel_math::{Point3, Vec3};
use vcad_kernel_sketch::SketchProfile;
use vcad_kernel_sweep::LoftOptions;

const SEGMENTS: u32 = 64;
const T: f64 = 2.0;
const H: f64 = 50.0;

fn rect(cx: f64, cy: f64, z: f64, w: f64, h: f64) -> SketchProfile {
    SketchProfile::rectangle(
        Point3::new(cx - w / 2.0, cy - h / 2.0, z),
        Vec3::x(),
        Vec3::y(),
        w,
        h,
    )
}

fn frustum() -> Solid {
    Solid::loft(
        &[
            rect(0.0, 0.0, 0.0, 40.0, 30.0),
            rect(0.0, 0.0, H, 20.0, 15.0),
        ],
        LoftOptions::default(),
    )
    .expect("loft")
}

fn hint(p: [f64; 3], n: [f64; 3]) -> FaceRef {
    FaceRef {
        name: None,
        point: Some(Point3::new(p[0], p[1], p[2])),
        normal: Some(Vec3::new(n[0], n[1], n[2])),
    }
}

/// The face pick the app sent: top face.
fn top() -> FaceRef {
    hint([2.33, -10.0, 50.0], [0.0, 0.0, 1.0])
}

fn bottom() -> FaceRef {
    hint([0.0, 0.0, 0.0], [0.0, 0.0, -1.0])
}

fn open_edges(s: &Solid) -> usize {
    s.to_mesh(SEGMENTS).boundary_edges().len()
}

/// Half-widths of the outer frustum at height z.
fn hx(z: f64) -> f64 {
    20.0 - 10.0 * z / H
}
fn hy(z: f64) -> f64 {
    15.0 - 7.5 * z / H
}

/// Volume of the cavity between `z0` and `z1`: each sloped wall's plane
/// moved inward by `T` along its normal shrinks the half-width by
/// `T·sqrt(1 + slope²)`. The cross-section is a product of two linear
/// functions of z, so Simpson's rule is exact.
fn cavity(z0: f64, z1: f64) -> f64 {
    let kx = T * (1.0 + (10.0 / H).powi(2)).sqrt();
    let ky = T * (1.0 + (7.5 / H).powi(2)).sqrt();
    let a = |z: f64| 4.0 * (hx(z) - kx) * (hy(z) - ky);
    (z1 - z0) / 6.0 * (a(z0) + 4.0 * a(0.5 * (z0 + z1)) + a(z1))
}

const OUTER: f64 = 35_000.0;

fn assert_close(got: f64, want: f64, what: &str) {
    assert!(
        (got - want).abs() < 1e-6 * want,
        "{what}: volume {got} vs closed form {want}"
    );
}

#[test]
fn loft_frustum_volume_is_exact() {
    assert_close(frustum().volume(), OUTER, "frustum");
}

#[test]
fn shell_loft_top_open() {
    let s = frustum()
        .shell_open_refs(T, &[top()])
        .expect("top-open shell of a frustum loft");
    assert_close(s.volume(), OUTER - cavity(T, H), "top-open shell");
    assert_eq!(open_edges(&s), 0, "top-open shell must be watertight");
}

#[test]
fn shell_loft_closed() {
    let s = frustum().shell(T).expect("closed shell of a frustum loft");
    assert_close(s.volume(), OUTER - cavity(T, H - T), "closed shell");
    assert_eq!(open_edges(&s), 0, "closed shell must be watertight");
}

#[test]
fn shell_loft_duct_both_ends_open() {
    let s = frustum()
        .shell_open_refs(T, &[top(), bottom()])
        .expect("duct: both ends open");
    assert_close(s.volume(), OUTER - cavity(0.0, H), "duct");
    assert_eq!(open_edges(&s), 0, "duct must be watertight");
}

/// Rectangle to circle: the ruled side faces are twisted (non-planar), so
/// this is about not crashing and staying sane, not an exact number.
#[test]
fn loft_rect_to_circle_and_shell_stay_sane() {
    let r = rect(0.0, 0.0, 0.0, 40.0, 30.0);
    let c = SketchProfile::circle(Point3::new(0.0, 0.0, H), Vec3::z(), 10.0, 4);
    let l = Solid::loft(&[r, c], LoftOptions::default()).expect("rect→circle loft");
    let v = l.volume();
    // Between the inscribed square of the circle (200 mm² top) and the
    // full 1200 mm² base, bounded by the prisms on either end.
    assert!(
        v > 200.0 * H && v < 1200.0 * H,
        "rect→circle loft volume {v} is not sane"
    );
    assert_eq!(open_edges(&l), 0, "rect→circle loft must be watertight");

    // Shelling may fail closed on the twisted faces, but must never
    // produce a bigger or inverted body.
    for res in [
        l.shell(T),
        l.shell_open_refs(T, &[hint([0.0, 0.0, H], [0.0, 0.0, 1.0])]),
    ] {
        if let Ok(s) = res {
            let sv = s.volume();
            assert!(sv > 0.0 && sv < v, "shelled volume {sv} vs loft {v}");
        }
    }
}

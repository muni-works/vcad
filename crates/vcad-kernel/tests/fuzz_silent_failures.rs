//! Regressions from fuzzing the Mac app: three operations that reported
//! success while producing wrong or empty geometry.

use vcad_kernel::{BlendError, Solid};

fn cube() -> Solid {
    Solid::cube(10.0, 10.0, 10.0)
}

/// `[shell 20 [cube 10 10 10]]` used to offset OUTWARD: 28000 mm³ with a
/// (-10..20) bbox. A thickness that consumes the solid is an error.
#[test]
fn overshell_is_an_error_not_a_bigger_solid() {
    let err = cube()
        .shell(20.0)
        .expect_err("shell 20 of a 10 cube must fail");
    assert!(matches!(err, BlendError::Shell(_)), "{err}");
    assert!(err.to_string().contains("consumes the solid"), "{err}");
}

#[test]
fn shell_boundary_on_a_10mm_cube() {
    let ok = cube().shell(4.9).expect("4.9 leaves a 0.2 mm cavity");
    let (lo, hi) = ok.try_bounding_box().unwrap();
    assert!(lo.iter().all(|c| c.abs() < 1e-6), "{lo:?}");
    assert!(hi.iter().all(|c| (c - 10.0).abs() < 1e-6), "{hi:?}");
    assert!((ok.volume() - (1000.0 - 0.008)).abs() < 1e-2);
    for t in [5.0, 5.1, 7.5] {
        assert!(cube().shell(t).is_err(), "shell {t} must be refused");
    }
    // The shipped-correct case stays correct.
    assert!((cube().shell(1.0).unwrap().volume() - 488.0).abs() < 1e-2);
}

#[test]
fn shell_thickness_must_be_positive_and_finite() {
    for t in [0.0, -1.0, f64::NAN, f64::INFINITY] {
        assert!(cube().shell(t).is_err(), "shell {t} must be refused");
    }
    // Curved solids take the same gates (cylinder r=5: t>=5 consumes it).
    let cyl = Solid::cylinder(5.0, 20.0, 32);
    assert!(cyl.shell(1.0).is_ok());
    assert!(cyl.shell(6.0).is_err());
}

/// `[fillet 20 [cube 10 10 10]]` must be an error with a reason (the app
/// used to show the part as "no geometry" and report success).
#[test]
fn overfillet_and_overchamfer_are_errors_with_reasons() {
    let err = cube().fillet(20.0).unwrap_err();
    assert!(err.to_string().contains("radius"), "{err}");
    let err = cube().chamfer(20.0).unwrap_err();
    assert!(!err.to_string().is_empty());
}

#[test]
fn fillet_and_chamfer_boundary_on_a_10mm_cube() {
    assert!(cube().fillet(4.9).is_ok(), "just under the max radius");
    assert!(cube().fillet(5.1).is_err(), "just over the max radius");
    // Chamfer's post-flight gate (output >= half the input volume) binds
    // near d≈3.5 on a cube; pin a working distance and an over-limit one.
    assert!(cube().chamfer(3.0).is_ok());
    assert!(cube().chamfer(5.1).is_err());
    for v in [0.0, -1.0, f64::NAN] {
        assert!(matches!(cube().fillet(v), Err(BlendError::InvalidSize(_))));
        assert!(matches!(cube().chamfer(v), Err(BlendError::InvalidSize(_))));
    }
}

/// loon `[difference [cube 10 10 10] [cube 10 10 5]]` is B − A = empty; its
/// bbox used to print as the ±f64::MAX sentinel.
#[test]
fn empty_boolean_has_no_bounding_box() {
    let empty = Solid::cube(10.0, 10.0, 5.0).difference(&cube());
    assert!(empty.volume().abs() < 1e-6);
    assert_eq!(empty.try_bounding_box(), None);
    assert_eq!(Solid::empty().try_bounding_box(), None);
    assert!(cube().try_bounding_box().is_some());
}

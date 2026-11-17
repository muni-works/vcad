//! URDF `rpy` ↔ `CsgOp::Rotate` angle conversion.
//!
//! The two conventions name the same three angles but compose them in
//! opposite orders:
//!
//! - URDF `rpy` = (roll, pitch, yaw) is **R = Rz(yaw)·Ry(pitch)·Rx(roll)**:
//!   roll acts on the part first.
//! - `CsgOp::Rotate { angles }` is **R = Rx·Ry·Rz**: Z acts first
//!   (`Transform::then` right-multiplies, and the evaluator composes
//!   `rx.then(ry).then(rz)`).
//!
//! They agree whenever at most one angle is nonzero, which is why copying
//! `rpy` straight into `Rotate` looked right for most links. With two or more
//! nonzero angles the copy places the visual wrongly, so the reader and writer
//! go through the rotation matrix instead.

/// Row-major 3×3 rotation matrix.
type Mat3 = [[f64; 3]; 3];

fn mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut m = [[0.0; 3]; 3];
    for (i, row) in m.iter_mut().enumerate() {
        for (j, v) in row.iter_mut().enumerate() {
            *v = (0..3).map(|k| a[i][k] * b[k][j]).sum();
        }
    }
    m
}

fn rx(a: f64) -> Mat3 {
    let (s, c) = a.sin_cos();
    [[1.0, 0.0, 0.0], [0.0, c, -s], [0.0, s, c]]
}

fn ry(a: f64) -> Mat3 {
    let (s, c) = a.sin_cos();
    [[c, 0.0, s], [0.0, 1.0, 0.0], [-s, 0.0, c]]
}

fn rz(a: f64) -> Mat3 {
    let (s, c) = a.sin_cos();
    [[c, -s, 0.0], [s, c, 0.0], [0.0, 0.0, 1.0]]
}

/// The matrix a `CsgOp::Rotate` with `angles` (radians) applies: Rx·Ry·Rz.
fn rotate_matrix(angles: [f64; 3]) -> Mat3 {
    mul(&mul(&rx(angles[0]), &ry(angles[1])), &rz(angles[2]))
}

/// The matrix a URDF `rpy` (radians) describes: Rz(yaw)·Ry(pitch)·Rx(roll).
fn rpy_matrix(rpy: [f64; 3]) -> Mat3 {
    mul(&mul(&rz(rpy[2]), &ry(rpy[1])), &rx(rpy[0]))
}

/// Decompose `m` as Rx(a)·Ry(b)·Rz(c), returning `[a, b, c]` in radians.
///
/// Row 0 is `[cb·cc, −cb·sc, sb]` and column 2 is `[sb, −sa·cb, ca·cb]`. At
/// gimbal lock (|sb| = 1) only a ± c is determined; c is folded into a.
fn rotate_angles_of(m: &Mat3) -> [f64; 3] {
    let sb = m[0][2].clamp(-1.0, 1.0);
    if sb.abs() < 1.0 - 1e-12 {
        [
            (-m[1][2]).atan2(m[2][2]),
            sb.asin(),
            (-m[0][1]).atan2(m[0][0]),
        ]
    } else {
        // c = 0: row 1 becomes [sa·sb, ca, 0].
        [
            (sb * m[1][0]).atan2(m[1][1]),
            sb.signum() * std::f64::consts::FRAC_PI_2,
            0.0,
        ]
    }
}

/// Decompose `m` as Rz(yaw)·Ry(pitch)·Rx(roll), returning `[roll, pitch, yaw]`.
///
/// Row 2 is `[−sp, sr·cp, cr·cp]` and column 0 is `[cy·cp, sy·cp, −sp]`. At
/// gimbal lock (|sp| = 1) yaw is folded into roll.
fn rpy_of(m: &Mat3) -> [f64; 3] {
    let sp = (-m[2][0]).clamp(-1.0, 1.0);
    if sp.abs() < 1.0 - 1e-12 {
        [m[2][1].atan2(m[2][2]), sp.asin(), m[1][0].atan2(m[0][0])]
    } else {
        // yaw = 0: row 0 becomes [0, sr·sp, cr·sp], row 1 [0, cr, −sr].
        [
            (-m[1][2]).atan2(m[1][1]),
            sp.signum() * std::f64::consts::FRAC_PI_2,
            0.0,
        ]
    }
}

/// URDF `rpy` (radians) → `CsgOp::Rotate` angles (radians) with the same
/// rotation.
pub(crate) fn rpy_to_rotate(rpy: [f64; 3]) -> [f64; 3] {
    rotate_angles_of(&rpy_matrix(rpy))
}

/// `CsgOp::Rotate` angles (radians) → URDF `rpy` (radians) with the same
/// rotation.
pub(crate) fn rotate_to_rpy(angles: [f64; 3]) -> [f64; 3] {
    rpy_of(&rotate_matrix(angles))
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::f64::consts::FRAC_PI_2;

    fn assert_same(a: &Mat3, b: &Mat3, what: &str) {
        for i in 0..3 {
            for j in 0..3 {
                assert!((a[i][j] - b[i][j]).abs() < 1e-9, "{what}: {a:?} vs {b:?}");
            }
        }
    }

    /// A small deterministic LCG: no `rand` dependency for a unit test.
    fn angles(n: usize) -> Vec<[f64; 3]> {
        let mut s: u64 = 0x9E37_79B9_7F4A_7C15;
        let mut next = || {
            s = s
                .wrapping_mul(6364136223846793005)
                .wrapping_add(1442695040888963407);
            ((s >> 11) as f64 / (1u64 << 53) as f64 - 0.5) * 2.0 * std::f64::consts::PI
        };
        let mut out: Vec<[f64; 3]> = (0..n).map(|_| [next(), next(), next()]).collect();
        // Gimbal lock is common in real URDFs (pitch = ±90°).
        for p in [FRAC_PI_2, -FRAC_PI_2] {
            out.push([0.3, p, -1.1]);
            out.push([0.0, p, 0.0]);
            out.push([-2.0, p, 2.5]);
        }
        out.push([0.0; 3]);
        out
    }

    #[test]
    fn rpy_to_rotate_preserves_the_rotation() {
        for rpy in angles(2000) {
            let got = rotate_matrix(rpy_to_rotate(rpy));
            assert_same(&got, &rpy_matrix(rpy), &format!("rpy {rpy:?}"));
        }
    }

    #[test]
    fn rotate_to_rpy_preserves_the_rotation() {
        for a in angles(2000) {
            let got = rpy_matrix(rotate_to_rpy(a));
            assert_same(&got, &rotate_matrix(a), &format!("angles {a:?}"));
        }
    }

    #[test]
    fn single_axis_angles_pass_through() {
        for (i, v) in [0.7, -1.2, 2.9].into_iter().enumerate() {
            let mut a = [0.0; 3];
            a[i] = v;
            let b = rpy_to_rotate(a);
            for k in 0..3 {
                assert!((a[k] - b[k]).abs() < 1e-12, "{a:?} → {b:?}");
            }
        }
    }

    #[test]
    fn rotate_matrix_matches_the_kernel_transform() {
        // Pin this module's Rx·Ry·Rz to what the evaluator actually builds.
        use vcad_kernel::vcad_kernel_math::{Transform, Vec3};
        let a = [0.4, -0.9, 1.7];
        let t = Transform::rotation_x(a[0])
            .then(&Transform::rotation_y(a[1]))
            .then(&Transform::rotation_z(a[2]));
        let m = rotate_matrix(a);
        for j in 0..3 {
            let mut e = [0.0; 3];
            e[j] = 1.0;
            let c = t.apply_vec(&Vec3::new(e[0], e[1], e[2]));
            for (i, v) in [c.x, c.y, c.z].into_iter().enumerate() {
                assert!((m[i][j] - v).abs() < 1e-12, "column {j}: {m:?}");
            }
        }
    }
}

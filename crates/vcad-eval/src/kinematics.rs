//! Forward kinematics solver for assembly joints.
//!
//! Ported from `packages/engine/src/kinematics.ts`.

use std::collections::{HashMap, HashSet, VecDeque};
use vcad_ir::{Document, Joint, JointKind, Transform3D, Vec3};

/// Create an identity transform.
fn identity() -> Transform3D {
    Transform3D::identity()
}

/// Degrees to radians.
fn deg_to_rad(deg: f64) -> f64 {
    deg * std::f64::consts::PI / 180.0
}

/// 3x3 rotation matrix stored row-major.
type Mat3 = [[f64; 3]; 3];

/// Euler angles (XYZ order, degrees) to rotation matrix.
fn euler_to_matrix(angles: &Vec3) -> Mat3 {
    let rx = deg_to_rad(angles.x);
    let ry = deg_to_rad(angles.y);
    let rz = deg_to_rad(angles.z);

    let (cx, sx) = (rx.cos(), rx.sin());
    let (cy, sy) = (ry.cos(), ry.sin());
    let (cz, sz) = (rz.cos(), rz.sin());

    // Rz * Ry * Rx
    [
        [cy * cz, sx * sy * cz - cx * sz, cx * sy * cz + sx * sz],
        [cy * sz, sx * sy * sz + cx * cz, cx * sy * sz - sx * cz],
        [-sy, sx * cy, cx * cy],
    ]
}

/// Multiply 3x3 matrix by Vec3.
fn mat_vec3(m: &Mat3, v: &Vec3) -> Vec3 {
    Vec3::new(
        m[0][0] * v.x + m[0][1] * v.y + m[0][2] * v.z,
        m[1][0] * v.x + m[1][1] * v.y + m[1][2] * v.z,
        m[2][0] * v.x + m[2][1] * v.y + m[2][2] * v.z,
    )
}

/// Multiply two 3x3 matrices.
fn mat_mul(a: &Mat3, b: &Mat3) -> Mat3 {
    let mut r = [[0.0; 3]; 3];
    for i in 0..3 {
        for j in 0..3 {
            r[i][j] = a[i][0] * b[0][j] + a[i][1] * b[1][j] + a[i][2] * b[2][j];
        }
    }
    r
}

/// Extract Euler angles (degrees) from rotation matrix.
fn matrix_to_euler(m: &Mat3) -> Vec3 {
    let sy = -m[2][0];
    let cy = (m[0][0] * m[0][0] + m[1][0] * m[1][0]).sqrt();

    if cy > 1e-6 {
        Vec3::new(
            m[2][1].atan2(m[2][2]) * 180.0 / std::f64::consts::PI,
            sy.atan2(cy) * 180.0 / std::f64::consts::PI,
            m[1][0].atan2(m[0][0]) * 180.0 / std::f64::consts::PI,
        )
    } else {
        // Gimbal lock
        Vec3::new(
            (-m[1][2]).atan2(m[1][1]) * 180.0 / std::f64::consts::PI,
            sy.atan2(cy) * 180.0 / std::f64::consts::PI,
            0.0,
        )
    }
}

fn vec3_add(a: &Vec3, b: &Vec3) -> Vec3 {
    Vec3::new(a.x + b.x, a.y + b.y, a.z + b.z)
}

fn vec3_sub(a: &Vec3, b: &Vec3) -> Vec3 {
    Vec3::new(a.x - b.x, a.y - b.y, a.z - b.z)
}

fn vec3_scale(v: &Vec3, s: f64) -> Vec3 {
    Vec3::new(v.x * s, v.y * s, v.z * s)
}

fn vec3_normalize(v: &Vec3) -> Vec3 {
    let len = (v.x * v.x + v.y * v.y + v.z * v.z).sqrt();
    if len < 1e-10 {
        Vec3::new(0.0, 0.0, 1.0)
    } else {
        vec3_scale(v, 1.0 / len)
    }
}

/// Rotation matrix from axis-angle (axis normalized, angle in degrees).
fn axis_angle_to_matrix(axis: &Vec3, angle_deg: f64) -> Mat3 {
    let angle = deg_to_rad(angle_deg);
    let c = angle.cos();
    let s = angle.sin();
    let t = 1.0 - c;
    let (x, y, z) = (axis.x, axis.y, axis.z);

    [
        [t * x * x + c, t * x * y - s * z, t * x * z + s * y],
        [t * x * y + s * z, t * y * y + c, t * y * z - s * x],
        [t * x * z - s * y, t * y * z + s * x, t * z * z + c],
    ]
}

/// Compose two transforms: result = outer * inner.
fn compose_transforms(outer: &Transform3D, inner: &Transform3D) -> Transform3D {
    let scale = Vec3::new(
        outer.scale.x * inner.scale.x,
        outer.scale.y * inner.scale.y,
        outer.scale.z * inner.scale.z,
    );

    let outer_rot = euler_to_matrix(&outer.rotation);
    let inner_rot = euler_to_matrix(&inner.rotation);
    let composed_rot = mat_mul(&outer_rot, &inner_rot);
    let rotation = matrix_to_euler(&composed_rot);

    let scaled_inner_trans = Vec3::new(
        outer.scale.x * inner.translation.x,
        outer.scale.y * inner.translation.y,
        outer.scale.z * inner.translation.z,
    );
    let rotated_inner_trans = mat_vec3(&outer_rot, &scaled_inner_trans);
    let translation = vec3_add(&outer.translation, &rotated_inner_trans);

    Transform3D {
        translation,
        rotation,
        scale,
    }
}

/// Apply joint couplings: every coupled joint's state becomes
/// `ratio · driver + offset`, resolving chains (gear trains) in dependency
/// order. A joint whose driver is missing or that sits on a coupling cycle
/// keeps its authored state — fail closed to the document, never to zero.
pub fn resolve_coupled_states(joints: &[Joint]) -> Vec<Joint> {
    let mut out = joints.to_vec();
    if !joints.iter().any(|j| j.coupling.is_some()) {
        return out;
    }
    let index: HashMap<&str, usize> = joints
        .iter()
        .enumerate()
        .map(|(i, j)| (j.id.as_str(), i))
        .collect();
    // 0 = unresolved, 1 = on the stack, 2 = done.
    let mut mark = vec![0u8; joints.len()];
    fn visit(
        i: usize,
        joints: &[Joint],
        index: &HashMap<&str, usize>,
        mark: &mut [u8],
        out: &mut [Joint],
    ) -> bool {
        match mark[i] {
            2 => return true,
            1 => return false, // cycle
            _ => {}
        }
        let Some(c) = &joints[i].coupling else {
            mark[i] = 2;
            return true;
        };
        let Some(&d) = index.get(c.driver.as_str()) else {
            mark[i] = 2;
            return true;
        };
        mark[i] = 1;
        let ok = d != i && visit(d, joints, index, mark, out);
        if ok {
            out[i].state = c.ratio * out[d].state + c.offset;
        }
        mark[i] = 2;
        ok
    }
    for i in 0..joints.len() {
        visit(i, joints, &index, &mut mark, &mut out);
    }
    out
}

/// Compute the transform induced by a joint at its current state.
fn compute_joint_transform(joint: &Joint) -> Transform3D {
    match &joint.kind {
        JointKind::Fixed => Transform3D {
            translation: vec3_sub(&joint.parent_anchor, &joint.child_anchor),
            rotation: Vec3::new(0.0, 0.0, 0.0),
            scale: Vec3::new(1.0, 1.0, 1.0),
        },

        JointKind::Revolute { axis, .. } => {
            let axis = vec3_normalize(axis);
            let rot_matrix = axis_angle_to_matrix(&axis, joint.state);
            let rotation = matrix_to_euler(&rot_matrix);
            let rotated_child = mat_vec3(&rot_matrix, &joint.child_anchor);
            let translation = vec3_sub(&joint.parent_anchor, &rotated_child);
            Transform3D {
                translation,
                rotation,
                scale: Vec3::new(1.0, 1.0, 1.0),
            }
        }

        JointKind::Slider { axis, .. } => {
            let axis = vec3_normalize(axis);
            let slide_offset = vec3_scale(&axis, joint.state);
            Transform3D {
                translation: vec3_add(
                    &vec3_sub(&joint.parent_anchor, &joint.child_anchor),
                    &slide_offset,
                ),
                rotation: Vec3::new(0.0, 0.0, 0.0),
                scale: Vec3::new(1.0, 1.0, 1.0),
            }
        }

        JointKind::Cylindrical { axis } => {
            let axis = vec3_normalize(axis);
            let rot_matrix = axis_angle_to_matrix(&axis, joint.state);
            let rotation = matrix_to_euler(&rot_matrix);
            let rotated_child = mat_vec3(&rot_matrix, &joint.child_anchor);
            let translation = vec3_sub(&joint.parent_anchor, &rotated_child);
            Transform3D {
                translation,
                rotation,
                scale: Vec3::new(1.0, 1.0, 1.0),
            }
        }

        // A Free (6-DOF floating) joint has no meaningful scalar state; the
        // document FK renders it at its zero pose — child placed at the
        // parent anchor with identity rotation, exactly like Fixed. The
        // dynamic pose lives in the physics state, not the document.
        JointKind::Free => Transform3D {
            translation: vec3_sub(&joint.parent_anchor, &joint.child_anchor),
            rotation: Vec3::new(0.0, 0.0, 0.0),
            scale: Vec3::new(1.0, 1.0, 1.0),
        },

        JointKind::Ball => {
            let z_axis = Vec3::new(0.0, 0.0, 1.0);
            let rot_matrix = axis_angle_to_matrix(&z_axis, joint.state);
            let rotation = matrix_to_euler(&rot_matrix);
            let rotated_child = mat_vec3(&rot_matrix, &joint.child_anchor);
            let translation = vec3_sub(&joint.parent_anchor, &rotated_child);
            Transform3D {
                translation,
                rotation,
                scale: Vec3::new(1.0, 1.0, 1.0),
            }
        }
    }
}

/// Solve forward kinematics for an assembly document.
///
/// Starting from root instances (not children of any joint), traverses the
/// joint tree via BFS and computes world transforms for each instance.
///
/// Contract: a joint fully determines its child's pose. For a jointed child,
/// `world = parent_world · joint_transform(anchors, state)` — the instance's
/// own `transform` is ignored (it applies only to instances that are not the
/// child of any joint). Composing both would double-apply the placement
/// whenever an author sets the instance transform to the joint-anchor
/// location, which is the natural way to author an assembly.
pub fn solve_forward_kinematics(doc: &Document) -> HashMap<String, Transform3D> {
    let instances = match &doc.instances {
        Some(i) if !i.is_empty() => i,
        _ => return HashMap::new(),
    };
    let mut joints = resolve_coupled_states(doc.joints.as_deref().unwrap_or(&[]));
    close_loops(instances, &mut joints);
    forward_tree(instances, &joints)
}

/// Solve every passive joint so each loop-closure joint's two anchors meet.
///
/// Returns the final closure residual (mm, RMS over closures; 0 when there is
/// nothing to close). Damped Gauss-Newton on the passive tree joints' states,
/// warm-started from their current values, with a finite-difference Jacobian:
/// a loop is a handful of scalars and each residual is one cheap tree walk.
/// An unreachable pose (a linkage stretched past its limit) leaves the best
/// least-squares pose and reports the residual rather than snapping anywhere.
pub fn close_loops(instances: &[vcad_ir::Instance], joints: &mut [Joint]) -> f64 {
    let mut seen: HashSet<&str> = HashSet::new();
    let mut closures: Vec<usize> = Vec::new();
    for (i, j) in joints.iter().enumerate() {
        if !seen.insert(j.child_instance_id.as_str()) {
            closures.push(i);
        }
    }
    if closures.is_empty() {
        return 0.0;
    }
    // Unknowns: passive tree joints (closure joints are constraints, coupled
    // joints are derived from their drivers).
    let unknowns: Vec<usize> = joints
        .iter()
        .enumerate()
        .filter(|(i, j)| j.passive && j.coupling.is_none() && !closures.contains(i))
        .map(|(i, _)| i)
        .collect();

    let residual = |joints: &[Joint]| -> Vec<f64> {
        let world = forward_tree(instances, joints);
        let mut r = Vec::with_capacity(closures.len() * 3);
        for &c in &closures {
            let j = &joints[c];
            let pw = j
                .parent_instance_id
                .as_ref()
                .and_then(|p| world.get(p))
                .cloned()
                .unwrap_or_else(identity);
            let cw = world
                .get(&j.child_instance_id)
                .cloned()
                .unwrap_or_else(identity);
            let a = transform_point(&pw, &j.parent_anchor);
            let b = transform_point(&cw, &j.child_anchor);
            r.extend([a.x - b.x, a.y - b.y, a.z - b.z]);
        }
        r
    };
    let norm = |r: &[f64]| (r.iter().map(|v| v * v).sum::<f64>() / (r.len().max(1) as f64)).sqrt();

    let mut r = residual(joints);
    if unknowns.is_empty() {
        return norm(&r);
    }
    let n = unknowns.len();
    let mut lambda = 1e-3;
    for _ in 0..60 {
        let e = norm(&r);
        if e < 1e-7 {
            break;
        }
        // Finite-difference Jacobian (states in degrees / mm).
        let h = 1e-4;
        let mut jac = vec![vec![0.0; n]; r.len()];
        for (col, &ui) in unknowns.iter().enumerate() {
            joints[ui].state += h;
            let rp = residual(joints);
            joints[ui].state -= h;
            for row in 0..r.len() {
                jac[row][col] = (rp[row] - r[row]) / h;
            }
        }
        // (JᵀJ + λ·diag) Δ = −Jᵀr, accept only improving steps (LM).
        let mut jtj = vec![vec![0.0; n]; n];
        let mut jtr = vec![0.0; n];
        for row in 0..r.len() {
            for a in 0..n {
                jtr[a] += jac[row][a] * r[row];
                for b in 0..n {
                    jtj[a][b] += jac[row][a] * jac[row][b];
                }
            }
        }
        let mut improved = false;
        for _ in 0..8 {
            let mut m = jtj.clone();
            for (a, row) in m.iter_mut().enumerate() {
                row[a] += lambda * (1.0 + jtj[a][a]);
            }
            let Some(delta) = solve_dense(m, jtr.iter().map(|v| -v).collect()) else {
                lambda *= 10.0;
                continue;
            };
            let saved: Vec<f64> = unknowns.iter().map(|&u| joints[u].state).collect();
            for (k, &u) in unknowns.iter().enumerate() {
                joints[u].state += delta[k];
            }
            let rn = residual(joints);
            if norm(&rn) < e {
                r = rn;
                lambda = (lambda * 0.3).max(1e-9);
                improved = true;
                break;
            }
            for (k, &u) in unknowns.iter().enumerate() {
                joints[u].state = saved[k];
            }
            lambda *= 10.0;
        }
        if !improved {
            break;
        }
    }
    norm(&r)
}

/// Gaussian elimination with partial pivoting; `None` when singular.
fn solve_dense(mut a: Vec<Vec<f64>>, mut b: Vec<f64>) -> Option<Vec<f64>> {
    let n = b.len();
    for col in 0..n {
        let piv = (col..n).max_by(|&i, &j| a[i][col].abs().total_cmp(&a[j][col].abs()))?;
        if a[piv][col].abs() < 1e-14 {
            return None;
        }
        a.swap(col, piv);
        b.swap(col, piv);
        let pivot_row = a[col].clone();
        for row in col + 1..n {
            let f = a[row][col] / pivot_row[col];
            for (v, p) in a[row].iter_mut().zip(&pivot_row).skip(col) {
                *v -= f * p;
            }
            b[row] -= f * b[col];
        }
    }
    let mut x = vec![0.0; n];
    for row in (0..n).rev() {
        let s: f64 = (row + 1..n).map(|k| a[row][k] * x[k]).sum();
        x[row] = (b[row] - s) / a[row][row];
    }
    Some(x)
}

/// Apply a transform (scale, then rotation, then translation) to a point.
fn transform_point(t: &Transform3D, p: &Vec3) -> Vec3 {
    let r = euler_to_matrix(&t.rotation);
    let scaled = Vec3::new(p.x * t.scale.x, p.y * t.scale.y, p.z * t.scale.z);
    vec3_add(&t.translation, &mat_vec3(&r, &scaled))
}

/// Walk the joint tree (first joint per child) from the ground and roots.
fn forward_tree(instances: &[vcad_ir::Instance], joints: &[Joint]) -> HashMap<String, Transform3D> {
    let mut results = HashMap::new();

    // Build child → (joint, parent_id) map
    let mut joint_tree: HashMap<&str, (&Joint, Option<&str>)> = HashMap::new();
    let mut children_by_parent: HashMap<Option<&str>, Vec<&str>> = HashMap::new();
    children_by_parent.insert(None, Vec::new());

    for joint in joints {
        let parent_id = joint.parent_instance_id.as_deref();
        // The first joint on a child places it; any later one is a loop
        // closure, solved by `close_loops` rather than walked.
        if joint_tree.contains_key(joint.child_instance_id.as_str()) {
            continue;
        }
        joint_tree.insert(&joint.child_instance_id, (joint, parent_id));
        children_by_parent
            .entry(parent_id)
            .or_default()
            .push(&joint.child_instance_id);
    }

    // Find root instances (not a child of any joint)
    let child_ids: HashSet<&str> = joints
        .iter()
        .map(|j| j.child_instance_id.as_str())
        .collect();
    let root_instances: Vec<_> = instances
        .iter()
        .filter(|i| !child_ids.contains(i.id.as_str()))
        .collect();

    // Initialize root instances with their base transforms
    for inst in &root_instances {
        results.insert(inst.id.clone(), inst.transform.unwrap_or_else(identity));
    }

    // BFS from null (ground) and root instances
    let mut queue: VecDeque<Option<&str>> = VecDeque::new();
    queue.push_back(None);
    for inst in &root_instances {
        queue.push_back(Some(&inst.id));
    }

    let mut visited: HashSet<Option<&str>> = HashSet::new();
    visited.insert(None);

    let instance_by_id: HashMap<&str, _> = instances.iter().map(|i| (i.id.as_str(), i)).collect();

    while let Some(parent_id) = queue.pop_front() {
        let children = children_by_parent
            .get(&parent_id)
            .cloned()
            .unwrap_or_default();

        for child_id in children {
            if visited.contains(&Some(child_id)) {
                continue;
            }
            visited.insert(Some(child_id));

            let entry = match joint_tree.get(child_id) {
                Some(e) => e,
                None => continue,
            };
            if !instance_by_id.contains_key(child_id) {
                continue;
            }

            let parent_world = parent_id
                .and_then(|pid| results.get(pid))
                .cloned()
                .unwrap_or_else(identity);

            let joint_transform = compute_joint_transform(entry.0);
            let world = compose_transforms(&parent_world, &joint_transform);

            results.insert(child_id.to_string(), world);
            queue.push_back(Some(child_id));
        }
    }

    results
}

#[cfg(test)]
mod tests {
    use super::*;

    /// Jansen's linkage, solved independently by circle intersection.
    fn jansen_f(theta: f64) -> (f64, f64) {
        let (a, b, c, d, e, f, g, j, k, l, m) = (
            38.0, 41.5, 39.3, 40.1, 55.8, 39.4, 36.7, 50.0, 61.9, 7.8, 15.0,
        );
        let cc = |p: (f64, f64), r1: f64, q: (f64, f64), r2: f64, s: f64| {
            let (dx, dy) = (q.0 - p.0, q.1 - p.1);
            let dd = (dx * dx + dy * dy).sqrt();
            let aa = (r1 * r1 - r2 * r2 + dd * dd) / (2.0 * dd);
            let hh = (r1 * r1 - aa * aa).max(0.0).sqrt();
            let (mx, my) = (p.0 + aa * dx / dd, p.1 + aa * dy / dd);
            (mx + s * hh * dy / dd, my - s * hh * dx / dd)
        };
        let bb = (-a, -l);
        let aa = (m * theta.cos(), m * theta.sin());
        let cpt = cc(aa, j, bb, b, 1.0);
        let dpt = cc(aa, k, bb, c, -1.0);
        let ept = cc(bb, d, cpt, e, -1.0);
        cc(ept, f, dpt, g, 1.0)
    }

    #[test]
    fn strandbeest_loops_close_and_match_circle_intersection() {
        let path = concat!(
            env!("CARGO_MANIFEST_DIR"),
            "/../../examples/strandbeest.vcad"
        );
        let mut doc: Document =
            serde_json::from_str(&std::fs::read_to_string(path).unwrap()).unwrap();
        let instances = doc.instances.clone().unwrap();
        for step in 0..=24 {
            let deg = step as f64 * 15.0;
            {
                let joints = doc.joints.as_mut().unwrap();
                joints
                    .iter_mut()
                    .find(|j| j.id == "crank_axle")
                    .unwrap()
                    .state = deg;
                // Warm start from the previous pose, like a drag does.
                let res = close_loops(&instances, joints);
                assert!(res < 1e-5, "loops open by {res} mm at crank {deg}°");
            }
            let world = solve_forward_kinematics(&doc);
            // The frame stands the linkage up (Rx 90°: local y -> world z)
            // and lifts it; undo that to compare in the drawing plane.
            let lift = instances
                .iter()
                .find(|i| i.id == "frame_i")
                .and_then(|i| i.transform)
                .map_or(0.0, |t| t.translation.z);
            let foot = &world["dfg_0_i"].translation;
            let (fx, fy) = jansen_f(deg.to_radians());
            assert!(
                (foot.x - fx).abs() < 0.05
                    && (foot.z - lift - fy).abs() < 0.05
                    && foot.y.abs() < 0.05,
                "crank {deg}°: F at ({:.3},{:.3}), circle intersection says ({fx:.3},{fy:.3})",
                foot.x,
                foot.z - lift
            );
        }
    }

    #[test]
    fn a_four_bar_closes_from_its_crank_alone() {
        // Ground 0..60 on x, crank 20 at origin, coupler 55, rocker 40 at (60,0).
        let rev = |id: &str,
                   parent: Option<&str>,
                   child: &str,
                   pa: (f64, f64),
                   ca: (f64, f64),
                   state: f64,
                   passive: bool| Joint {
            id: id.into(),
            name: None,
            parent_instance_id: parent.map(Into::into),
            child_instance_id: child.into(),
            parent_anchor: Vec3::new(pa.0, pa.1, 0.0),
            child_anchor: Vec3::new(ca.0, ca.1, 0.0),
            kind: JointKind::Revolute {
                axis: Vec3::new(0.0, 0.0, 1.0),
                limits: None,
                effort_limit: None,
                velocity_limit: None,
            },
            state,
            coupling: None,
            passive,
        };
        let inst = |id: &str| vcad_ir::Instance {
            id: id.into(),
            part_def_id: "p".into(),
            name: None,
            tags: vec![],
            transform: None,
            material: None,
            explode: None,
        };
        let instances = vec![inst("crank"), inst("coupler"), inst("rocker")];
        let mut joints = vec![
            rev("crank", None, "crank", (0.0, 0.0), (0.0, 0.0), 40.0, false),
            rev(
                "coupler",
                Some("crank"),
                "coupler",
                (20.0, 0.0),
                (0.0, 0.0),
                -30.0,
                true,
            ),
            rev(
                "rocker",
                None,
                "rocker",
                (60.0, 0.0),
                (0.0, 0.0),
                80.0,
                true,
            ),
            rev(
                "close",
                Some("coupler"),
                "rocker",
                (55.0, 0.0),
                (40.0, 0.0),
                0.0,
                true,
            ),
        ];
        let res = close_loops(&instances, &mut joints);
        assert!(res < 1e-6, "four-bar residual {res}");
        assert_eq!(joints[0].state, 40.0, "the driver is never moved");
    }

    fn coupled_joint(id: &str, driver: Option<(&str, f64, f64)>, state: f64) -> Joint {
        Joint {
            id: id.into(),
            name: None,
            parent_instance_id: None,
            child_instance_id: format!("{id}_inst"),
            parent_anchor: Vec3::new(0.0, 0.0, 0.0),
            child_anchor: Vec3::new(0.0, 0.0, 0.0),
            kind: JointKind::Revolute {
                axis: Vec3::new(0.0, 0.0, 1.0),
                limits: None,
                effort_limit: None,
                velocity_limit: None,
            },
            state,
            coupling: driver.map(|(d, ratio, offset)| vcad_ir::JointCoupling {
                driver: d.into(),
                ratio,
                offset,
            }),
            passive: false,
        }
    }

    #[test]
    fn gear_train_couplings_resolve_in_dependency_order() {
        // Declared out of order: c depends on b depends on a.
        let joints = vec![
            coupled_joint("c", Some(("b", -28.0 / 12.0, 0.0)), 999.0),
            coupled_joint("b", Some(("a", -16.0 / 28.0, 3.0)), 999.0),
            coupled_joint("a", None, 56.0),
        ];
        let r = resolve_coupled_states(&joints);
        let b = -16.0 / 28.0 * 56.0 + 3.0;
        assert!((r[1].state - b).abs() < 1e-12);
        assert!((r[0].state - (-28.0 / 12.0 * b)).abs() < 1e-12);
        assert_eq!(r[2].state, 56.0);
    }

    #[test]
    fn coupling_cycles_and_missing_drivers_keep_authored_state() {
        let joints = vec![
            coupled_joint("x", Some(("y", 2.0, 0.0)), 10.0),
            coupled_joint("y", Some(("x", 2.0, 0.0)), 20.0),
            coupled_joint("z", Some(("nope", 2.0, 0.0)), 30.0),
        ];
        let r = resolve_coupled_states(&joints);
        assert_eq!((r[0].state, r[1].state, r[2].state), (10.0, 20.0, 30.0));
    }

    /// A revolute joint whose parent anchor matches the child instance's own
    /// transform — the natural authoring pattern. The joint must fully place
    /// the child; the instance transform must NOT be applied on top.
    fn doc_with_instance_transform(state: f64) -> Document {
        serde_json::from_value(serde_json::json!({
            "version": "0.1",
            "nodes": {
                "0": {"id": 0, "op": {"type": "Cube", "size": {"x": 10.0, "y": 10.0, "z": 10.0}}}
            },
            "roots": [],
            "materials": {},
            "part_materials": {},
            "partDefs": {
                "p": {"id": "p", "name": "p", "root": 0}
            },
            "instances": [
                {"id": "base", "partDefId": "p"},
                {"id": "arm", "partDefId": "p",
                 "transform": {"translation": {"x": 10.0, "y": 0.0, "z": 90.0},
                                "rotation": {"x": 0.0, "y": 0.0, "z": 0.0},
                                "scale": {"x": 1.0, "y": 1.0, "z": 1.0}}}
            ],
            "joints": [
                {"id": "j0", "parentInstanceId": "base", "childInstanceId": "arm",
                 "parentAnchor": {"x": 10.0, "y": 0.0, "z": 90.0},
                 "childAnchor": {"x": 0.0, "y": 0.0, "z": 0.0},
                 "kind": {"type": "Revolute", "axis": {"x": 0.0, "y": 1.0, "z": 0.0}, "limits": [-360.0, 360.0]},
                 "state": state}
            ],
            "groundInstanceId": "base"
        }))
        .expect("valid doc")
    }

    #[test]
    fn jointed_child_ignores_instance_transform() {
        let doc = doc_with_instance_transform(0.0);
        let world = solve_forward_kinematics(&doc);
        let arm = world.get("arm").expect("arm solved");
        // Joint places the child at the parent anchor exactly once.
        assert!(
            (arm.translation.x - 10.0).abs() < 1e-9,
            "x = {}",
            arm.translation.x
        );
        assert!(
            (arm.translation.z - 90.0).abs() < 1e-9,
            "z = {}",
            arm.translation.z
        );
    }

    #[test]
    fn jointed_child_rotates_about_anchor() {
        let doc = doc_with_instance_transform(90.0);
        let world = solve_forward_kinematics(&doc);
        let arm = world.get("arm").expect("arm solved");
        // child_anchor is the origin, so the anchor point itself stays put.
        assert!((arm.translation.x - 10.0).abs() < 1e-9);
        assert!((arm.translation.z - 90.0).abs() < 1e-9);
        assert!(
            (arm.rotation.y - 90.0).abs() < 1e-6,
            "ry = {}",
            arm.rotation.y
        );
    }

    /// Apply a solved world transform to a part-local point.
    fn world_point(t: &Transform3D, p: Vec3) -> Vec3 {
        vec3_add(&t.translation, &mat_vec3(&euler_to_matrix(&t.rotation), &p))
    }

    /// The defining invariant: the child's anchor point lands on the parent's
    /// anchor point at *every* joint angle. A zero anchor cannot distinguish
    /// "rotate about the anchor" from "rotate about the world origin", so
    /// this pins a nonzero parent anchor and a nonzero child anchor together.
    #[test]
    fn anchor_points_stay_coincident_at_every_angle() {
        for state in [0.0, -30.0, 45.0, 137.0] {
            let mut doc = doc_with_instance_transform(state);
            let child_anchor = Vec3::new(3.0, -2.0, 7.0);
            doc.joints.as_mut().expect("joints")[0].child_anchor = child_anchor;
            let expect = doc.joints.as_ref().unwrap()[0].parent_anchor; // parent is ground-identity
            let world = solve_forward_kinematics(&doc);
            let arm = world.get("arm").expect("arm solved");

            let anchor = world_point(arm, child_anchor);
            assert!(
                (anchor.x - expect.x).abs() < 1e-9
                    && (anchor.y - expect.y).abs() < 1e-9
                    && (anchor.z - expect.z).abs() < 1e-9,
                "state {state}: anchor at {anchor:?}, want {expect:?}"
            );

            // And the pivot really is the anchor, not the world origin: a
            // material point keeps its distance to the anchor, not to (0,0,0).
            let probe = world_point(arm, Vec3::new(0.0, 0.0, 0.0));
            let d_anchor = ((probe.x - expect.x).powi(2)
                + (probe.y - expect.y).powi(2)
                + (probe.z - expect.z).powi(2))
            .sqrt();
            let want = (3.0f64.powi(2) + 2.0f64.powi(2) + 7.0f64.powi(2)).sqrt();
            assert!(
                (d_anchor - want).abs() < 1e-9,
                "state {state}: |probe - anchor| = {d_anchor}, want {want}"
            );
        }
    }

    #[test]
    fn root_instance_keeps_own_transform() {
        let mut doc = doc_with_instance_transform(0.0);
        doc.joints = Some(vec![]);
        let world = solve_forward_kinematics(&doc);
        let arm = world.get("arm").expect("arm solved");
        assert!((arm.translation.x - 10.0).abs() < 1e-9);
        assert!((arm.translation.z - 90.0).abs() < 1e-9);
    }
}

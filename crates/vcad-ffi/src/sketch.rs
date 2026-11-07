//! Sketch constraint solver over the C ABI (`vcad_sketch_*`).
//!
//! Wraps [`vcad_kernel_constraints::Sketch2D`]: build points, lines, arcs and
//! circles, attach geometric and dimensional constraints, solve with
//! Levenberg-Marquardt, read back the solved geometry and the DOF /
//! over-constraint status, then export the closed profile as the IR
//! `Sketch2D` op the document format (and [`crate::features`]) consumes.
//!
//! Entities are addressed by small integer handles (1-based, `0` = error)
//! rather than the kernel's slotmap keys, which do not round-trip through C.
//! Constraints are addressed by the index they were added at, so a driving
//! dimension can be edited in place ([`vcad_sketch_set_dimension`]) and the
//! sketch re-solved — that is the whole "drag a dimension" interaction.
//!
//! Splines are not offered: the kernel solver has no spline entity yet, and a
//! handle that silently ignored constraints would be worse than none.

use std::ffi::c_char;
use std::ptr;

use serde::Serialize;
use vcad_kernel::vcad_kernel_constraints::{
    Constraint, EntityId, EntityRef, Sketch2D, SketchEntity, SolveStatus,
};
use vcad_kernel_math::{Point3, Vec3};

use crate::err;
use crate::json::{guard_ptr, json_out};

/// Opaque sketch handle.
pub struct VcadSketch {
    inner: Sketch2D,
    /// handle - 1 → kernel entity id.
    handles: Vec<EntityId>,
}

impl VcadSketch {
    fn entity(&self, h: u32) -> Option<EntityId> {
        if h == 0 {
            return None;
        }
        self.handles.get(h as usize - 1).copied()
    }

    fn push(&mut self, id: EntityId) -> u32 {
        self.handles.push(id);
        self.handles.len() as u32
    }

    fn is_point(&self, h: u32) -> bool {
        self.entity(h)
            .and_then(|id| self.inner.entities.get(id))
            .is_some_and(SketchEntity::is_point)
    }

    /// Resolve `(entity handle, point selector)` to a kernel point reference.
    fn point_ref(&self, h: u32, which: u32) -> Option<EntityRef> {
        let id = self.entity(h)?;
        let e = self.inner.entities.get(id)?;
        Some(match (e, which) {
            (SketchEntity::Point(_), _) => EntityRef::Point(id),
            (SketchEntity::Line(_), VCAD_SKETCH_REF_START) => EntityRef::LineStart(id),
            (SketchEntity::Line(_), VCAD_SKETCH_REF_END) => EntityRef::LineEnd(id),
            (SketchEntity::Arc(_), VCAD_SKETCH_REF_START) => EntityRef::ArcStart(id),
            (SketchEntity::Arc(_), VCAD_SKETCH_REF_END) => EntityRef::ArcEnd(id),
            (SketchEntity::Arc(_) | SketchEntity::Circle(_), VCAD_SKETCH_REF_CENTER) => {
                EntityRef::Center(id)
            }
            _ => return None,
        })
    }
}

/// Point selector: the entity itself (point entities).
pub const VCAD_SKETCH_REF_SELF: u32 = 0;
/// Point selector: start point of a line or arc.
pub const VCAD_SKETCH_REF_START: u32 = 1;
/// Point selector: end point of a line or arc.
pub const VCAD_SKETCH_REF_END: u32 = 2;
/// Point selector: center of an arc or circle.
pub const VCAD_SKETCH_REF_CENTER: u32 = 3;

/// Constraint kinds accepted by [`vcad_sketch_add_constraint`]. Which fields
/// of [`VcadSketchConstraint`] each kind reads:
///
/// | kind | reads |
/// |---|---|
/// | COINCIDENT | (e0,r0) point, (e1,r1) point |
/// | HORIZONTAL / VERTICAL | e0 line |
/// | PARALLEL / PERPENDICULAR / EQUAL_LENGTH | e0 line, e1 line |
/// | TANGENT | e0 line, e1 arc/circle, (e2,r2) tangency point |
/// | DISTANCE | (e0,r0), (e1,r1), value |
/// | LENGTH | e0 line, value |
/// | RADIUS / DIAMETER | e0 circle, value |
/// | ANGLE | e0 line, e1 line, value (degrees) |
/// | EQUAL_RADIUS / CONCENTRIC | e0, e1 circles |
/// | FIXED | (e0,r0), value = x, value2 = y |
/// | POINT_ON_LINE / MIDPOINT | (e0,r0) point, e1 line |
/// | POINT_ON_CIRCLE | (e0,r0) point, e1 circle |
/// | HORIZONTAL_DISTANCE | (e0,r0), value = x |
/// | VERTICAL_DISTANCE | (e0,r0), value = y |
/// | POINT_LINE_DISTANCE | (e0,r0) point, e1 line, value |
/// | SYMMETRIC | (e0,r0), (e1,r1), e2 axis line |
pub mod kind {
    #![allow(missing_docs)]
    pub const COINCIDENT: u32 = 0;
    pub const HORIZONTAL: u32 = 1;
    pub const VERTICAL: u32 = 2;
    pub const PARALLEL: u32 = 3;
    pub const PERPENDICULAR: u32 = 4;
    pub const TANGENT: u32 = 5;
    pub const DISTANCE: u32 = 6;
    pub const LENGTH: u32 = 7;
    pub const RADIUS: u32 = 8;
    pub const ANGLE: u32 = 9;
    pub const EQUAL_LENGTH: u32 = 10;
    pub const EQUAL_RADIUS: u32 = 11;
    pub const FIXED: u32 = 12;
    pub const POINT_ON_LINE: u32 = 13;
    pub const CONCENTRIC: u32 = 14;
    pub const POINT_ON_CIRCLE: u32 = 15;
    pub const MIDPOINT: u32 = 16;
    pub const HORIZONTAL_DISTANCE: u32 = 17;
    pub const VERTICAL_DISTANCE: u32 = 18;
    pub const DIAMETER: u32 = 19;
    pub const POINT_LINE_DISTANCE: u32 = 20;
    pub const SYMMETRIC: u32 = 21;
}

/// One constraint, flattened for C. Unused fields are ignored (see [`kind`]).
#[repr(C)]
#[derive(Clone, Copy, Debug, Default)]
pub struct VcadSketchConstraint {
    /// One of the [`kind`] constants.
    pub kind: u32,
    /// First entity handle.
    pub e0: u32,
    /// Point selector on `e0` (`VCAD_SKETCH_REF_*`).
    pub r0: u32,
    /// Second entity handle.
    pub e1: u32,
    /// Point selector on `e1`.
    pub r1: u32,
    /// Third entity handle.
    pub e2: u32,
    /// Point selector on `e2`.
    pub r2: u32,
    /// Dimension value (mm, or degrees for ANGLE).
    pub value: f64,
    /// Second value (FIXED's y).
    pub value2: f64,
}

/// Over/under-constraint classification reported by a solve.
pub const VCAD_SKETCH_UNDER: u8 = 0;
/// DOF == 0 and the solve converged.
pub const VCAD_SKETCH_FULL: u8 = 1;
/// More equations than unknowns, or a solve that cannot satisfy them.
pub const VCAD_SKETCH_OVER: u8 = 2;

/// Result of [`vcad_sketch_solve`].
#[repr(C)]
#[derive(Clone, Copy, Debug)]
pub struct VcadSketchSolve {
    /// 1 when the residual fell below tolerance.
    pub converged: u8,
    /// `VCAD_SKETCH_UNDER` / `_FULL` / `_OVER`.
    pub state: u8,
    /// Kernel termination reason: 0 converged, 1 max iterations, 2 lambda
    /// overflow, 3 no constraints, 4 no parameters, 5 singular matrix,
    /// 255 bad handle.
    pub status: u8,
    /// Degrees of freedom: parameters − constraint equations.
    pub dof: i32,
    /// Iterations used.
    pub iterations: u32,
    /// Final sum of squared residuals.
    pub residual: f64,
}

fn sketch_mut<'a>(s: *mut VcadSketch) -> Option<&'a mut VcadSketch> {
    if s.is_null() {
        err::set_error("sketch: null handle");
        return None;
    }
    Some(unsafe { &mut *s })
}

fn sketch_ref<'a>(s: *const VcadSketch) -> Option<&'a VcadSketch> {
    if s.is_null() {
        return None;
    }
    Some(unsafe { &*s })
}

fn vec3(p: *const f64) -> Option<Vec3> {
    if p.is_null() {
        return None;
    }
    let v = unsafe { std::slice::from_raw_parts(p, 3) };
    Some(Vec3::new(v[0], v[1], v[2]))
}

/// New sketch on the world XY plane.
#[no_mangle]
pub extern "C" fn vcad_sketch_new() -> *mut VcadSketch {
    Box::into_raw(Box::new(VcadSketch {
        inner: Sketch2D::new(),
        handles: Vec::new(),
    }))
}

/// New sketch on an arbitrary plane: `origin`, `x_dir`, `y_dir` are 3 doubles
/// each (kernel Z-up coords). Null on a null pointer or degenerate axes.
#[no_mangle]
pub extern "C" fn vcad_sketch_new_on_plane(
    origin: *const f64,
    x_dir: *const f64,
    y_dir: *const f64,
) -> *mut VcadSketch {
    err::clear_error();
    let (Some(o), Some(x), Some(y)) = (vec3(origin), vec3(x_dir), vec3(y_dir)) else {
        err::set_error("sketch plane: null vector");
        return ptr::null_mut();
    };
    if x.norm() < 1e-12 || y.norm() < 1e-12 || x.cross(y).norm() < 1e-12 {
        err::set_error("sketch plane: degenerate axes");
        return ptr::null_mut();
    }
    guard_ptr("sketch plane", || {
        Box::into_raw(Box::new(VcadSketch {
            inner: Sketch2D::on_plane(Point3::new(o.x, o.y, o.z), x, y),
            handles: Vec::new(),
        }))
    })
}

/// Release a sketch. Null is a no-op.
#[no_mangle]
pub extern "C" fn vcad_sketch_free(s: *mut VcadSketch) {
    if !s.is_null() {
        drop(unsafe { Box::from_raw(s) });
    }
}

/// Add a point; returns its handle (0 on null sketch).
#[no_mangle]
pub extern "C" fn vcad_sketch_add_point(s: *mut VcadSketch, x: f64, y: f64) -> u32 {
    let Some(s) = sketch_mut(s) else { return 0 };
    let id = s.inner.add_point(x, y);
    s.push(id)
}

/// Add a line between two existing point handles; 0 if either is not a point.
#[no_mangle]
pub extern "C" fn vcad_sketch_add_line(s: *mut VcadSketch, start: u32, end: u32) -> u32 {
    let Some(s) = sketch_mut(s) else { return 0 };
    if !s.is_point(start) || !s.is_point(end) || start == end {
        err::set_error("add_line: endpoints must be two distinct point handles");
        return 0;
    }
    let (a, b) = (s.entity(start).unwrap(), s.entity(end).unwrap());
    let id = s.inner.add_line(a, b);
    s.push(id)
}

/// Add an arc from `start` to `end` about `center` (all point handles),
/// counter-clockwise when `ccw`. 0 on bad handles.
///
/// The solver does not implicitly keep |center−start| = |center−end|; add an
/// EQUAL-distance pair (two DISTANCE constraints, or POINT_ON_CIRCLE against a
/// construction circle) when the arc must stay circular under drag.
#[no_mangle]
pub extern "C" fn vcad_sketch_add_arc(
    s: *mut VcadSketch,
    start: u32,
    end: u32,
    center: u32,
    ccw: bool,
) -> u32 {
    let Some(s) = sketch_mut(s) else { return 0 };
    if ![start, end, center].iter().all(|&h| s.is_point(h)) {
        err::set_error("add_arc: start/end/center must be point handles");
        return 0;
    }
    let ids = [start, end, center].map(|h| s.entity(h).unwrap());
    let id = s.inner.add_arc(ids[0], ids[1], ids[2], ccw);
    s.push(id)
}

/// Add a circle about an existing `center` point handle. 0 on a bad handle or
/// non-positive radius.
#[no_mangle]
pub extern "C" fn vcad_sketch_add_circle(s: *mut VcadSketch, center: u32, radius: f64) -> u32 {
    let Some(s) = sketch_mut(s) else { return 0 };
    if !s.is_point(center) || radius.is_nan() || radius <= 0.0 {
        err::set_error("add_circle: center must be a point handle and radius > 0");
        return 0;
    }
    let id = s.inner.add_circle(s.entity(center).unwrap(), radius);
    s.push(id)
}

fn build_constraint(s: &VcadSketch, c: &VcadSketchConstraint) -> Result<Constraint, String> {
    let ent = |h: u32, what: &str| {
        s.entity(h)
            .ok_or_else(|| format!("{what}: bad entity handle {h}"))
    };
    let pref = |h: u32, r: u32, what: &str| {
        s.point_ref(h, r)
            .ok_or_else(|| format!("{what}: handle {h} selector {r} does not name a point"))
    };
    use kind::*;
    Ok(match c.kind {
        COINCIDENT => Constraint::Coincident {
            point_a: pref(c.e0, c.r0, "coincident a")?,
            point_b: pref(c.e1, c.r1, "coincident b")?,
        },
        HORIZONTAL => Constraint::Horizontal {
            line: ent(c.e0, "horizontal")?,
        },
        VERTICAL => Constraint::Vertical {
            line: ent(c.e0, "vertical")?,
        },
        PARALLEL => Constraint::Parallel {
            line_a: ent(c.e0, "parallel")?,
            line_b: ent(c.e1, "parallel")?,
        },
        PERPENDICULAR => Constraint::Perpendicular {
            line_a: ent(c.e0, "perpendicular")?,
            line_b: ent(c.e1, "perpendicular")?,
        },
        TANGENT => Constraint::Tangent {
            line: ent(c.e0, "tangent line")?,
            curve: ent(c.e1, "tangent curve")?,
            at_point: pref(c.e2, c.r2, "tangent point")?,
        },
        DISTANCE => Constraint::Distance {
            point_a: pref(c.e0, c.r0, "distance a")?,
            point_b: pref(c.e1, c.r1, "distance b")?,
            distance: c.value,
        },
        LENGTH => Constraint::Length {
            line: ent(c.e0, "length")?,
            length: c.value,
        },
        RADIUS => Constraint::Radius {
            circle: ent(c.e0, "radius")?,
            radius: c.value,
        },
        DIAMETER => Constraint::Diameter {
            circle: ent(c.e0, "diameter")?,
            diameter: c.value,
        },
        ANGLE => Constraint::Angle {
            line_a: ent(c.e0, "angle")?,
            line_b: ent(c.e1, "angle")?,
            angle_rad: c.value.to_radians(),
        },
        EQUAL_LENGTH => Constraint::EqualLength {
            line_a: ent(c.e0, "equal length")?,
            line_b: ent(c.e1, "equal length")?,
        },
        EQUAL_RADIUS => Constraint::EqualRadius {
            circle_a: ent(c.e0, "equal radius")?,
            circle_b: ent(c.e1, "equal radius")?,
        },
        CONCENTRIC => Constraint::Concentric {
            circle_a: ent(c.e0, "concentric")?,
            circle_b: ent(c.e1, "concentric")?,
        },
        FIXED => Constraint::Fixed {
            point: pref(c.e0, c.r0, "fixed")?,
            x: c.value,
            y: c.value2,
        },
        POINT_ON_LINE => Constraint::PointOnLine {
            point: pref(c.e0, c.r0, "point on line")?,
            line: ent(c.e1, "point on line")?,
        },
        POINT_ON_CIRCLE => Constraint::PointOnCircle {
            point: pref(c.e0, c.r0, "point on circle")?,
            circle: ent(c.e1, "point on circle")?,
        },
        MIDPOINT => Constraint::Midpoint {
            point: pref(c.e0, c.r0, "midpoint")?,
            line: ent(c.e1, "midpoint")?,
        },
        HORIZONTAL_DISTANCE => Constraint::HorizontalDistance {
            point: pref(c.e0, c.r0, "horizontal distance")?,
            x: c.value,
        },
        VERTICAL_DISTANCE => Constraint::VerticalDistance {
            point: pref(c.e0, c.r0, "vertical distance")?,
            y: c.value,
        },
        POINT_LINE_DISTANCE => Constraint::PointLineDistance {
            point: pref(c.e0, c.r0, "point-line distance")?,
            line: ent(c.e1, "point-line distance")?,
            distance: c.value,
        },
        SYMMETRIC => Constraint::Symmetric {
            point_a: pref(c.e0, c.r0, "symmetric a")?,
            point_b: pref(c.e1, c.r1, "symmetric b")?,
            axis: ent(c.e2, "symmetric axis")?,
        },
        k => return Err(format!("unknown constraint kind {k}")),
    })
}

/// Add a constraint. Returns its index (≥ 0) for [`vcad_sketch_set_dimension`],
/// or −1 on a bad handle / unknown kind (see `vcad_last_error`).
#[no_mangle]
pub extern "C" fn vcad_sketch_add_constraint(
    s: *mut VcadSketch,
    c: *const VcadSketchConstraint,
) -> i32 {
    err::clear_error();
    let Some(s) = sketch_mut(s) else { return -1 };
    if c.is_null() {
        err::set_error("add_constraint: null constraint");
        return -1;
    }
    match build_constraint(s, unsafe { &*c }) {
        Ok(k) => {
            s.inner.add_constraint(k);
            (s.inner.constraints.len() - 1) as i32
        }
        Err(e) => {
            err::set_error(format!("add_constraint: {e}"));
            -1
        }
    }
}

/// Change the value of driving dimension `index` (DISTANCE, LENGTH, RADIUS,
/// DIAMETER, ANGLE in degrees, FIXED x/y via `value`/`value2`, …) without
/// rebuilding the sketch. Returns false when `index` is out of range or names
/// a dimensionless constraint. Call [`vcad_sketch_solve`] afterwards.
#[no_mangle]
pub extern "C" fn vcad_sketch_set_dimension(
    s: *mut VcadSketch,
    index: u32,
    value: f64,
    value2: f64,
) -> bool {
    let Some(s) = sketch_mut(s) else { return false };
    let Some(c) = s.inner.constraints.get_mut(index as usize) else {
        err::set_error(format!("set_dimension: no constraint {index}"));
        return false;
    };
    match c {
        Constraint::Distance { distance, .. } | Constraint::PointLineDistance { distance, .. } => {
            *distance = value
        }
        Constraint::Length { length, .. } => *length = value,
        Constraint::Radius { radius, .. } => *radius = value,
        Constraint::Diameter { diameter, .. } => *diameter = value,
        Constraint::Angle { angle_rad, .. } => *angle_rad = value.to_radians(),
        Constraint::HorizontalDistance { x, .. } => *x = value,
        Constraint::VerticalDistance { y, .. } => *y = value,
        Constraint::Fixed { x, y, .. } => {
            *x = value;
            *y = value2;
        }
        Constraint::OffsetCoincident { dx, dy, .. } => {
            *dx = value;
            *dy = value2;
        }
        _ => {
            err::set_error(format!(
                "set_dimension: constraint {index} has no dimension"
            ));
            return false;
        }
    }
    true
}

/// Remove the most recently added constraint (undo of an add). Returns false
/// when the sketch has none. Indices of earlier constraints are unchanged.
#[no_mangle]
pub extern "C" fn vcad_sketch_pop_constraint(s: *mut VcadSketch) -> bool {
    let Some(s) = sketch_mut(s) else { return false };
    s.inner.constraints.pop().is_some()
}

fn classify(dof: i32, converged: bool) -> u8 {
    if dof < 0 || (!converged && dof == 0) {
        VCAD_SKETCH_OVER
    } else if dof == 0 {
        VCAD_SKETCH_FULL
    } else {
        VCAD_SKETCH_UNDER
    }
}

/// Current DOF without solving.
#[no_mangle]
pub extern "C" fn vcad_sketch_dof(s: *const VcadSketch) -> i32 {
    sketch_ref(s).map_or(0, |s| s.inner.degrees_of_freedom())
}

/// Solve in place and report convergence + constraint status.
///
/// `state` is OVER when there are more equations than unknowns **or** the
/// solver could not drive the residual to zero with DOF ≤ 0 (conflicting
/// dimensions). A sketch with no constraints reports converged + UNDER.
#[no_mangle]
pub extern "C" fn vcad_sketch_solve(s: *mut VcadSketch) -> VcadSketchSolve {
    let bad = VcadSketchSolve {
        converged: 0,
        state: VCAD_SKETCH_OVER,
        status: 255,
        dof: 0,
        iterations: 0,
        residual: f64::NAN,
    };
    let Some(s) = sketch_mut(s) else { return bad };
    std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
        let r = s.inner.solve_default();
        let dof = s.inner.degrees_of_freedom();
        let converged = r.converged
            || matches!(
                r.status,
                SolveStatus::NoConstraints | SolveStatus::NoParameters
            );
        VcadSketchSolve {
            converged: converged as u8,
            state: classify(dof, converged),
            status: match r.status {
                SolveStatus::Converged => 0,
                SolveStatus::MaxIterations => 1,
                SolveStatus::LambdaOverflow => 2,
                SolveStatus::NoConstraints => 3,
                SolveStatus::NoParameters => 4,
                SolveStatus::SingularMatrix => 5,
            },
            dof,
            iterations: r.iterations as u32,
            residual: r.residual_norm,
        }
    }))
    .unwrap_or(bad)
}

/// Solved position of a point handle — or of the start/end/center of a
/// line/arc/circle via `which` — in sketch-local 2D. False on a bad handle.
#[no_mangle]
pub extern "C" fn vcad_sketch_point(
    s: *const VcadSketch,
    handle: u32,
    which: u32,
    out_xy: *mut f64,
) -> bool {
    let Some(s) = sketch_ref(s) else { return false };
    if out_xy.is_null() {
        return false;
    }
    let pid = match s.point_ref(handle, which) {
        Some(EntityRef::Point(id)) => Some(id),
        Some(EntityRef::LineStart(id)) => s
            .inner
            .entities
            .get(id)
            .and_then(|e| e.as_line())
            .map(|l| l.start),
        Some(EntityRef::LineEnd(id)) => s
            .inner
            .entities
            .get(id)
            .and_then(|e| e.as_line())
            .map(|l| l.end),
        Some(EntityRef::ArcStart(id)) => s
            .inner
            .entities
            .get(id)
            .and_then(|e| e.as_arc())
            .map(|a| a.start),
        Some(EntityRef::ArcEnd(id)) => s
            .inner
            .entities
            .get(id)
            .and_then(|e| e.as_arc())
            .map(|a| a.end),
        Some(EntityRef::Center(id)) => match s.inner.entities.get(id) {
            Some(SketchEntity::Arc(a)) => Some(a.center),
            Some(SketchEntity::Circle(c)) => Some(c.center),
            _ => None,
        },
        None => None,
    };
    let Some((x, y)) = pid.and_then(|id| s.inner.get_point(id)) else {
        return false;
    };
    unsafe {
        *out_xy = x;
        *out_xy.add(1) = y;
    }
    true
}

/// Solved radius of a circle handle (NaN for anything else).
#[no_mangle]
pub extern "C" fn vcad_sketch_radius(s: *const VcadSketch, handle: u32) -> f64 {
    sketch_ref(s)
        .and_then(|s| s.entity(handle).and_then(|id| s.inner.get_radius(id)))
        .unwrap_or(f64::NAN)
}

#[derive(Serialize)]
struct IrVec2 {
    x: f64,
    y: f64,
}

#[derive(Serialize)]
struct IrVec3 {
    x: f64,
    y: f64,
    z: f64,
}

/// Mirrors `vcad_ir::SketchSegment2D`'s serde shape exactly (and is checked
/// against it in the tests), so the output deserializes as IR.
#[derive(Serialize)]
#[serde(tag = "type")]
enum Seg {
    Line {
        start: IrVec2,
        end: IrVec2,
    },
    Arc {
        start: IrVec2,
        end: IrVec2,
        center: IrVec2,
        ccw: bool,
    },
}

#[derive(Serialize)]
struct Profile {
    origin: IrVec3,
    x_dir: IrVec3,
    y_dir: IrVec3,
    segments: Vec<Seg>,
}

fn v2(x: f64, y: f64) -> IrVec2 {
    IrVec2 { x, y }
}

/// Export the solved sketch as an IR profile JSON:
/// `{"origin":{x,y,z},"x_dir":…,"y_dir":…,"segments":[SketchSegment2D…]}` —
/// exactly the `profile` object [`crate::features`] takes, and the fields of
/// an IR `Sketch2D` op.
///
/// Lines/arcs must form one closed loop (ordered here); a sketch whose only
/// curve is a single circle exports as two half-arcs. Null on an open or
/// empty sketch; free with `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_sketch_profile_json(s: *const VcadSketch) -> *mut c_char {
    err::clear_error();
    let Some(s) = sketch_ref(s) else {
        err::set_error("profile: null sketch");
        return ptr::null_mut();
    };
    guard_ptr("profile", || {
        let sk = &s.inner;
        let has_curves = sk.entities.values().any(|e| e.is_line() || e.is_arc());
        let segments = if has_curves {
            match sk.to_profile() {
                Ok(p) => p
                    .segments
                    .iter()
                    .map(|seg| match seg {
                        vcad_kernel::vcad_kernel_sketch::SketchSegment::Line { start, end } => {
                            Seg::Line {
                                start: v2(start.x, start.y),
                                end: v2(end.x, end.y),
                            }
                        }
                        vcad_kernel::vcad_kernel_sketch::SketchSegment::Arc {
                            start,
                            end,
                            center,
                            ccw,
                        } => Seg::Arc {
                            start: v2(start.x, start.y),
                            end: v2(end.x, end.y),
                            center: v2(center.x, center.y),
                            ccw: *ccw,
                        },
                    })
                    .collect(),
                Err(e) => {
                    err::set_error(format!("profile: {e}"));
                    return ptr::null_mut();
                }
            }
        } else {
            let circles: Vec<_> = sk.entities.iter().filter(|(_, e)| e.is_circle()).collect();
            let [(id, SketchEntity::Circle(c))] = circles.as_slice() else {
                err::set_error(
                    "profile: sketch has no closed loop (needs lines/arcs or one circle)",
                );
                return ptr::null_mut();
            };
            let (cx, cy) = sk.get_point(c.center).unwrap_or((0.0, 0.0));
            let r = sk.get_radius(*id).unwrap_or(0.0);
            vec![
                Seg::Arc {
                    start: v2(cx + r, cy),
                    end: v2(cx - r, cy),
                    center: v2(cx, cy),
                    ccw: true,
                },
                Seg::Arc {
                    start: v2(cx - r, cy),
                    end: v2(cx + r, cy),
                    center: v2(cx, cy),
                    ccw: true,
                },
            ]
        };
        let o = sk.origin;
        let (x, y) = (sk.x_dir.into_inner(), sk.y_dir.into_inner());
        json_out(&Profile {
            origin: IrVec3 {
                x: o.x,
                y: o.y,
                z: o.z,
            },
            x_dir: IrVec3 {
                x: x.x,
                y: x.y,
                z: x.z,
            },
            y_dir: IrVec3 {
                x: y.x,
                y: y.y,
                z: y.z,
            },
            segments,
        })
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::take;

    fn c(kind: u32) -> VcadSketchConstraint {
        VcadSketchConstraint {
            kind,
            ..Default::default()
        }
    }

    /// Rectangle: 4 points, 4 lines, closed by coincident-free shared points,
    /// then H/V + fixed corner + two driving dimensions → fully constrained.
    fn rect(s: *mut VcadSketch) -> ([u32; 4], [u32; 4], i32) {
        let p = [
            vcad_sketch_add_point(s, 0.1, -0.2),
            vcad_sketch_add_point(s, 9.0, 0.3),
            vcad_sketch_add_point(s, 8.7, 4.1),
            vcad_sketch_add_point(s, -0.3, 5.2),
        ];
        let l = [
            vcad_sketch_add_line(s, p[0], p[1]),
            vcad_sketch_add_line(s, p[1], p[2]),
            vcad_sketch_add_line(s, p[2], p[3]),
            vcad_sketch_add_line(s, p[3], p[0]),
        ];
        assert!(l.iter().all(|&h| h != 0));
        for (i, &line) in l.iter().enumerate() {
            let k = if i % 2 == 0 {
                kind::HORIZONTAL
            } else {
                kind::VERTICAL
            };
            assert!(vcad_sketch_add_constraint(s, &VcadSketchConstraint { e0: line, ..c(k) }) >= 0);
        }
        assert!(
            vcad_sketch_add_constraint(
                s,
                &VcadSketchConstraint {
                    e0: p[0],
                    value: 0.0,
                    value2: 0.0,
                    ..c(kind::FIXED)
                }
            ) >= 0
        );
        assert!(
            vcad_sketch_add_constraint(
                s,
                &VcadSketchConstraint {
                    e0: l[0],
                    value: 20.0,
                    ..c(kind::LENGTH)
                }
            ) >= 0
        );
        let width_dim = vcad_sketch_add_constraint(
            s,
            &VcadSketchConstraint {
                e0: l[1],
                value: 10.0,
                ..c(kind::LENGTH)
            },
        );
        assert!(width_dim >= 0);
        (p, l, width_dim)
    }

    fn pt(s: *const VcadSketch, h: u32) -> (f64, f64) {
        let mut xy = [0.0; 2];
        assert!(vcad_sketch_point(
            s,
            h,
            VCAD_SKETCH_REF_SELF,
            xy.as_mut_ptr()
        ));
        (xy[0], xy[1])
    }

    #[test]
    fn constraint_solve_round_trip() {
        let s = vcad_sketch_new();
        let (p, _l, width_dim) = rect(s);
        assert!(vcad_sketch_dof(s) > 0 || vcad_sketch_dof(s) == 0);
        let r = vcad_sketch_solve(s);
        assert_eq!(r.converged, 1, "{r:?}");
        assert_eq!(r.dof, 0, "{r:?}");
        assert_eq!(r.state, VCAD_SKETCH_FULL);
        let (x2, y2) = pt(s, p[2]);
        assert!(
            (x2 - 20.0).abs() < 1e-6 && (y2 - 10.0).abs() < 1e-6,
            "({x2},{y2})"
        );

        // Drive the width dimension and re-solve.
        assert!(vcad_sketch_set_dimension(s, width_dim as u32, 15.0, 0.0));
        let r = vcad_sketch_solve(s);
        assert_eq!(r.converged, 1);
        let (_, y2) = pt(s, p[2]);
        assert!((y2 - 15.0).abs() < 1e-6, "{y2}");

        // Profile export deserializes as the IR segment type.
        let json = take(vcad_sketch_profile_json(s));
        let v: serde_json::Value = serde_json::from_str(&json).unwrap();
        let segs: Vec<vcad_ir::SketchSegment2D> =
            serde_json::from_value(v["segments"].clone()).unwrap();
        assert_eq!(segs.len(), 4);

        // One more dimension over-constrains it.
        assert!(
            vcad_sketch_add_constraint(
                s,
                &VcadSketchConstraint {
                    e0: p[0],
                    e1: p[2],
                    value: 1.0,
                    ..c(kind::DISTANCE)
                }
            ) >= 0
        );
        let r = vcad_sketch_solve(s);
        assert_eq!(r.state, VCAD_SKETCH_OVER, "{r:?}");
        assert!(vcad_sketch_pop_constraint(s));
        assert_eq!(vcad_sketch_solve(s).state, VCAD_SKETCH_FULL);
        vcad_sketch_free(s);
    }

    #[test]
    fn circle_radius_and_tangent_line() {
        let s = vcad_sketch_new();
        let center = vcad_sketch_add_point(s, 0.0, 0.0);
        let circle = vcad_sketch_add_circle(s, center, 3.0);
        assert!(
            vcad_sketch_add_constraint(
                s,
                &VcadSketchConstraint {
                    e0: circle,
                    value: 5.0,
                    ..c(kind::RADIUS)
                }
            ) >= 0
        );
        assert!(
            vcad_sketch_add_constraint(
                s,
                &VcadSketchConstraint {
                    e0: center,
                    ..c(kind::FIXED)
                }
            ) >= 0
        );
        let r = vcad_sketch_solve(s);
        assert_eq!(r.converged, 1);
        assert!((vcad_sketch_radius(s, circle) - 5.0).abs() < 1e-6);
        let json = take(vcad_sketch_profile_json(s));
        assert!(json.contains("\"Arc\""), "{json}");
        vcad_sketch_free(s);
    }

    #[test]
    fn bad_handles_fail_closed() {
        let s = vcad_sketch_new();
        let p = vcad_sketch_add_point(s, 0.0, 0.0);
        assert_eq!(vcad_sketch_add_line(s, p, 99), 0);
        assert_eq!(
            vcad_sketch_add_constraint(
                s,
                &VcadSketchConstraint {
                    e0: 42,
                    ..c(kind::HORIZONTAL)
                }
            ),
            -1
        );
        assert_eq!(vcad_sketch_add_constraint(s, &c(999)), -1);
        assert!(vcad_sketch_profile_json(s).is_null());
        vcad_sketch_free(s);
    }
}

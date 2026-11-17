//! 3D constructive geometry for room-scale transport (M7).
//!
//! The 1D [`crate::geometry`] stacks answer "how thick?"; a real bench
//! needs "what does the operator's chair see, with the floor, the walls
//! and the door in the picture?". A [`Scene`] is an ordered list of
//! [`Cell`]s, each a convex primitive ([`Shape`]: axis-aligned box,
//! sphere, axis-aligned cylinder) filled with a library material.
//!
//! **Priority semantics.** A point belongs to the *first* cell in the
//! list whose shape contains it. Hollow parts are expressed by listing
//! the hole first: a steel chamber is `[vacuum sphere r=73, steel sphere
//! r=76]`, a concrete room is `[air box (interior), concrete box
//! (exterior)]`. This is the "region priority" construction used by
//! voxel-free MC codes for simple scenes, and it keeps every primitive
//! convex — so a ray's entry/exit with each primitive is a single
//! interval, and the distance to the next material change is the nearest
//! interval endpoint over all primitives.
//!
//! **Fail-closed bounding cell.** The *last* cell is the world: every
//! other cell must lie inside it (checked on the 8 corners of each cell's
//! bounding box — sufficient because every primitive is convex). A
//! neutron that leaves the last cell has escaped the model (leakage).
//! Because the world contains every other cell, a point inside the world
//! always resolves to some cell: there are no accidental gaps that would
//! silently kill neutrons.
//!
//! Units: the public constructors take **millimeters** (vcad
//! convention); everything inside is centimeters.

use crate::materials::Material;

/// A principal axis (cylinder orientation).
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum Axis {
    /// The x axis.
    X,
    /// The y axis.
    Y,
    /// The z axis (vertical, vcad Z-up).
    Z,
}

impl Axis {
    fn index(self) -> usize {
        match self {
            Axis::X => 0,
            Axis::Y => 1,
            Axis::Z => 2,
        }
    }
}

/// A convex primitive, stored in **centimeters**.
#[derive(Debug, Clone, PartialEq)]
pub enum Shape {
    /// Axis-aligned box `[min, max]`.
    Box {
        /// Lower corner, cm.
        min: [f64; 3],
        /// Upper corner, cm.
        max: [f64; 3],
    },
    /// Sphere.
    Sphere {
        /// Center, cm.
        center: [f64; 3],
        /// Radius, cm.
        radius: f64,
    },
    /// Right circular cylinder along a principal axis.
    Cylinder {
        /// Center of the base disc, cm.
        base: [f64; 3],
        /// Axis direction (the cylinder extends along +axis from `base`).
        axis: Axis,
        /// Radius, cm.
        radius: f64,
        /// Length along the axis, cm.
        height: f64,
    },
}

const MM: f64 = 0.1;

fn mm3(v: [f64; 3]) -> [f64; 3] {
    [v[0] * MM, v[1] * MM, v[2] * MM]
}

impl Shape {
    /// Axis-aligned box from corners in mm.
    pub fn box_mm(min: [f64; 3], max: [f64; 3]) -> Shape {
        Shape::Box {
            min: mm3(min),
            max: mm3(max),
        }
    }

    /// Axis-aligned box centered at `center` with half-extents, mm.
    pub fn box_centered_mm(center: [f64; 3], half: [f64; 3]) -> Shape {
        Shape::box_mm(
            [
                center[0] - half[0],
                center[1] - half[1],
                center[2] - half[2],
            ],
            [
                center[0] + half[0],
                center[1] + half[1],
                center[2] + half[2],
            ],
        )
    }

    /// Sphere, mm.
    pub fn sphere_mm(center: [f64; 3], radius: f64) -> Shape {
        Shape::Sphere {
            center: mm3(center),
            radius: radius * MM,
        }
    }

    /// Cylinder, mm.
    pub fn cylinder_mm(base: [f64; 3], axis: Axis, radius: f64, height: f64) -> Shape {
        Shape::Cylinder {
            base: mm3(base),
            axis,
            radius: radius * MM,
            height: height * MM,
        }
    }

    /// True when `p` (cm) is inside or on the shape.
    pub fn contains(&self, p: [f64; 3]) -> bool {
        match self {
            Shape::Box { min, max } => (0..3).all(|i| p[i] >= min[i] && p[i] <= max[i]),
            Shape::Sphere { center, radius } => dist2(p, *center) <= radius * radius,
            Shape::Cylinder {
                base,
                axis,
                radius,
                height,
            } => {
                let a = axis.index();
                let t = p[a] - base[a];
                if !(0.0..=*height).contains(&t) {
                    return false;
                }
                let mut r2 = 0.0;
                for i in 0..3 {
                    if i != a {
                        r2 += (p[i] - base[i]).powi(2);
                    }
                }
                r2 <= radius * radius
            }
        }
    }

    /// The ray's parameter interval `(t_in, t_out)` inside the shape
    /// (`dir` a unit vector), or `None` if the line misses it. `t_in` may
    /// be negative (the origin is inside).
    pub fn ray_interval(&self, o: [f64; 3], d: [f64; 3]) -> Option<(f64, f64)> {
        match self {
            Shape::Box { min, max } => {
                let mut t0 = f64::NEG_INFINITY;
                let mut t1 = f64::INFINITY;
                for i in 0..3 {
                    if !slab(o[i], d[i], min[i], max[i], &mut t0, &mut t1) {
                        return None;
                    }
                }
                Some((t0, t1))
            }
            Shape::Sphere { center, radius } => {
                let oc = sub(o, *center);
                let b = dot(oc, d);
                let c = dot(oc, oc) - radius * radius;
                let disc = b * b - c;
                if disc <= 0.0 {
                    return None;
                }
                let s = disc.sqrt();
                Some((-b - s, -b + s))
            }
            Shape::Cylinder {
                base,
                axis,
                radius,
                height,
            } => {
                let a = axis.index();
                let mut t0 = f64::NEG_INFINITY;
                let mut t1 = f64::INFINITY;
                if !slab(o[a], d[a], base[a], base[a] + height, &mut t0, &mut t1) {
                    return None;
                }
                // Infinite-cylinder interval in the plane ⟂ axis.
                let (i, j) = match a {
                    0 => (1, 2),
                    1 => (0, 2),
                    _ => (0, 1),
                };
                let (ox, oy) = (o[i] - base[i], o[j] - base[j]);
                let (dx, dy) = (d[i], d[j]);
                let qa = dx * dx + dy * dy;
                let qc = ox * ox + oy * oy - radius * radius;
                if qa < 1.0e-300 {
                    // Flight parallel to the axis: inside the disc or not.
                    if qc > 0.0 {
                        return None;
                    }
                } else {
                    let qb = ox * dx + oy * dy;
                    let disc = qb * qb - qa * qc;
                    if disc <= 0.0 {
                        return None;
                    }
                    let s = disc.sqrt();
                    t0 = t0.max((-qb - s) / qa);
                    t1 = t1.min((-qb + s) / qa);
                }
                if t0 < t1 {
                    Some((t0, t1))
                } else {
                    None
                }
            }
        }
    }

    /// Axis-aligned bounding box `(min, max)`, cm.
    pub fn aabb(&self) -> ([f64; 3], [f64; 3]) {
        match self {
            Shape::Box { min, max } => (*min, *max),
            Shape::Sphere { center, radius } => (
                [center[0] - radius, center[1] - radius, center[2] - radius],
                [center[0] + radius, center[1] + radius, center[2] + radius],
            ),
            Shape::Cylinder {
                base,
                axis,
                radius,
                height,
            } => {
                let a = axis.index();
                let mut lo = [base[0] - radius, base[1] - radius, base[2] - radius];
                let mut hi = [base[0] + radius, base[1] + radius, base[2] + radius];
                lo[a] = base[a];
                hi[a] = base[a] + height;
                (lo, hi)
            }
        }
    }

    /// Exact volume, cm³ (of the primitive itself, ignoring priority).
    pub fn volume_cc(&self) -> f64 {
        match self {
            Shape::Box { min, max } => (0..3).map(|i| max[i] - min[i]).product(),
            Shape::Sphere { radius, .. } => 4.0 / 3.0 * std::f64::consts::PI * radius.powi(3),
            Shape::Cylinder { radius, height, .. } => {
                std::f64::consts::PI * radius * radius * height
            }
        }
    }

    fn validate(&self) -> bool {
        let finite = |v: &[f64]| v.iter().all(|x| x.is_finite());
        match self {
            Shape::Box { min, max } => {
                finite(min) && finite(max) && (0..3).all(|i| max[i] > min[i])
            }
            Shape::Sphere { center, radius } => {
                finite(center) && radius.is_finite() && *radius > 0.0
            }
            Shape::Cylinder {
                base,
                radius,
                height,
                ..
            } => {
                finite(base)
                    && radius.is_finite()
                    && *radius > 0.0
                    && height.is_finite()
                    && *height > 0.0
            }
        }
    }
}

/// Slab clip: narrow `[t0, t1]` to the ray's interval inside
/// `lo ≤ o + t·d ≤ hi`; false when empty.
fn slab(o: f64, d: f64, lo: f64, hi: f64, t0: &mut f64, t1: &mut f64) -> bool {
    if d.abs() < 1.0e-300 {
        return o >= lo && o <= hi;
    }
    let (mut a, mut b) = ((lo - o) / d, (hi - o) / d);
    if a > b {
        std::mem::swap(&mut a, &mut b);
    }
    *t0 = t0.max(a);
    *t1 = t1.min(b);
    *t0 < *t1
}

pub(crate) fn sub(a: [f64; 3], b: [f64; 3]) -> [f64; 3] {
    [a[0] - b[0], a[1] - b[1], a[2] - b[2]]
}

pub(crate) fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}

pub(crate) fn dist2(a: [f64; 3], b: [f64; 3]) -> f64 {
    let d = sub(a, b);
    dot(d, d)
}

/// One material cell.
#[derive(Debug, Clone)]
pub struct Cell {
    /// Label (unique within the scene; tallies are reported by it).
    pub name: String,
    /// The convex primitive.
    pub shape: Shape,
    /// Fill material.
    pub material: Material,
}

impl Cell {
    /// Convenience constructor.
    pub fn new(name: impl Into<String>, shape: Shape, material: Material) -> Self {
        Cell {
            name: name.into(),
            shape,
            material,
        }
    }
}

/// Scene validation failures (fail-closed).
#[derive(Debug, Clone, PartialEq)]
pub enum SceneError {
    /// No cells.
    Empty,
    /// A primitive had non-finite or non-positive dimensions.
    BadShape(String),
    /// Two cells share a name.
    DuplicateName(String),
    /// A cell pokes outside the bounding (last) cell — that region
    /// would silently leak.
    OutsideWorld(String),
}

impl std::fmt::Display for SceneError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            SceneError::Empty => write!(f, "scene has no cells"),
            SceneError::BadShape(n) => write!(f, "cell {n:?} has invalid dimensions"),
            SceneError::DuplicateName(n) => write!(f, "duplicate cell name {n:?}"),
            SceneError::OutsideWorld(n) => write!(
                f,
                "cell {n:?} extends outside the bounding (last) cell — the last cell must \
                 enclose every other cell"
            ),
        }
    }
}

impl std::error::Error for SceneError {}

/// An ordered (priority) list of cells; the last cell bounds the world.
#[derive(Debug, Clone)]
pub struct Scene {
    /// Cells in priority order (first match wins); the last is the world.
    pub cells: Vec<Cell>,
}

impl Scene {
    /// Construct and validate.
    pub fn new(cells: Vec<Cell>) -> Result<Scene, SceneError> {
        let s = Scene { cells };
        s.validate()?;
        Ok(s)
    }

    /// Fail-closed validation (see module docs).
    pub fn validate(&self) -> Result<(), SceneError> {
        let world = self.cells.last().ok_or(SceneError::Empty)?;
        let mut names = std::collections::BTreeSet::new();
        for c in &self.cells {
            if !c.shape.validate() {
                return Err(SceneError::BadShape(c.name.clone()));
            }
            if !names.insert(c.name.as_str()) {
                return Err(SceneError::DuplicateName(c.name.clone()));
            }
        }
        // Tolerance: 1 µm, so flush faces (wall touching the world) pass.
        let tol = 1.0e-4;
        for c in &self.cells[..self.cells.len() - 1] {
            if !inside_world(&world.shape, &c.shape, tol) {
                return Err(SceneError::OutsideWorld(c.name.clone()));
            }
        }
        Ok(())
    }

    /// Index of the cell containing `p` (cm), or `None` outside the world.
    pub fn locate(&self, p: [f64; 3]) -> Option<usize> {
        self.cells.iter().position(|c| c.shape.contains(p))
    }

    /// Distance (cm) along unit `d` from `p` to the nearest surface of
    /// any primitive (strictly ahead, > `eps`). Surfaces of hidden
    /// lower-priority cells may also stop the flight; that is harmless
    /// (the walker relocates into the same cell and resamples — flights
    /// are memoryless).
    pub fn distance_to_surface(&self, p: [f64; 3], d: [f64; 3], eps: f64) -> f64 {
        let mut best = f64::INFINITY;
        for c in &self.cells {
            if let Some((t0, t1)) = c.shape.ray_interval(p, d) {
                if t0 > eps && t0 < best {
                    best = t0;
                } else if t1 > eps && t1 < best {
                    best = t1;
                }
            }
        }
        best
    }

    /// Index of a cell by name.
    pub fn cell_index(&self, name: &str) -> Option<usize> {
        self.cells.iter().position(|c| c.name == name)
    }
}

/// Is convex `cell` inside convex `world`? Checks the cell's extreme
/// points: box corners; for a sphere world, sphere-in-sphere and the two
/// rim circles of a cylinder exactly. A cylindrical world falls back to
/// the cell's bounding-box corners (sufficient, conservative).
fn inside_world(world: &Shape, cell: &Shape, tol: f64) -> bool {
    let corners_inside = |s: &Shape| {
        let (lo, hi) = s.aabb();
        (0..8).all(|k| {
            let p = [
                if k & 1 == 0 { lo[0] } else { hi[0] },
                if k & 2 == 0 { lo[1] } else { hi[1] },
                if k & 4 == 0 { lo[2] } else { hi[2] },
            ];
            world_contains_tol(world, p, tol)
        })
    };
    match (world, cell) {
        (
            Shape::Sphere {
                center: wc,
                radius: wr,
            },
            Shape::Sphere { center, radius },
        ) => dist2(*wc, *center).sqrt() + radius <= wr + tol,
        (
            Shape::Sphere {
                center: wc,
                radius: wr,
            },
            Shape::Cylinder {
                base,
                axis,
                radius,
                height,
            },
        ) => {
            let a = axis.index();
            [0.0, *height].iter().all(|&t| {
                let mut q = *base;
                q[a] += t;
                let rel = sub(q, *wc);
                let along = rel[a];
                let perp: f64 = (0..3)
                    .filter(|&i| i != a)
                    .map(|i| rel[i] * rel[i])
                    .sum::<f64>()
                    .sqrt();
                (along * along + (perp + radius).powi(2)).sqrt() <= wr + tol
            })
        }
        _ => corners_inside(cell),
    }
}

fn world_contains_tol(world: &Shape, p: [f64; 3], tol: f64) -> bool {
    match world {
        Shape::Box { min, max } => (0..3).all(|i| p[i] >= min[i] - tol && p[i] <= max[i] + tol),
        Shape::Sphere { center, radius } => dist2(p, *center).sqrt() <= radius + tol,
        Shape::Cylinder {
            base,
            axis,
            radius,
            height,
        } => {
            let a = axis.index();
            let t = p[a] - base[a];
            if t < -tol || t > height + tol {
                return false;
            }
            let r2: f64 = (0..3)
                .filter(|&i| i != a)
                .map(|i| (p[i] - base[i]).powi(2))
                .sum();
            r2.sqrt() <= radius + tol
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::materials::{air, concrete, Material};

    #[test]
    fn primitive_intervals() {
        let b = Shape::box_mm([0.0; 3], [100.0; 3]);
        let (t0, t1) = b.ray_interval([-5.0, 5.0, 5.0], [1.0, 0.0, 0.0]).unwrap();
        assert!((t0 - 5.0).abs() < 1e-12 && (t1 - 15.0).abs() < 1e-12);
        let s = Shape::sphere_mm([0.0; 3], 100.0);
        let (t0, t1) = s.ray_interval([0.0; 3], [0.0, 0.0, 1.0]).unwrap();
        assert!((t0 + 10.0).abs() < 1e-12 && (t1 - 10.0).abs() < 1e-12);
        let c = Shape::cylinder_mm([0.0; 3], Axis::Z, 50.0, 200.0);
        // Horizontal ray through the axis at mid-height: chord = diameter.
        let (t0, t1) = c.ray_interval([-10.0, 0.0, 10.0], [1.0, 0.0, 0.0]).unwrap();
        assert!((t1 - t0 - 10.0).abs() < 1e-12);
        // Vertical ray inside the disc: clipped by the caps.
        let (t0, t1) = c.ray_interval([1.0, 1.0, -3.0], [0.0, 0.0, 1.0]).unwrap();
        assert!((t0 - 3.0).abs() < 1e-12 && (t1 - 23.0).abs() < 1e-12);
        // Miss.
        assert!(c.ray_interval([20.0, 0.0, 10.0], [0.0, 1.0, 0.0]).is_none());
        assert!(c.contains([0.0, 0.0, 10.0]) && !c.contains([0.0, 0.0, 21.0]));
    }

    #[test]
    fn priority_locate_and_surfaces() {
        let scene = Scene::new(vec![
            Cell::new("hole", Shape::sphere_mm([0.0; 3], 100.0), Material::void()),
            Cell::new("wall", Shape::sphere_mm([0.0; 3], 200.0), concrete()),
            Cell::new("world", Shape::box_centered_mm([0.0; 3], [500.0; 3]), air()),
        ])
        .unwrap();
        assert_eq!(scene.locate([0.0; 3]), Some(0));
        assert_eq!(scene.locate([15.0, 0.0, 0.0]), Some(1));
        assert_eq!(scene.locate([45.0, 0.0, 0.0]), Some(2));
        assert_eq!(scene.locate([55.0, 0.0, 0.0]), None);
        let d = scene.distance_to_surface([0.0; 3], [1.0, 0.0, 0.0], 1e-9);
        assert!((d - 10.0).abs() < 1e-12);
        let d = scene.distance_to_surface([10.0 + 1e-7, 0.0, 0.0], [1.0, 0.0, 0.0], 1e-9);
        assert!((d - 10.0).abs() < 1e-6);
    }

    #[test]
    fn validation_fails_closed() {
        assert_eq!(Scene::new(vec![]).unwrap_err(), SceneError::Empty);
        let err = Scene::new(vec![
            Cell::new("big", Shape::sphere_mm([0.0; 3], 600.0), air()),
            Cell::new("world", Shape::box_centered_mm([0.0; 3], [500.0; 3]), air()),
        ])
        .unwrap_err();
        assert_eq!(err, SceneError::OutsideWorld("big".into()));
        let err = Scene::new(vec![
            Cell::new("a", Shape::sphere_mm([0.0; 3], 10.0), air()),
            Cell::new("a", Shape::sphere_mm([0.0; 3], 20.0), air()),
        ])
        .unwrap_err();
        assert_eq!(err, SceneError::DuplicateName("a".into()));
        let err = Scene::new(vec![Cell::new(
            "w",
            Shape::sphere_mm([0.0; 3], -1.0),
            air(),
        )])
        .unwrap_err();
        assert_eq!(err, SceneError::BadShape("w".into()));
    }
}

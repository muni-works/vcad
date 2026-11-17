//! Ergonomic helper for authoring parts without hand-rolling [`vcad_ir::Document`] node maps.
//!
//! A [`Builder`] auto-assigns [`vcad_ir::NodeId`]s, tracks the root node, and
//! exposes chainable primitive/boolean/transform methods. Each builder call
//! returns the new node's id for composition.

use vcad_ir::{CsgOp, Document, Node, NodeId, SceneEntry, SketchSegment2D, Vec2, Vec3};

/// Fluent [`Document`] builder.
pub struct Builder {
    doc: Document,
    next_id: NodeId,
}

impl Default for Builder {
    fn default() -> Self {
        Self::new()
    }
}

impl Builder {
    /// Create a fresh builder with an empty document.
    pub fn new() -> Self {
        Self {
            doc: Document::new(),
            next_id: 1,
        }
    }

    fn add(&mut self, op: CsgOp) -> NodeId {
        let id = self.next_id;
        self.next_id += 1;
        self.doc.nodes.insert(id, Node { id, name: None, op });
        id
    }

    /// Axis-aligned box with its corner at the origin, extending to
    /// `(sx, sy, sz)` (the `CsgOp::Cube` convention).
    pub fn cube(&mut self, sx: f64, sy: f64, sz: f64) -> NodeId {
        self.add(CsgOp::Cube {
            size: Vec3::new(sx, sy, sz),
        })
    }

    /// Cylinder along the Z axis.
    pub fn cylinder(&mut self, radius: f64, height: f64) -> NodeId {
        self.add(CsgOp::Cylinder {
            radius,
            height,
            segments: 0,
        })
    }

    /// Cylinder with an explicit segment count (e.g. 6 for hex-looking profile).
    pub fn cylinder_segments(&mut self, radius: f64, height: f64, segments: u32) -> NodeId {
        self.add(CsgOp::Cylinder {
            radius,
            height,
            segments,
        })
    }

    /// Cone along the Z axis.
    pub fn cone(&mut self, r_bottom: f64, r_top: f64, height: f64) -> NodeId {
        self.add(CsgOp::Cone {
            radius_bottom: r_bottom,
            radius_top: r_top,
            height,
            segments: 0,
        })
    }

    /// Sphere at origin.
    pub fn sphere(&mut self, radius: f64) -> NodeId {
        self.add(CsgOp::Sphere {
            radius,
            segments: 0,
        })
    }

    /// Boolean union.
    pub fn union(&mut self, left: NodeId, right: NodeId) -> NodeId {
        self.add(CsgOp::Union { left, right })
    }

    /// Boolean difference (left minus right).
    pub fn difference(&mut self, left: NodeId, right: NodeId) -> NodeId {
        self.add(CsgOp::Difference { left, right })
    }

    /// Boolean intersection.
    pub fn intersection(&mut self, left: NodeId, right: NodeId) -> NodeId {
        self.add(CsgOp::Intersection { left, right })
    }

    /// Translate a node by an offset.
    pub fn translate(&mut self, child: NodeId, dx: f64, dy: f64, dz: f64) -> NodeId {
        self.add(CsgOp::Translate {
            child,
            offset: Vec3::new(dx, dy, dz),
        })
    }

    /// Rotate a node by Euler angles (degrees, XYZ order).
    pub fn rotate(&mut self, child: NodeId, rx: f64, ry: f64, rz: f64) -> NodeId {
        self.add(CsgOp::Rotate {
            child,
            angles: Vec3::new(rx, ry, rz),
        })
    }

    /// Linear pattern along a direction.
    pub fn linear_pattern(&mut self, child: NodeId, dir: Vec3, count: u32, spacing: f64) -> NodeId {
        self.add(CsgOp::LinearPattern {
            child,
            direction: dir,
            count,
            spacing,
        })
    }

    /// Circular pattern around an axis.
    pub fn circular_pattern(
        &mut self,
        child: NodeId,
        axis_origin: Vec3,
        axis_dir: Vec3,
        count: u32,
        angle_deg: f64,
    ) -> NodeId {
        self.add(CsgOp::CircularPattern {
            child,
            axis_origin,
            axis_dir,
            count,
            angle_deg,
        })
    }

    /// Closed circle as two arcs (the kernel keeps it a true circle).
    pub fn circle_loop(cx: f64, cy: f64, r: f64) -> Vec<SketchSegment2D> {
        let a = Vec2 { x: cx + r, y: cy };
        let b = Vec2 { x: cx - r, y: cy };
        let c = Vec2 { x: cx, y: cy };
        vec![
            SketchSegment2D::Arc {
                start: a,
                end: b,
                center: c,
                ccw: true,
            },
            SketchSegment2D::Arc {
                start: b,
                end: a,
                center: c,
                ccw: true,
            },
        ]
    }

    /// Closed polygon loop through `pts`.
    pub fn polygon_loop(pts: &[[f64; 2]]) -> Vec<SketchSegment2D> {
        (0..pts.len())
            .map(|i| {
                let p = pts[i];
                let q = pts[(i + 1) % pts.len()];
                SketchSegment2D::Line {
                    start: Vec2 { x: p[0], y: p[1] },
                    end: Vec2 { x: q[0], y: q[1] },
                }
            })
            .collect()
    }

    /// Sketch on the plane `z = z0` (local XY), with optional hole loops.
    pub fn sketch_xy(
        &mut self,
        z0: f64,
        outer: Vec<SketchSegment2D>,
        holes: Vec<Vec<SketchSegment2D>>,
    ) -> NodeId {
        self.add(CsgOp::Sketch2D {
            origin: Vec3::new(0.0, 0.0, z0),
            x_dir: Vec3::new(1.0, 0.0, 0.0),
            y_dir: Vec3::new(0.0, 1.0, 0.0),
            segments: outer,
            holes: (!holes.is_empty()).then_some(holes),
        })
    }

    /// Extrude a sketch along +Z by `dz`.
    pub fn extrude(&mut self, sketch: NodeId, dz: f64) -> NodeId {
        self.add(CsgOp::Extrude {
            sketch,
            direction: Vec3::new(0.0, 0.0, dz),
            twist_angle: None,
            scale_end: None,
        })
    }

    /// Revolve a closed `(r, z)` profile 360° about the Z axis. The profile
    /// lives in the XZ half-plane `x ≥ 0`.
    pub fn revolve_z(&mut self, profile_rz: &[[f64; 2]]) -> NodeId {
        let sk = self.add(CsgOp::Sketch2D {
            origin: Vec3::new(0.0, 0.0, 0.0),
            x_dir: Vec3::new(1.0, 0.0, 0.0),
            y_dir: Vec3::new(0.0, 0.0, 1.0),
            segments: Self::polygon_loop(profile_rz),
            holes: None,
        });
        self.add(CsgOp::Revolve {
            sketch: sk,
            axis_origin: Vec3::new(0.0, 0.0, 0.0),
            axis_dir: Vec3::new(0.0, 0.0, 1.0),
            angle_deg: 360.0,
        })
    }

    /// Fillet edges.
    pub fn fillet(&mut self, child: NodeId, radius: f64) -> NodeId {
        self.add(CsgOp::Fillet { child, radius })
    }

    /// Chamfer edges.
    pub fn chamfer(&mut self, child: NodeId, distance: f64) -> NodeId {
        self.add(CsgOp::Chamfer { child, distance })
    }

    /// Finalize the builder: mark `root` as the scene root with the given
    /// material name and return the document.
    pub fn finish(mut self, root: NodeId, material: &str) -> Document {
        self.doc.roots.push(SceneEntry {
            root,
            material: material.to_string(),
            visible: None,
        });
        self.doc
    }
}

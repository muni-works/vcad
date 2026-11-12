//! Slim kernel bindings for the vcad.io landing demos.
//!
//! The full `vcad-kernel-wasm` bundle (~35 MB) carries physics, ECAD, CAM,
//! slicers and every analysis solver — none of which the landing page's
//! hero reel, engine cards or prove demo call. This crate exposes exactly
//! the surface those demos use, with the same JS names, so the site can
//! swap bundles without code changes:
//!
//! - `Solid`: `cube`, `cylinder`, `sphere`, `union`, `difference`,
//!   `intersection`, `translate`, `rotate`, `scale`, `chamfer`, `fillet`,
//!   `shell`, `linearPattern`, `circularPattern`, `getMesh`, `volume`
//! - `evaluateSheetMetalChain`, `atoms_parse_xyz`, `get_kernel_version`
#![warn(missing_docs)]

use wasm_bindgen::prelude::*;

/// The crate version, for "which kernel is this?" in the console.
#[wasm_bindgen]
pub fn get_kernel_version() -> String {
    env!("CARGO_PKG_VERSION").to_string()
}

/// Evaluate a sheet-metal op chain → mesh, flat pattern, summary (JSON).
#[wasm_bindgen(js_name = evaluateSheetMetalChain)]
pub fn evaluate_sheet_metal_chain(chain_json: &str) -> String {
    vcad_kernel::sheet_json::evaluate_sheet_metal_chain(chain_json)
}

/// Parse XYZ / extended-XYZ text into a `MoleculeSystem` JSON string.
#[wasm_bindgen]
pub fn atoms_parse_xyz(text: &str) -> Result<String, JsValue> {
    let err = |e: &dyn std::fmt::Display| JsValue::from_str(&e.to_string());
    let mol = vcad_kernel_atoms::io::parse_xyz(text).map_err(|e| err(&e))?;
    serde_json::to_string(&mol).map_err(|e| err(&e))
}

/// A 3D solid.
#[wasm_bindgen]
pub struct Solid {
    inner: vcad_kernel::Solid,
}

fn wrap(inner: vcad_kernel::Solid) -> Solid {
    Solid { inner }
}

fn js_err(what: &str, e: impl std::fmt::Display) -> JsError {
    JsError::new(&format!("{what}: {e}"))
}

#[wasm_bindgen]
impl Solid {
    /// Box with a corner at the origin.
    pub fn cube(sx: f64, sy: f64, sz: f64) -> Solid {
        wrap(vcad_kernel::Solid::cube(sx, sy, sz))
    }

    /// Cylinder along +Z.
    pub fn cylinder(radius: f64, height: f64, segments: Option<u32>) -> Solid {
        wrap(vcad_kernel::Solid::cylinder(
            radius,
            height,
            segments.unwrap_or(32),
        ))
    }

    /// Sphere centred at the origin.
    pub fn sphere(radius: f64, segments: Option<u32>) -> Solid {
        wrap(vcad_kernel::Solid::sphere(radius, segments.unwrap_or(32)))
    }

    /// Boolean union.
    pub fn union(&self, other: &Solid) -> Result<Solid, JsError> {
        self.inner
            .try_union(&other.inner)
            .map(wrap)
            .map_err(|e| js_err("union", e))
    }

    /// Boolean difference (self − other).
    pub fn difference(&self, other: &Solid) -> Result<Solid, JsError> {
        self.inner
            .try_difference(&other.inner)
            .map(wrap)
            .map_err(|e| js_err("difference", e))
    }

    /// Boolean intersection.
    pub fn intersection(&self, other: &Solid) -> Result<Solid, JsError> {
        self.inner
            .try_intersection(&other.inner)
            .map(wrap)
            .map_err(|e| js_err("intersection", e))
    }

    /// Translate by (x, y, z).
    pub fn translate(&self, x: f64, y: f64, z: f64) -> Solid {
        wrap(self.inner.translate(x, y, z))
    }

    /// Rotate by Euler angles in degrees.
    pub fn rotate(&self, x_deg: f64, y_deg: f64, z_deg: f64) -> Solid {
        wrap(self.inner.rotate(x_deg, y_deg, z_deg))
    }

    /// Scale by (x, y, z).
    pub fn scale(&self, x: f64, y: f64, z: f64) -> Solid {
        wrap(self.inner.scale(x, y, z))
    }

    /// Chamfer every edge.
    pub fn chamfer(&self, distance: f64) -> Result<Solid, JsError> {
        self.inner
            .chamfer(distance)
            .map(wrap)
            .map_err(|e| js_err("chamfer", e))
    }

    /// Fillet every edge.
    pub fn fillet(&self, radius: f64) -> Result<Solid, JsError> {
        self.inner
            .fillet(radius)
            .map(wrap)
            .map_err(|e| js_err("fillet", e))
    }

    /// Hollow inward by `thickness`.
    pub fn shell(&self, thickness: f64) -> Result<Solid, JsError> {
        self.inner
            .shell(thickness)
            .map(wrap)
            .map_err(|e| js_err("shell", e))
    }

    /// Linear pattern: `count` copies `spacing` apart along a direction.
    #[wasm_bindgen(js_name = linearPattern)]
    pub fn linear_pattern(&self, dx: f64, dy: f64, dz: f64, count: u32, spacing: f64) -> Solid {
        use vcad_kernel::vcad_kernel_math::Vec3;
        wrap(
            self.inner
                .linear_pattern(Vec3::new(dx, dy, dz), count, spacing),
        )
    }

    /// Circular pattern about an axis.
    #[wasm_bindgen(js_name = circularPattern)]
    #[allow(clippy::too_many_arguments)]
    pub fn circular_pattern(
        &self,
        ox: f64,
        oy: f64,
        oz: f64,
        ax: f64,
        ay: f64,
        az: f64,
        count: u32,
        angle_deg: f64,
    ) -> Solid {
        use vcad_kernel::vcad_kernel_math::{Point3, Vec3};
        wrap(self.inner.circular_pattern(
            Point3::new(ox, oy, oz),
            Vec3::new(ax, ay, az),
            count,
            angle_deg,
        ))
    }

    /// Triangle mesh: `{ positions: Float32Array, indices: Uint32Array,
    /// normals?: Float32Array }`, with creased render normals baked in.
    #[wasm_bindgen(js_name = getMesh)]
    pub fn get_mesh(&self, segments: Option<u32>) -> Result<JsValue, JsValue> {
        let mut mesh = self.inner.to_mesh(segments.unwrap_or(32));
        vcad_kernel_tessellate::render_bake_default(&mut mesh);
        let n = mesh.vertices.len() / 3;
        mesh.indices = mesh
            .indices
            .as_chunks::<3>()
            .0
            .iter()
            .filter(|t| t.iter().all(|&i| (i as usize) < n))
            .flatten()
            .copied()
            .collect();
        let out = js_sys::Object::new();
        let set = |k: &str, v: &JsValue| js_sys::Reflect::set(&out, &k.into(), v).map(|_| ());
        set(
            "positions",
            &js_sys::Float32Array::from(&mesh.vertices[..]).into(),
        )?;
        set(
            "indices",
            &js_sys::Uint32Array::from(&mesh.indices[..]).into(),
        )?;
        if mesh.normals.len() == mesh.vertices.len() {
            set(
                "normals",
                &js_sys::Float32Array::from(&mesh.normals[..]).into(),
            )?;
        }
        Ok(out.into())
    }

    /// Volume in mm³.
    pub fn volume(&self) -> f64 {
        self.inner.volume()
    }
}

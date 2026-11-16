//! Modal frequencies as optimizer QoIs: the `vcad_eval::optimize` plug-in
//! for "this part must ring above 400 Hz".
//!
//! [`ModalFrequencyQoi`] reads natural frequency `n` of a part from its
//! triangle mesh alone, so it is a *mesh-only* QoI: the optimizer reads it on
//! the seam's frozen tessellation and central-differences it along the
//! seam's exact mesh velocity — each probe is the same mesh moved by
//! `±h·dx/dθ`, with no document re-evaluation (provenance `seam_fd(step)`).
//! There is no shape adjoint for the voxel modal solver wired to document
//! parameters yet, so this route is honest about being a difference.
//!
//! **Pinned discretization.** The first evaluation fixes the solver's cell
//! size for the rest of the run ([`crate::part_modes_mesh_pinned`]). Without
//! that, every probe re-derives the pitch from its own geometry and the
//! cell-size jump dominates the difference quotient.

use std::sync::Mutex;

use vcad_ir::Document;

use vcad_eval::optimize::{QoiInput, ScalarQoi};

use crate::{modal_discretization, part_modes_mesh_pinned, ModesRequest, Quality};

/// Natural frequency `mode + 1` (1-based in the name: `modal_f1` is the
/// first elastic mode) of a visible part, free-free, in Hz.
pub struct ModalFrequencyQoi {
    name: String,
    mode: usize,
    quality: Quality,
    material: Option<String>,
    pinned: Mutex<Option<(usize, [f64; 3])>>,
}

impl ModalFrequencyQoi {
    /// The first elastic mode, registered as `modal_f1`.
    pub fn first() -> Self {
        Self::nth(0)
    }

    /// Elastic mode `mode` (0-based), registered as `modal_f{mode+1}`.
    pub fn nth(mode: usize) -> Self {
        Self {
            name: format!("modal_f{}", mode + 1),
            mode,
            quality: Quality::Preview,
            material: None,
            pinned: Mutex::new(None),
        }
    }

    /// Solver fidelity (default [`Quality::Preview`]).
    pub fn quality(mut self, q: Quality) -> Self {
        self.quality = q;
        self
    }

    /// Material override (key or name), else the part's own.
    pub fn material(mut self, key: impl Into<String>) -> Self {
        self.material = Some(key.into());
        self
    }

    /// Pin the discretization explicitly instead of from the first call.
    pub fn pinned(self, resolution: usize, pitch_mm: [f64; 3]) -> Self {
        *self.pinned.lock().unwrap_or_else(|e| e.into_inner()) = Some((resolution, pitch_mm));
        self
    }

    /// Frequency on an explicit mesh of scene part `part`, pinning the
    /// discretization on the first call.
    fn solve(
        &self,
        doc: &Document,
        part: usize,
        positions: &[f32],
        indices: &[u32],
    ) -> Result<f64, String> {
        if indices.is_empty() {
            return Err(format!("part {part} has an empty mesh"));
        }
        let pin = {
            let mut g = self.pinned.lock().unwrap_or_else(|e| e.into_inner());
            *g.get_or_insert_with(|| modal_discretization(positions, indices, self.quality))
        };
        let req = ModesRequest {
            part,
            modes: self.mode + 1,
            quality: self.quality,
            material: self.material.clone(),
            shapes: false,
        };
        let r = part_modes_mesh_pinned(doc, &req, positions, indices, Some(pin))
            .map_err(|e| e.to_string())?;
        r.modes.get(self.mode).map(|m| m.hz).ok_or_else(|| {
            format!(
                "solver returned {} modes, wanted {}",
                r.modes.len(),
                self.mode + 1
            )
        })
    }
}

impl ScalarQoi for ModalFrequencyQoi {
    fn name(&self) -> &str {
        &self.name
    }

    /// `part` indexes the scene's visible parts (default 0).
    fn evaluate(&self, input: &QoiInput, part: Option<usize>) -> Result<f64, String> {
        let idx = part.unwrap_or(0);
        let p = input
            .scene
            .parts
            .get(idx)
            .ok_or_else(|| format!("no part {idx}"))?;
        self.solve(input.doc, idx, &p.mesh.positions, &p.mesh.indices)
    }

    fn mesh_only(&self) -> bool {
        true
    }

    fn evaluate_mesh(
        &self,
        doc: &Document,
        part: Option<usize>,
        positions: &[f32],
        indices: &[u32],
    ) -> Result<f64, String> {
        self.solve(doc, part.unwrap_or(0), positions, indices)
    }
}

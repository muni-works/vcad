//! Modal frequencies as optimizer QoIs: the `vcad_eval::optimize` plug-in
//! for "this part must ring above 400 Hz".
//!
//! [`ModalFrequencyQoi`] reads natural frequency `n` of a part from the
//! scene the optimizer already evaluated (no second evaluation), and the
//! optimizer central-differences it over the named parameters — provenance
//! `fd(step)`. There is no shape adjoint for the voxel modal solver wired to
//! document parameters yet, so this route is honest about being FD.
//!
//! **Pinned discretization.** The first evaluation fixes the solver's cell
//! size for the rest of the run ([`crate::part_modes_mesh_pinned`]). Without
//! that, every probe re-derives the pitch from its own geometry and the
//! cell-size jump dominates the difference quotient.

use std::cell::RefCell;

use vcad_eval::optimize::{QoiInput, ScalarQoi};

use crate::{modal_discretization, part_modes_mesh_pinned, ModesRequest, Quality};

/// Natural frequency `mode + 1` (1-based in the name: `modal_f1` is the
/// first elastic mode) of a visible part, free-free, in Hz.
pub struct ModalFrequencyQoi {
    name: String,
    mode: usize,
    quality: Quality,
    material: Option<String>,
    pinned: RefCell<Option<(usize, [f64; 3])>>,
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
            pinned: RefCell::new(None),
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
        *self.pinned.borrow_mut() = Some((resolution, pitch_mm));
        self
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
        if p.mesh.indices.is_empty() {
            return Err(format!("part {idx} has an empty mesh"));
        }
        let pin = *self.pinned.borrow();
        let pin = match pin {
            Some(x) => x,
            None => {
                let x = modal_discretization(&p.mesh.positions, &p.mesh.indices, self.quality);
                *self.pinned.borrow_mut() = Some(x);
                x
            }
        };
        let req = ModesRequest {
            part: idx,
            modes: self.mode + 1,
            quality: self.quality,
            material: self.material.clone(),
            shapes: false,
        };
        let r = part_modes_mesh_pinned(
            input.doc,
            &req,
            &p.mesh.positions,
            &p.mesh.indices,
            Some(pin),
        )
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

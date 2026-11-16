//! One-call bass-reflex analysis of a ported box: lumped band, field-solved
//! tuning, port volume-velocity curve, and the `vcad.acoustics-claims/1`
//! claim set — the bundle the `simulate_cavity` tool returns.
//!
//! Document-agnostic: a [`PortedBoxModel`] is a [`CavitySpec`] (the field
//! geometry, possibly with named parameters) plus the lumped quantities the
//! field geometry alone doesn't name (the box compliance volume, the port
//! bore/length/mount) and a probe point in the box air. `vcad-eval`'s cavity
//! extractor measures one from a `.vcad` document; a caller can also write
//! one by hand.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::cavity::{Cavity, EndCondition};
use crate::complex::Cplx;
use crate::fom::port_volume_velocity;
use crate::helmholtz::{solve_driven, Source};
use crate::lumped::{ported_box_tuning_mount_mm, PortMount};
use crate::receipt::{predicted_claims_mount, ClaimSet, Provenance};
use crate::spec::{CavitySpec, SpecError};
use crate::sweep::{find_peaks, frequency_sweep, Probe};

/// A ported box ready to analyze.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortedBoxModel {
    /// Field geometry: driver piston on the bottom, open port mouth on top.
    pub spec: CavitySpec,
    /// Lumped compliance volume `V_box` — the box air, **excluding** the
    /// port's own air column and solid walls, mm³.
    pub box_volume_mm3: f64,
    /// Port bore radius, mm.
    pub port_radius_mm: f64,
    /// Port length (inner end to mouth), mm.
    pub port_length_mm: f64,
    /// How the port meets the box (sets which end correction is flanged).
    pub mount: PortMount,
    /// Probe point `[r, z]` (mm, spec frame) in the bulk box air, where the
    /// Helmholtz mode's pressure is strong.
    pub probe_mm: [f64; 2],
    /// Suggested grid node spacing, mm: fine enough that every obstacle
    /// wall holds a node.
    pub grid_mm: f64,
}

/// Analysis knobs. Every field has a default.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AnalysisOptions {
    /// Grid spacing override, mm (default: the model's `grid_mm`).
    pub grid_mm: Option<f64>,
    /// Tuning-search band `[f_lo, f_hi]`, Hz (default: 0.5·f_min … 2·f_max of
    /// the lumped band).
    pub search_hz: Option<[f64; 2]>,
    /// Coarse sweep points for the tuning search (refined around the peak).
    pub sweep_points: usize,
    /// Port volume-velocity curve band `[f_lo, f_hi]`, Hz (default: the
    /// search band).
    pub curve_hz: Option<[f64; 2]>,
    /// Port volume-velocity curve sample count.
    pub curve_points: usize,
    /// Refuse grids larger than this many nodes (fail-closed instead of
    /// running for minutes).
    pub max_nodes: usize,
}

impl Default for AnalysisOptions {
    fn default() -> Self {
        Self {
            grid_mm: None,
            search_hz: None,
            sweep_points: 48,
            curve_hz: None,
            curve_points: 60,
            max_nodes: 60_000,
        }
    }
}

/// The lumped tuning band, serializable.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct LumpedBand {
    /// Lowest plausible tuning (both ends flanged), Hz.
    pub f_min_hz: f64,
    /// Best-estimate tuning, Hz.
    pub f_nominal_hz: f64,
    /// Highest plausible tuning (interior end only), Hz.
    pub f_max_hz: f64,
}

/// One sample of the port volume-velocity curve (unit piston velocity).
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct PortVelocityPoint {
    /// Frequency, Hz.
    pub f_hz: f64,
    /// `|U_port|`, m³/s per 1 m/s of piston velocity.
    pub volume_velocity_m3_s: f64,
    /// `|U_port| / U_cone` — port output relative to the cone's own volume
    /// velocity (`π a_d² · 1 m/s`). Peaks at the box tuning.
    pub port_to_cone: f64,
}

/// Everything [`analyze`] produces.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PortedBoxReport {
    /// Lumped band (port mount-aware end corrections).
    pub lumped: LumpedBand,
    /// Field-solved tuning `f_b`, Hz (`None` if no peak in the search band).
    pub field_tuning_hz: Option<f64>,
    /// `field / lumped nominal`.
    pub field_over_nominal: Option<f64>,
    /// Grid used, `[nr, nz]`.
    pub grid: [usize; 2],
    /// Node spacing actually used, `[dr, dz]` mm.
    pub spacing_mm: [f64; 2],
    /// Total field-model fluid volume (box + port bore), mm³.
    pub fluid_volume_mm3: f64,
    /// Lumped `V_box`, mm³.
    pub box_volume_mm3: f64,
    /// Port volume-velocity curve.
    pub port_velocity: Vec<PortVelocityPoint>,
    /// Predicted claims (`vcad.acoustics-claims/1`).
    pub claims: ClaimSet,
    /// Model assumptions and limits, spelled out.
    pub assumptions: Vec<String>,
}

/// Analysis failures.
#[derive(Debug, Clone, PartialEq)]
pub enum AnalysisError {
    /// The spec failed to resolve against the parameters.
    Spec(SpecError),
    /// The model is not a driven ported box (piston bottom, open top).
    NotPortedBox(String),
    /// The grid would exceed `max_nodes`.
    GridTooLarge {
        /// Nodes requested.
        nodes: usize,
        /// The cap.
        max_nodes: usize,
    },
    /// A field solve failed outright (bad obstacle, …).
    Solve(String),
}

impl std::fmt::Display for AnalysisError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            AnalysisError::Spec(e) => write!(f, "{e}"),
            AnalysisError::NotPortedBox(s) => write!(f, "not a driven ported box: {s}"),
            AnalysisError::GridTooLarge { nodes, max_nodes } => write!(
                f,
                "grid would have {nodes} nodes (cap {max_nodes}); pass a coarser grid_mm \
                 (every obstacle wall must still hold a node) or raise max_nodes"
            ),
            AnalysisError::Solve(s) => write!(f, "field solve failed: {s}"),
        }
    }
}

impl std::error::Error for AnalysisError {}

fn grid_for(cav: &Cavity, h: f64) -> (usize, usize) {
    let (z0, z1) = cav.z_span_mm();
    let nr = (cav.r_max_mm() / h).ceil() as usize + 1;
    let nz = ((z1 - z0) / h).ceil() as usize + 1;
    (nr.max(3), nz.max(3))
}

/// Resolve and analyze a ported box.
pub fn analyze(
    model: &PortedBoxModel,
    params: &BTreeMap<String, f64>,
    opts: &AnalysisOptions,
) -> Result<PortedBoxReport, AnalysisError> {
    let cav = model.spec.resolve(params).map_err(AnalysisError::Spec)?;
    let driver_r = match cav.bottom {
        EndCondition::Piston { radius_mm } => radius_mm,
        _ => return Err(AnalysisError::NotPortedBox("bottom is not a piston".into())),
    };
    if cav.top != EndCondition::Open {
        return Err(AnalysisError::NotPortedBox(
            "top is not an open port mouth".into(),
        ));
    }
    let medium = cav.medium;
    let band = ported_box_tuning_mount_mm(
        &medium,
        model.box_volume_mm3,
        model.port_radius_mm,
        model.port_length_mm,
        model.mount,
    );
    let h = opts.grid_mm.unwrap_or(model.grid_mm);
    let (nr, nz) = grid_for(&cav, h);
    if nr * nz > opts.max_nodes {
        return Err(AnalysisError::GridTooLarge {
            nodes: nr * nz,
            max_nodes: opts.max_nodes,
        });
    }
    let (z0, z1) = cav.z_span_mm();
    let spacing = [
        cav.r_max_mm() / (nr - 1) as f64,
        (z1 - z0) / (nz - 1) as f64,
    ];
    let src = Source::Piston {
        velocity: Cplx::ONE,
    };
    // Fail fast on a structural problem (unresolved obstacle, no fluid)
    // before sweeping: probe one off-resonance solve.
    solve_driven(&cav, nr, nz, band.f_nominal_hz * 0.731, src)
        .map_err(|e| AnalysisError::Solve(e.to_string()))?;

    let [s_lo, s_hi] = opts
        .search_hz
        .unwrap_or([(0.5 * band.f_min_hz).max(1.0), 2.0 * band.f_max_hz]);
    let probe = Probe {
        r_mm: model.probe_mm[0],
        z_mm: model.probe_mm[1],
    };
    let n = opts.sweep_points.max(8);
    let coarse = frequency_sweep(&cav, nr, nz, src, probe, s_lo, s_hi, n);
    // `find_peaks` keeps the N *strongest* peaks (sorted by frequency), so ask
    // for several and take the lowest: the Helmholtz mode, not whichever
    // lossless pole happened to sample highest.
    let field_tuning_hz = find_peaks(&coarse, 8).first().map(|p| {
        // Refine on a fine sweep one coarse step either side.
        let step = (s_hi - s_lo) / (n - 1) as f64;
        let (lo, hi) = ((p.f_hz - step).max(s_lo * 0.5), p.f_hz + step);
        let fine = frequency_sweep(&cav, nr, nz, src, probe, lo, hi, 33);
        find_peaks(&fine, 8).first().map_or(p.f_hz, |q| q.f_hz)
    });

    let [c_lo, c_hi] = opts.curve_hz.unwrap_or([s_lo, s_hi]);
    let cone_u = std::f64::consts::PI * (driver_r * 1e-3).powi(2);
    let m = opts.curve_points.max(2);
    let port_velocity = (0..m)
        .filter_map(|i| {
            let f = c_lo + (c_hi - c_lo) * i as f64 / (m - 1) as f64;
            // Nudge off an exact pole like the sweep does.
            let fld = solve_driven(&cav, nr, nz, f, src)
                .or_else(|_| solve_driven(&cav, nr, nz, f * (1.0 + 1e-6), src))
                .ok()?;
            let u = port_volume_velocity(&fld, &cav).abs();
            Some(PortVelocityPoint {
                f_hz: f,
                volume_velocity_m3_s: u,
                port_to_cone: u / cone_u,
            })
        })
        .collect();

    let mut model_tags = vec![
        "linear".to_string(),
        "lossless".to_string(),
        "axisymmetric".to_string(),
        "pressure_release_mouth".to_string(),
        "rigid_walls".to_string(),
    ];
    if model.mount == PortMount::Internal {
        model_tags.push("internal_port".to_string());
    }
    let provenance = Provenance {
        grid: [nr, nz],
        sweep_hz: [s_lo, s_hi],
        sweep_points: n,
        sound_speed_m_s: medium.c,
        density_kg_m3: medium.rho,
        model: model_tags,
    };
    let modes: Vec<f64> = field_tuning_hz.into_iter().collect();
    let claims =
        predicted_claims_mount(provenance, model.mount, field_tuning_hz, band, &modes, &[]);

    let mut assumptions = vec![
        "Lossless linear acoustics: no thermoviscous, port or radiation damping, so the \
         field peak is unbounded and port Q reads optimistic."
            .to_string(),
        "Pressure-release (p = 0) port mouth: omits the exterior radiation mass, so the \
         field tuning reads above the lumped nominal (toward or past the band top)."
            .to_string(),
        "Rigid walls and a rigid flat piston: no wall compliance, no cone mass/suspension \
         (this is the box+port tuning f_b, not the driver-loaded system response)."
            .to_string(),
    ];
    if model.mount == PortMount::Internal {
        assumptions.push(
            "Internal port modelled exactly in the axisymmetric field (tube wall = rigid \
             annular obstacle, bore couples to the box air only at its hanging end); the \
             wall is discretised on the vertex grid with ±dr/2 face placement, so its \
             effective thickness and the bore radius carry an O(dr) error."
                .to_string(),
        );
    }

    Ok(PortedBoxReport {
        lumped: LumpedBand {
            f_min_hz: band.f_min_hz,
            f_nominal_hz: band.f_nominal_hz,
            f_max_hz: band.f_max_hz,
        },
        field_over_nominal: field_tuning_hz.map(|f| f / band.f_nominal_hz),
        field_tuning_hz,
        grid: [nr, nz],
        spacing_mm: spacing,
        fluid_volume_mm3: cav.volume_mm3(),
        box_volume_mm3: model.box_volume_mm3,
        port_velocity,
        claims,
        assumptions,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::spec::{EndSpec, ObstacleSpec, SegmentSpec};

    /// A ported box with a 30 mm internal port, coarse enough for a debug
    /// test but with the 2 mm wall still resolved (1 mm grid).
    fn small_internal() -> PortedBoxModel {
        let (r, h, top, rp, wall, lp) = (30.0, 60.0, 2.0, 6.0, 2.0, 30.0);
        let lit = |v: f64| v.into();
        PortedBoxModel {
            spec: CavitySpec {
                segments: vec![
                    SegmentSpec {
                        z0_mm: lit(0.0),
                        z1_mm: lit(h),
                        radius_mm: lit(r),
                    },
                    SegmentSpec {
                        z0_mm: lit(h),
                        z1_mm: lit(h + top),
                        radius_mm: lit(rp),
                    },
                ],
                obstacles: vec![ObstacleSpec {
                    r_in_mm: lit(rp),
                    r_out_mm: lit(rp + wall),
                    z0_mm: lit(h + top - lp),
                    z1_mm: lit(h),
                }],
                bottom: EndSpec::Piston {
                    radius_mm: lit(20.0),
                },
                top: EndSpec::Open,
                temp_c: lit(20.0),
            },
            box_volume_mm3: std::f64::consts::PI * (r * r * h - (rp + wall).powi(2) * (lp - top)),
            port_radius_mm: rp,
            port_length_mm: lp,
            mount: PortMount::Internal,
            probe_mm: [0.0, 5.0],
            grid_mm: 1.0,
        }
    }

    #[test]
    fn internal_port_field_tuning_lands_near_the_lumped_band() {
        let model = small_internal();
        let rep = analyze(
            &model,
            &BTreeMap::new(),
            &AnalysisOptions {
                sweep_points: 24,
                curve_points: 6,
                ..Default::default()
            },
        )
        .expect("analyze");
        let f = rep.field_tuning_hz.expect("a tuning peak");
        // Pressure-release mouth reads high; the M0 doc's ~15% figure for a
        // flush port. Accept band-low … 1.25 × band-high.
        assert!(
            f > rep.lumped.f_min_hz && f < 1.25 * rep.lumped.f_max_hz,
            "field {f} vs band {:?}",
            rep.lumped
        );
        assert_eq!(rep.claims.schema, crate::receipt::CLAIM_SCHEMA);
        assert!(rep.claims.claims.iter().any(|c| c.name == "tuning_hz"));
        assert_eq!(rep.port_velocity.len(), 6);
    }

    #[test]
    fn oversized_grid_fails_closed() {
        let model = small_internal();
        let err = analyze(
            &model,
            &BTreeMap::new(),
            &AnalysisOptions {
                grid_mm: Some(0.05),
                ..Default::default()
            },
        )
        .unwrap_err();
        assert!(matches!(err, AnalysisError::GridTooLarge { .. }));
    }
}

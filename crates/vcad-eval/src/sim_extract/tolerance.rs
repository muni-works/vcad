//! Document → [`StackupSpec`]: a labeled clearance assertion plus the
//! document's toleranced parameters become a linear tolerance stack.
//!
//! This is the adapter `vcad_kernel_tolerance::spec` describes as landing
//! "on the vcad side of the seam":
//!
//! - **The gap is measured, analytically.** The clearance assertion names
//!   the two part groups; the fit feature between them is measured from
//!   the evaluated BRep — a coaxial bore/shaft pair (`radial`: signed gap
//!   `r_bore − r_shaft`, negative = interference). Planar (axial) fits
//!   are not extracted yet. Mesh clearance is not used: its chord
//!   error is the size of the tolerances being priced, and it can't sign
//!   an interference.
//! - **The chain is derived, not written.** Each literal document
//!   parameter is perturbed to `p ± h` (h = its own tolerance) and the
//!   document re-evaluated; the central difference is the chain
//!   coefficient `∂gap/∂p`. A parameter the gap doesn't depend on drops
//!   out. A gap that isn't linear in a parameter over its tolerance band
//!   (`g(p+h) + g(p−h) ≠ 2·g(p)`) is refused — a linear stack would
//!   misprice it.
//! - **Every contributor is toleranced.** A parameter that moves the gap
//!   but declares no tolerance is an error unless the selection supplies a
//!   process default (FDM ±0.1 mm, say) — never silently exact.
//! - **The stack is homogeneous by construction.** Untoleranced geometry
//!   (literal dimensions not driven by parameters) is folded into one
//!   zero-tolerance `fixed geometry` contributor so the stack's nominal
//!   gap equals the measured gap exactly.
//! - **The requirement is the assertion.** `lower_mm` is the clearance
//!   assertion's `min_mm`, its optional `max_mm` the upper bound a press
//!   or transition fit needs (the selection's `upper_mm`/`lower_mm`
//!   override either), and its label names the requirement. A fit-mode
//!   assertion (`max_mm` or `fit: "interference"`) is re-verified on the
//!   receipt with this same analytic gap, so the receipt verdict and the
//!   stack talk about the same number; a tolerance claim then prices the
//!   probability that the assertion holds in production.
//!
//! Nominals are carried as [`ParamValue::Named`] so scrubbing a dimension
//! in the document re-prices the stack without re-extraction — valid while
//! the coefficients hold (they are constants for the linear geometry this
//! accepts).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use vcad_ir::{Document, Expr};
use vcad_kernel_geom::CylinderSurface;
use vcad_kernel_tolerance::dist::{DistributionSource, SigmaConvention};
use vcad_kernel_tolerance::spec::{
    ContributorSpec, DistSpec, ParamValue, RequirementSpec, StackupSpec,
};
use vcad_kernel_topo::Orientation;

use super::{
    canonical_axis, d3, dot, find_part, line_angle, off_axis, p3, resolve_env, surface_of,
    SimExtractError, Tolerance, V3,
};
use crate::{evaluate_document, EvaluatedScene};

/// Which fit feature between the two groups the stack is about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FitFeature {
    /// A coaxial shaft/bore pair (a lid in a rim, a pin in a hole):
    /// gap = r_bore − r_shaft.
    Radial,
}

/// Selection for [`extract_stackup_spec`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StackupExtraction {
    /// Label of the document's clearance assertion (`clearance_specs`).
    pub clearance: String,
    /// The fit feature to measure.
    pub feature: FitFeature,
    /// Only feature pairs whose |gap| is at most this are candidates (a
    /// fit is by definition close). Default 1.0 mm.
    #[serde(default = "default_window")]
    pub fit_window_mm: f64,
    /// Upper gap bound, mm (overrides the assertion's `max_mm`). A press
    /// or transition fit needs one: too little interference rattles.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub upper_mm: Option<f64>,
    /// Lower gap bound, mm (overrides the assertion's `min_mm`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub lower_mm: Option<f64>,
    /// Symmetric process tolerance (±mm, 3σ) for parameters that drive the
    /// gap but declare no tolerance of their own. Absent = such a
    /// parameter is an error.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub process_tolerance_mm: Option<f64>,
    /// Restrict the candidate parameters (default: every literal document
    /// parameter).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub parameters: Option<Vec<String>>,
    /// Matching tolerances.
    #[serde(default)]
    pub tolerance: Tolerance,
}

fn default_window() -> f64 {
    1.0
}

/// One derived chain member.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct DerivedContributor {
    /// Document parameter.
    pub parameter: String,
    /// `∂gap/∂p` by central difference over `p ± h`.
    pub coeff: f64,
    /// Nominal value.
    pub nominal: f64,
    /// `(minus, plus)` tolerance used.
    pub tol: (f64, f64),
    /// Where the tolerance came from: `"parameter"` or `"process"`.
    pub tol_source: String,
    /// Linearity residual `|g(p+h) + g(p−h) − 2·g(p)| / 2`, mm.
    pub curvature_mm: f64,
}

/// Result of [`extract_stackup_spec`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExtractedStackup {
    /// The stack, ready for `StackupSpec::resolve(&parameters)`.
    pub spec: StackupSpec,
    /// The document's resolved parameter environment.
    pub parameters: BTreeMap<String, f64>,
    /// Measured nominal gap, mm (negative = interference).
    pub gap_mm: f64,
    /// The two parts whose faces carry the fit, `[group_a part, group_b part]`.
    pub fit_parts: [String; 2],
    /// Chain members with their coefficients.
    pub contributors: Vec<DerivedContributor>,
    /// Literal parameters that don't move this gap.
    pub independent: Vec<String>,
    /// Gap contribution of untoleranced literal geometry, mm.
    pub fixed_geometry_mm: f64,
}

/// A candidate fit: signed gap and the two parts carrying it.
#[derive(Debug, Clone)]
struct Fit {
    gap: f64,
    parts: [String; 2],
}

struct CylFace {
    origin: V3,
    axis: V3,
    radius: f64,
    inward: bool,
    z: (f64, f64),
}

fn cyl_faces(brep: &vcad_kernel::vcad_kernel_primitives::BRepSolid) -> Vec<CylFace> {
    let topo = &brep.topology;
    let mut cyls = Vec::new();
    for (fid, f) in topo.faces.iter() {
        let s = surface_of(brep, fid);
        let Some(c) = s.as_any().downcast_ref::<CylinderSurface>() else {
            continue;
        };
        let o = p3(&c.center);
        let a = canonical_axis(d3(&c.axis));
        let (mut lo, mut hi) = (f64::INFINITY, f64::NEG_INFINITY);
        for lp in std::iter::once(f.outer_loop).chain(f.inner_loops.iter().copied()) {
            for v in topo.loop_vertices(lp) {
                let z = dot(super::sub(p3(&topo.vertices[v].point), o), a);
                lo = lo.min(z);
                hi = hi.max(z);
            }
        }
        cyls.push(CylFace {
            origin: o,
            axis: a,
            radius: c.radius,
            inward: f.orientation == Orientation::Reversed,
            z: (lo, hi),
        });
    }
    cyls
}

/// Measure the fit between the clearance assertion's groups.
fn measure_fit(
    doc: &Document,
    scene: &EvaluatedScene,
    group_a: &[String],
    group_b: &[String],
    sel: &StackupExtraction,
) -> Result<Fit, SimExtractError> {
    let tol = &sel.tolerance;
    let load = |ids: &[String]| -> Result<Vec<(String, Vec<CylFace>)>, SimExtractError> {
        ids.iter()
            .map(|id| {
                let p = find_part(doc, scene, id)?;
                Ok((p.name, cyl_faces(p.brep)))
            })
            .collect()
    };
    let (ga, gb) = (load(group_a)?, load(group_b)?);
    let mut fits: Vec<Fit> = Vec::new();
    for (na, ca) in &ga {
        for (nb, cb) in &gb {
            match sel.feature {
                FitFeature::Radial => {
                    // (shaft, bore) in either group order.
                    for (shafts, bores) in [(ca, cb), (cb, ca)] {
                        for s in shafts.iter().filter(|c| !c.inward) {
                            for b in bores.iter().filter(|c| c.inward) {
                                if line_angle(s.axis, b.axis) > tol.angle_rad
                                    || off_axis(s.origin, b.origin, b.axis) > tol.length_mm
                                {
                                    continue;
                                }
                                // Axial overlap in the bore's frame.
                                let shift = dot(super::sub(s.origin, b.origin), b.axis);
                                let (s0, s1) = (s.z.0 + shift, s.z.1 + shift);
                                if s1.min(b.z.1) - s0.max(b.z.0) <= tol.length_mm {
                                    continue;
                                }
                                let gap = b.radius - s.radius;
                                if gap.abs() <= sel.fit_window_mm {
                                    fits.push(Fit {
                                        gap,
                                        parts: [na.clone(), nb.clone()],
                                    });
                                }
                            }
                        }
                    }
                }
            }
        }
    }
    let Some(first) = fits.first().cloned() else {
        return Err(SimExtractError::Invalid(format!(
            "clearance {:?}: no {:?} fit within ±{} mm between {:?} and {:?} — needs an outward \
             cylinder in one group coaxial with, and axially overlapping, an inward (bore) \
             cylinder in the other",
            sel.clearance, sel.feature, sel.fit_window_mm, group_a, group_b,
        )));
    };
    let distinct: Vec<f64> = {
        let mut g: Vec<f64> = fits.iter().map(|f| f.gap).collect();
        g.sort_by(f64::total_cmp);
        g.dedup_by(|a, b| (*a - *b).abs() <= tol.length_mm);
        g
    };
    if distinct.len() > 1 {
        return Err(SimExtractError::Invalid(format!(
            "clearance {:?}: {} distinct {:?} fits within ±{} mm (gaps {:?} mm) — ambiguous; \
             narrow fit_window_mm or the assertion's groups",
            sel.clearance,
            distinct.len(),
            sel.feature,
            sel.fit_window_mm,
            distinct
        )));
    }
    Ok(first)
}

/// A signed analytic radial fit gap between two part groups.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RadialGap {
    /// `r_bore − r_shaft`, mm (negative = interference).
    pub gap_mm: f64,
    /// The parts carrying the shaft/bore pair, `[group_a part, group_b part]`.
    pub parts: [String; 2],
}

/// Measure the signed radial gap between a coaxial shaft/bore pair, one
/// cylinder in each group, analytically from the evaluated BRep (the
/// fit-mode measurement of a [`vcad_ir::ClearanceSpec`]). Only pairs with
/// `|gap| ≤ fit_window_mm` are candidates; no candidate, more than one
/// distinct gap, or a part without analytic BRep geometry is an error —
/// never a guess.
pub fn signed_radial_gap(
    doc: &Document,
    scene: &EvaluatedScene,
    label: &str,
    group_a: &[String],
    group_b: &[String],
    fit_window_mm: f64,
) -> Result<RadialGap, SimExtractError> {
    let sel = StackupExtraction {
        clearance: label.to_string(),
        feature: FitFeature::Radial,
        fit_window_mm,
        upper_mm: None,
        lower_mm: None,
        process_tolerance_mm: None,
        parameters: None,
        tolerance: Tolerance::default(),
    };
    let f = measure_fit(doc, scene, group_a, group_b, &sel)?;
    Ok(RadialGap {
        gap_mm: f.gap,
        parts: f.parts,
    })
}

/// [`signed_radial_gap`] on a fresh evaluation of `doc` (BRep solids kept;
/// no root cache).
pub fn signed_radial_gap_of(
    doc: &Document,
    label: &str,
    group_a: &[String],
    group_b: &[String],
    fit_window_mm: f64,
) -> Result<RadialGap, SimExtractError> {
    let scene = evaluate_document(doc, &eval_opts())?;
    signed_radial_gap(doc, &scene, label, group_a, group_b, fit_window_mm)
}

fn eval_opts() -> crate::EvalOptions {
    super::extraction_eval_options()
}

fn gap_at(
    doc: &Document,
    param: &str,
    value: f64,
    group_a: &[String],
    group_b: &[String],
    sel: &StackupExtraction,
) -> Result<f64, SimExtractError> {
    let mut d = doc.clone();
    d.parameters.get_mut(param).expect("parameter exists").value = Expr::Number(value);
    let scene = evaluate_document(&d, &eval_opts())?;
    measure_fit(&d, &scene, group_a, group_b, sel)
        .map(|f| f.gap)
        .map_err(|e| {
            SimExtractError::Invalid(format!("re-measuring the fit at {param} = {value}: {e}"))
        })
}

/// Evaluate `doc` and extract the [`StackupSpec`] for one clearance
/// assertion.
pub fn extract_stackup_spec(
    doc: &Document,
    sel: &StackupExtraction,
) -> Result<ExtractedStackup, SimExtractError> {
    let scene = evaluate_document(doc, &eval_opts())?;
    extract_stackup_spec_from_scene(doc, &scene, sel)
}

/// [`extract_stackup_spec`] on an already-evaluated nominal scene (the
/// perturbed re-evaluations still run).
pub fn extract_stackup_spec_from_scene(
    doc: &Document,
    scene: &EvaluatedScene,
    sel: &StackupExtraction,
) -> Result<ExtractedStackup, SimExtractError> {
    let cs = doc
        .clearance_specs
        .iter()
        .find(|c| c.label == sel.clearance)
        .ok_or_else(|| {
            let labels: Vec<&str> = doc
                .clearance_specs
                .iter()
                .map(|c| c.label.as_str())
                .collect();
            SimExtractError::Invalid(format!(
                "document has no clearance assertion {:?} (has: {})",
                sel.clearance,
                if labels.is_empty() {
                    "none".to_string()
                } else {
                    labels.join(", ")
                }
            ))
        })?;
    if let Some(p) = sel.process_tolerance_mm {
        if !(p.is_finite() && p > 0.0) {
            return Err(SimExtractError::Invalid(
                "process_tolerance_mm must be a positive number".into(),
            ));
        }
    }
    let env = resolve_env(doc)?;
    let fit = measure_fit(doc, scene, &cs.group_a, &cs.group_b, sel)?;
    let g0 = fit.gap;

    let candidates: Vec<String> = match &sel.parameters {
        Some(list) => {
            for n in list {
                match doc.parameters.get(n) {
                    None => {
                        return Err(SimExtractError::UnknownParameter {
                            name: n.clone(),
                            role: "a stackup contributor".into(),
                        })
                    }
                    Some(p) if p.value.is_formula() => {
                        return Err(SimExtractError::Invalid(format!(
                            "parameter {n:?} is derived (a formula); tolerance the parameters it \
                             is computed from instead"
                        )))
                    }
                    _ => {}
                }
            }
            list.clone()
        }
        None => {
            let mut v: Vec<String> = doc
                .parameters
                .iter()
                .filter(|(_, p)| !p.value.is_formula())
                .map(|(n, _)| n.clone())
                .collect();
            v.sort();
            v
        }
    };

    let mut contributors = Vec::new();
    let mut independent = Vec::new();
    let mut specs = Vec::new();
    let mut chain_nominal = 0.0;
    for name in &candidates {
        let p = &doc.parameters[name];
        let nominal = env[name];
        let declared = p.tolerance;
        if let Some(t) = declared {
            if !(t.plus >= 0.0 && t.minus >= 0.0 && t.plus.is_finite() && t.minus.is_finite()) {
                return Err(SimExtractError::Invalid(format!(
                    "parameter {name:?} has a malformed tolerance {t:?}"
                )));
            }
        }
        // Probe step: the tolerance itself (the band the stack prices),
        // else the process tolerance, else a small probe to decide
        // whether the parameter matters at all.
        let h = declared
            .map(|t| t.plus.max(t.minus))
            .or(sel.process_tolerance_mm)
            .filter(|h| *h > 0.0)
            .unwrap_or(1e-3);
        let gp = gap_at(doc, name, nominal + h, &cs.group_a, &cs.group_b, sel)?;
        let gm = gap_at(doc, name, nominal - h, &cs.group_a, &cs.group_b, sel)?;
        let coeff = (gp - gm) / (2.0 * h);
        let curvature = (gp + gm - 2.0 * g0).abs() / 2.0;
        let noise = 10.0 * sel.tolerance.length_mm.max(1e-9);
        if coeff.abs() * h <= noise {
            independent.push(name.clone());
            continue;
        }
        if curvature > noise.max(1e-3 * coeff.abs() * h) {
            return Err(SimExtractError::Invalid(format!(
                "the gap is not linear in {name:?} over ±{h} (g(p−h) = {gm:.6}, g(p) = {g0:.6}, \
                 g(p+h) = {gp:.6} mm); a linear stack would misprice it"
            )));
        }
        let (minus, plus, tol_source, k) = match declared {
            Some(t) => (t.minus, t.plus, "parameter", t.sigma_k.unwrap_or(3.0)),
            None => match sel.process_tolerance_mm {
                Some(pt) => (pt, pt, "process", 3.0),
                None => {
                    return Err(SimExtractError::Invalid(format!(
                        "parameter {name:?} moves the {:?} gap (∂gap/∂p = {coeff:.4}) but declares \
                         no tolerance; add one (loon `:tol`) or pass process_tolerance_mm",
                        sel.clearance
                    )))
                }
            },
        };
        let convention = if k == 3.0 {
            SigmaConvention::ThreeSigma
        } else {
            SigmaConvention::KSigma { k }
        };
        let dist = if (plus - minus).abs() <= 1e-12 {
            DistSpec::NormalFromTol { convention }
        } else {
            // Asymmetric band: normal centred on the band's midpoint.
            DistSpec::Normal {
                mean: ParamValue::Literal(0.5 * (plus - minus)),
                sigma: ParamValue::Literal((plus + minus) / (2.0 * k)),
            }
        };
        chain_nominal += coeff * nominal;
        specs.push(ContributorSpec {
            name: name.clone(),
            coeff: ParamValue::Literal(coeff),
            nominal: ParamValue::Named(name.clone()),
            tol_minus: ParamValue::Literal(minus),
            tol_plus: ParamValue::Literal(plus),
            dist,
            source: DistributionSource::Assumed { convention },
        });
        contributors.push(DerivedContributor {
            parameter: name.clone(),
            coeff,
            nominal,
            tol: (minus, plus),
            tol_source: tol_source.into(),
            curvature_mm: curvature,
        });
    }
    if specs.is_empty() {
        return Err(SimExtractError::Invalid(format!(
            "no document parameter moves the {:?} gap ({g0:.4} mm) — nothing to tolerance; \
             drive the fit dimensions from parameters",
            sel.clearance
        )));
    }
    let fixed = g0 - chain_nominal;
    specs.push(ContributorSpec {
        name: "fixed geometry".into(),
        coeff: ParamValue::Literal(1.0),
        nominal: ParamValue::Literal(fixed),
        tol_minus: ParamValue::Literal(0.0),
        tol_plus: ParamValue::Literal(0.0),
        dist: DistSpec::Normal {
            mean: ParamValue::Literal(0.0),
            sigma: ParamValue::Literal(0.0),
        },
        source: DistributionSource::default(),
    });
    let lower = sel.lower_mm.unwrap_or(cs.min_mm);
    let upper = sel.upper_mm.or(cs.max_mm);
    let spec = StackupSpec {
        name: format!(
            "{} ({:?} fit {} / {})",
            cs.label, sel.feature, fit.parts[0], fit.parts[1]
        ),
        contributors: specs,
        requirement: RequirementSpec {
            name: cs.label.clone(),
            lower_mm: Some(ParamValue::Literal(lower)),
            upper_mm: upper.map(ParamValue::Literal),
        },
    };
    // Invariant: the resolved stack reproduces the measured gap.
    let resolved = spec
        .resolve(&env)
        .map_err(|e| SimExtractError::Invalid(format!("derived stack does not resolve: {e}")))?;
    if (resolved.nominal_gap() - g0).abs() > 1e-9 {
        return Err(SimExtractError::Invalid(format!(
            "derived stack's nominal gap {} does not reproduce the measured gap {g0} mm",
            resolved.nominal_gap()
        )));
    }
    Ok(ExtractedStackup {
        spec,
        parameters: env,
        gap_mm: g0,
        fit_parts: fit.parts,
        contributors,
        independent,
        fixed_geometry_mm: fixed,
    })
}

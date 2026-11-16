//! The `.vcad` seam for non-axisymmetric devices: a serde schema for
//! [`Device3d`] with named parameters, the 3D sibling of
//! [`crate::spec::DeviceSpec`].
//!
//! Every numeric field is a [`ParamValue`] (literal or document-parameter
//! name). Resolution is **fail-closed**: unbound names, unknown JSON
//! fields, unknown electrode kinds, and geometry that fails
//! [`Device3d::validate`] are all errors — never defaults or repairs.
//!
//! ```json
//! {
//!   "chamber": { "kind": "sphere", "radius_mm": 76.2 },
//!   "electrodes": [
//!     { "kind": "great_circle_globe", "layout": "icosidodecahedral6",
//!       "grid_radius_mm": "grid_r", "wire_radius_mm": 1.0,
//!       "potential_v": "cathode_v" }
//!   ]
//! }
//! ```
//!
//! No 3D adjoint exists yet, so every named parameter takes finite
//! differences ([`ParamRole::FiniteDifference`]).

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::spec::{DeviceSpec, ParamRole, ParamValue, SpecError};
use crate::wire3d::{Chamber3d, Device3d, GeometryError, GreatCircleLayout, Wire3d, WirePath};

fn zero() -> ParamValue {
    ParamValue::Literal(0.0)
}

/// Serializable chamber.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ChamberSpec {
    /// Grounded sphere centered on the origin.
    Sphere {
        /// Inner radius, mm.
        radius_mm: ParamValue,
    },
    /// No chamber (potential → 0 at infinity).
    Unbounded,
}

/// Named great-circle layouts (see [`GreatCircleLayout`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LayoutSpec {
    /// [`GreatCircleLayout::Orthogonal3`].
    Orthogonal3,
    /// [`GreatCircleLayout::Cuboctahedral4`].
    Cuboctahedral4,
    /// [`GreatCircleLayout::Icosidodecahedral6`].
    Icosidodecahedral6,
}

impl From<LayoutSpec> for GreatCircleLayout {
    fn from(l: LayoutSpec) -> Self {
        match l {
            LayoutSpec::Orthogonal3 => GreatCircleLayout::Orthogonal3,
            LayoutSpec::Cuboctahedral4 => GreatCircleLayout::Cuboctahedral4,
            LayoutSpec::Icosidodecahedral6 => GreatCircleLayout::Icosidodecahedral6,
        }
    }
}

/// A point or direction with named-parameter components.
pub type Vec3Spec = [ParamValue; 3];

/// One electrode (or electrode generator) in the spec.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case", deny_unknown_fields)]
pub enum ElectrodeSpec {
    /// A coaxial ring about z — the axisymmetric [`crate::spec::RingSpec`].
    Ring {
        /// Ring radius, mm.
        ring_radius_mm: ParamValue,
        /// Axial position, mm.
        z_mm: ParamValue,
        /// Wire radius, mm.
        wire_radius_mm: ParamValue,
        /// Potential, V.
        potential_v: ParamValue,
        /// Current, ampere-turns (default 0).
        #[serde(default = "zero")]
        ampere_turns: ParamValue,
    },
    /// A circle in any plane ([`WirePath::Circle`]).
    Circle {
        /// Center, mm.
        center_mm: Vec3Spec,
        /// Plane normal.
        normal: Vec3Spec,
        /// Circle radius, mm.
        radius_mm: ParamValue,
        /// Wire radius, mm.
        wire_radius_mm: ParamValue,
        /// Potential, V.
        potential_v: ParamValue,
        /// Current, ampere-turns (default 0).
        #[serde(default = "zero")]
        ampere_turns: ParamValue,
    },
    /// A circular arc ([`WirePath::Arc`]).
    Arc {
        /// Center, mm.
        center_mm: Vec3Spec,
        /// Plane normal.
        normal: Vec3Spec,
        /// Start point on the arc, mm.
        start_mm: Vec3Spec,
        /// Sweep, degrees (counter-clockwise about `normal`).
        sweep_deg: ParamValue,
        /// Wire radius, mm.
        wire_radius_mm: ParamValue,
        /// Potential, V.
        potential_v: ParamValue,
    },
    /// A polyline wire ([`WirePath::Polyline`]) — stalks, feed wires.
    Polyline {
        /// Vertices, mm.
        points_mm: Vec<Vec3Spec>,
        /// Closed loop (default false).
        #[serde(default)]
        closed: bool,
        /// Wire radius, mm.
        wire_radius_mm: ParamValue,
        /// Potential, V.
        potential_v: ParamValue,
    },
    /// A great-circle globe: a named `layout` **or** explicit plane
    /// `normals` (exactly one).
    GreatCircleGlobe {
        /// Globe radius, mm.
        grid_radius_mm: ParamValue,
        /// Named symmetric layout.
        #[serde(default)]
        layout: Option<LayoutSpec>,
        /// Explicit circle plane normals.
        #[serde(default)]
        normals: Option<Vec<Vec3Spec>>,
        /// Wire radius, mm.
        wire_radius_mm: ParamValue,
        /// Potential, V.
        potential_v: ParamValue,
    },
    /// A latitude/longitude globe ([`Device3d::lat_long_globe`]).
    LatLongGlobe {
        /// Globe radius, mm.
        grid_radius_mm: ParamValue,
        /// Latitude ring count.
        n_lat: usize,
        /// Meridian (pole-to-pole half circle) count.
        n_long: usize,
        /// Wire radius, mm.
        wire_radius_mm: ParamValue,
        /// Potential, V.
        potential_v: ParamValue,
    },
}

/// Serializable 3D device with named parameters.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct DeviceSpec3d {
    /// Enclosing chamber.
    pub chamber: ChamberSpec,
    /// Wall potential, V (default 0).
    #[serde(default = "zero")]
    pub wall_potential_v: ParamValue,
    /// Electrodes, in order (wire indices in trace outcomes follow the
    /// expanded order: each globe contributes its circles/arcs in place).
    pub electrodes: Vec<ElectrodeSpec>,
}

/// Resolution failures for [`DeviceSpec3d::resolve`].
#[derive(Debug, Clone, PartialEq)]
pub enum Spec3dError {
    /// A named parameter had no binding.
    UnknownParameter(String),
    /// A structurally invalid spec (e.g. a globe with both or neither of
    /// `layout` and `normals`).
    Invalid(String),
    /// The resolved geometry failed validation.
    Geometry(GeometryError),
}

impl std::fmt::Display for Spec3dError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Spec3dError::UnknownParameter(n) => write!(f, "unbound device parameter: {n:?}"),
            Spec3dError::Invalid(s) => write!(f, "invalid 3D device spec: {s}"),
            Spec3dError::Geometry(e) => write!(f, "invalid 3D device geometry: {e}"),
        }
    }
}

impl std::error::Error for Spec3dError {}

impl From<SpecError> for Spec3dError {
    fn from(e: SpecError) -> Self {
        match e {
            SpecError::UnknownParameter(n) => Spec3dError::UnknownParameter(n),
        }
    }
}

fn r(v: &ParamValue, p: &BTreeMap<String, f64>) -> Result<f64, Spec3dError> {
    Ok(v.resolve(p)?)
}

fn r3(v: &Vec3Spec, p: &BTreeMap<String, f64>) -> Result<[f64; 3], Spec3dError> {
    Ok([r(&v[0], p)?, r(&v[1], p)?, r(&v[2], p)?])
}

impl DeviceSpec3d {
    /// Resolve every field against `params` and validate, fail-closed.
    pub fn resolve(&self, params: &BTreeMap<String, f64>) -> Result<Device3d, Spec3dError> {
        let chamber = match &self.chamber {
            ChamberSpec::Sphere { radius_mm } => Chamber3d::Sphere {
                radius_mm: r(radius_mm, params)?,
            },
            ChamberSpec::Unbounded => Chamber3d::Unbounded,
        };
        let wall_potential_v = r(&self.wall_potential_v, params)?;
        let mut wires = Vec::new();
        for e in &self.electrodes {
            match e {
                ElectrodeSpec::Ring {
                    ring_radius_mm,
                    z_mm,
                    wire_radius_mm,
                    potential_v,
                    ampere_turns,
                } => wires.push(Wire3d {
                    path: WirePath::Circle {
                        center_mm: [0.0, 0.0, r(z_mm, params)?],
                        normal: [0.0, 0.0, 1.0],
                        radius_mm: r(ring_radius_mm, params)?,
                    },
                    wire_radius_mm: r(wire_radius_mm, params)?,
                    potential_v: r(potential_v, params)?,
                    ampere_turns: r(ampere_turns, params)?,
                }),
                ElectrodeSpec::Circle {
                    center_mm,
                    normal,
                    radius_mm,
                    wire_radius_mm,
                    potential_v,
                    ampere_turns,
                } => wires.push(Wire3d {
                    path: WirePath::Circle {
                        center_mm: r3(center_mm, params)?,
                        normal: r3(normal, params)?,
                        radius_mm: r(radius_mm, params)?,
                    },
                    wire_radius_mm: r(wire_radius_mm, params)?,
                    potential_v: r(potential_v, params)?,
                    ampere_turns: r(ampere_turns, params)?,
                }),
                ElectrodeSpec::Arc {
                    center_mm,
                    normal,
                    start_mm,
                    sweep_deg,
                    wire_radius_mm,
                    potential_v,
                } => wires.push(Wire3d {
                    path: WirePath::Arc {
                        center_mm: r3(center_mm, params)?,
                        normal: r3(normal, params)?,
                        start_mm: r3(start_mm, params)?,
                        sweep_deg: r(sweep_deg, params)?,
                    },
                    wire_radius_mm: r(wire_radius_mm, params)?,
                    potential_v: r(potential_v, params)?,
                    ampere_turns: 0.0,
                }),
                ElectrodeSpec::Polyline {
                    points_mm,
                    closed,
                    wire_radius_mm,
                    potential_v,
                } => wires.push(Wire3d {
                    path: WirePath::Polyline {
                        points_mm: points_mm
                            .iter()
                            .map(|p| r3(p, params))
                            .collect::<Result<_, _>>()?,
                        closed: *closed,
                    },
                    wire_radius_mm: r(wire_radius_mm, params)?,
                    potential_v: r(potential_v, params)?,
                    ampere_turns: 0.0,
                }),
                ElectrodeSpec::GreatCircleGlobe {
                    grid_radius_mm,
                    layout,
                    normals,
                    wire_radius_mm,
                    potential_v,
                } => {
                    let normals: Vec<[f64; 3]> = match (layout, normals) {
                        (Some(l), None) => GreatCircleLayout::from(*l).normals(),
                        (None, Some(ns)) => {
                            if ns.is_empty() {
                                return Err(Spec3dError::Invalid(
                                    "great_circle_globe.normals is empty".into(),
                                ));
                            }
                            ns.iter().map(|n| r3(n, params)).collect::<Result<_, _>>()?
                        }
                        _ => {
                            return Err(Spec3dError::Invalid(
                                "great_circle_globe needs exactly one of `layout` or `normals`"
                                    .into(),
                            ))
                        }
                    };
                    let g = Device3d::great_circle_globe(
                        1.0,
                        r(grid_radius_mm, params)?,
                        &normals,
                        r(wire_radius_mm, params)?,
                        r(potential_v, params)?,
                    );
                    wires.extend(g.wires);
                }
                ElectrodeSpec::LatLongGlobe {
                    grid_radius_mm,
                    n_lat,
                    n_long,
                    wire_radius_mm,
                    potential_v,
                } => {
                    if n_lat + n_long == 0 {
                        return Err(Spec3dError::Invalid(
                            "lat_long_globe needs at least one wire".into(),
                        ));
                    }
                    let g = Device3d::lat_long_globe(
                        1.0,
                        r(grid_radius_mm, params)?,
                        *n_lat,
                        *n_long,
                        r(wire_radius_mm, params)?,
                        r(potential_v, params)?,
                    );
                    wires.extend(g.wires);
                }
            }
        }
        let device = Device3d {
            chamber,
            wall_potential_v,
            wires,
        };
        device.validate().map_err(Spec3dError::Geometry)?;
        Ok(device)
    }

    /// The 3D spec of an axisymmetric [`DeviceSpec`], keeping its named
    /// parameters: rings become [`ElectrodeSpec::Ring`]. The cylinder is
    /// not carried over — pass the chamber explicitly.
    pub fn from_axisym(spec: &DeviceSpec, chamber: ChamberSpec) -> Self {
        Self {
            chamber,
            wall_potential_v: spec.wall_potential_v.clone(),
            electrodes: spec
                .rings
                .iter()
                .map(|ring| ElectrodeSpec::Ring {
                    ring_radius_mm: ring.ring_radius_mm.clone(),
                    z_mm: ring.z_mm.clone(),
                    wire_radius_mm: ring.wire_radius_mm.clone(),
                    potential_v: ring.potential_v.clone(),
                    ampere_turns: ring.ampere_turns.clone(),
                })
                .collect(),
        }
    }

    /// Every named parameter. All take finite differences: there is no
    /// 3D adjoint yet.
    pub fn parameter_roles(&self) -> BTreeMap<String, ParamRole> {
        let mut out = BTreeMap::new();
        let mut put = |v: &ParamValue| {
            if let Some(n) = v.name() {
                out.insert(n.to_string(), ParamRole::FiniteDifference);
            }
        };
        if let ChamberSpec::Sphere { radius_mm } = &self.chamber {
            put(radius_mm);
        }
        put(&self.wall_potential_v);
        for e in &self.electrodes {
            match e {
                ElectrodeSpec::Ring {
                    ring_radius_mm,
                    z_mm,
                    wire_radius_mm,
                    potential_v,
                    ampere_turns,
                } => {
                    for v in [ring_radius_mm, z_mm, wire_radius_mm, potential_v, ampere_turns] {
                        put(v);
                    }
                }
                ElectrodeSpec::Circle {
                    center_mm,
                    normal,
                    radius_mm,
                    wire_radius_mm,
                    potential_v,
                    ampere_turns,
                } => {
                    for v in center_mm.iter().chain(normal) {
                        put(v);
                    }
                    for v in [radius_mm, wire_radius_mm, potential_v, ampere_turns] {
                        put(v);
                    }
                }
                ElectrodeSpec::Arc {
                    center_mm,
                    normal,
                    start_mm,
                    sweep_deg,
                    wire_radius_mm,
                    potential_v,
                } => {
                    for v in center_mm.iter().chain(normal).chain(start_mm) {
                        put(v);
                    }
                    for v in [sweep_deg, wire_radius_mm, potential_v] {
                        put(v);
                    }
                }
                ElectrodeSpec::Polyline {
                    points_mm,
                    wire_radius_mm,
                    potential_v,
                    ..
                } => {
                    for v in points_mm.iter().flatten() {
                        put(v);
                    }
                    put(wire_radius_mm);
                    put(potential_v);
                }
                ElectrodeSpec::GreatCircleGlobe {
                    grid_radius_mm,
                    normals,
                    wire_radius_mm,
                    potential_v,
                    ..
                } => {
                    for v in normals.iter().flatten().flatten() {
                        put(v);
                    }
                    for v in [grid_radius_mm, wire_radius_mm, potential_v] {
                        put(v);
                    }
                }
                ElectrodeSpec::LatLongGlobe {
                    grid_radius_mm,
                    wire_radius_mm,
                    potential_v,
                    ..
                } => {
                    for v in [grid_radius_mm, wire_radius_mm, potential_v] {
                        put(v);
                    }
                }
            }
        }
        out
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn params(pairs: &[(&str, f64)]) -> BTreeMap<String, f64> {
        pairs.iter().map(|(k, v)| (k.to_string(), *v)).collect()
    }

    const GLOBE: &str = r#"{
        "chamber": { "kind": "sphere", "radius_mm": 76.2 },
        "electrodes": [
            { "kind": "great_circle_globe", "layout": "icosidodecahedral6",
              "grid_radius_mm": "grid_r", "wire_radius_mm": 1.0,
              "potential_v": "cathode_v" },
            { "kind": "polyline", "points_mm": [[0, 0, 19], [0, 0, 60]],
              "wire_radius_mm": 1.5, "potential_v": "cathode_v" }
        ]
    }"#;

    #[test]
    fn json_globe_with_named_parameters_resolves() {
        let spec: DeviceSpec3d = serde_json::from_str(GLOBE).unwrap();
        let d = spec
            .resolve(&params(&[("grid_r", 19.05), ("cathode_v", -30e3)]))
            .unwrap();
        assert_eq!(d.wires.len(), 7);
        assert!(d.wires.iter().all(|w| w.potential_v == -30e3));
        let roles = spec.parameter_roles();
        assert_eq!(roles.len(), 2);
        assert!(roles.values().all(|r| *r == ParamRole::FiniteDifference));
        // Round-trips through JSON.
        let back: DeviceSpec3d =
            serde_json::from_str(&serde_json::to_string(&spec).unwrap()).unwrap();
        assert_eq!(back, spec);
    }

    #[test]
    fn fails_closed() {
        let spec: DeviceSpec3d = serde_json::from_str(GLOBE).unwrap();
        assert_eq!(
            spec.resolve(&params(&[("grid_r", 19.0)])),
            Err(Spec3dError::UnknownParameter("cathode_v".into()))
        );
        // Unknown field and unknown kind are parse errors.
        let typo = GLOBE.replace("\"grid_radius_mm\"", "\"grid_radius\"");
        assert!(serde_json::from_str::<DeviceSpec3d>(&typo).is_err());
        let kind = GLOBE.replace("great_circle_globe", "geodesic_dome");
        assert!(serde_json::from_str::<DeviceSpec3d>(&kind).is_err());
        // Both layout and normals.
        let both = GLOBE.replace(
            "\"layout\": \"icosidodecahedral6\",",
            "\"layout\": \"icosidodecahedral6\", \"normals\": [[0,0,1]],",
        );
        let spec: DeviceSpec3d = serde_json::from_str(&both).unwrap();
        assert!(matches!(
            spec.resolve(&params(&[("grid_r", 19.0), ("cathode_v", -1.0)])),
            Err(Spec3dError::Invalid(_))
        ));
        // Geometry outside the chamber.
        let spec: DeviceSpec3d = serde_json::from_str(GLOBE).unwrap();
        assert!(matches!(
            spec.resolve(&params(&[("grid_r", 80.0), ("cathode_v", -1.0)])),
            Err(Spec3dError::Geometry(_))
        ));
    }

    #[test]
    fn axisym_spec_lifts_with_its_parameters() {
        let device = crate::device::Device::classic_fusor(76.0, 19.0, 4, 1.0, -30e3);
        let mut spec2 = DeviceSpec::from_device(&device);
        spec2.rings[0].potential_v = ParamValue::Named("cathode_v".into());
        let spec3 = DeviceSpec3d::from_axisym(
            &spec2,
            ChamberSpec::Sphere {
                radius_mm: 76.0.into(),
            },
        );
        let d3 = spec3.resolve(&params(&[("cathode_v", -20e3)])).unwrap();
        assert_eq!(d3.wires.len(), 4);
        assert_eq!(d3.wires[0].potential_v, -20e3);
        assert_eq!(
            d3,
            Device3d::from_rings(
                &spec2.resolve(&params(&[("cathode_v", -20e3)])).unwrap(),
                Chamber3d::Sphere { radius_mm: 76.0 }
            )
        );
    }
}

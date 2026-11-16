//! Physical material properties: one library of defaults and one resolver
//! every solver reads through.
//!
//! A document's [`MaterialDef`] may carry a [`MaterialPhysical`] block and a
//! top-level `density`. [`resolve_material`] merges those with the library,
//! **per property**, with this precedence:
//!
//! 1. the document's own value (`materials[key].physical.*`, `.density`);
//! 2. the library entry — named explicitly by `physical.library`, else
//!    matched from the material key, then from the material's display name;
//! 3. nothing: reading the property is a typed [`MaterialError`].
//!
//! There is no silent fallback to aluminium (or anything else). A consumer
//! that wants one — the mallet tool still rings an unknown part as
//! aluminium so a click always makes a sound — must choose it explicitly and
//! say so in its result.
//!
//! Library matching is case-insensitive and tried in order: exact id, exact
//! alias, then substring of an alias in the key (`"pla_red"` → PLA,
//! `"Brass C360"` → brass). Substring order matters and is fixed by
//! [`LIBRARY`]'s order: stainless before steel, PETG before PLA.
//!
//! ```
//! use vcad_ir::materials::{resolve_material, Property};
//! # let doc = vcad_ir::Document::default();
//! // A document with no material named "pla_red" still resolves it from the
//! // library by key:
//! let m = resolve_material(&doc, "pla_red").unwrap();
//! assert_eq!(m.library_id, Some("pla"));
//! assert_eq!(m.youngs_modulus_mpa().unwrap(), 3500.0);
//! // ...and a property nobody knows is an error, not a guess:
//! assert!(resolve_material(&doc, "air").unwrap().youngs_modulus_mpa().is_err());
//! ```

use serde::Serialize;

use crate::{Document, MaterialDef, MaterialPhysical, ThermalConductivity};

/// A library entry: defaults for one material.
#[derive(Debug, Clone, Copy, PartialEq, Serialize)]
pub struct LibraryMaterial {
    /// Stable id (`"aluminum_6061"`, `"pla"`, …). Accepted as
    /// `physical.library` and as a material key.
    pub id: &'static str,
    /// Display name, with grade where it matters.
    pub label: &'static str,
    /// Lower-case fragments matched against a material key or name.
    #[serde(skip)]
    pub aliases: &'static [&'static str],
    /// Density, kg/m³.
    pub density_kg_m3: Option<f64>,
    /// Young's modulus, MPa.
    pub youngs_modulus_mpa: Option<f64>,
    /// Poisson's ratio.
    pub poisson: Option<f64>,
    /// Yield strength, MPa.
    pub yield_strength_mpa: Option<f64>,
    /// Thermal conductivity, W/(m·K).
    pub thermal_conductivity_w_mk: Option<ThermalConductivity>,
    /// Specific heat, J/(kg·K).
    pub specific_heat_j_kgk: Option<f64>,
    /// Emissivity, 0..1.
    pub emissivity: Option<f64>,
    /// Relative permittivity.
    pub relative_permittivity: Option<f64>,
    /// Loss tangent.
    pub loss_tangent: Option<f64>,
    /// Relative permeability.
    pub relative_permeability: Option<f64>,
    /// Remanence, T.
    pub remanence_t: Option<f64>,
    /// Resistivity, Ω·m.
    pub resistivity_ohm_m: Option<f64>,
}

const NONE: LibraryMaterial = LibraryMaterial {
    id: "",
    label: "",
    aliases: &[],
    density_kg_m3: None,
    youngs_modulus_mpa: None,
    poisson: None,
    yield_strength_mpa: None,
    thermal_conductivity_w_mk: None,
    specific_heat_j_kgk: None,
    emissivity: None,
    relative_permittivity: None,
    loss_tangent: None,
    relative_permeability: None,
    remanence_t: None,
    resistivity_ohm_m: None,
};

const fn iso(k: f64) -> Option<ThermalConductivity> {
    Some(ThermalConductivity::Isotropic(k))
}

/// The library: room-temperature handbook values (MatWeb / manufacturer
/// datasheets). Printed-plastic values are for the bulk polymer, not a
/// printed part's (weaker, anisotropic) layers. Order is the substring
/// match order.
///
/// A property a material has no meaningful value for is `None` — air has no
/// Young's modulus, a metal no permittivity — so asking for it fails.
pub const LIBRARY: &[LibraryMaterial] = &[
    // ── metals ──────────────────────────────────────────────────────────
    LibraryMaterial {
        id: "stainless_304",
        label: "Stainless 304",
        aliases: &["stainless", "304", "316", "inox"],
        density_kg_m3: Some(8000.0),
        youngs_modulus_mpa: Some(193_000.0),
        poisson: Some(0.29),
        yield_strength_mpa: Some(215.0),
        thermal_conductivity_w_mk: iso(16.2),
        specific_heat_j_kgk: Some(500.0),
        emissivity: Some(0.35),
        relative_permeability: Some(1.008),
        resistivity_ohm_m: Some(7.2e-7),
        ..NONE
    },
    LibraryMaterial {
        id: "steel",
        label: "Steel (1018)",
        aliases: &["steel", "iron", "1018", "1045"],
        density_kg_m3: Some(7870.0),
        youngs_modulus_mpa: Some(205_000.0),
        poisson: Some(0.29),
        yield_strength_mpa: Some(370.0),
        thermal_conductivity_w_mk: iso(51.9),
        specific_heat_j_kgk: Some(486.0),
        emissivity: Some(0.30),
        // Linear, unsaturated approximation of low-carbon steel — a magnetic
        // circuit near saturation needs a B–H curve, not this number.
        relative_permeability: Some(1000.0),
        resistivity_ohm_m: Some(1.59e-7),
        ..NONE
    },
    LibraryMaterial {
        id: "brass",
        label: "Brass C360",
        aliases: &["brass", "c360"],
        density_kg_m3: Some(8500.0),
        youngs_modulus_mpa: Some(97_000.0),
        poisson: Some(0.31),
        yield_strength_mpa: Some(310.0),
        thermal_conductivity_w_mk: iso(115.0),
        specific_heat_j_kgk: Some(380.0),
        emissivity: Some(0.10),
        relative_permeability: Some(1.0),
        resistivity_ohm_m: Some(6.6e-8),
        ..NONE
    },
    LibraryMaterial {
        id: "bronze",
        label: "Phosphor bronze C510",
        aliases: &["bronze", "c510"],
        density_kg_m3: Some(8860.0),
        youngs_modulus_mpa: Some(110_000.0),
        poisson: Some(0.34),
        thermal_conductivity_w_mk: iso(69.0),
        specific_heat_j_kgk: Some(380.0),
        emissivity: Some(0.10),
        relative_permeability: Some(1.0),
        resistivity_ohm_m: Some(1.1e-7),
        ..NONE
    },
    LibraryMaterial {
        id: "copper",
        label: "Copper C110",
        aliases: &["copper", "c110"],
        density_kg_m3: Some(8960.0),
        youngs_modulus_mpa: Some(117_000.0),
        poisson: Some(0.34),
        // Annealed; half-hard sheet is ~250 MPa.
        yield_strength_mpa: Some(69.0),
        thermal_conductivity_w_mk: iso(391.0),
        specific_heat_j_kgk: Some(385.0),
        emissivity: Some(0.05),
        relative_permeability: Some(1.0),
        resistivity_ohm_m: Some(1.72e-8),
        ..NONE
    },
    LibraryMaterial {
        id: "titanium",
        label: "Titanium Ti-6Al-4V",
        aliases: &["titan", "ti64", "ti-6al-4v"],
        density_kg_m3: Some(4430.0),
        youngs_modulus_mpa: Some(114_000.0),
        poisson: Some(0.34),
        yield_strength_mpa: Some(880.0),
        thermal_conductivity_w_mk: iso(6.7),
        specific_heat_j_kgk: Some(526.0),
        emissivity: Some(0.30),
        relative_permeability: Some(1.0),
        resistivity_ohm_m: Some(1.78e-6),
        ..NONE
    },
    LibraryMaterial {
        id: "aluminum_6061",
        label: "Aluminum 6061-T6",
        aliases: &["alum", "6061", "7075"],
        density_kg_m3: Some(2700.0),
        youngs_modulus_mpa: Some(68_900.0),
        poisson: Some(0.33),
        yield_strength_mpa: Some(276.0),
        thermal_conductivity_w_mk: iso(167.0),
        specific_heat_j_kgk: Some(896.0),
        // Bare, untreated; anodised is ~0.8.
        emissivity: Some(0.09),
        relative_permeability: Some(1.0),
        resistivity_ohm_m: Some(3.99e-8),
        ..NONE
    },
    // ── magnets ─────────────────────────────────────────────────────────
    LibraryMaterial {
        id: "n42",
        label: "NdFeB N42",
        aliases: &["n42", "ndfeb", "neodymium"],
        density_kg_m3: Some(7500.0),
        youngs_modulus_mpa: Some(160_000.0),
        poisson: Some(0.24),
        thermal_conductivity_w_mk: iso(9.0),
        specific_heat_j_kgk: Some(440.0),
        relative_permeability: Some(1.05),
        remanence_t: Some(1.30),
        resistivity_ohm_m: Some(1.4e-6),
        ..NONE
    },
    LibraryMaterial {
        id: "ferrite_y30",
        label: "Hard ferrite Y30",
        aliases: &["ferrite", "y30", "ceramic magnet"],
        density_kg_m3: Some(4900.0),
        youngs_modulus_mpa: Some(150_000.0),
        poisson: Some(0.28),
        thermal_conductivity_w_mk: iso(4.0),
        specific_heat_j_kgk: Some(800.0),
        emissivity: Some(0.90),
        relative_permeability: Some(1.05),
        remanence_t: Some(0.38),
        resistivity_ohm_m: Some(1.0e4),
        ..NONE
    },
    // ── plastics (bulk polymer) ─────────────────────────────────────────
    LibraryMaterial {
        id: "petg",
        label: "PETG",
        aliases: &["petg"],
        density_kg_m3: Some(1270.0),
        youngs_modulus_mpa: Some(2_100.0),
        poisson: Some(0.38),
        yield_strength_mpa: Some(50.0),
        thermal_conductivity_w_mk: iso(0.20),
        specific_heat_j_kgk: Some(1200.0),
        emissivity: Some(0.90),
        relative_permittivity: Some(2.6),
        loss_tangent: Some(0.02),
        relative_permeability: Some(1.0),
        resistivity_ohm_m: Some(1.0e13),
        ..NONE
    },
    LibraryMaterial {
        id: "pla",
        label: "PLA",
        aliases: &["pla"],
        density_kg_m3: Some(1240.0),
        youngs_modulus_mpa: Some(3_500.0),
        poisson: Some(0.36),
        yield_strength_mpa: Some(50.0),
        thermal_conductivity_w_mk: iso(0.13),
        specific_heat_j_kgk: Some(1800.0),
        emissivity: Some(0.92),
        relative_permittivity: Some(3.0),
        loss_tangent: Some(0.01),
        relative_permeability: Some(1.0),
        resistivity_ohm_m: Some(1.0e14),
        ..NONE
    },
    LibraryMaterial {
        id: "abs",
        label: "ABS",
        aliases: &["abs"],
        density_kg_m3: Some(1050.0),
        youngs_modulus_mpa: Some(2_200.0),
        poisson: Some(0.35),
        yield_strength_mpa: Some(40.0),
        thermal_conductivity_w_mk: iso(0.17),
        specific_heat_j_kgk: Some(1400.0),
        emissivity: Some(0.90),
        relative_permittivity: Some(2.9),
        loss_tangent: Some(0.01),
        relative_permeability: Some(1.0),
        resistivity_ohm_m: Some(1.0e14),
        ..NONE
    },
    LibraryMaterial {
        id: "nylon",
        label: "Nylon PA12",
        aliases: &["nylon", "pa12", "pa6", "polyamide"],
        density_kg_m3: Some(1010.0),
        youngs_modulus_mpa: Some(1_700.0),
        poisson: Some(0.39),
        yield_strength_mpa: Some(48.0),
        thermal_conductivity_w_mk: iso(0.25),
        specific_heat_j_kgk: Some(1700.0),
        emissivity: Some(0.90),
        relative_permittivity: Some(3.5),
        loss_tangent: Some(0.03),
        relative_permeability: Some(1.0),
        resistivity_ohm_m: Some(1.0e12),
        ..NONE
    },
    // ── laminates, glass, wood ──────────────────────────────────────────
    LibraryMaterial {
        id: "fr4",
        label: "FR4",
        aliases: &["fr4", "fr-4", "pcb"],
        density_kg_m3: Some(1850.0),
        // In-plane (warp); through-plane is roughly half.
        youngs_modulus_mpa: Some(24_000.0),
        poisson: Some(0.12),
        // Ultimate tensile in-plane; FR4 does not yield.
        yield_strength_mpa: Some(310.0),
        thermal_conductivity_w_mk: Some(ThermalConductivity::Anisotropic {
            in_plane: 0.81,
            through_plane: 0.29,
        }),
        specific_heat_j_kgk: Some(1150.0),
        emissivity: Some(0.90),
        relative_permittivity: Some(4.4),
        loss_tangent: Some(0.02),
        relative_permeability: Some(1.0),
        resistivity_ohm_m: Some(1.0e14),
        ..NONE
    },
    LibraryMaterial {
        id: "glass",
        label: "Soda-lime glass",
        aliases: &["glass"],
        density_kg_m3: Some(2500.0),
        youngs_modulus_mpa: Some(70_000.0),
        poisson: Some(0.22),
        thermal_conductivity_w_mk: iso(1.0),
        specific_heat_j_kgk: Some(840.0),
        emissivity: Some(0.92),
        relative_permittivity: Some(7.0),
        loss_tangent: Some(0.01),
        relative_permeability: Some(1.0),
        ..NONE
    },
    LibraryMaterial {
        id: "wood",
        label: "Wood (hard maple, along grain)",
        aliases: &["wood", "maple", "rosewood", "oak"],
        density_kg_m3: Some(700.0),
        youngs_modulus_mpa: Some(12_000.0),
        poisson: Some(0.35),
        thermal_conductivity_w_mk: iso(0.16),
        specific_heat_j_kgk: Some(1700.0),
        emissivity: Some(0.90),
        relative_permeability: Some(1.0),
        ..NONE
    },
    // ── fluids ──────────────────────────────────────────────────────────
    LibraryMaterial {
        id: "air",
        label: "Air (20 °C, 1 atm)",
        aliases: &["air"],
        density_kg_m3: Some(1.204),
        thermal_conductivity_w_mk: iso(0.0257),
        specific_heat_j_kgk: Some(1005.0),
        relative_permittivity: Some(1.0006),
        loss_tangent: Some(0.0),
        relative_permeability: Some(1.0),
        ..NONE
    },
];

/// Library entry with exactly this id (case-insensitive).
pub fn library_by_id(id: &str) -> Option<&'static LibraryMaterial> {
    LIBRARY
        .iter()
        .find(|m| m.id.eq_ignore_ascii_case(id.trim()))
}

/// Library entry for a material key or name: exact id, then exact alias,
/// then an alias contained in the key — in [`LIBRARY`] order. `None` when
/// nothing matches.
pub fn library_match(key: &str) -> Option<&'static LibraryMaterial> {
    let k = key.trim().to_ascii_lowercase();
    if k.is_empty() {
        return None;
    }
    library_by_id(&k)
        .or_else(|| LIBRARY.iter().find(|m| m.aliases.contains(&k.as_str())))
        .or_else(|| {
            LIBRARY
                .iter()
                .find(|m| m.aliases.iter().any(|a| k.contains(a)))
        })
}

/// A resolvable physical property.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Property {
    /// Density, kg/m³.
    Density,
    /// Young's modulus, MPa.
    YoungsModulus,
    /// Poisson's ratio.
    Poisson,
    /// Yield strength, MPa.
    YieldStrength,
    /// Thermal conductivity, W/(m·K).
    ThermalConductivity,
    /// Specific heat, J/(kg·K).
    SpecificHeat,
    /// Emissivity.
    Emissivity,
    /// Relative permittivity.
    RelativePermittivity,
    /// Loss tangent.
    LossTangent,
    /// Relative permeability.
    RelativePermeability,
    /// Remanence, T.
    Remanence,
    /// Resistivity, Ω·m.
    Resistivity,
}

impl Property {
    /// Every property, in declaration order.
    pub const ALL: [Property; 12] = [
        Property::Density,
        Property::YoungsModulus,
        Property::Poisson,
        Property::YieldStrength,
        Property::ThermalConductivity,
        Property::SpecificHeat,
        Property::Emissivity,
        Property::RelativePermittivity,
        Property::LossTangent,
        Property::RelativePermeability,
        Property::Remanence,
        Property::Resistivity,
    ];

    /// Human name with unit, for messages.
    pub fn describe(self) -> &'static str {
        match self {
            Property::Density => "density (kg/m³)",
            Property::YoungsModulus => "Young's modulus (MPa)",
            Property::Poisson => "Poisson's ratio",
            Property::YieldStrength => "yield strength (MPa)",
            Property::ThermalConductivity => "thermal conductivity (W/(m·K))",
            Property::SpecificHeat => "specific heat (J/(kg·K))",
            Property::Emissivity => "emissivity",
            Property::RelativePermittivity => "relative permittivity",
            Property::LossTangent => "loss tangent",
            Property::RelativePermeability => "relative permeability",
            Property::Remanence => "remanence (T)",
            Property::Resistivity => "resistivity (Ω·m)",
        }
    }

    /// The JSON key in [`MaterialPhysical`] (`density` lives on
    /// [`MaterialDef`] itself).
    pub fn json_key(self) -> &'static str {
        match self {
            Property::Density => "density",
            Property::YoungsModulus => "youngsModulusMpa",
            Property::Poisson => "poisson",
            Property::YieldStrength => "yieldStrengthMpa",
            Property::ThermalConductivity => "thermalConductivityWMk",
            Property::SpecificHeat => "specificHeatJKgk",
            Property::Emissivity => "emissivity",
            Property::RelativePermittivity => "relativePermittivity",
            Property::LossTangent => "lossTangent",
            Property::RelativePermeability => "relativePermeability",
            Property::Remanence => "remanenceT",
            Property::Resistivity => "resistivityOhmM",
        }
    }
}

/// Why a material or one of its properties could not be resolved.
#[derive(Debug, Clone, PartialEq, Serialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum MaterialError {
    /// The key names no document material and matches no library entry.
    UnknownMaterial {
        /// The material key asked for.
        key: String,
    },
    /// `physical.library` names an id that is not in [`LIBRARY`].
    UnknownLibraryId {
        /// The material key.
        key: String,
        /// The id it named.
        library: String,
    },
    /// Neither the document nor the library supplies this property.
    MissingProperty {
        /// The material key.
        key: String,
        /// The property asked for.
        property: Property,
        /// The library entry consulted, if any.
        library: Option<&'static str>,
    },
}

impl std::fmt::Display for MaterialError {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            MaterialError::UnknownMaterial { key } => write!(
                f,
                "material `{key}` has no physical properties: it is not a library material \
                 and the document gives none — set materials.{key}.physical (and density), \
                 or physical.library to one of: {}",
                library_ids()
            ),
            MaterialError::UnknownLibraryId { key, library } => write!(
                f,
                "material `{key}`: physical.library `{library}` is not a library material \
                 (known: {})",
                library_ids()
            ),
            MaterialError::MissingProperty {
                key,
                property,
                library,
            } => {
                write!(f, "material `{key}` has no {}", property.describe())?;
                if let Some(id) = library {
                    write!(f, " (library `{id}` doesn't define one)")?;
                }
                write!(
                    f,
                    " — set materials.{key}.{}{}",
                    if *property == Property::Density {
                        ""
                    } else {
                        "physical."
                    },
                    property.json_key()
                )
            }
        }
    }
}

impl std::error::Error for MaterialError {}

fn library_ids() -> String {
    LIBRARY.iter().map(|m| m.id).collect::<Vec<_>>().join(", ")
}

/// Where a resolved value came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Source {
    /// The document's own material definition.
    Document,
    /// The library entry.
    Library,
}

/// A material's merged physical properties, with per-property provenance.
#[derive(Debug, Clone, PartialEq, Serialize)]
pub struct ResolvedMaterial {
    /// The material key resolved.
    pub key: String,
    /// The library entry merged in, if any.
    pub library_id: Option<&'static str>,
    /// Display label: the library label, else the document material's name,
    /// else the key.
    pub label: String,
    /// Density, kg/m³.
    pub density_kg_m3: Option<f64>,
    /// Merged physical properties (`library` is the id merged, if any).
    pub physical: MaterialPhysical,
    /// Where each present property came from.
    pub sources: Vec<(Property, Source)>,
}

impl ResolvedMaterial {
    fn get(&self, p: Property) -> Option<f64> {
        let m = &self.physical;
        match p {
            Property::Density => self.density_kg_m3,
            Property::YoungsModulus => m.youngs_modulus_mpa,
            Property::Poisson => m.poisson,
            Property::YieldStrength => m.yield_strength_mpa,
            Property::ThermalConductivity => m.thermal_conductivity_w_mk.map(|k| k.effective()),
            Property::SpecificHeat => m.specific_heat_j_kgk,
            Property::Emissivity => m.emissivity,
            Property::RelativePermittivity => m.relative_permittivity,
            Property::LossTangent => m.loss_tangent,
            Property::RelativePermeability => m.relative_permeability,
            Property::Remanence => m.remanence_t,
            Property::Resistivity => m.resistivity_ohm_m,
        }
    }

    /// A scalar property, or [`MaterialError::MissingProperty`]. Thermal
    /// conductivity comes back as [`ThermalConductivity::effective`]; use
    /// [`thermal_conductivity`](Self::thermal_conductivity) for both axes.
    pub fn require(&self, p: Property) -> Result<f64, MaterialError> {
        self.get(p).ok_or_else(|| MaterialError::MissingProperty {
            key: self.key.clone(),
            property: p,
            library: self.library_id,
        })
    }

    /// The property if known (no error).
    pub fn optional(&self, p: Property) -> Option<f64> {
        self.get(p)
    }

    /// Where `p` came from, if present.
    pub fn source(&self, p: Property) -> Option<Source> {
        self.sources.iter().find(|(q, _)| *q == p).map(|(_, s)| *s)
    }

    /// Density, kg/m³.
    pub fn density_kg_m3(&self) -> Result<f64, MaterialError> {
        self.require(Property::Density)
    }
    /// Young's modulus, MPa.
    pub fn youngs_modulus_mpa(&self) -> Result<f64, MaterialError> {
        self.require(Property::YoungsModulus)
    }
    /// Poisson's ratio.
    pub fn poisson(&self) -> Result<f64, MaterialError> {
        self.require(Property::Poisson)
    }
    /// Yield strength, MPa.
    pub fn yield_strength_mpa(&self) -> Result<f64, MaterialError> {
        self.require(Property::YieldStrength)
    }
    /// Thermal conductivity (both axes), W/(m·K).
    pub fn thermal_conductivity(&self) -> Result<ThermalConductivity, MaterialError> {
        self.physical
            .thermal_conductivity_w_mk
            .ok_or_else(|| MaterialError::MissingProperty {
                key: self.key.clone(),
                property: Property::ThermalConductivity,
                library: self.library_id,
            })
    }
    /// Specific heat, J/(kg·K).
    pub fn specific_heat_j_kgk(&self) -> Result<f64, MaterialError> {
        self.require(Property::SpecificHeat)
    }
    /// Emissivity.
    pub fn emissivity(&self) -> Result<f64, MaterialError> {
        self.require(Property::Emissivity)
    }
    /// Relative permittivity.
    pub fn relative_permittivity(&self) -> Result<f64, MaterialError> {
        self.require(Property::RelativePermittivity)
    }
    /// Loss tangent.
    pub fn loss_tangent(&self) -> Result<f64, MaterialError> {
        self.require(Property::LossTangent)
    }
    /// Relative permeability.
    pub fn relative_permeability(&self) -> Result<f64, MaterialError> {
        self.require(Property::RelativePermeability)
    }
    /// Remanence, T.
    pub fn remanence_t(&self) -> Result<f64, MaterialError> {
        self.require(Property::Remanence)
    }
    /// Resistivity, Ω·m.
    pub fn resistivity_ohm_m(&self) -> Result<f64, MaterialError> {
        self.require(Property::Resistivity)
    }
}

/// Resolve material `key` of `doc` (see the [module docs](self) for the
/// precedence). Errors only when the material is unknown to both the
/// document and the library, or names an unknown `physical.library`; a
/// missing individual property errors when it is read.
pub fn resolve_material(doc: &Document, key: &str) -> Result<ResolvedMaterial, MaterialError> {
    resolve_def(key, doc.materials.get(key))
}

/// As [`resolve_material`] for a definition the caller already holds (or
/// none — then the key alone selects a library entry).
pub fn resolve_def(
    key: &str,
    def: Option<&MaterialDef>,
) -> Result<ResolvedMaterial, MaterialError> {
    let phys = def.and_then(|d| d.physical.as_ref());
    let lib = match phys.and_then(|p| p.library.as_deref()) {
        Some(id) => Some(
            library_by_id(id).ok_or_else(|| MaterialError::UnknownLibraryId {
                key: key.to_string(),
                library: id.to_string(),
            })?,
        ),
        None => library_match(key).or_else(|| def.and_then(|d| library_match(&d.name))),
    };
    let doc_has_any = def.is_some_and(|d| d.density.is_some()) || phys.is_some_and(has_any);
    if lib.is_none() && !doc_has_any {
        return Err(MaterialError::UnknownMaterial {
            key: key.to_string(),
        });
    }

    let l = lib.copied().unwrap_or(NONE);
    let d = phys.cloned().unwrap_or_default();
    let mut sources = Vec::new();
    let thermal = match (d.thermal_conductivity_w_mk, l.thermal_conductivity_w_mk) {
        (Some(k), _) => {
            sources.push((Property::ThermalConductivity, Source::Document));
            Some(k)
        }
        (None, Some(k)) => {
            sources.push((Property::ThermalConductivity, Source::Library));
            Some(k)
        }
        _ => None,
    };
    let mut pick = |p: Property, d: Option<f64>, l: Option<f64>| -> Option<f64> {
        match (d, l) {
            (Some(v), _) => {
                sources.push((p, Source::Document));
                Some(v)
            }
            (None, Some(v)) => {
                sources.push((p, Source::Library));
                Some(v)
            }
            _ => None,
        }
    };
    let density = pick(
        Property::Density,
        def.and_then(|d| d.density),
        l.density_kg_m3,
    );
    let physical = MaterialPhysical {
        library: lib.map(|m| m.id.to_string()),
        youngs_modulus_mpa: pick(
            Property::YoungsModulus,
            d.youngs_modulus_mpa,
            l.youngs_modulus_mpa,
        ),
        poisson: pick(Property::Poisson, d.poisson, l.poisson),
        yield_strength_mpa: pick(
            Property::YieldStrength,
            d.yield_strength_mpa,
            l.yield_strength_mpa,
        ),
        thermal_conductivity_w_mk: thermal,
        specific_heat_j_kgk: pick(
            Property::SpecificHeat,
            d.specific_heat_j_kgk,
            l.specific_heat_j_kgk,
        ),
        emissivity: pick(Property::Emissivity, d.emissivity, l.emissivity),
        relative_permittivity: pick(
            Property::RelativePermittivity,
            d.relative_permittivity,
            l.relative_permittivity,
        ),
        loss_tangent: pick(Property::LossTangent, d.loss_tangent, l.loss_tangent),
        relative_permeability: pick(
            Property::RelativePermeability,
            d.relative_permeability,
            l.relative_permeability,
        ),
        remanence_t: pick(Property::Remanence, d.remanence_t, l.remanence_t),
        resistivity_ohm_m: pick(
            Property::Resistivity,
            d.resistivity_ohm_m,
            l.resistivity_ohm_m,
        ),
    };
    let label = match (lib, def) {
        (Some(m), _) => m.label.to_string(),
        (None, Some(d)) if !d.name.is_empty() => d.name.clone(),
        _ => key.to_string(),
    };
    Ok(ResolvedMaterial {
        key: key.to_string(),
        library_id: lib.map(|m| m.id),
        label,
        density_kg_m3: density,
        physical,
        sources,
    })
}

fn has_any(p: &MaterialPhysical) -> bool {
    let MaterialPhysical {
        library: _,
        youngs_modulus_mpa,
        poisson,
        yield_strength_mpa,
        thermal_conductivity_w_mk,
        specific_heat_j_kgk,
        emissivity,
        relative_permittivity,
        loss_tangent,
        relative_permeability,
        remanence_t,
        resistivity_ohm_m,
    } = p;
    [
        youngs_modulus_mpa,
        poisson,
        yield_strength_mpa,
        specific_heat_j_kgk,
        emissivity,
        relative_permittivity,
        loss_tangent,
        relative_permeability,
        remanence_t,
        resistivity_ohm_m,
    ]
    .iter()
    .any(|v| v.is_some())
        || thermal_conductivity_w_mk.is_some()
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::{CsgOp, Node, SceneEntry, Vec3};

    fn doc_with(key: &str, def: MaterialDef) -> Document {
        let mut doc = Document::default();
        doc.nodes.insert(
            1,
            Node {
                id: 1,
                name: None,
                op: CsgOp::Cube {
                    size: Vec3::new(1.0, 1.0, 1.0),
                },
            },
        );
        doc.roots.push(SceneEntry {
            root: 1,
            material: key.into(),
            visible: None,
        });
        doc.materials.insert(key.into(), def);
        doc
    }

    #[test]
    fn library_ids_unique_and_matchable() {
        let mut ids: Vec<_> = LIBRARY.iter().map(|m| m.id).collect();
        ids.sort();
        ids.dedup();
        assert_eq!(ids.len(), LIBRARY.len());
        for m in LIBRARY {
            assert_eq!(library_match(m.id).unwrap().id, m.id, "{}", m.id);
            for a in m.aliases {
                assert_eq!(library_match(a).unwrap().id, m.id, "alias {a}");
            }
        }
    }

    #[test]
    fn required_library_entries_exist() {
        for id in [
            "pla",
            "petg",
            "abs",
            "nylon",
            "aluminum_6061",
            "steel",
            "stainless_304",
            "brass",
            "copper",
            "fr4",
            "ferrite_y30",
            "n42",
            "air",
            "bronze",
            "titanium",
            "glass",
            "wood",
        ] {
            assert!(library_by_id(id).is_some(), "missing {id}");
        }
    }

    #[test]
    fn substring_order() {
        assert_eq!(
            library_match("stainless_steel").unwrap().id,
            "stainless_304"
        );
        assert_eq!(library_match("mild steel").unwrap().id, "steel");
        assert_eq!(library_match("petg_clear").unwrap().id, "petg");
        assert_eq!(library_match("pla_red").unwrap().id, "pla");
        assert_eq!(library_match("Brass C360").unwrap().id, "brass");
        assert_eq!(library_match("aluminum").unwrap().id, "aluminum_6061");
        assert_eq!(library_match("FR-4").unwrap().id, "fr4");
        assert!(library_match("unobtainium").is_none());
        assert!(library_match("default").is_none());
        assert!(library_match("").is_none());
    }

    #[test]
    fn document_beats_library_per_property() {
        let doc = doc_with(
            "enclosure_pla",
            MaterialDef {
                name: "enclosure".into(),
                density: Some(1100.0),
                physical: Some(MaterialPhysical {
                    youngs_modulus_mpa: Some(2_400.0),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let m = resolve_material(&doc, "enclosure_pla").unwrap();
        assert_eq!(m.library_id, Some("pla"));
        assert_eq!(m.youngs_modulus_mpa().unwrap(), 2_400.0);
        assert_eq!(m.source(Property::YoungsModulus), Some(Source::Document));
        assert_eq!(m.density_kg_m3().unwrap(), 1100.0);
        assert_eq!(m.source(Property::Density), Some(Source::Document));
        // unset in the doc → library
        assert_eq!(m.poisson().unwrap(), 0.36);
        assert_eq!(m.source(Property::Poisson), Some(Source::Library));
    }

    #[test]
    fn explicit_library_beats_key_match() {
        let doc = doc_with(
            "steel_look_plastic",
            MaterialDef {
                physical: Some(MaterialPhysical {
                    library: Some("abs".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let m = resolve_material(&doc, "steel_look_plastic").unwrap();
        assert_eq!(m.library_id, Some("abs"));
        assert_eq!(m.youngs_modulus_mpa().unwrap(), 2_200.0);
    }

    #[test]
    fn unknown_library_id_is_error() {
        let doc = doc_with(
            "x",
            MaterialDef {
                physical: Some(MaterialPhysical {
                    library: Some("unobtainium".into()),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        assert!(matches!(
            resolve_material(&doc, "x"),
            Err(MaterialError::UnknownLibraryId { .. })
        ));
    }

    #[test]
    fn unknown_material_is_error_not_aluminium() {
        let doc = doc_with("default", MaterialDef::default());
        let e = resolve_material(&doc, "default").unwrap_err();
        assert_eq!(
            e,
            MaterialError::UnknownMaterial {
                key: "default".into()
            }
        );
        assert!(e.to_string().contains("physical"));
        assert!(resolve_material(&Document::default(), "unobtainium").is_err());
    }

    #[test]
    fn display_name_matches_when_key_does_not() {
        let doc = doc_with(
            "mat1",
            MaterialDef {
                name: "Brass C360".into(),
                ..Default::default()
            },
        );
        assert_eq!(
            resolve_material(&doc, "mat1").unwrap().library_id,
            Some("brass")
        );
    }

    #[test]
    fn doc_only_material_resolves_and_reports_missing() {
        let doc = doc_with(
            "mystery",
            MaterialDef {
                physical: Some(MaterialPhysical {
                    youngs_modulus_mpa: Some(5_000.0),
                    ..Default::default()
                }),
                ..Default::default()
            },
        );
        let m = resolve_material(&doc, "mystery").unwrap();
        assert_eq!(m.library_id, None);
        assert_eq!(m.youngs_modulus_mpa().unwrap(), 5_000.0);
        let e = m.poisson().unwrap_err();
        assert!(matches!(
            e,
            MaterialError::MissingProperty {
                property: Property::Poisson,
                library: None,
                ..
            }
        ));
        assert!(e.to_string().contains("physical.poisson"));
    }

    #[test]
    fn air_has_no_stiffness() {
        let m = resolve_material(&Document::default(), "air").unwrap();
        assert!(m.density_kg_m3().unwrap() < 2.0);
        assert!(matches!(
            m.youngs_modulus_mpa(),
            Err(MaterialError::MissingProperty {
                property: Property::YoungsModulus,
                library: Some("air"),
                ..
            })
        ));
    }

    #[test]
    fn fr4_is_anisotropic_magnets_have_remanence() {
        let fr4 = resolve_material(&Document::default(), "fr4").unwrap();
        let k = fr4.thermal_conductivity().unwrap();
        assert!(k.in_plane() > k.through_plane());
        assert!((fr4.relative_permittivity().unwrap() - 4.4).abs() < 1e-12);
        let n42 = resolve_material(&Document::default(), "n42").unwrap();
        assert!((n42.remanence_t().unwrap() - 1.30).abs() < 1e-12);
        let y30 = resolve_material(&Document::default(), "ferrite_y30").unwrap();
        assert!(y30.remanence_t().unwrap() < n42.remanence_t().unwrap());
    }

    #[test]
    fn physical_json_round_trip() {
        let def = MaterialDef {
            name: "board".into(),
            color: [0.1, 0.4, 0.2],
            density: Some(1850.0),
            physical: Some(MaterialPhysical {
                library: Some("fr4".into()),
                youngs_modulus_mpa: Some(22_000.0),
                thermal_conductivity_w_mk: Some(ThermalConductivity::Anisotropic {
                    in_plane: 0.9,
                    through_plane: 0.3,
                }),
                relative_permittivity: Some(4.2),
                loss_tangent: Some(0.018),
                remanence_t: Some(0.0),
                ..Default::default()
            }),
            ..Default::default()
        };
        let json = serde_json::to_string(&def).unwrap();
        assert!(json.contains("\"youngsModulusMpa\":22000"), "{json}");
        assert!(json.contains("\"inPlane\":0.9"), "{json}");
        assert!(!json.contains("emissivity"), "unset fields skipped: {json}");
        let back: MaterialDef = serde_json::from_str(&json).unwrap();
        assert_eq!(back, def);
        // isotropic k is a bare number
        let iso: MaterialPhysical =
            serde_json::from_str(r#"{"thermalConductivityWMk": 0.2}"#).unwrap();
        assert_eq!(
            iso.thermal_conductivity_w_mk,
            Some(ThermalConductivity::Isotropic(0.2))
        );
        // snake_case accepted on input
        let snake: MaterialPhysical = serde_json::from_str(
            r#"{"youngs_modulus_mpa": 3, "thermal_conductivity_w_mk": {"in_plane": 1, "through_plane": 0.5}, "remanence_t": 1.2}"#,
        )
        .unwrap();
        assert_eq!(snake.youngs_modulus_mpa, Some(3.0));
        assert_eq!(snake.remanence_t, Some(1.2));
        assert_eq!(
            snake.thermal_conductivity_w_mk.unwrap().through_plane(),
            0.5
        );
    }

    #[test]
    fn old_documents_load_unchanged() {
        // A pre-physical material definition: loads with physical = None and
        // re-serializes byte-identically (no new keys appear).
        let old = r#"{"name":"aluminum","color":[0.9,0.9,0.9],"metallic":1.0,"roughness":0.3,"density":2700.0}"#;
        let def: MaterialDef = serde_json::from_str(old).unwrap();
        assert!(def.physical.is_none());
        assert_eq!(serde_json::to_string(&def).unwrap(), old);
        // and still resolves from the library, density from the document
        let m = resolve_def("aluminum", Some(&def)).unwrap();
        assert_eq!(m.library_id, Some("aluminum_6061"));
        assert_eq!(m.source(Property::Density), Some(Source::Document));
        assert_eq!(m.youngs_modulus_mpa().unwrap(), 68_900.0);
    }

    #[test]
    fn json_keys_match_serde() {
        // Property::json_key must name the real serialized key.
        let full = MaterialPhysical {
            library: None,
            youngs_modulus_mpa: Some(1.0),
            poisson: Some(1.0),
            yield_strength_mpa: Some(1.0),
            thermal_conductivity_w_mk: Some(ThermalConductivity::Isotropic(1.0)),
            specific_heat_j_kgk: Some(1.0),
            emissivity: Some(1.0),
            relative_permittivity: Some(1.0),
            loss_tangent: Some(1.0),
            relative_permeability: Some(1.0),
            remanence_t: Some(1.0),
            resistivity_ohm_m: Some(1.0),
        };
        let v = serde_json::to_value(&full).unwrap();
        for p in Property::ALL {
            if p == Property::Density {
                continue;
            }
            assert!(v.get(p.json_key()).is_some(), "{p:?} → {}", p.json_key());
        }
    }
}

//! `[material-physics ...]`: physical properties authored in loon reach the
//! document, merge with `[material ...]` in either order, and survive
//! doc → loon → doc.

use vcad_ir::materials::{resolve_material, Property, Source};
use vcad_ir::ThermalConductivity;

#[test]
fn physics_form_sets_properties_and_merges_with_material() {
    // physics before the PBR definition: order must not matter
    let doc = vcad_loon::eval_vcad(
        r#"
[material-physics "case" :library "pla" :density 1200 :youngs-modulus 2400]
[material "case" 0.9 0.2 0.2 0.0 0.5]
[root [cube 10 10 10] "case"]
"#,
        None,
    )
    .unwrap();
    let def = &doc.materials["case"];
    assert_eq!(def.color, [0.9, 0.2, 0.2], "PBR kept");
    assert_eq!(def.density, Some(1200.0));
    let p = def.physical.as_ref().unwrap();
    assert_eq!(p.library.as_deref(), Some("pla"));
    assert_eq!(p.youngs_modulus_mpa, Some(2400.0));

    let m = resolve_material(&doc, "case").unwrap();
    assert_eq!(m.youngs_modulus_mpa().unwrap(), 2400.0);
    assert_eq!(m.source(Property::YoungsModulus), Some(Source::Document));
    assert_eq!(m.poisson().unwrap(), 0.36);
    assert_eq!(m.source(Property::Poisson), Some(Source::Library));
}

#[test]
fn physics_form_alone_creates_the_material() {
    let doc = vcad_loon::eval_vcad(
        r#"
[material-physics "board" :library "fr4" :k-in-plane 0.9 :k-through-plane 0.3 :permittivity 4.2 :loss-tangent 0.018]
[material-physics "magnet" :remanence 1.32 :permeability 1.05 :density 7500]
[root [cube 10 10 1.6] "board"]
"#,
        None,
    )
    .unwrap();
    let board = doc.materials["board"].physical.as_ref().unwrap();
    assert_eq!(
        board.thermal_conductivity_w_mk,
        Some(ThermalConductivity::Anisotropic {
            in_plane: 0.9,
            through_plane: 0.3
        })
    );
    assert_eq!(board.relative_permittivity, Some(4.2));
    let magnet = resolve_material(&doc, "magnet").unwrap();
    assert_eq!(magnet.library_id, None, "no library match for `magnet`");
    assert_eq!(magnet.remanence_t().unwrap(), 1.32);
    assert!(magnet.youngs_modulus_mpa().is_err());
}

#[test]
fn later_forms_override_per_property_inside_a_vec() {
    let doc = vcad_loon::eval_vcad(
        r#"
#[[material-physics "m" :youngs-modulus 1000 :poisson 0.3]
  [material-physics "m" :youngs-modulus 2000]]
"#,
        None,
    )
    .unwrap();
    let p = doc.materials["m"].physical.as_ref().unwrap();
    assert_eq!(p.youngs_modulus_mpa, Some(2000.0));
    assert_eq!(p.poisson, Some(0.3));
}

#[test]
fn bad_options_are_errors() {
    for (src, needle) in [
        (r#"[material-physics "m" :youngs 3]"#, "unknown option"),
        (
            r#"[material-physics "m" :library "unobtainium"]"#,
            "unknown :library",
        ),
        (r#"[material-physics "m" :k-in-plane 1]"#, "go together"),
        (r#"[material-physics "m" :poisson]"#, "needs a value"),
        (r#"[material-physics "m" :density -1]"#, ">= 0"),
    ] {
        let err = vcad_loon::eval_vcad(src, None).unwrap_err();
        assert!(err.contains(needle), "{src}: {err}");
    }
}

#[test]
fn doc_to_loon_to_doc_keeps_physical_properties() {
    let doc = vcad_loon::eval_vcad(
        r#"
[material-physics "board" :library "fr4" :density 1850 :k-in-plane 0.81 :k-through-plane 0.29 :resistivity 0.0000000172 :youngs-modulus 24000]
[root [cube 10 10 1.6] "board"]
"#,
        None,
    )
    .unwrap();
    let loon = vcad_ir::to_loon::document_to_loon(&doc);
    let back = vcad_loon::eval_vcad(&loon, None).unwrap_or_else(|e| panic!("{e}\n{loon}"));
    assert_eq!(
        back.materials["board"].physical, doc.materials["board"].physical,
        "{loon}"
    );
    assert_eq!(back.materials["board"].density, Some(1850.0));
}

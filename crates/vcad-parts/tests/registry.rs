//! Registry integration tests: every part builds with defaults, manifest
//! serializes, and unknown paths error cleanly.

use std::collections::HashMap;
use vcad_parts::{all_parts, build_part, find_part, manifest_json};

#[test]
fn every_part_builds_with_default_params() {
    for entry in all_parts() {
        let params: HashMap<String, serde_json::Value> = entry
            .meta
            .params
            .iter()
            .map(|p| (p.name().to_string(), p.default_json()))
            .collect();
        let doc = (entry.build)(&vcad_parts::Params::new(entry.meta.params, &params))
            .unwrap_or_else(|e| panic!("part {} failed to build: {e}", entry.meta.id));
        assert!(
            !doc.nodes.is_empty(),
            "part {} produced an empty document",
            entry.meta.id
        );
        assert!(
            !doc.roots.is_empty(),
            "part {} produced no scene root",
            entry.meta.id
        );
    }
}

#[test]
fn build_part_dispatches_by_path() {
    let mut params = HashMap::new();
    params.insert("size".to_string(), serde_json::json!("M6"));
    params.insert("length".to_string(), serde_json::json!(20.0));
    let doc = build_part("std:fastener.bolt.socket-head", &params).unwrap();
    assert!(doc.nodes.len() >= 4);
}

#[test]
fn build_part_accepts_path_without_std_prefix() {
    let params = HashMap::new();
    let doc = build_part("bearing.608", &params).unwrap();
    assert!(!doc.nodes.is_empty());
}

#[test]
fn unknown_path_errors() {
    let params = HashMap::new();
    let err = build_part("std:does.not.exist", &params).unwrap_err();
    assert!(err.contains("unknown part"));
}

#[test]
fn manifest_json_is_valid_and_contains_all_parts() {
    let json = manifest_json();
    let parsed: serde_json::Value = serde_json::from_str(&json).unwrap();
    let arr = parsed.as_array().expect("manifest is a JSON array");
    assert_eq!(arr.len(), all_parts().len());
    for entry in all_parts() {
        let found = arr
            .iter()
            .any(|v| v.get("id").and_then(|i| i.as_str()) == Some(entry.meta.id));
        assert!(found, "manifest is missing {}", entry.meta.id);
    }
}

#[test]
fn find_part_is_case_sensitive_and_stable() {
    assert!(find_part("std:fastener.washer.flat").is_some());
    assert!(find_part("fastener.washer.flat").is_some());
    assert!(find_part("std:fastener.WASHER.flat").is_none());
}

#[test]
fn search_tokens_include_xref_numbers() {
    for entry in all_parts() {
        let manifest = entry.manifest_entry();
        for xref in &manifest.xrefs {
            if let Some(m) = &xref.mcmaster {
                assert!(
                    manifest.search_tokens.iter().any(|t| t == m),
                    "mcmaster number {m} missing from search tokens for {}",
                    entry.meta.id
                );
            }
        }
    }
}

#[test]
fn described_parts_have_well_formed_ports() {
    use vcad_parts::part_info;
    for entry in all_parts().iter().filter(|e| e.info.is_some()) {
        let info = part_info(entry.meta.id, &HashMap::new())
            .unwrap_or_else(|e| panic!("{}: {e}", entry.meta.id));
        assert!(!info.ports.is_empty(), "{} has no ports", entry.meta.id);
        let mut names: Vec<_> = info.ports.iter().map(|p| p.name.as_str()).collect();
        names.sort_unstable();
        names.dedup();
        assert_eq!(
            names.len(),
            info.ports.len(),
            "{}: duplicate port names",
            entry.meta.id
        );
        for p in &info.ports {
            let n = |v: [f64; 3]| (v[0] * v[0] + v[1] * v[1] + v[2] * v[2]).sqrt();
            let d = p.axis[0] * p.up[0] + p.axis[1] * p.up[1] + p.axis[2] * p.up[2];
            assert!(
                (n(p.axis) - 1.0).abs() < 1e-9,
                "{}:{} axis",
                entry.meta.id,
                p.name
            );
            assert!(
                (n(p.up) - 1.0).abs() < 1e-9,
                "{}:{} up",
                entry.meta.id,
                p.name
            );
            assert!(d.abs() < 1e-9, "{}:{} up not ⟂ axis", entry.meta.id, p.name);
            assert!(p.family == "CF" || p.family == "KF");
        }
        assert!(
            info.metadata
                .get("bom")
                .and_then(|b| b.as_array())
                .is_some_and(|b| !b.is_empty()),
            "{} has no BOM line",
            entry.meta.id
        );
    }
}

#[test]
fn every_vacuum_size_builds() {
    use vcad_parts::vacuum::{CF_SIZES, KF_SIZES};
    for s in CF_SIZES {
        let mut p = HashMap::new();
        p.insert("size".to_string(), serde_json::json!(s));
        for id in [
            "vacuum.cf.flange",
            "vacuum.cf.gasket",
            "vacuum.cf.bolt-set",
            "vacuum.nipple",
        ] {
            build_part(id, &p).unwrap_or_else(|e| panic!("{id} {s}: {e}"));
        }
    }
    for s in KF_SIZES {
        let mut p = HashMap::new();
        p.insert("size".to_string(), serde_json::json!(s));
        for id in [
            "vacuum.kf.flange",
            "vacuum.kf.centering-ring",
            "vacuum.kf.clamp",
            "vacuum.tee",
            "vacuum.cross",
        ] {
            build_part(id, &p).unwrap_or_else(|e| panic!("{id} {s}: {e}"));
        }
    }
}

#[test]
fn uncatalogued_dn25_tee_fails_closed() {
    let mut p = HashMap::new();
    p.insert("size".to_string(), serde_json::json!("DN25"));
    let err = build_part("vacuum.tee", &p).unwrap_err();
    assert!(err.contains("center_to_face"), "{err}");
    p.insert("center_to_face".to_string(), serde_json::json!(60.0));
    build_part("vacuum.tee", &p).unwrap();
}

#[test]
fn hv_rating_is_echoed_not_derived() {
    let mut p = HashMap::new();
    p.insert("rated_kv".to_string(), serde_json::json!(40));
    let info = vcad_parts::part_info("vacuum.feedthrough.hv", &p).unwrap();
    assert_eq!(info.metadata["rated_kv"], 40.0);
    assert!(info.metadata["creepage_air_mm_model"].as_f64().unwrap() >= 120.0);
    assert_eq!(info.metadata["suggested_clearance"][0]["min_mm"], 40.0);
}

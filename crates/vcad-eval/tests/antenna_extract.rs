//! Board → antenna extraction (`vcad_eval::antenna_extract`).
//!
//! Fixture: a 50 × 50 mm, 1.6 mm FR-4 board with a GND pour on both
//! copper layers over y ∈ [0, 30] and a 2.4 GHz printed monopole on net
//! ANT, fed by U.FL pad J1.1 sitting on the pour edge at (25, 30). The
//! board JSON is the same `Pcb` shape the ECAD MCP tools (`place_components`
//! → `add_zone` → `add_trace`) write into a `PcbBoard` node.
//!
//! The headline test compares the extracted spec against a hand-built
//! `WireGrid` of the same geometry (the PoC's construction): Z_in must
//! agree to solver precision. The rest pin every refusal branch.

use std::collections::BTreeMap;

use serde_json::{json, Value};
use vcad_eval::antenna_extract::{
    extract_antenna_spec, AntennaExtractError, AntennaExtraction, FeedPad, FeedTreatment,
    GroundModel,
};
use vcad_ir::ecad::Pcb;
use vcad_ir::{CsgOp, Document, Node};
use vcad_kernel_antenna::ecad::add_trace_as_wire;
use vcad_kernel_antenna::receipt::{predicted_claims, FrequencyBand};
use vcad_kernel_antenna::{solve_driven, Mesh, SolveOptions, WireGrid};

fn v(x: f64, y: f64) -> Value {
    json!({"x": x, "y": y})
}

fn trace(a: (f64, f64), b: (f64, f64), w: f64, layer: &str, net: &str) -> Value {
    json!({"start": v(a.0, a.1), "end": v(b.0, b.1), "width": w, "layer": layer, "net": net})
}

fn rect_zone(x0: f64, y0: f64, x1: f64, y1: f64, layer: &str, net: &str) -> Value {
    json!({
        "outline": [v(x0, y0), v(x1, y0), v(x1, y1), v(x0, y1)],
        "net": net, "layer": layer, "clearance": 0.3
    })
}

/// The fixture board with the given antenna traces.
fn board(traces: Vec<Value>) -> Value {
    json!({
        "outline": {
            "vertices": [v(0.0, 0.0), v(50.0, 0.0), v(50.0, 50.0), v(0.0, 50.0)],
            "thickness": 1.6
        },
        "stackup": {"layers": [
            {"layer": "FCu", "copperThickness": 0.035},
            {"layer": "BCu", "copperThickness": 0.035,
             "dielectricThickness": 1.53, "dielectricEr": 4.4, "material": "FR4"}
        ]},
        "nets": [{"id": "ANT", "name": "ANT"}, {"id": "GND", "name": "GND"}],
        "rules": {
            "defaultRules": {"name": "Default", "traceWidth": 0.25, "clearance": 0.2,
                             "viaDiameter": 0.6, "viaDrill": 0.3},
            "netClassAssignments": {},
            "edgeClearance": 0.3, "holeToHole": 0.25, "minAnnularRing": 0.13, "minDrill": 0.3
        },
        "footprints": [{
            "ref": "J1", "value": "U.FL", "footprintName": "U.FL",
            "position": v(25.0, 30.0),
            "pads": [
                {"number": "1", "padType": "SMD", "shape": {"type": "Rect", "width": 1.0, "height": 1.0},
                 "position": v(0.0, 0.0), "net": "ANT", "layers": ["FCu"]},
                {"number": "2", "padType": "SMD", "shape": {"type": "Rect", "width": 1.0, "height": 2.0},
                 "position": v(-1.5, -1.5), "net": "GND", "layers": ["FCu"]}
            ]
        }],
        "traces": traces,
        "zones": [
            rect_zone(0.0, 0.0, 50.0, 30.0, "FCu", "GND"),
            rect_zone(0.0, 0.0, 50.0, 30.0, "BCu", "GND")
        ]
    })
}

fn l_traces() -> Vec<Value> {
    vec![
        trace((25.0, 30.0), (25.0, 45.0), 1.0, "FCu", "ANT"),
        trace((25.0, 45.0), (39.0, 45.0), 1.0, "FCu", "ANT"),
    ]
}

fn doc_of(board: Value) -> Document {
    let pcb: Pcb = serde_json::from_value(board).expect("fixture board parses as Pcb");
    let mut doc = Document::new();
    doc.nodes.insert(
        7,
        Node {
            id: 7,
            name: Some("PCB Board".into()),
            op: CsgOp::PcbBoard {
                board: Box::new(pcb),
            },
        },
    );
    doc
}

fn sel() -> AntennaExtraction {
    AntennaExtraction {
        board_node: None,
        net: None,
        feed: FeedPad {
            footprint: "J1".into(),
            pad: "1".into(),
        },
        ground_net: "GND".into(),
        f_max_hz: 3.0e9,
        segments_per_wavelength: 20.0,
        max_feed_gap_mm: 2.0,
        ground: GroundModel::InfiniteImage,
    }
}

fn z_in(mesh: &Mesh, feed: usize, f: f64) -> vcad_kernel_antenna::Complex {
    solve_driven(mesh, feed, f, &SolveOptions::default())
        .unwrap()
        .z_in
}

#[test]
fn extracted_l_monopole_matches_the_hand_built_grid() {
    let ex = extract_antenna_spec(&doc_of(board(l_traces())), &sel()).unwrap();
    assert_eq!(ex.board_node, 7);
    assert_eq!(ex.net, "ANT");
    assert_eq!(ex.trace_indices, vec![0, 1]);
    assert_eq!(ex.feed.treatment, FeedTreatment::OnEdge);
    assert!(ex.spec.ground_plane);
    // Edge (50,30)→(0,30): solver +x = board −x, solver +z = board +y.
    assert_eq!(ex.frame.origin_board_mm, [25.0, 30.0]);
    assert_eq!(ex.frame.x_axis_board, [-1.0, 0.0]);
    assert_eq!(ex.frame.z_axis_board, [0.0, 1.0]);
    // λ(3 GHz)/20 ≈ 5.0 mm → 15 mm leg: 4 segments, 14 mm leg: 3.
    assert_eq!(ex.segments, 7);

    // The PoC's construction, by hand: base-fed trace over infinite ground.
    let mut g = WireGrid::new();
    g.set_ground_plane(true);
    add_trace_as_wire(
        &mut g,
        &[[0.0, 0.0, 0.0], [0.0, 0.0, 15.0], [-14.0, 0.0, 15.0]],
        1.0,
        &[4, 3],
    )
    .unwrap();
    let hand = Mesh::build(&g).unwrap();
    let hand_feed = hand.nearest_basis([0.0, 0.0, 0.0]).unwrap();

    let (mesh, feed) = ex.spec.resolve(&ex.parameters).unwrap();
    assert_eq!(mesh.segments.len(), hand.segments.len());
    assert_eq!(mesh.bases.len(), hand.bases.len());
    for f in [2.0e9, 2.4e9, 2.45e9, 2.8e9] {
        let (a, b) = (z_in(&mesh, feed, f), z_in(&hand, hand_feed, f));
        let rel = (a - b).abs() / b.abs();
        assert!(
            rel < 1e-9,
            "Z_in at {f:.3e} Hz: extracted {a:?} vs hand {b:?} (rel {rel:.2e})"
        );
    }

    // Metadata for the dielectric model (G7), never defaulted.
    assert_eq!(ex.substrate.board_thickness_mm, 1.6);
    assert_eq!(ex.substrate.dielectric_er, Some(4.4));
    assert_eq!(ex.substrate.dielectric_thickness_mm, Some(1.53));
    assert_eq!(ex.substrate.antenna_copper_thickness_mm, Some(0.035));
    // Ground approximation recorded with the real pour size.
    assert_eq!(ex.ground.model, "infinite_image");
    assert!((ex.ground.pour_along_edge_mm - 50.0).abs() < 1e-9);
    assert!((ex.ground.pour_depth_mm - 30.0).abs() < 1e-9);
    assert!((ex.ground.pour_along_edge_wavelengths - 0.5003).abs() < 1e-3);
    assert!(ex.notes.iter().any(|n| n.contains("20–30 % BELOW")));
}

#[test]
fn extracted_straight_monopole_matches_the_poc_build_and_prices_claims() {
    // The PoC monopole: 1.0 mm wide, 16 segments, base-fed, ant_len 18 mm
    // here (fits the 20 mm keepout).
    let ex = extract_antenna_spec(
        &doc_of(board(vec![trace(
            (25.0, 30.0),
            (25.0, 48.0),
            1.0,
            "FCu",
            "ANT",
        )])),
        &AntennaExtraction {
            segments_per_wavelength: 85.0, // λ(3 GHz)/85 ≈ 1.18 mm → 18 mm in 16 segments
            ..sel()
        },
    )
    .unwrap();
    assert_eq!(ex.segments, 16);
    let mut g = WireGrid::new();
    g.set_ground_plane(true);
    add_trace_as_wire(&mut g, &[[0.0, 0.0, 0.0], [0.0, 0.0, 18.0]], 1.0, &[16]).unwrap();
    let hand = Mesh::build(&g).unwrap();
    let hand_feed = hand.nearest_basis([0.0, 0.0, 0.0]).unwrap();
    let (mesh, feed) = ex.spec.resolve(&BTreeMap::new()).unwrap();
    for f in [2.4e9, 3.0e9, 3.8e9] {
        let (a, b) = (z_in(&mesh, feed, f), z_in(&hand, hand_feed, f));
        assert!((a - b).abs() / b.abs() < 1e-9, "{f}: {a:?} vs {b:?}");
    }
    // The claim set a receipt consumes runs on the extracted mesh.
    let band = FrequencyBand {
        f_lo_hz: 3.0e9,
        f_hi_hz: 4.4e9,
        points: 15,
    };
    let opts = SolveOptions::default();
    let claims = predicted_claims(&mesh, feed, band, 50.0, &opts).unwrap();
    assert_eq!(claims.schema, "vcad.antenna-claims/1");
    assert!(claims.provenance.ground_plane);
}

#[test]
fn feed_gap_is_bridged_or_snapped_and_recorded() {
    // Pour pulled back 1.5 mm (≥ width 1.0) → lead wire.
    let mut b = board(l_traces());
    b["zones"] = json!([rect_zone(0.0, 0.0, 50.0, 28.5, "FCu", "GND")]);
    let ex = extract_antenna_spec(&doc_of(b), &sel()).unwrap();
    assert_eq!(ex.feed.treatment, FeedTreatment::LeadWire);
    assert!((ex.feed.gap_mm - 1.5).abs() < 1e-9);
    assert_eq!(ex.spec.elements.len(), 3);
    assert!(ex.notes.iter().any(|n| n.contains("lead wire")));

    // 0.4 mm gap (< width) → feed end snapped onto the edge.
    let mut b = board(l_traces());
    b["zones"] = json!([rect_zone(0.0, 0.0, 50.0, 29.6, "FCu", "GND")]);
    let ex = extract_antenna_spec(&doc_of(b), &sel()).unwrap();
    assert_eq!(ex.feed.treatment, FeedTreatment::Snapped);
    assert_eq!(ex.spec.elements.len(), 2);
    assert!(ex.notes.iter().any(|n| n.contains("moved")));
}

#[test]
fn net_is_inferred_from_the_feed_pad_and_must_agree() {
    let doc = doc_of(board(l_traces()));
    let ex = extract_antenna_spec(&doc, &sel()).unwrap();
    assert_eq!(ex.net, "ANT");
    let err = extract_antenna_spec(
        &doc,
        &AntennaExtraction {
            net: Some("GND".into()),
            ..sel()
        },
    )
    .unwrap_err();
    assert!(matches!(err, AntennaExtractError::Feed(_)), "{err}");
}

// ── refusals ─────────────────────────────────────────────────────────

fn refuse(b: Value, s: AntennaExtraction) -> AntennaExtractError {
    extract_antenna_spec(&doc_of(b), &s).expect_err("extraction must fail closed")
}

fn msg(e: &AntennaExtractError) -> String {
    e.to_string()
}

#[test]
fn refuses_arc_on_antenna_net() {
    let mut b = board(l_traces());
    b["traceArcs"] = json!([{"center": v(30.0, 45.0), "radius": 3.0, "startAngle": 0.0,
        "endAngle": 90.0, "width": 1.0, "layer": "FCu", "net": "ANT"}]);
    let e = refuse(b, sel());
    assert!(
        matches!(e, AntennaExtractError::Unsupported(_)) && msg(&e).contains("arc"),
        "{e}"
    );
}

#[test]
fn refuses_via_on_antenna_net() {
    let mut b = board(l_traces());
    b["vias"] = json!([{"position": v(25.0, 45.0), "diameter": 0.6, "drill": 0.3,
        "startLayer": "FCu", "endLayer": "BCu", "net": "ANT"}]);
    assert!(msg(&refuse(b, sel())).contains("via"));
}

#[test]
fn refuses_zone_on_antenna_net() {
    let mut b = board(l_traces());
    b["zones"]
        .as_array_mut()
        .unwrap()
        .push(rect_zone(30.0, 40.0, 40.0, 48.0, "FCu", "ANT"));
    assert!(msg(&refuse(b, sel())).contains("zone"));
}

#[test]
fn refuses_extra_pad_on_antenna_net() {
    let mut b = board(l_traces());
    b["footprints"].as_array_mut().unwrap().push(json!({
        "ref": "C1", "value": "1p", "footprintName": "C0402", "position": v(39.0, 45.0),
        "pads": [{"number": "1", "padType": "SMD", "shape": {"type": "Rect", "width": 0.5, "height": 0.5},
                  "position": v(0.0, 0.0), "net": "ANT", "layers": ["FCu"]}]
    }));
    let e = refuse(b, sel());
    assert!(msg(&e).contains("C1.1"), "{e}");
}

#[test]
fn refuses_multi_layer_copper() {
    let mut t = l_traces();
    t[1]["layer"] = json!("BCu");
    assert!(msg(&refuse(board(t), sel())).contains("single-layer"));
}

#[test]
fn refuses_disconnected_islands() {
    let mut t = l_traces();
    t.push(trace((5.0, 40.0), (15.0, 40.0), 1.0, "FCu", "ANT"));
    assert!(msg(&refuse(board(t), sel())).contains("islands"));
}

#[test]
fn refuses_feed_pad_without_a_trace_end() {
    let t = vec![
        trace((20.0, 35.0), (20.0, 45.0), 1.0, "FCu", "ANT"),
        trace((20.0, 45.0), (30.0, 45.0), 1.0, "FCu", "ANT"),
    ];
    let e = refuse(board(t), sel());
    assert!(matches!(e, AntennaExtractError::Feed(_)), "{e}");
}

#[test]
fn refuses_feed_that_is_not_a_free_end() {
    // A trace continuing along the pour edge from the feed: the feed node
    // has degree 2 (and that trace would lie on the plane anyway).
    let mut t = l_traces();
    t.push(trace((25.0, 30.0), (25.0, 33.0), 1.0, "FCu", "ANT"));
    t[0] = trace((25.0, 33.0), (25.0, 45.0), 1.0, "FCu", "ANT");
    t.push(trace((25.0, 30.0), (20.0, 33.0), 1.0, "FCu", "ANT"));
    let e = refuse(board(t), sel());
    assert!(msg(&e).contains("free trace end"), "{e}");
}

#[test]
fn refuses_copper_behind_the_pour_edge() {
    // A microstrip feedline over the pour, then the radiator.
    let t = vec![
        trace((25.0, 30.0), (25.0, 25.0), 1.0, "FCu", "ANT"),
        trace((25.0, 25.0), (30.0, 25.0), 1.0, "FCu", "ANT"),
    ];
    let e = refuse(board(t), sel());
    assert!(msg(&e).contains("behind"), "{e}");
}

#[test]
fn refuses_a_pour_wrapping_the_antenna() {
    let mut b = board(l_traces());
    b["zones"]
        .as_array_mut()
        .unwrap()
        .push(rect_zone(0.0, 30.0, 10.0, 50.0, "BCu", "GND"));
    let e = refuse(b, sel());
    assert!(
        matches!(e, AntennaExtractError::Ground(_)) && msg(&e).contains("past"),
        "{e}"
    );
}

#[test]
fn refuses_large_feed_gap() {
    let mut b = board(l_traces());
    b["zones"] = json!([rect_zone(0.0, 0.0, 50.0, 25.0, "FCu", "GND")]);
    assert!(msg(&refuse(b, sel())).contains("max_feed_gap_mm"));
}

#[test]
fn refuses_feed_inside_the_pour() {
    let mut b = board(l_traces());
    b["zones"] = json!([rect_zone(0.0, 0.0, 50.0, 31.0, "FCu", "GND")]);
    let e = refuse(b, sel());
    assert!(msg(&e).contains("inside the pour"), "{e}");
}

#[test]
fn refuses_missing_ground_pour() {
    let mut b = board(l_traces());
    b["zones"] = json!([]);
    let e = refuse(b, sel());
    assert!(matches!(e, AntennaExtractError::Ground(_)), "{e}");
}

#[test]
fn refuses_trace_too_short_for_a_thin_wire_segment() {
    let mut t = l_traces();
    t[1] = trace((25.0, 45.0), (25.6, 45.0), 1.0, "FCu", "ANT");
    let e = refuse(board(t), sel());
    assert!(msg(&e).contains("thin-wire"), "{e}");
}

#[test]
fn refuses_missing_or_ambiguous_board() {
    let e = extract_antenna_spec(&Document::new(), &sel()).unwrap_err();
    assert!(matches!(e, AntennaExtractError::Board(_)), "{e}");
    let mut doc = doc_of(board(l_traces()));
    let n = doc.nodes[&7].clone();
    doc.nodes.insert(8, Node { id: 8, ..n });
    let e = extract_antenna_spec(&doc, &sel()).unwrap_err();
    assert!(msg(&e).contains("board_node"), "{e}");
    let ok = extract_antenna_spec(
        &doc,
        &AntennaExtraction {
            board_node: Some(8),
            ..sel()
        },
    )
    .unwrap();
    assert_eq!(ok.board_node, 8);
}

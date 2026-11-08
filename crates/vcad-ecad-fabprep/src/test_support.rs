//! Small hand-built boards for the unit tests.
//!
//! Deliberately tiny: the pipeline's behaviour under a fixture baseline, a
//! strippable offender, and a fail-closed stall is all reproducible on a 2-layer
//! 10×10 mm board, and a test that takes a minute is a test nobody runs.

use vcad_ir::ecad::{
    BoardOutline, DesignRules, Footprint, LayerStackup, NetClassRules, Pad, PadShape, PadType, Pcb,
    PcbLayer, StackupLayer, Trace,
};
use vcad_ir::Vec2;

/// An empty 10×10 mm two-layer board with ordinary rules.
pub fn test_board() -> Pcb {
    Pcb {
        outline: BoardOutline {
            vertices: vec![
                Vec2::new(0.0, 0.0),
                Vec2::new(10.0, 0.0),
                Vec2::new(10.0, 10.0),
                Vec2::new(0.0, 10.0),
            ],
            cutouts: vec![],
            thickness: 1.6,
        },
        stackup: LayerStackup {
            layers: vec![
                StackupLayer {
                    role: None,
                    role_inferred: false,
                    layer: PcbLayer::FCu,
                    copper_thickness: Some(0.035),
                    dielectric_thickness: Some(1.5),
                    dielectric_er: Some(4.5),
                    material: Some("FR4".into()),
                },
                StackupLayer {
                    role: None,
                    role_inferred: false,
                    layer: PcbLayer::BCu,
                    copper_thickness: Some(0.035),
                    dielectric_thickness: None,
                    dielectric_er: None,
                    material: Some("FR4".into()),
                },
            ],
        },
        nets: vec![],
        rules: DesignRules {
            allow_stacked_microvias: false,
            inferred_floors: Vec::new(),
            rule_areas: Vec::new(),
            min_microvia_drill: 0.1,
            min_microvia_diameter: 0.2,
            default_rules: NetClassRules {
                neck_width: None,
                neck_length: None,
                name: "Default".into(),
                trace_width: 0.2,
                clearance: 0.2,
                via_diameter: 0.6,
                via_drill: 0.3,
                diff_pair_gap: None,
                diff_pair_width: None,
                target_impedance: None,
                target_diff_impedance: None,
            },
            class_rules: vec![],
            net_class_assignments: Default::default(),
            edge_clearance: 0.2,
            hole_to_hole: 0.25,
            min_annular_ring: 0.1,
            min_drill: 0.2,
        },
        footprints: vec![],
        traces: vec![],
        trace_arcs: vec![],
        vias: vec![],
        zones: vec![],
        keepouts: vec![],
        net_ties: vec![],
    }
}

fn footprint_mut<'a>(pcb: &'a mut Pcb, reference: &str) -> &'a mut Footprint {
    if let Some(i) = pcb.footprints.iter().position(|f| f.reference == reference) {
        return &mut pcb.footprints[i];
    }
    pcb.footprints.push(Footprint {
        reference: reference.to_string(),
        value: "TEST".into(),
        footprint_name: "Test:Pad".into(),
        position: Vec2::new(0.0, 0.0),
        rotation: 0.0,
        front: true,
        pads: vec![],
        graphics: vec![],
        model_3d: None,
        properties: Default::default(),
    });
    pcb.footprints.last_mut().expect("just pushed")
}

/// Add a 0.5 mm square SMD pad on the front copper layer.
pub fn with_smd_pad(pcb: &mut Pcb, reference: &str, number: &str, x: f64, y: f64, net: &str) {
    let fp = footprint_mut(pcb, reference);
    fp.pads.push(Pad {
        number: number.to_string(),
        pad_type: PadType::SMD,
        shape: PadShape::Rect {
            width: 0.5,
            height: 0.5,
        },
        position: Vec2::new(x, y),
        rotation: 0.0,
        drill: None,
        net: Some(net.to_string()),
        layers: vec![PcbLayer::FCu],
    });
}

/// Add a through-hole pad with the given drill diameter.
pub fn with_tht_pad(pcb: &mut Pcb, reference: &str, number: &str, x: f64, y: f64, drill: f64) {
    let fp = footprint_mut(pcb, reference);
    fp.pads.push(Pad {
        number: number.to_string(),
        pad_type: PadType::THT,
        shape: PadShape::Circle {
            diameter: drill + 0.3,
        },
        position: Vec2::new(x, y),
        rotation: 0.0,
        drill: Some(vcad_ir::ecad::DrillSpec {
            diameter: drill,
            oval: false,
            oval_height: None,
        }),
        net: Some(format!("N-{reference}-{number}")),
        layers: vec![PcbLayer::FCu, PcbLayer::BCu],
    });
}

/// Add a front-copper trace at the default class width.
pub fn with_trace(pcb: &mut Pcb, net: &str, x0: f64, y0: f64, x1: f64, y1: f64) {
    let width = pcb.rules.default_rules.trace_width;
    pcb.traces.push(Trace {
        start: Vec2::new(x0, y0),
        end: Vec2::new(x1, y1),
        width,
        layer: PcbLayer::FCu,
        net: net.to_string(),
        source: None,
    });
}

/// An empty `w`×`h` mm FOUR-layer board (F.Cu, In1, In2, B.Cu) with ordinary
/// JLC-class rules — the standard process, through vias only.
pub fn four_layer_board(w: f64, h: f64) -> Pcb {
    let mut pcb = test_board();
    pcb.outline.vertices = vec![
        Vec2::new(0.0, 0.0),
        Vec2::new(w, 0.0),
        Vec2::new(w, h),
        Vec2::new(0.0, h),
    ];
    pcb.stackup.layers = [
        PcbLayer::FCu,
        PcbLayer::In1Cu,
        PcbLayer::In2Cu,
        PcbLayer::BCu,
    ]
    .into_iter()
    .map(|layer| StackupLayer {
        role: None,
        role_inferred: false,
        layer,
        copper_thickness: Some(0.035),
        dielectric_thickness: Some(0.2),
        dielectric_er: Some(4.5),
        material: Some("FR4".into()),
    })
    .collect();
    pcb
}

/// Three 0603-ish resistors on F.Cu wired in a triangle (`N1`, `N2`, `N3`),
/// with no copper at all — the smallest SMD board that reproduced the
/// terminal-pinning bug.
pub fn three_resistor_board() -> Pcb {
    let mut pcb = four_layer_board(20.0, 20.0);
    with_smd_pad(&mut pcb, "R1", "1", 4.0, 4.0, "N3");
    with_smd_pad(&mut pcb, "R1", "2", 5.6, 4.0, "N1");
    with_smd_pad(&mut pcb, "R2", "1", 12.0, 4.0, "N1");
    with_smd_pad(&mut pcb, "R2", "2", 13.6, 4.0, "N2");
    with_smd_pad(&mut pcb, "R3", "1", 12.0, 14.0, "N3");
    with_smd_pad(&mut pcb, "R3", "2", 13.6, 14.0, "N2");
    pcb
}

/// A four-layer board whose one connection (`SIG`, F.Cu pad to F.Cu pad) is
/// cut on F.Cu by a full-height wall of another net (`WALL`, itself a
/// legitimately connected pad-to-pad trace). The only way across is down
/// through a via and back — so a router that does not pin terminals to the
/// pad's layer starts the trace on an inner layer, floating.
pub fn walled_board() -> Pcb {
    let mut pcb = four_layer_board(20.0, 20.0);
    with_smd_pad(&mut pcb, "R1", "1", 4.0, 10.0, "SIG");
    with_smd_pad(&mut pcb, "R2", "1", 16.0, 10.0, "SIG");
    with_smd_pad(&mut pcb, "W1", "1", 10.0, 0.8, "WALL");
    with_smd_pad(&mut pcb, "W2", "1", 10.0, 19.2, "WALL");
    with_trace(&mut pcb, "WALL", 10.0, 0.8, 10.0, 19.2);
    pcb
}

/// Every trace end that sits on a pad of its net is on one of that pad's
/// copper layers, or on a via of the net at that point. Returns the
/// offending `(net, layer, x, y)` tuples.
pub fn floating_pad_attachments(pcb: &Pcb) -> Vec<(String, PcbLayer, f64, f64)> {
    let mut bad = Vec::new();
    for t in &pcb.traces {
        for p in [t.start, t.end] {
            for fp in &pcb.footprints {
                for pad in &fp.pads {
                    if pad.net.as_deref() != Some(t.net.as_str()) {
                        continue;
                    }
                    let w = vcad_ecad_pcb::geometry::pad_world_position(fp, pad);
                    // On the pad (0.5 mm square pads in these fixtures).
                    if (w.x - p.x).abs() > 0.25 || (w.y - p.y).abs() > 0.25 {
                        continue;
                    }
                    if pad.layers.contains(&t.layer) {
                        continue;
                    }
                    let via_here = pcb.vias.iter().any(|v| {
                        v.net == t.net
                            && ((v.position.x - p.x).powi(2) + (v.position.y - p.y).powi(2))
                                .sqrt()
                                <= v.diameter / 2.0
                            && t.layer.spanned_by(v.start_layer, v.end_layer)
                            && pad
                                .layers
                                .iter()
                                .any(|l| l.spanned_by(v.start_layer, v.end_layer))
                    });
                    if !via_here {
                        bad.push((t.net.clone(), t.layer, p.x, p.y));
                    }
                }
            }
        }
    }
    bad
}

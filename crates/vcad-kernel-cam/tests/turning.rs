//! Turning: plan → post → replay the posted G-code → verify, for the two
//! hobby-fusor parts, plus mutations that the verifier must catch.

use vcad_kernel_cam::turning::*;

/// PEEK HV feedthrough standoff: Ø12 spigot (chamfered), Ø20 body with
/// three creepage grooves (2 wide × 2 deep, R0.2 floor corners), Ø6.5
/// through bore with a 0.3 entry chamfer. 30 mm long.
fn peek_standoff() -> TurnProfile {
    let mut b = ProfileBuilder::start([0.0, 3.55])
        .line_to([0.0, 5.5])
        .line_to([-0.5, 6.0])
        .line_to([-4.5, 6.0])
        .arc_to([-5.0, 6.5], [-4.5, 6.5], false)
        .line_to([-5.0, 10.0]);
    for zc in [-10.0, -16.0, -22.0] {
        let (zr, zl) = (zc + 1.0, zc - 1.0);
        b = b
            .line_to([zr, 10.0])
            .line_to([zr, 8.2])
            .arc_to([zr - 0.2, 8.0], [zr - 0.2, 8.2], false)
            .line_to([zl + 0.2, 8.0])
            .arc_to([zl, 8.2], [zl + 0.2, 8.2], false)
            .line_to([zl, 10.0]);
    }
    b.line_to([-30.0, 10.0])
        .line_to([-30.0, 3.25])
        .line_to([-0.3, 3.25])
        .close()
        .unwrap()
}

/// Delrin stalk bushing: Ø12 × 15 body (R0.5 root), Ø16 × 3 flange,
/// Ø6.35 (¼") through bore.
fn delrin_bushing() -> TurnProfile {
    ProfileBuilder::start([0.0, 3.175])
        .line_to([0.0, 5.6])
        .line_to([-0.4, 6.0])
        .line_to([-14.5, 6.0])
        .arc_to([-15.0, 6.5], [-14.5, 6.5], false)
        .line_to([-15.0, 8.0])
        .line_to([-18.0, 8.0])
        .line_to([-18.0, 3.175])
        .close()
        .unwrap()
}

fn css(v: f64, max: f64) -> SpindleSpeed {
    SpindleSpeed::Css {
        m_per_min: v,
        max_rpm: max,
    }
}

fn params(fpr: f64, doc: f64) -> CutParams {
    CutParams {
        speed: css(120.0, 2500.0),
        feed_per_rev: fpr,
        depth_of_cut: doc,
        clearance: 1.0,
    }
}

struct Tools {
    rough: LatheTool,
    finish: LatheTool,
    drill: LatheTool,
    bore: LatheTool,
    groove: LatheTool,
    part: LatheTool,
}

fn tools() -> Tools {
    Tools {
        rough: LatheTool::cnmg_95(1, 0.4),
        finish: LatheTool::dnmg_93(2, 0.2),
        drill: LatheTool::drill(3, 6.0, 118.0, 45.0),
        bore: LatheTool::boring_rh(4, 0.2, 95.0, 80.0, 5.5, 45.0),
        groove: LatheTool::grooving(5, 1.5, 0.2, 4.0),
        part: LatheTool::parting(6, 2.0, 0.1, 12.0),
    }
}

fn plan_standoff() -> (TurnProgram, TurnStock, TurnProfile) {
    let profile = peek_standoff();
    let stock = TurnStock::bar(22.0, 60.0, 0.5);
    let t = tools();
    let drill_p = CutParams {
        speed: SpindleSpeed::Rpm { rpm: 1200.0 },
        ..params(0.08, 1.0)
    };
    let mut plan = TurnPlanner::new(profile.clone(), stock).unwrap();
    plan.face(&t.rough, &params(0.15, 0.5))
        .unwrap()
        .drill(&t.drill, &drill_p, 33.0, 4.0)
        .unwrap()
        .rough_od(&t.rough, &params(0.2, 1.0), 0.2)
        .unwrap()
        .finish_od(&t.finish, &params(0.08, 1.0))
        .unwrap()
        .rough_bore(&t.bore, &params(0.1, 0.3), 0.08)
        .unwrap()
        .finish_bore(&t.bore, &params(0.06, 0.3))
        .unwrap();
    for zc in [-10.0, -16.0, -22.0] {
        plan.groove(&t.groove, &params(0.04, 1.0), zc - 1.0, zc + 1.0)
            .unwrap();
    }
    plan.part_off(&t.part, &params(0.04, 1.0), 0.2).unwrap();
    (plan.program("peek-standoff"), stock, profile)
}

fn plan_bushing() -> (TurnProgram, TurnStock, TurnProfile) {
    let profile = delrin_bushing();
    let stock = TurnStock::bar(19.05, 50.0, 0.5);
    let t = tools();
    let drill_p = CutParams {
        speed: SpindleSpeed::Rpm { rpm: 1200.0 },
        ..params(0.08, 1.0)
    };
    let mut plan = TurnPlanner::new(profile.clone(), stock).unwrap();
    plan.face(&t.rough, &params(0.15, 0.5))
        .unwrap()
        .drill(&t.drill, &drill_p, 21.0, 4.0)
        .unwrap()
        .rough_od(&t.rough, &params(0.2, 1.0), 0.2)
        .unwrap()
        .finish_od(&t.finish, &params(0.08, 1.0))
        .unwrap()
        .rough_bore(&t.bore, &params(0.1, 0.3), 0.05)
        .unwrap()
        .finish_bore(&t.bore, &params(0.06, 0.3))
        .unwrap()
        .part_off(&t.part, &params(0.04, 1.0), 0.2)
        .unwrap();
    (plan.program("delrin-bushing"), stock, profile)
}

fn verify(
    prog: &TurnProgram,
    gcode: &str,
    stock: &TurnStock,
    profile: &TurnProfile,
) -> TurnVerdict {
    verify_gcode(
        gcode,
        &prog.tools,
        stock,
        profile,
        &VerifyOptions::default(),
    )
}

#[test]
fn peek_standoff_linuxcnc_verifies() {
    let (prog, stock, profile) = plan_standoff();
    let g = LathePost::linuxcnc().post(&prog);
    assert!(g.contains("G7") && g.contains("G95") && g.contains("G96 D2500 S120"));
    let v = verify(&prog, &g, &stock, &profile);
    assert!(
        v.ok,
        "{:#?}\nmax_excess {} max_gouge {}",
        v.findings, v.max_excess, v.max_gouge
    );
    assert!(v.max_excess <= 0.01 && v.max_gouge <= 0.01);
}

#[test]
fn delrin_bushing_linuxcnc_verifies() {
    let (prog, stock, profile) = plan_bushing();
    let g = LathePost::linuxcnc().post(&prog);
    let v = verify(&prog, &g, &stock, &profile);
    assert!(v.ok, "{:#?}", v.findings);
}

#[test]
fn delrin_bushing_grbl_both_x_modes_verify() {
    let (prog, stock, profile) = plan_bushing();
    for mode in [XMode::Radius, XMode::Diameter] {
        let post = LathePost::grbl(mode);
        let g = post.post(&prog);
        for w in ["G7", "G95", "G96", "M6"] {
            assert!(
                !g.lines().any(|l| !l.starts_with('(') && l.contains(w)),
                "grbl dialect emitted {w}"
            );
        }
        let ok = prog.post_verified(&post, &stock, &profile, &VerifyOptions::default());
        assert!(ok.is_ok(), "{mode:?}: {:#?}", ok.err().map(|v| v.findings));
    }
}

#[test]
fn mutation_radius_read_as_diameter_fails() {
    let (prog, stock, profile) = plan_bushing();
    let g = LathePost::grbl(XMode::Radius).post(&prog);
    let g = g.replace("(vcad-x-mode: radius)", "");
    let opts = VerifyOptions {
        x_mode: XMode::Diameter,
        ..VerifyOptions::default()
    };
    let v = verify_gcode(&g, &prog.tools, &stock, &profile, &opts);
    assert!(!v.ok);
    assert!(v
        .findings
        .iter()
        .any(|f| matches!(f, Finding::Gouge { .. })));
}

#[test]
fn mutation_declared_x_mode_mismatch_fails() {
    let (prog, stock, profile) = plan_bushing();
    let g = LathePost::grbl(XMode::Radius).post(&prog);
    let opts = VerifyOptions {
        x_mode: XMode::Diameter,
        ..VerifyOptions::default()
    };
    assert!(!verify_gcode(&g, &prog.tools, &stock, &profile, &opts).ok);
}

/// Shift every X word of the finishing op down by `d` (diameter).
fn shift_op_x(g: &str, op: &str, d: f64) -> String {
    let mut out = String::new();
    let mut inside = false;
    for l in g.lines() {
        if l.starts_with("(op:") {
            inside = l.contains(op);
        }
        if inside && l.starts_with("G1 ") {
            let mut words: Vec<String> = Vec::new();
            for w in l.split_whitespace() {
                if let Some(v) = w.strip_prefix('X') {
                    let x: f64 = v.parse().unwrap();
                    words.push(format!("X{:.4}", x - d));
                } else {
                    words.push(w.to_string());
                }
            }
            out.push_str(&words.join(" "));
        } else {
            out.push_str(l);
        }
        out.push('\n');
    }
    out
}

#[test]
fn mutation_gouging_finish_fails() {
    let (prog, stock, profile) = plan_bushing();
    let g = LathePost::linuxcnc().post(&prog);
    let bad = shift_op_x(&g, "finish OD", 0.06);
    let v = verify(&prog, &bad, &stock, &profile);
    assert!(!v.ok);
    assert!(v.max_gouge > 0.02, "max_gouge {}", v.max_gouge);
}

#[test]
fn mutation_wrong_nose_radius_fails() {
    let (prog, stock, profile) = plan_bushing();
    let g = LathePost::linuxcnc().post(&prog);
    // The machine actually holds an r0.8 insert in pocket 2.
    let mut tools = prog.tools.clone();
    for t in &mut tools {
        if t.number == 2 {
            *t = LatheTool::dnmg_93(2, 0.8);
        }
    }
    let v = verify_gcode(&g, &tools, &stock, &profile, &VerifyOptions::default());
    assert!(!v.ok);
}

#[test]
fn mutation_missing_finish_fails() {
    let profile = delrin_bushing();
    let stock = TurnStock::bar(19.05, 50.0, 0.5);
    let t = tools();
    let mut plan = TurnPlanner::new(profile.clone(), stock).unwrap();
    plan.face(&t.rough, &params(0.15, 0.5))
        .unwrap()
        .rough_od(&t.rough, &params(0.2, 1.0), 0.2)
        .unwrap();
    let prog = plan.program("rough only");
    let g = LathePost::linuxcnc().post(&prog);
    let v = verify(&prog, &g, &stock, &profile);
    assert!(!v.ok);
    assert!(v
        .findings
        .iter()
        .any(|f| matches!(f, Finding::Unfinished { .. })));
}

#[test]
fn mutation_unreadable_and_state_fail() {
    let (prog, stock, profile) = plan_bushing();
    let g = LathePost::linuxcnc().post(&prog);
    for (from, to) in [
        ("G96 D2500 S120 M3", "G96 S120 M3"),
        ("G18 G21 G90 G40 G80", "G18 G21 G90 G41 G80"),
        ("G7\n", "G7\nG2 X1 Z1 I1 K0\n"),
    ] {
        let bad = g.replacen(from, to, 1);
        assert_ne!(bad, g, "mutation {from} did not apply");
        assert!(!verify(&prog, &bad, &stock, &profile).ok, "{to} passed");
    }
    // Spindle stopped before the first feed.
    let bad = g.replacen("G96 D2500 S120 M3", "G96 D2500 S120 M5", 1);
    let v = verify(&prog, &bad, &stock, &profile);
    assert!(v
        .findings
        .iter()
        .any(|f| matches!(f, Finding::State { .. })));
}

#[test]
fn refuses_unreachable_geometry() {
    let t = tools();
    // Groove narrower than the blade.
    let mut plan = TurnPlanner::new(peek_standoff(), TurnStock::bar(22.0, 60.0, 0.5)).unwrap();
    let e = plan.groove(&t.groove, &params(0.04, 1.0), -11.0, -9.8);
    assert!(matches!(e, Err(TurnError::NotTurnable(_))), "{e:?}");
    // Boring bar that needs a bigger hole than the drill made.
    let mut plan = TurnPlanner::new(delrin_bushing(), TurnStock::bar(19.05, 50.0, 0.5)).unwrap();
    let drill_p = CutParams {
        speed: SpindleSpeed::Rpm { rpm: 1200.0 },
        ..params(0.08, 1.0)
    };
    plan.drill(&LatheTool::drill(3, 5.0, 118.0, 45.0), &drill_p, 21.0, 4.0)
        .unwrap();
    let e = plan.rough_bore(&t.bore, &params(0.1, 0.3), 0.05);
    assert!(matches!(e, Err(TurnError::NotTurnable(_))), "{e:?}");
    // Drilling under CSS.
    let e = plan.drill(&t.drill, &params(0.08, 1.0), 21.0, 4.0);
    assert!(matches!(e, Err(TurnError::BadParams(_))));
    // Part bigger than stock.
    assert!(TurnPlanner::new(peek_standoff(), TurnStock::bar(18.0, 60.0, 0.5)).is_err());
}

#[test]
fn back_facing_wall_is_left_and_flagged() {
    // A Ø16 collar between two Ø12 diameters: the collar's chuck-side wall
    // faces −Z. A right-hand insert cannot finish it; the oracle must say
    // so rather than pass.
    let profile = TurnProfile::from_points(&[
        [0.0, 0.0],
        [0.0, 6.0],
        [-5.0, 6.0],
        [-5.0, 8.0],
        [-8.0, 8.0],
        [-8.0, 6.0],
        [-15.0, 6.0],
        [-15.0, 0.0],
    ])
    .unwrap();
    let stock = TurnStock::bar(18.0, 40.0, 0.5);
    let t = tools();
    let mut plan = TurnPlanner::new(profile.clone(), stock).unwrap();
    plan.face(&t.rough, &params(0.15, 0.5))
        .unwrap()
        .rough_od(&t.rough, &params(0.2, 1.0), 0.2)
        .unwrap()
        .finish_od(&t.finish, &params(0.08, 1.0))
        .unwrap()
        .part_off(&t.part, &params(0.04, 1.0), 0.2)
        .unwrap();
    let prog = plan.program("collar");
    let g = LathePost::linuxcnc().post(&prog);
    let v = verify(&prog, &g, &stock, &profile);
    assert!(!v.ok);
    assert_eq!(v.max_gouge, 0.0, "never gouges to reach it");
}

#[test]
fn rotary_carvera_solid_standoff_verifies() {
    // Solid Delrin standoff turned on the Carvera's 4th axis with a Ø4 flat
    // endmill: Ø12 spigot, Ø16 body, from Ø19.05 bar.
    // A flat endmill makes cylinders, faces and sharp inside corners exactly;
    // on a taper it leaves a stair of slope × pitch (see the ball test).
    let profile = ProfileBuilder::start([0.0, 0.0])
        .line_to([0.0, 6.0])
        .line_to([-6.0, 6.0])
        .line_to([-6.0, 8.0])
        .line_to([-20.0, 8.0])
        .line_to([-20.0, 0.0])
        .close()
        .unwrap();
    let stock = TurnStock::bar(19.05, 45.0, 0.5);
    let mill = Endmill::Flat { diameter: 4.0 };
    let machine = RotaryMachine::carvera_air(ATravel::Continuous);
    let p = RotaryParams {
        spindle_rpm: 12000.0,
        surface_feed: 600.0,
        stepdown: 1.0,
        rough_pitch: 2.5,
        finish_pitch: 0.5,
        allowance: 0.2,
        clearance: 1.0,
        neck_radius: 2.0,
    };
    let prog = plan_rotary("standoff", &profile, &stock, mill, machine, p).unwrap();
    let g = prog.post_carvera();
    assert!(g.contains("G93"));
    let opts = RotaryVerifyOptions::default();
    let v = verify_rotary_gcode(&g, &profile, &stock, mill, &machine, &opts);
    assert!(
        v.ok,
        "{:#?} gouge {} excess {}",
        v.findings, v.max_gouge, v.max_excess
    );

    // Mutations: drop the cutter 0.05 mm on the finish pass → gouge.
    let mut lines: Vec<String> = g.lines().map(String::from).collect();
    let n = lines.len();
    for l in lines.iter_mut().skip(n * 3 / 4) {
        if l.starts_with("G1 ") {
            *l = l
                .split_whitespace()
                .map(|w| match w.strip_prefix('Z') {
                    Some(v) => format!("Z{:.4}", v.parse::<f64>().unwrap() - 0.05),
                    None => w.to_string(),
                })
                .collect::<Vec<_>>()
                .join(" ");
        }
    }
    let bad = lines.join("\n");
    let v = verify_rotary_gcode(&bad, &profile, &stock, mill, &machine, &opts);
    assert!(!v.ok && v.max_gouge > 0.02, "gouge {}", v.max_gouge);
    // No G93 → refused.
    let bad = g.replace("G93\n", "");
    assert!(!verify_rotary_gcode(&bad, &profile, &stock, mill, &machine, &opts).ok);
    // A slower machine → A-rate violations.
    let slow = RotaryMachine {
        a_max_deg_per_min: 600.0,
        ..machine
    };
    assert!(!verify_rotary_gcode(&g, &profile, &stock, mill, &slow, &opts).ok);
    // Off-centreline Y is refused.
    let bad = g.replacen(" Y0 A", " Y0.5 A", 1);
    assert!(!verify_rotary_gcode(&bad, &profile, &stock, mill, &machine, &opts).ok);
    // Limited A travel refuses at plan time.
    let lim = RotaryMachine::carvera_air(ATravel::Limited { max_deg: 300.0 });
    assert!(plan_rotary("x", &profile, &stock, mill, lim, p).is_err());
}

#[test]
fn rotary_refuses_bores() {
    let stock = TurnStock::bar(19.05, 45.0, 0.5);
    let r = plan_rotary(
        "x",
        &delrin_bushing(),
        &stock,
        Endmill::Flat { diameter: 4.0 },
        RotaryMachine::carvera_air(ATravel::Continuous),
        RotaryParams {
            spindle_rpm: 12000.0,
            surface_feed: 600.0,
            stepdown: 1.0,
            rough_pitch: 2.5,
            finish_pitch: 0.5,
            allowance: 0.2,
            clearance: 1.0,
            neck_radius: 2.0,
        },
    );
    assert!(matches!(r, Err(TurnError::NotTurnable(_))));
}

#[test]
fn rotary_ball_endmill_taper_and_fillet_verify() {
    // Ø16 body, 30° taper down to a Ø10 nose, with an R2.5 fillet (≥ the
    // ball radius) in the concave nose/taper junction. Pre-faced stock.
    let (r, half) = (2.5f64, 75f64.to_radians());
    let d = r / half.sin();
    // Bisector of the 150° air-side corner points 75° up from +z.
    let c = [-4.0 + d * half.cos(), 5.0 + r];
    let t1 = [c[0], 5.0];
    let along = d * half.cos();
    let t2 = [-4.0 - along * 30f64.to_radians().cos(), 5.0 + along * 0.5];
    let z_body = -4.0 - 3.0 / 30f64.to_radians().tan();
    let profile = ProfileBuilder::start([0.0, 0.0])
        .line_to([0.0, 5.0])
        .line_to(t1)
        .arc_to(t2, c, false)
        .line_to([z_body, 8.0])
        .line_to([-22.0, 8.0])
        .line_to([-22.0, 0.0])
        .close()
        .unwrap();
    let stock = TurnStock::bar(19.05, 45.0, 0.0);
    let mill = Endmill::Ball { diameter: 4.0 };
    let machine = RotaryMachine::carvera_air(ATravel::Continuous);
    let p = RotaryParams {
        spindle_rpm: 12000.0,
        surface_feed: 600.0,
        stepdown: 1.0,
        rough_pitch: 2.0,
        finish_pitch: 0.125, // scallop in the R2.5/R2 fillet scales with pitch²
        allowance: 0.2,
        clearance: 1.0,
        neck_radius: 3.0,
    };
    let prog = plan_rotary("taper", &profile, &stock, mill, machine, p).unwrap();
    let g = prog.post_carvera();
    let v = verify_rotary_gcode(
        &g,
        &profile,
        &stock,
        mill,
        &machine,
        &RotaryVerifyOptions {
            tolerance: 0.02,
            column: 0.02,
        },
    );
    assert!(
        v.ok,
        "{:?} gouge {} excess {}",
        v.findings, v.max_gouge, v.max_excess
    );
    // Without the fillet the ball cannot reach the sharp concave junction;
    // the oracle reports the leftover instead of passing.
    let sharp = ProfileBuilder::start([0.0, 0.0])
        .line_to([0.0, 5.0])
        .line_to([-4.0, 5.0])
        .line_to([z_body, 8.0])
        .line_to([-22.0, 8.0])
        .line_to([-22.0, 0.0])
        .close()
        .unwrap();
    let prog = plan_rotary("sharp", &sharp, &stock, mill, machine, p).unwrap();
    let v = verify_rotary_gcode(
        &prog.post_carvera(),
        &sharp,
        &stock,
        mill,
        &machine,
        &RotaryVerifyOptions {
            tolerance: 0.02,
            column: 0.02,
        },
    );
    assert!(!v.ok && v.max_gouge <= 0.02 && v.max_excess > 0.05);
}

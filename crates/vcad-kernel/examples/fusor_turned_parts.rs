//! Turned parts for the hobby fusor, planned, posted and verified.
//!
//! ```bash
//! cargo run -p vcad-kernel --example fusor_turned_parts -- out/
//! ```
//!
//! 1. **PEEK HV feedthrough standoff** — Ø20 body with three creepage
//!    grooves (more tracking length at 40 kV), Ø12 spigot, Ø6.5 bore.
//!    Profile authored with arcs (R0.2 groove floors, R0.5 shoulder root),
//!    so it verifies strictly.
//! 2. **Delrin stalk bushing** — built as a user would: a line-only
//!    `Solid::revolve`, read back with `turning_profile`. Its body/flange
//!    corner is sharp, so the nose-radius fillet there is accepted
//!    explicitly (`inside_corner_allowance`).
//! 3. **Delrin standoff on the Carvera Air 4th axis** — no lathe: a Ø4 flat
//!    endmill on the rotary centreline, A as the feed axis, `G93`.
//!
//! Every program is replayed from its G-code text and checked against the
//! part before it is written; a refusal is printed instead of G-code.

use std::path::{Path, PathBuf};
use vcad_kernel::vcad_kernel_cam::turning::*;
use vcad_kernel::vcad_kernel_math::{Point2, Point3, Vec3};
use vcad_kernel::vcad_kernel_sketch::{SketchProfile, SketchSegment};
use vcad_kernel::{turning_profile, FaceEnd, Solid};

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
        .expect("standoff profile")
}

fn revolved_bushing() -> Solid {
    let l = |a: (f64, f64), b: (f64, f64)| SketchSegment::Line {
        start: Point2::new(a.0, a.1),
        end: Point2::new(b.0, b.1),
    };
    let sketch = SketchProfile::new(
        Point3::origin(),
        Vec3::x(),
        Vec3::z(),
        vec![
            l((3.175, 0.0), (8.0, 0.0)),
            l((8.0, 0.0), (8.0, 3.0)),
            l((8.0, 3.0), (6.0, 3.0)),
            l((6.0, 3.0), (6.0, 17.6)),
            l((6.0, 17.6), (5.6, 18.0)),
            l((5.6, 18.0), (3.175, 18.0)),
            l((3.175, 18.0), (3.175, 0.0)),
        ],
    )
    .expect("sketch");
    Solid::revolve(sketch, Point3::origin(), Vec3::z(), 360.0).expect("revolve")
}

fn params(fpr: f64, doc: f64) -> CutParams {
    CutParams {
        speed: SpindleSpeed::Css {
            m_per_min: 120.0,
            max_rpm: 2500.0,
        },
        feed_per_rev: fpr,
        depth_of_cut: doc,
        clearance: 1.0,
    }
}

fn report(
    name: &str,
    prog: &TurnProgram,
    stock: &TurnStock,
    profile: &TurnProfile,
    opts: &VerifyOptions,
    out: &Path,
) {
    for (tag, post) in [
        ("linuxcnc", LathePost::linuxcnc()),
        ("grbl-dia", LathePost::grbl(XMode::Diameter)),
    ] {
        match prog.post_verified(&post, stock, profile, opts) {
            Ok(g) => {
                let path = out.join(format!("{name}.{tag}.nc"));
                std::fs::write(&path, &g).expect("write");
                println!(
                    "  {tag:9} PASS  {} lines, ~{:.0} s cycle → {}",
                    g.lines().count(),
                    prog.estimated_time(3000.0),
                    path.display()
                );
            }
            Err(v) => {
                println!("  {tag:9} REFUSED: {:#?}", v.findings);
            }
        }
    }
}

fn main() {
    let out = PathBuf::from(
        std::env::args()
            .nth(1)
            .unwrap_or_else(|| "target/fusor-turning".into()),
    );
    std::fs::create_dir_all(&out).expect("out dir");
    let strict = VerifyOptions::default();
    let rough = LatheTool::cnmg_95(1, 0.4);
    let finish = LatheTool::dnmg_93(2, 0.2);
    let drill = LatheTool::drill(3, 6.0, 118.0, 45.0);
    let bore = LatheTool::boring_rh(4, 0.2, 95.0, 80.0, 5.5, 45.0);
    let groove = LatheTool::grooving(5, 1.5, 0.2, 4.0);
    let part = LatheTool::parting(6, 2.0, 0.1, 12.0);
    let drill_p = CutParams {
        speed: SpindleSpeed::Rpm { rpm: 1200.0 },
        ..params(0.08, 1.0)
    };

    println!("PEEK feedthrough standoff (Ø22 bar)");
    let profile = peek_standoff();
    let stock = TurnStock::bar(22.0, 60.0, 0.5);
    let mut plan = TurnPlanner::new(profile.clone(), stock).expect("plan");
    plan.face(&rough, &params(0.15, 0.5))
        .and_then(|p| p.drill(&drill, &drill_p, 33.0, 4.0))
        .and_then(|p| p.rough_od(&rough, &params(0.2, 1.0), 0.2))
        .and_then(|p| p.finish_od(&finish, &params(0.08, 1.0)))
        .and_then(|p| p.rough_bore(&bore, &params(0.1, 0.3), 0.08))
        .and_then(|p| p.finish_bore(&bore, &params(0.06, 0.3)))
        .expect("standoff ops");
    for zc in [-10.0, -16.0, -22.0] {
        plan.groove(&groove, &params(0.04, 1.0), zc - 1.0, zc + 1.0)
            .expect("groove");
    }
    plan.part_off(&part, &params(0.04, 1.0), 0.2)
        .expect("part off");
    report(
        "peek-standoff",
        &plan.program("peek-standoff"),
        &stock,
        &profile,
        &strict,
        &out,
    );

    println!("Delrin stalk bushing (Solid::revolve → turning_profile, Ø19.05 bar)");
    let (profile, axis) = turning_profile(&revolved_bushing(), FaceEnd::Max).expect("profile");
    println!("  axis {:?} face at {:.3}", axis.dir, axis.face_z);
    let stock = TurnStock::bar(19.05, 50.0, 0.5);
    let mut plan = TurnPlanner::new(profile.clone(), stock).expect("plan");
    plan.face(&rough, &params(0.15, 0.5))
        .and_then(|p| p.drill(&drill, &drill_p, 21.0, 4.0))
        .and_then(|p| p.rough_od(&rough, &params(0.2, 1.0), 0.2))
        .and_then(|p| p.finish_od(&finish, &params(0.08, 1.0)))
        .and_then(|p| p.rough_bore(&bore, &params(0.1, 0.3), 0.05))
        .and_then(|p| p.finish_bore(&bore, &params(0.06, 0.3)))
        .and_then(|p| p.part_off(&part, &params(0.04, 1.0), 0.2))
        .expect("bushing ops");
    let lenient = VerifyOptions {
        inside_corner_allowance: 0.25,
        ..strict
    };
    report(
        "delrin-bushing",
        &plan.program("delrin-bushing"),
        &stock,
        &profile,
        &lenient,
        &out,
    );

    println!("Delrin standoff on the Carvera Air 4th axis (Ø4 flat endmill)");
    let profile = ProfileBuilder::start([0.0, 0.0])
        .line_to([0.0, 6.0])
        .line_to([-6.0, 6.0])
        .line_to([-6.0, 8.0])
        .line_to([-20.0, 8.0])
        .line_to([-20.0, 0.0])
        .close()
        .expect("profile");
    let stock = TurnStock::bar(19.05, 45.0, 0.5);
    let mill = Endmill::Flat { diameter: 4.0 };
    let machine = RotaryMachine::carvera_air(ATravel::Continuous);
    let rp = RotaryParams {
        spindle_rpm: 12000.0,
        surface_feed: 600.0,
        stepdown: 1.0,
        rough_pitch: 2.5,
        finish_pitch: 0.5,
        allowance: 0.2,
        clearance: 1.0,
        neck_radius: 2.0,
    };
    let prog =
        plan_rotary("carvera-standoff", &profile, &stock, mill, machine, rp).expect("rotary");
    let g = prog.post_carvera();
    let v = verify_rotary_gcode(
        &g,
        &profile,
        &stock,
        mill,
        &machine,
        &RotaryVerifyOptions::default(),
    );
    if v.ok {
        let path = out.join("carvera-standoff.nc");
        std::fs::write(&path, &g).expect("write");
        println!(
            "  carvera   PASS  {} lines, ~{:.0} min (A-rate bound) → {}",
            g.lines().count(),
            prog.estimated_time(3000.0) / 60.0,
            path.display()
        );
    } else {
        println!("  carvera   REFUSED: {:#?}", v.findings);
    }
}

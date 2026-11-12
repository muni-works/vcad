//! Beam tuner vs solid modal solver on variants of the C6 bar.
use vcad_kernel_acoustics::instrument::{modal, BarGeometry, BarMaterial, ModelOptions};
use vcad_sound::{evaluate, part_modes, ModesRequest, Quality};

fn solid(src: &str, q: Quality) -> (f64, usize) {
    let r = solid_full(src, q);
    eprintln!(
        "   {:?}: mass {:.5} kg  cell {:?} dofs {}",
        q, r.2, r.3, r.1
    );
    (r.0, r.1)
}

fn solid_full(src: &str, q: Quality) -> (f64, usize, f64, [f64; 3]) {
    let doc = vcad_loon::eval_vcad(src, None).unwrap();
    let scene = evaluate(&doc).unwrap();
    let pm = part_modes(
        &doc,
        &scene,
        &ModesRequest {
            part: 0,
            modes: 1,
            quality: q,
            material: None,
            shapes: false,
        },
    )
    .unwrap();
    (pm.modes[0].hz, pm.dofs, pm.mass_kg, pm.pitch_mm)
}

fn main() {
    let (l, w, t, d, span, hd) = (131.32f64, 25.0f64, 6.0f64, 2.74f64, 0.2f64, 4.0f64);
    let a = 0.5 * (1.0 - span) * l;
    let holes = [0.222 * l, 0.778 * l];
    let cases: Vec<(&str, f64, bool)> = vec![
        ("plain", 0.0, false),
        ("holes", 0.0, true),
        ("undercut", d, false),
        ("undercut+holes", d, true),
    ];
    for (name, depth, with_holes) in cases {
        let g = BarGeometry {
            length_mm: l,
            width_mm: w,
            thickness_mm: t,
            undercut_depth_mm: depth,
            undercut_span: span,
            hole_dia_mm: if with_holes { hd } else { 0.0 },
            holes_mm: if with_holes { holes.to_vec() } else { vec![] },
        };
        let beam = modal(&g, BarMaterial::Aluminium6061, 1, &ModelOptions::default()).hz[0];
        let mut tools = vec![];
        if depth > 0.0 {
            tools.push(format!(
                "[translate {a} -1 -1 [cube {} {} {}]]",
                span * l,
                w + 2.0,
                depth + 1.0
            ));
        }
        if with_holes {
            for h in holes {
                tools.push(format!(
                    "[translate {h} {} -1 [cylinder {} {}]]",
                    w / 2.0,
                    hd / 2.0,
                    t + 2.0
                ));
            }
        }
        let body = format!("[cube {l} {w} {t}]");
        let shape = if tools.is_empty() {
            body
        } else {
            let tool = tools
                .iter()
                .skip(1)
                .fold(tools[0].clone(), |acc, x| format!("[union {acc} {x}]"));
            format!("[difference {tool} {body}]")
        };
        let src = format!("[let bar {shape}]\n[root bar \"aluminum\"]\n");
        let (sp, dp) = solid(&src, Quality::Preview);
        let (sf, df) = solid(&src, Quality::Fine);
        println!("{name:<16} beam {beam:8.1}  solid preview {sp:8.1} ({dp} dof)  fine {sf:8.1} ({df} dof)  Δ {:+.0} c", 1200.0 * (sf / beam).log2());
    }
}

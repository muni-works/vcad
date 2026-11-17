//! 6" OD 304 stainless vacuum shells under one atmosphere: the shell
//! screen across 0.5–3 mm walls, plus an eigenvalue-FE cross-check of the
//! perfect-shell bifurcation pressure on body-fitted meshes.
//!
//! `cargo run --release -p vcad-kernel-fea --example vacuum_shell`

use vcad_kernel_fea::buckling::{
    buckling_on_mesh, capped_cylinder_spec, cylinder_shell, sphere_shell, BucklingSpec,
    FaceSelection, PressureLoad,
};
use vcad_kernel_fea::shell::{check_shell, CylinderLoading, ShellCase, ShellGeometry, ATM_MPA};

const E: f64 = 193_000.0;
const NU: f64 = 0.29;
const YIELD: f64 = 205.0;
const OD: f64 = 152.4;
const SPAN: f64 = 300.0;

fn case(geometry: ShellGeometry) -> ShellCase {
    ShellCase {
        geometry,
        youngs_modulus_mpa: E,
        poisson: NU,
        yield_strength_mpa: Some(YIELD),
        external_pressure_mpa: ATM_MPA,
        design_factor: 3.0,
    }
}

fn main() {
    println!("6\" OD 304 SS, 1 atm external (E {E} MPa, nu {NU}, yield {YIELD} MPa)\n");
    println!(
        "{:<9} {:>5} | {:>10} {:>6} {:>10} {:>10} {:>8} | {:>9}",
        "shell", "t mm", "classical", "kd", "collapse", "allowable", "margin", "FE bifurc"
    );
    for t in [0.5, 1.0, 1.5, 2.0, 3.0] {
        let cyl = check_shell(&case(ShellGeometry::Cylinder {
            outer_diameter_mm: OD,
            wall_mm: t,
            length_mm: SPAN,
            loading: CylinderLoading::Hydrostatic,
        }))
        .unwrap();
        let r = 0.5 * (OD - t);
        let n_theta = 96;
        let sh = cylinder_shell(r, t, SPAN, n_theta, 48, 2);
        let spec = capped_cylinder_spec(&sh, r, t, E, NU, ATM_MPA, CylinderLoading::Hydrostatic);
        let fe = buckling_on_mesh(&sh.mesh, &spec)
            .map(|b| format!("{:.3}", b.modes[0].load_factor * ATM_MPA))
            .unwrap_or_else(|e| format!("({e})"));
        row(&format!("cyl L{SPAN:.0}"), t, &cyl, &fe);

        let sph = check_shell(&case(ShellGeometry::Sphere {
            outer_diameter_mm: OD,
            wall_mm: t,
        }))
        .unwrap();
        let rm = 0.5 * OD - 0.5 * t;
        let fe = if t >= 1.5 {
            let sh = sphere_shell(rm, t, 24, 1);
            let spec = BucklingSpec {
                youngs_modulus_mpa: E,
                poisson: NU,
                pressures: vec![PressureLoad {
                    pressure_mpa: ATM_MPA,
                    faces: FaceSelection::Faces {
                        faces: sh.outer_faces.clone(),
                    },
                    follower: true,
                }],
                forces: vec![],
                supports: vec![],
                node_supports: vec![],
                kinematic: true,
                modes: 1,
                knockdown: None,
            };
            buckling_on_mesh(&sh.mesh, &spec)
                .map(|b| format!("{:.3}", b.modes[0].load_factor * ATM_MPA))
                .unwrap_or_else(|e| format!("({e})"))
        } else {
            "(mesh)".into()
        };
        row("sphere", t, &sph, &fe);
    }
    println!(
        "\npressures in MPa (1 atm = {ATM_MPA}); kd = imperfection knockdown applied; FE = \
         perfect-shell eigenvalue (UNCONSERVATIVE, compare with 'classical'); thin spheres need \
         a finer cube-sphere than this example affords"
    );
}

fn row(name: &str, t: f64, c: &vcad_kernel_fea::shell::ShellCheck, fe: &str) {
    println!(
        "{:<9} {:>5.1} | {:>10.3} {:>6.3} {:>10.3} {:>10.4} {:>8.2} | {:>9}",
        name,
        t,
        c.classical_pressure_mpa,
        c.knockdown,
        c.collapse_pressure_mpa,
        c.allowable_pressure_mpa,
        c.margin,
        fe
    );
}

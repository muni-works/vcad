//! STEP B-spline faces end to end: parse → B-rep → tessellation.
//!
//! Hand-cut from the failure modes of a SolidWorks 2021 part (411080037.STEP,
//! 294 faces) that imported with 112 faces missing and the rest scattered:
//!
//! - `control_points_list` is `[u][v]`; reading it as `[v][u]` rejected every
//!   non-square patch ("invalid knot vector") and transposed the square ones;
//! - complex-entity partials carry no `name`, so every rational
//!   (`RATIONAL_B_SPLINE_SURFACE`) patch was rejected outright;
//! - B-spline faces were meshed over the whole untrimmed surface;
//! - `same_sense` written backwards on a few patches turned them inside out.

use vcad_kernel_tessellate::{tessellate_brep_by_face, TessellationParams, TriangleMesh};

/// One flat face on a non-square B-spline patch — 2 control points in u
/// (degree 1) by 3 in v (degree 2), spanning x∈[0,10], y∈[0,20] at z=0 —
/// trimmed to the triangle (0,0)–(10,0)–(0,20): area 100 of the patch's 200.
/// `SURFACE` and `SAME_SENSE` are substituted per test.
const FACE: &str = r#"ISO-10303-21;
HEADER;
FILE_DESCRIPTION((''), '2;1');
FILE_NAME('trimmed_bspline.step', '2026-11-10', (''), (''), '', '', '');
FILE_SCHEMA(('CONFIG_CONTROL_DESIGN'));
ENDSEC;
DATA;
#1 = CARTESIAN_POINT('', (0.0, 0.0, 0.0));
#2 = CARTESIAN_POINT('', (0.0, 10.0, 0.0));
#3 = CARTESIAN_POINT('', (0.0, 20.0, 0.0));
#4 = CARTESIAN_POINT('', (10.0, 0.0, 0.0));
#5 = CARTESIAN_POINT('', (10.0, 10.0, 0.0));
#6 = CARTESIAN_POINT('', (10.0, 20.0, 0.0));
SURFACE
#20 = VERTEX_POINT('', #1);
#21 = VERTEX_POINT('', #4);
#22 = VERTEX_POINT('', #3);
#30 = DIRECTION('', (1.0, 0.0, 0.0));
#31 = DIRECTION('', (-0.4472135954999579, 0.8944271909999159, 0.0));
#32 = DIRECTION('', (0.0, -1.0, 0.0));
#33 = VECTOR('', #30, 1.0);
#34 = VECTOR('', #31, 1.0);
#35 = VECTOR('', #32, 1.0);
#36 = LINE('', #1, #33);
#37 = LINE('', #4, #34);
#38 = LINE('', #3, #35);
#40 = EDGE_CURVE('', #20, #21, #36, .T.);
#41 = EDGE_CURVE('', #21, #22, #37, .T.);
#42 = EDGE_CURVE('', #22, #20, #38, .T.);
#50 = ORIENTED_EDGE('', *, *, #40, .T.);
#51 = ORIENTED_EDGE('', *, *, #41, .T.);
#52 = ORIENTED_EDGE('', *, *, #42, .T.);
#53 = EDGE_LOOP('', (#50, #51, #52));
#54 = FACE_OUTER_BOUND('', #53, .T.);
#60 = ADVANCED_FACE('', (#54), #10, SAME_SENSE);
#61 = OPEN_SHELL('', (#60));
#62 = MANIFOLD_SOLID_BREP('', #61);
ENDSEC;
END-ISO-10303-21;
"#;

const PLAIN_SURFACE: &str = "#10 = B_SPLINE_SURFACE_WITH_KNOTS('', 1, 2, \
    ((#1, #2, #3), (#4, #5, #6)), .UNSPECIFIED., .F., .F., .F., \
    (2, 2), (3, 3), (0.0, 1.0), (0.0, 1.0), .UNSPECIFIED.);";

/// The same patch as a complex entity, rational (weights 1, 2, 1 along v):
/// the partials carry only their own attributes, no leading name.
const RATIONAL_SURFACE: &str = "#10 = ( BOUNDED_SURFACE ( ) B_SPLINE_SURFACE ( 1, 2, \
    ((#1, #2, #3), (#4, #5, #6)), .UNSPECIFIED., .F., .F., .F. ) \
    B_SPLINE_SURFACE_WITH_KNOTS ( (2, 2), (3, 3), (0.0, 1.0), (0.0, 1.0), .UNSPECIFIED. ) \
    GEOMETRIC_REPRESENTATION_ITEM ( ) RATIONAL_B_SPLINE_SURFACE ( ((1.0, 2.0, 1.0), (1.0, 2.0, 1.0)) ) \
    REPRESENTATION_ITEM ( '' ) SURFACE ( ) );";

fn fixture(surface: &str, same_sense: &str) -> String {
    FACE.replace("SURFACE\n", &format!("{surface}\n"))
        .replace("SAME_SENSE", same_sense)
}

fn area_and_normal(m: &TriangleMesh) -> (f64, [f64; 3]) {
    let p = |i: u32| {
        let k = i as usize * 3;
        [0, 1, 2].map(|c| m.vertices[k + c] as f64)
    };
    let (mut area, mut n) = (0.0, [0.0; 3]);
    for t in m.indices.chunks(3) {
        let (a, b, c) = (p(t[0]), p(t[1]), p(t[2]));
        let u = [b[0] - a[0], b[1] - a[1], b[2] - a[2]];
        let v = [c[0] - a[0], c[1] - a[1], c[2] - a[2]];
        let x = [
            u[1] * v[2] - u[2] * v[1],
            u[2] * v[0] - u[0] * v[2],
            u[0] * v[1] - u[1] * v[0],
        ];
        area += 0.5 * (x[0] * x[0] + x[1] * x[1] + x[2] * x[2]).sqrt();
        for k in 0..3 {
            n[k] += 0.5 * x[k];
        }
    }
    (area, n)
}

fn mesh_face(step: &str) -> (TriangleMesh, vcad_kernel_step::StepImportReport) {
    let read = vcad_kernel_step::read_step_from_buffer_with_report(step.as_bytes())
        .expect("fixture must import");
    assert_eq!(read.solids.len(), 1);
    assert!(
        read.report.is_clean(),
        "no face may be skipped: {:?}",
        read.report.summary()
    );
    let faces = tessellate_brep_by_face(&read.solids[0], &TessellationParams::default());
    assert_eq!(faces.len(), 1);
    (faces.into_iter().next().unwrap().2, read.report)
}

#[test]
fn non_square_bspline_face_is_read_and_trimmed() {
    let (mesh, _) = mesh_face(&fixture(PLAIN_SURFACE, ".T."));
    let (area, n) = area_and_normal(&mesh);
    // The trimmed triangle, not the 10×20 patch.
    assert!((area - 100.0).abs() < 1e-3, "area {area}");
    assert!(n[2] > 0.0, "face normal must be +z, got {n:?}");
    let max_y = mesh
        .vertices
        .chunks(3)
        .map(|v| v[1] as f64)
        .fold(f64::MIN, f64::max);
    assert!(
        (max_y - 20.0).abs() < 1e-4,
        "patch spans y to 20, got {max_y}"
    );
}

#[test]
fn rational_complex_bspline_face_is_read() {
    let (mesh, _) = mesh_face(&fixture(RATIONAL_SURFACE, ".T."));
    let (area, n) = area_and_normal(&mesh);
    assert!((area - 100.0).abs() < 1e-3, "area {area}");
    assert!(n[2] > 0.0, "face normal must be +z, got {n:?}");
}

/// The loop winds counter-clockwise about ∂u × ∂v (+z). A `same_sense` of
/// `.F.` contradicts it; the loop wins, and the report says so.
#[test]
fn backwards_same_sense_is_oriented_by_the_loop() {
    let (mesh, report) = mesh_face(&fixture(PLAIN_SURFACE, ".F."));
    let (area, n) = area_and_normal(&mesh);
    assert!((area - 100.0).abs() < 1e-3, "area {area}");
    assert!(n[2] > 0.0, "face must face +z like its loop, got {n:?}");
    assert!(
        report.solids[0]
            .notes
            .iter()
            .any(|n| n.contains("oriented by its boundary loop")),
        "notes: {:?}",
        report.solids[0].notes
    );
}

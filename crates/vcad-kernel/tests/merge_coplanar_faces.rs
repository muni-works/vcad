//! `Solid::merge_coplanar_faces`: the planar fragments a boolean leaves go
//! back into single faces, and everything downstream of the solid — face
//! ids on the mesh, names, STEP, further booleans — still works.

use std::collections::{HashMap, HashSet};

use vcad_kernel::vcad_kernel_geom::SurfaceKind;
use vcad_kernel::{Solid, SolidFidelity};

fn sectioned_can() -> Solid {
    let can = Solid::cylinder(79.5, 159.0, 0).translate(0.0, 0.0, -79.5);
    let cavity = Solid::cylinder(76.0, 152.0, 0).translate(0.0, 0.0, -76.0);
    let quadrant = Solid::cube(400.0, 400.0, 800.0).translate(0.0, -400.0, -400.0);
    can.difference(&cavity).difference(&quadrant)
}

/// (planar faces, all faces) of the solid's outer shell.
fn face_counts(s: &Solid) -> (usize, usize) {
    let b = s.brep().expect("b-rep");
    let planar = b
        .topology
        .faces
        .values()
        .filter(|f| b.geometry.surfaces[f.surface_index].surface_type() == SurfaceKind::Plane)
        .count();
    (planar, b.topology.faces.len())
}

/// Planar faces per smooth group: after merging, a plane group has more
/// than one face only where the plane really is in separate regions.
fn planar_group_sizes(s: &Solid) -> Vec<usize> {
    let b = s.brep().expect("b-rep");
    let topo = &b.topology;
    let shell = &topo.shells[topo.solids[b.solid_id].outer_shell];
    let groups = s.smooth_face_groups().expect("b-rep");
    let mut sizes: HashMap<u32, usize> = HashMap::new();
    for (ordinal, f) in shell.faces.iter().enumerate() {
        let face = &topo.faces[*f];
        if b.geometry.surfaces[face.surface_index].surface_type() == SurfaceKind::Plane {
            *sizes.entry(groups[ordinal]).or_default() += 1;
        }
    }
    let mut v: Vec<usize> = sizes.into_values().collect();
    v.sort_unstable();
    v
}

#[test]
fn a_sectioned_can_has_six_planar_faces() {
    let split = sectioned_can();
    let merged = split.merge_coplanar_faces();
    let (p0, n0) = face_counts(&split);
    let (p1, n1) = face_counts(&merged);
    eprintln!("sectioned can: {n0} faces ({p0} planar) -> {n1} ({p1} planar)");
    assert!(p0 > 6, "expected the caps in pieces, got {p0} planar faces");
    // Two outer caps, two cavity caps, one C-shaped face per section plane.
    assert_eq!(p1, 6);
    // Nothing left for the outline grouping to do among planes.
    assert_eq!(planar_group_sizes(&merged), vec![1; 6]);
    assert_eq!(merged.fidelity(), SolidFidelity::Analytic);
    assert!((merged.volume() - split.volume()).abs() < 1e-6 * split.volume());
    // Idempotent.
    assert_eq!(face_counts(&merged.merge_coplanar_faces()), (p1, n1));
}

/// The id a triangle carries is the ordinal of its face in the outer
/// shell (what picking in the app relies on): after a merge every id still
/// names a live face, every face still has triangles, and a triangle on a
/// planar face lies on that face's plane.
#[test]
fn mesh_face_ids_follow_the_merged_shell() {
    let merged = sectioned_can().merge_coplanar_faces();
    let b = merged.brep().expect("b-rep");
    let topo = &b.topology;
    let shell = &topo.shells[topo.solids[b.solid_id].outer_shell];
    let mesh = merged.to_mesh(32);
    assert_eq!(mesh.face_ids.len(), mesh.indices.len() / 3);
    let used: HashSet<u32> = mesh.face_ids.iter().copied().collect();
    assert_eq!(used.len(), shell.faces.len());
    assert!(used.iter().all(|&i| (i as usize) < shell.faces.len()));
    for (t, &id) in mesh.face_ids.iter().enumerate() {
        let face = &topo.faces[shell.faces[id as usize]];
        let surface = &b.geometry.surfaces[face.surface_index];
        let Some(plane) = surface
            .as_any()
            .downcast_ref::<vcad_kernel::vcad_kernel_geom::Plane>()
        else {
            continue;
        };
        for k in 0..3 {
            let i = mesh.indices[3 * t + k] as usize * 3;
            let p = vcad_kernel::vcad_kernel_math::Point3::new(
                mesh.vertices[i] as f64,
                mesh.vertices[i + 1] as f64,
                mesh.vertices[i + 2] as f64,
            );
            let d = (p - plane.origin).dot(*plane.normal_dir.as_ref());
            assert!(
                d.abs() < 1e-3,
                "triangle {t} of face {id} is {d} off its plane"
            );
        }
    }
}

#[test]
fn a_merged_solid_round_trips_through_step() {
    let merged = sectioned_can().merge_coplanar_faces();
    let step = merged.to_step_buffer().expect("export");
    let back = Solid::from_step_buffer(&step).expect("import");
    // An imported solid meshes its rims on the plain segment grid, not the
    // boolean's frozen one, so the volume moves by the inscribed-polygon
    // deficit — exactly as it does for the split solid.
    let split_back = Solid::from_step_buffer(&sectioned_can().to_step_buffer().expect("export"))
        .expect("import");
    let (v0, v1) = (split_back.volume(), back.volume());
    assert!((v0 - v1).abs() < 1e-6 * v0, "volume {v0} -> {v1}");
    assert!((merged.volume() - v1).abs() < 5e-3 * v1);
    assert_eq!(face_counts(&back), face_counts(&merged));
}

#[test]
fn a_merged_solid_takes_further_booleans() {
    let split = sectioned_can();
    let merged = split.merge_coplanar_faces();
    let drill = Solid::cylinder(5.0, 40.0, 0).translate(-30.0, 30.0, 60.0);
    let slot = Solid::cube(20.0, 30.0, 10.0).translate(70.0, -15.0, -5.0);
    for tool in [&drill, &slot] {
        let (a, b) = (split.difference(tool), merged.difference(tool));
        assert_eq!(
            b.fidelity(),
            SolidFidelity::Analytic,
            "{:?}",
            b.degradations()
        );
        assert!(a.volume() < split.volume() - 1.0);
        assert!(
            (a.volume() - b.volume()).abs() < 1e-6 * a.volume(),
            "{} vs {}",
            a.volume(),
            b.volume()
        );
    }
}

/// Names follow the merge: absorbed faces leave the map, no name is
/// duplicated, and fragments reunited into one face lose their ordinals.
#[test]
fn reunited_fragments_get_their_name_back() {
    // Two flush boxes: the union's top is box A's top and box B's top —
    // differently named faces, so the merged face is anonymous.
    let a = Solid::cube(10.0, 10.0, 10.0);
    let b = Solid::cube(10.0, 10.0, 10.0).translate(10.0, 0.0, 0.0);
    let u = a.union(&b).merge_coplanar_faces();
    assert_eq!(face_counts(&u), (6, 6));

    let split = sectioned_can();
    let names = split.names().expect("names");
    let merged = split.merge_coplanar_faces();
    let merged_names = merged.names().expect("names");
    let live: HashSet<_> = merged.brep().unwrap().topology.faces.keys().collect();
    assert!(merged_names.faces.keys().all(|f| live.contains(f)));
    // No name is carried by two faces, and no fragment name outlives the
    // merge that reunited every fragment of its face.
    let mut seen = HashSet::new();
    for n in merged_names.faces.values() {
        assert!(seen.insert(n.clone()), "duplicate name {n}");
    }
    let split_fragments = names.faces.values().filter(|n| !n.path.is_empty()).count();
    let merged_fragments = merged_names
        .faces
        .values()
        .filter(|n| !n.path.is_empty())
        .count();
    eprintln!("fragment names: {split_fragments} -> {merged_fragments}");
    assert!(merged_fragments < split_fragments);
}

/// A face reference opens the whole flat face it lands on. The solid an
/// operation resolves against still has the boolean's fragments while the
/// document root shows them merged, so a pick on the merged lid — whose
/// name and centroid belong to no single fragment — must open every piece.
#[test]
fn a_face_reference_opens_the_whole_flat_face() {
    use vcad_kernel::vcad_kernel_math::{Point3, Vec3};
    use vcad_kernel::FaceRef;

    let a = Solid::cube(10.0, 10.0, 10.0);
    let b = Solid::cube(10.0, 10.0, 10.0).translate(10.0, 0.0, 0.0);
    let split = a.union(&b);
    assert!(
        face_counts(&split).1 > 6,
        "the union's top should be in pieces"
    );
    let top = FaceRef {
        name: None,
        point: Some(Point3::new(3.0, 5.0, 10.0)),
        normal: Some(Vec3::new(0.0, 0.0, 1.0)),
    };
    let opened = split.shell_open_refs(1.0, &[top.clone()]).expect("shell");
    // The same tray cut from one 20 × 10 × 10 block.
    let whole = Solid::cube(20.0, 10.0, 10.0)
        .shell_open_refs(1.0, &[top])
        .expect("shell");
    let expect = 20.0 * 10.0 * 10.0 - 18.0 * 8.0 * 9.0;
    assert!((whole.volume() - expect).abs() < 1e-6, "{}", whole.volume());
    assert!(
        (opened.volume() - expect).abs() < 1e-6,
        "opened {} of the top, expected the whole face ({expect})",
        opened.volume()
    );
}

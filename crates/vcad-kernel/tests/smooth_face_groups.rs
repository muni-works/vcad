//! Faces a boolean left on one surface form one group for outline drawing.

use std::collections::HashSet;

use vcad_kernel::Solid;

fn distinct(groups: &[u32]) -> usize {
    groups.iter().collect::<HashSet<_>>().len()
}

/// Model-outline segments of the solid's mesh, by raw face id and by group.
fn outline_counts(s: &Solid) -> (usize, usize) {
    let groups = s.smooth_face_groups().expect("b-rep");
    let mut mesh = s.to_mesh(32);
    let raw = mesh.topology_edge_segments(30.0).len();
    mesh.face_ids = mesh
        .face_ids
        .iter()
        .map(|&f| groups.get(f as usize).copied().unwrap_or(f))
        .collect();
    (raw, mesh.topology_edge_segments(30.0).len())
}

/// A drum with a bore across it: the wall is partitioned around the two
/// bore mouths, but it is one cylinder — two caps, one wall, one bore.
#[test]
fn a_cross_bored_drum_has_four_surfaces() {
    let drum = Solid::cylinder(40.0, 80.0, 0).translate(0.0, 0.0, -40.0);
    let bore = Solid::cylinder(10.0, 120.0, 0)
        .translate(0.0, 0.0, -60.0)
        .rotate(90.0, 0.0, 0.0);
    let cut = drum.difference(&bore);
    let groups = cut.smooth_face_groups().expect("b-rep");
    assert!(
        groups.len() > 4,
        "expected the wall in pieces, got {} faces",
        groups.len()
    );
    assert_eq!(distinct(&groups), 4, "groups {groups:?}");
    // Every group id is the ordinal of a face in that group.
    for (i, &g) in groups.iter().enumerate() {
        assert!(g as usize <= i && groups[g as usize] == g);
    }
    let (raw, grouped) = outline_counts(&cut);
    assert!(
        grouped < raw,
        "outline did not lose the wall's internal seams ({raw} → {grouped} segments)"
    );
}

/// A quadrant cut out of a can with a cavity: the section planes run on
/// across the caps and the cavity's cylinder scores them, yet the result
/// has only the surfaces a drawing would show — two outer caps, the outer
/// wall, the cavity's wall and two ends, and the two section planes.
#[test]
fn a_sectioned_can_groups_its_cap_and_wall_pieces() {
    let can = Solid::cylinder(79.5, 159.0, 0).translate(0.0, 0.0, -79.5);
    let cavity = Solid::cylinder(76.0, 152.0, 0).translate(0.0, 0.0, -76.0);
    let quadrant = Solid::cube(400.0, 400.0, 800.0).translate(0.0, -400.0, -400.0);
    let cut = can.difference(&cavity).difference(&quadrant);
    let groups = cut.smooth_face_groups().expect("b-rep");
    assert_eq!(
        distinct(&groups),
        8,
        "{} faces, groups {groups:?}",
        groups.len()
    );
}

/// Tangent edges are still edges: a filleted box keeps every blend apart
/// from the planes it meets and from the other blends.
#[test]
fn fillets_stay_separate_from_their_walls() {
    let plain = Solid::cube(30.0, 20.0, 10.0);
    assert_eq!(distinct(&plain.smooth_face_groups().expect("b-rep")), 6);
    let filleted = plain.fillet(3.0).expect("fillet");
    let groups = filleted.smooth_face_groups().expect("b-rep");
    // 6 planes, 12 edge blends, 8 corner blends.
    assert_eq!(distinct(&groups), 26, "groups {groups:?}");
}

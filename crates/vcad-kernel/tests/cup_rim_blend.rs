//! A cup — a cylinder minus a smaller one raised by the floor thickness —
//! fillets on its bottom rim by name, through the boolean's polyline rim.

use vcad_kernel::vcad_kernel_fillet::{BlendKey, BlendSection};
use vcad_kernel::vcad_kernel_math::{Point3, Vec3};
use vcad_kernel::vcad_kernel_naming::EdgeHint;
use vcad_kernel::Solid;

fn cup() -> Solid {
    let mut outer = Solid::cylinder(86.0, 40.0, 0);
    outer.set_name_scope("n2");
    let mut inner = Solid::cylinder(84.0, 40.0, 0);
    inner.set_name_scope("n1");
    outer.difference(&inner.translate(0.0, 0.0, 2.0))
}

#[test]
fn cup_bottom_rim_fillets_by_name() {
    let c = cup();
    let a = std::f64::consts::PI / 512.0;
    let hint = EdgeHint {
        midpoint: Point3::new(86.0 * a.cos(), 86.0 * a.sin(), 0.0),
        direction: Vec3::new(0.0, 1.0, 0.0),
        length: 2.0 * std::f64::consts::PI * 86.0 / 512.0,
    };
    let keys = [BlendKey {
        t: 0.0,
        section: BlendSection {
            size: 5.0,
            shape: 1.0,
        },
    }];
    let out = c
        .edge_blend_named_hinted("n2:side", "n2:bottom", Some(hint), &keys)
        .expect("the bottom rim fillets");
    assert!(
        out.volume() < c.volume(),
        "a convex fillet removes material"
    );
}

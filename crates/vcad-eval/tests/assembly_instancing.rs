//! Big imported assemblies open fast: identical STEP solids tessellate once
//! and are instanced, parts stream to the viewport in order, and the preview
//! LOD never pollutes the full-quality cache.
//!
//! The Voron 2.4 STEP (docs/assembly-stress.md) holds 1560 separate
//! `MANIFOLD_SOLID_BREP`s — every screw its own copy — so sharing has to come
//! from the solids' content, not the file's structure.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use vcad_eval::cache::{MemoryMeshCache, RootKey, RootMeshCache};
use vcad_eval::{evaluate_document, step_sources, EvalOptions, EvaluatedMesh};
use vcad_ir::{CsgOp, Document, Node, SceneEntry, Vec3};

/// Counts stores, so a test can see how many meshes were tessellated.
#[derive(Default)]
struct Counting {
    inner: MemoryMeshCache,
    puts: RefCell<Vec<RootKey>>,
}

impl RootMeshCache for Counting {
    fn get(&self, key: &RootKey) -> Option<EvaluatedMesh> {
        self.inner.get(key)
    }
    fn put(&self, key: &RootKey, mesh: &EvaluatedMesh) {
        self.puts.borrow_mut().push(key.clone());
        self.inner.put(key, mesh);
    }
}

/// Three bodies: two identical 10 mm cubes and a 2 mm cube.
fn register(path: &str) {
    let a = vcad_kernel::Solid::cube(10.0, 10.0, 10.0);
    let b = vcad_kernel::Solid::cube(10.0, 10.0, 10.0);
    let c = vcad_kernel::Solid::cube(2.0, 2.0, 2.0);
    let bytes = vcad_kernel::Solid::solids_to_step_buffer(&[(&a, "a"), (&b, "b"), (&c, "c")])
        .expect("primitives export to STEP");
    step_sources::register(path, bytes);
}

/// One root per `(solid_index, x offset)`, placed the way the STEP importer
/// places occurrences (`Translate(StepImport)`).
fn doc(path: &str, placed: &[(u32, f64)]) -> Document {
    let mut nodes = HashMap::new();
    let mut roots = Vec::new();
    let mut id = 0u64;
    for &(index, x) in placed {
        id += 1;
        let leaf = id;
        nodes.insert(
            leaf,
            Node {
                id: leaf,
                name: None,
                op: CsgOp::StepImport {
                    path: path.into(),
                    solid_index: Some(index),
                },
            },
        );
        id += 1;
        nodes.insert(
            id,
            Node {
                id,
                name: None,
                op: CsgOp::Translate {
                    child: leaf,
                    offset: Vec3 { x, y: 0.0, z: 0.0 },
                },
            },
        );
        roots.push(SceneEntry {
            root: id,
            material: "default".into(),
            visible: None,
        });
    }
    Document {
        nodes,
        roots,
        ..Default::default()
    }
}

fn min_x(mesh: &EvaluatedMesh) -> f32 {
    mesh.positions
        .chunks(3)
        .map(|p| p[0])
        .fold(f32::INFINITY, f32::min)
}

#[test]
fn identical_step_solids_share_a_content_key() {
    let path = "test://assembly_instancing/keys.step";
    register(path);
    let k0 = step_sources::solid_key(path, 0).expect("key for solid 0");
    let k1 = step_sources::solid_key(path, 1).expect("key for solid 1");
    let k2 = step_sources::solid_key(path, 2).expect("key for solid 2");
    assert_eq!(
        k0, k1,
        "two separately written identical cubes are one shape"
    );
    assert_ne!(k0, k2, "a different cube is a different shape");
    assert!(step_sources::solid_key(path, 9).is_none());
}

#[test]
fn identical_parts_tessellate_once_and_are_placed() {
    let path = "test://assembly_instancing/instancing.step";
    register(path);
    // Six occurrences of two distinct shapes.
    let placed = [
        (0, 0.0),
        (1, 20.0),
        (0, 40.0),
        (2, 60.0),
        (1, 80.0),
        (2, 100.0),
    ];
    let cache = Rc::new(Counting::default());
    let order = Rc::new(RefCell::new(Vec::new()));
    let seen = order.clone();
    let opts = EvalOptions {
        root_cache: Some(cache.clone()),
        on_root: Some(Box::new(move |i, n, _| seen.borrow_mut().push((i, n)))),
        ..Default::default()
    };
    let scene = evaluate_document(&doc(path, &placed), &opts).expect("evaluates");

    assert_eq!(scene.parts.len(), 6);
    assert!(scene.failures.is_empty(), "{:?}", scene.failures);
    let puts = cache.puts.borrow();
    assert_eq!(
        puts.len(),
        2,
        "two distinct shapes → two tessellations, not six (keys {puts:?})"
    );
    for (part, &(_, x)) in scene.parts.iter().zip(&placed) {
        assert!(!part.mesh.indices.is_empty());
        assert!(
            (min_x(&part.mesh) - x as f32).abs() < 1e-3,
            "a shared mesh is still placed at its own occurrence (want x={x})"
        );
    }
    let order = order.borrow();
    assert_eq!(
        *order,
        (0..6).map(|i| (i, 6)).collect::<Vec<_>>(),
        "parts land once each, in document order"
    );
}

#[test]
fn preview_lod_uses_its_own_cache_keys() {
    let path = "test://assembly_instancing/preview.step";
    register(path);
    let d = doc(path, &[(0, 0.0), (2, 30.0)]);
    let cache = Rc::new(Counting::default());
    let opts = EvalOptions {
        root_cache: Some(cache.clone()),
        ..Default::default()
    };
    let preview = {
        let _p = vcad_eval::evaluate::DisplayPreviewGuard::enable(true);
        evaluate_document(&d, &opts).expect("preview evaluates")
    };
    assert!(preview.parts.iter().all(|p| !p.mesh.indices.is_empty()));
    let full = evaluate_document(&d, &opts).expect("full evaluates");
    assert!(full.parts.iter().all(|p| !p.mesh.indices.is_empty()));
    let puts = cache.puts.borrow();
    assert_eq!(
        puts.len(),
        4,
        "the full pass must not be served the preview meshes"
    );
}

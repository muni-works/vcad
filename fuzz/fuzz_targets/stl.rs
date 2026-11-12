#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = vcad_eval::mesh_inline::stl_to_mesh(data, None);
});

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = vcad_app::DocumentApi::load(data, vcad_crdt::ReplicaId(1));
});

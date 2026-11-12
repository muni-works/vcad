#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    if let Ok(s) = std::str::from_utf8(data) {
        let _ = vcad_app::DocumentApi::from_ir(s, vcad_crdt::ReplicaId(1));
    }
});

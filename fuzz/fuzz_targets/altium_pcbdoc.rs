#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = vcad_ecad_symbols::parse_altium_pcbdoc(data);
});

#![no_main]
use libfuzzer_sys::fuzz_target;

fuzz_target!(|data: &[u8]| {
    let _ = vcad_kernel_step::read_step_from_buffer(data);
});

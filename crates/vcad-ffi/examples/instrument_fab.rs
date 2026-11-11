//! Fab dry runs for a tuned instrument document, through the same C ABI the
//! Mac app's Tune panel calls (`vcad_instrument_call`): slice the bars and
//! frame plates to G-code, and mill every bar (two setups each) to .nc
//! files. Writes files only — nothing is sent to a printer or a mill.
//!
//! `cargo run --release -p vcad-ffi --example instrument_fab -- doc.vcad out/`

use std::ffi::{c_char, CStr, CString};

extern "C" {
    // Exported by vcad-ffi (json.rs); declared here since the module is private.
    fn vcad_string_free(s: *mut c_char);
}

fn call(doc: &CString, req: serde_json::Value) -> serde_json::Value {
    let r = CString::new(req.to_string()).unwrap();
    let p = vcad_ffi::instrument::vcad_instrument_call(doc.as_ptr(), r.as_ptr());
    if p.is_null() {
        let mut len = 0usize;
        let e = vcad_ffi::vcad_last_error(&mut len);
        let msg = unsafe { std::str::from_utf8_unchecked(std::slice::from_raw_parts(e, len)) };
        panic!("vcad_instrument_call failed: {msg}");
    }
    let s = unsafe { CStr::from_ptr(p) }.to_string_lossy().into_owned();
    unsafe { vcad_string_free(p) };
    serde_json::from_str(&s).unwrap()
}

fn main() {
    let args: Vec<String> = std::env::args().collect();
    let doc_path = args.get(1).expect("usage: instrument_fab doc.vcad out_dir");
    let out = args.get(2).map(String::as_str).unwrap_or(".");
    std::fs::create_dir_all(out).unwrap();
    let doc = CString::new(std::fs::read_to_string(doc_path).unwrap()).unwrap();
    for plate in ["bars", "frame"] {
        let path = format!("{out}/{plate}-plate.gcode");
        let r = call(
            &doc,
            serde_json::json!({"op": "print", "plate": plate, "gcode_path": path,
            "settings": {"printer": "bambu_p1s"}}),
        );
        println!(
            "print {plate:<5} → {path}  ({} lines, fits P1S bed: {}, {:.0} min est.)",
            r["gcodeLines"],
            r["fitsBed"],
            r["stats"]["print_time_seconds"]
                .as_f64()
                .unwrap_or(f64::NAN)
                / 60.0
        );
    }
    let receipt = call(&doc, serde_json::json!({"op": "receipt"}));
    for b in receipt["bars"].as_array().unwrap() {
        let note = b["note"].as_str().unwrap();
        let r = call(
            &doc,
            serde_json::json!({"op": "mill", "note": note, "out_dir": out}),
        );
        let files: Vec<String> = r["setups"]
            .as_array()
            .unwrap()
            .iter()
            .map(|s| {
                format!(
                    "{} ({} lines)",
                    s["path"].as_str().unwrap().rsplit('/').next().unwrap(),
                    s["gcode_lines"]
                )
            })
            .collect();
        println!("mill  {note:<4} → {}", files.join(", "));
    }
}

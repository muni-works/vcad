//! Write a drawing of a `.vcad` (or `.loon`) document as PDF + sheet DXF +
//! the sheet JSON — the same calls the Mac app's Drawing workspace makes.
//!
//! ```text
//! cargo run -p vcad-ffi --example drawing_sample -- part.vcad out/part [view]
//! ```
//! writes `out/part.pdf`, `out/part.dxf`, `out/part.json`. `view` defaults to
//! `standard` (front / top / right / iso, auto sheet + scale).

use std::ffi::{CStr, CString};

fn main() {
    let args: Vec<String> = std::env::args().collect();
    if args.len() < 3 {
        eprintln!("usage: drawing_sample <doc.vcad|doc.loon> <out-prefix> [view]");
        std::process::exit(2);
    }
    let path = std::path::Path::new(&args[1]);
    let text = std::fs::read_to_string(path).expect("read document");
    let doc_json = if path.extension().is_some_and(|e| e == "loon") {
        let doc = vcad_loon::eval_vcad_file(path).expect("evaluate loon");
        serde_json::to_string(&doc).unwrap()
    } else {
        text
    };
    let view = args.get(3).map(String::as_str).unwrap_or("standard");
    let name = path.file_stem().unwrap().to_string_lossy().to_string();
    let base = path
        .parent()
        .map(|p| p.to_string_lossy().to_string())
        .unwrap_or_default();
    let req = serde_json::json!({
        "view": view,
        "base_dir": base,
        "drawing": serde_json::from_str::<serde_json::Value>(&doc_json).ok()
            .and_then(|d| d.get("drawing").cloned())
            .filter(|d| !d.is_null())
            .unwrap_or_else(|| serde_json::json!({
                "titleBlock": {"partName": name, "author": "vcad", "date": "2026-11-11",
                               "scale": "", "material": "", "revision": "A"}
            })),
    })
    .to_string();
    let d = CString::new(doc_json).unwrap();
    let r = CString::new(req).unwrap();
    let out = &args[2];
    if let Some(dir) = std::path::Path::new(out).parent() {
        let _ = std::fs::create_dir_all(dir);
    }
    let p = vcad_ffi::drafting::vcad_drawing_sheet_json(d.as_ptr(), r.as_ptr());
    if p.is_null() {
        panic!("drawing failed: {:?}", last_error());
    }
    let json = unsafe { CStr::from_ptr(p) }.to_string_lossy().to_string();
    std::fs::write(format!("{out}.json"), &json).unwrap();
    let v: serde_json::Value = serde_json::from_str(&json).unwrap();
    for (ext, fmt) in [("pdf", 1u32), ("dxf", 2u32)] {
        let o = CString::new(format!("{out}.{ext}")).unwrap();
        let n = vcad_ffi::drafting::vcad_drawing_export(d.as_ptr(), r.as_ptr(), o.as_ptr(), fmt);
        assert!(n >= 0, "{ext} export failed: {:?}", last_error());
    }
    println!(
        "{name}: {} {} · {} views · {} annotations · {} circles · {} visible / {} hidden edges",
        v["sheet_size"],
        v["main"]["scale_note"],
        v["views"].as_array().map_or(0, |a| a.len()),
        v["dimensions"].as_array().map_or(0, |a| a.len()),
        v["circles"],
        v["visible_edges"],
        v["hidden_edges"]
    );
}

fn last_error() -> Option<String> {
    let mut len = 0usize;
    let p = vcad_ffi::vcad_last_error(&mut len);
    (!p.is_null())
        .then(|| String::from_utf8_lossy(unsafe { std::slice::from_raw_parts(p, len) }).to_string())
}

//! Run the native app's static FEA (`vcad_ffi::fea::solve`, the same path the
//! Mac app's Analyze workspace calls) on a document from the command line.
//!
//! ```bash
//! cargo run --release -p vcad-ffi --example fea_request -- part.vcad request.json
//! ```
//!
//! `request.json` is the `vcad_fea_solve` request: `{"part_index", "material"?,
//! "elastic"?, "resolution"?, "levels"?, "loads": [...], "supports": [...]}`.
//! Prints the result as JSON, without the overlay field grid.

fn main() -> Result<(), Box<dyn std::error::Error>> {
    let mut args = std::env::args().skip(1);
    let doc_path = args
        .next()
        .expect("usage: fea_request <doc.vcad> <request.json>");
    let req_path = args
        .next()
        .expect("usage: fea_request <doc.vcad> <request.json>");
    let doc: vcad_ir::Document = serde_json::from_str(&std::fs::read_to_string(doc_path)?)?;
    let mut req: vcad_ffi::fea::SolveReq =
        serde_json::from_str(&std::fs::read_to_string(req_path)?)?;
    req.fields = false;
    match vcad_ffi::fea::solve(&doc, &req) {
        Ok(r) => println!("{}", serde_json::to_string_pretty(&r)?),
        Err(e) => {
            println!("{}", serde_json::json!({ "error": e }));
            std::process::exit(2);
        }
    }
    Ok(())
}

//! DXF (R12, AC1009) export of a projected view.
//!
//! This is the writer behind the web app's "Export DXF" (`exportProjectedViewToDxf`)
//! and the native app's File ▸ Export Drawing ▸ DXF — one implementation so the
//! two exports are byte-identical for the same view.

use std::io::Write;

use crate::types::{ProjectedView, Visibility};

/// Serialize a projected view as an R12 DXF: visible edges on layer `VISIBLE`
/// (CONTINUOUS), hidden edges on layer `HIDDEN` (HIDDEN linetype), one `LINE`
/// entity per edge, coordinates in view units (mm).
pub fn projected_view_to_dxf(view: &ProjectedView) -> Vec<u8> {
    let mut buffer = Vec::new();

    // Header
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "SECTION").unwrap();
    writeln!(buffer, "2").unwrap();
    writeln!(buffer, "HEADER").unwrap();
    writeln!(buffer, "9").unwrap();
    writeln!(buffer, "$ACADVER").unwrap();
    writeln!(buffer, "1").unwrap();
    writeln!(buffer, "AC1009").unwrap();
    writeln!(buffer, "9").unwrap();
    writeln!(buffer, "$INSUNITS").unwrap();
    writeln!(buffer, "70").unwrap();
    writeln!(buffer, "4").unwrap();
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "ENDSEC").unwrap();

    // Tables
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "SECTION").unwrap();
    writeln!(buffer, "2").unwrap();
    writeln!(buffer, "TABLES").unwrap();

    // Linetypes
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "TABLE").unwrap();
    writeln!(buffer, "2").unwrap();
    writeln!(buffer, "LTYPE").unwrap();
    writeln!(buffer, "70").unwrap();
    writeln!(buffer, "2").unwrap();

    // Continuous
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "LTYPE").unwrap();
    writeln!(buffer, "2").unwrap();
    writeln!(buffer, "CONTINUOUS").unwrap();
    writeln!(buffer, "70").unwrap();
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "3").unwrap();
    writeln!(buffer, "Solid line").unwrap();
    writeln!(buffer, "72").unwrap();
    writeln!(buffer, "65").unwrap();
    writeln!(buffer, "73").unwrap();
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "40").unwrap();
    writeln!(buffer, "0.0").unwrap();

    // Hidden
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "LTYPE").unwrap();
    writeln!(buffer, "2").unwrap();
    writeln!(buffer, "HIDDEN").unwrap();
    writeln!(buffer, "70").unwrap();
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "3").unwrap();
    writeln!(buffer, "Hidden line").unwrap();
    writeln!(buffer, "72").unwrap();
    writeln!(buffer, "65").unwrap();
    writeln!(buffer, "73").unwrap();
    writeln!(buffer, "2").unwrap();
    writeln!(buffer, "40").unwrap();
    writeln!(buffer, "9.525").unwrap();
    writeln!(buffer, "49").unwrap();
    writeln!(buffer, "6.35").unwrap();
    writeln!(buffer, "49").unwrap();
    writeln!(buffer, "-3.175").unwrap();
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "ENDTAB").unwrap();

    // Layers
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "TABLE").unwrap();
    writeln!(buffer, "2").unwrap();
    writeln!(buffer, "LAYER").unwrap();
    writeln!(buffer, "70").unwrap();
    writeln!(buffer, "2").unwrap();

    // VISIBLE layer
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "LAYER").unwrap();
    writeln!(buffer, "2").unwrap();
    writeln!(buffer, "VISIBLE").unwrap();
    writeln!(buffer, "70").unwrap();
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "62").unwrap();
    writeln!(buffer, "7").unwrap();
    writeln!(buffer, "6").unwrap();
    writeln!(buffer, "CONTINUOUS").unwrap();

    // HIDDEN layer
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "LAYER").unwrap();
    writeln!(buffer, "2").unwrap();
    writeln!(buffer, "HIDDEN").unwrap();
    writeln!(buffer, "70").unwrap();
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "62").unwrap();
    writeln!(buffer, "8").unwrap();
    writeln!(buffer, "6").unwrap();
    writeln!(buffer, "HIDDEN").unwrap();
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "ENDTAB").unwrap();

    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "ENDSEC").unwrap();

    // Entities
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "SECTION").unwrap();
    writeln!(buffer, "2").unwrap();
    writeln!(buffer, "ENTITIES").unwrap();

    for edge in &view.edges {
        let (layer, linetype) = match edge.visibility {
            Visibility::Visible => ("VISIBLE", "CONTINUOUS"),
            Visibility::Hidden => ("HIDDEN", "HIDDEN"),
        };

        writeln!(buffer, "0").unwrap();
        writeln!(buffer, "LINE").unwrap();
        writeln!(buffer, "8").unwrap();
        writeln!(buffer, "{}", layer).unwrap();
        writeln!(buffer, "6").unwrap();
        writeln!(buffer, "{}", linetype).unwrap();
        writeln!(buffer, "10").unwrap();
        writeln!(buffer, "{:.6}", edge.start.x).unwrap();
        writeln!(buffer, "20").unwrap();
        writeln!(buffer, "{:.6}", edge.start.y).unwrap();
        writeln!(buffer, "11").unwrap();
        writeln!(buffer, "{:.6}", edge.end.x).unwrap();
        writeln!(buffer, "21").unwrap();
        writeln!(buffer, "{:.6}", edge.end.y).unwrap();
    }

    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "ENDSEC").unwrap();

    // EOF
    writeln!(buffer, "0").unwrap();
    writeln!(buffer, "EOF").unwrap();

    buffer
}

/// Number of `LINE` entities [`projected_view_to_dxf`] writes for `view`.
pub fn dxf_entity_count(view: &ProjectedView) -> usize {
    view.edges.len()
}

//! DXF (R12, AC1009) export of a projected view.
//!
//! This is the writer behind the web app's "Export DXF" (`exportProjectedViewToDxf`)
//! and the native app's File ▸ Export Drawing ▸ DXF — one implementation so the
//! two exports are byte-identical for the same view.

use std::io::Write;

use crate::sheet::{DrawingSheet, LineClass};
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

/// R12 text escapes: `%%c` ⌀, `%%d` °, `%%p` ±; other non-ASCII → `?`.
fn dxf_text(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for ch in text.chars() {
        match ch {
            '⌀' | 'Ø' => out.push_str("%%c"),
            '°' => out.push_str("%%d"),
            '±' => out.push_str("%%p"),
            '×' => out.push('x'),
            c if (c as u32) < 128 => out.push(c),
            _ => out.push('?'),
        }
    }
    out
}

/// Serialize a whole composed sheet as an R12 DXF in **model** millimetres:
/// every coordinate is multiplied by `model_per_sheet` (the inverse of the
/// sheet scale), so projected views come out at true size — measure a 40 mm
/// edge in any CAD/CAM tool and it reads 40 — while the border, title block,
/// dimensions and text keep their sheet proportions, as a scaled model-space
/// drawing does. Pass `1.0` for a paper-space (sheet mm) DXF.
///
/// One layer per [`LineClass`] (VISIBLE, HIDDEN, CENTER, DIMENSION, …) with
/// matching linetypes; arcs are `ARC`/`CIRCLE`, arrowheads `SOLID`, text
/// `TEXT` with R12 alignment codes.
pub fn sheet_to_dxf(sheet: &DrawingSheet, model_per_sheet: f64) -> Vec<u8> {
    use std::fmt::Write as _;
    let k = if model_per_sheet.is_finite() && model_per_sheet > 0.0 {
        model_per_sheet
    } else {
        1.0
    };
    const CLASSES: [LineClass; 8] = [
        LineClass::Visible,
        LineClass::Hidden,
        LineClass::Section,
        LineClass::Hatch,
        LineClass::Dimension,
        LineClass::CuttingPlane,
        LineClass::Center,
        LineClass::Border,
    ];
    let ltype = |c: LineClass| match c {
        LineClass::Hidden => "HIDDEN",
        LineClass::Center => "CENTER",
        LineClass::CuttingPlane => "PHANTOM",
        _ => "CONTINUOUS",
    };
    let color = |c: LineClass| match c {
        LineClass::Visible | LineClass::Section | LineClass::Border => 7,
        LineClass::Hidden => 8,
        LineClass::Center => 4,
        LineClass::CuttingPlane => 1,
        LineClass::Hatch => 9,
        LineClass::Dimension => 3,
    };
    let mut s = String::new();
    let mut g = |code: i32, v: &str| {
        let _ = writeln!(s, "{code}\n{v}");
    };
    g(0, "SECTION");
    g(2, "HEADER");
    g(9, "$ACADVER");
    g(1, "AC1009");
    g(9, "$INSUNITS");
    g(70, "4");
    g(9, "$LTSCALE");
    g(40, &format!("{k:.6}"));
    g(0, "ENDSEC");
    g(0, "SECTION");
    g(2, "TABLES");
    g(0, "TABLE");
    g(2, "LTYPE");
    g(70, "4");
    // name, description, pattern (sheet mm; $LTSCALE scales to model)
    let ltypes: [(&str, &str, &[f64]); 4] = [
        ("CONTINUOUS", "Solid line", &[]),
        ("HIDDEN", "Hidden __ __ __", &[3.0, -1.5]),
        ("CENTER", "Center ____ _ ____", &[6.0, -1.5, 1.0, -1.5]),
        (
            "PHANTOM",
            "Phantom ____ _ _ ____",
            &[8.0, -1.5, 1.5, -1.5, 1.5, -1.5],
        ),
    ];
    for (name, desc, pat) in ltypes {
        g(0, "LTYPE");
        g(2, name);
        g(70, "0");
        g(3, desc);
        g(72, "65");
        g(73, &pat.len().to_string());
        g(
            40,
            &format!("{:.4}", pat.iter().map(|v| v.abs()).sum::<f64>()),
        );
        for v in pat {
            g(49, &format!("{v:.4}"));
        }
    }
    g(0, "ENDTAB");
    g(0, "TABLE");
    g(2, "LAYER");
    g(70, &CLASSES.len().to_string());
    for c in CLASSES {
        g(0, "LAYER");
        g(2, c.dxf_layer());
        g(70, "0");
        g(62, &color(c).to_string());
        g(6, ltype(c));
    }
    g(0, "ENDTAB");
    g(0, "ENDSEC");

    g(0, "SECTION");
    g(2, "ENTITIES");
    let f = |v: f64| format!("{:.6}", v * k);
    for l in &sheet.lines {
        g(0, "LINE");
        g(8, l.class.dxf_layer());
        g(10, &f(l.start.x));
        g(20, &f(l.start.y));
        g(11, &f(l.end.x));
        g(21, &f(l.end.y));
    }
    for a in &sheet.arcs {
        let arc = &a.arc;
        let full = (arc.end_angle - arc.start_angle).abs() >= std::f64::consts::TAU - 1e-9;
        g(0, if full { "CIRCLE" } else { "ARC" });
        g(8, a.class.dxf_layer());
        g(10, &f(arc.center.x));
        g(20, &f(arc.center.y));
        g(40, &f(arc.radius));
        if !full {
            // DXF arcs run CCW from start to end.
            let (s0, s1) = if arc.end_angle >= arc.start_angle {
                (arc.start_angle, arc.end_angle)
            } else {
                (arc.end_angle, arc.start_angle)
            };
            g(50, &format!("{:.6}", s0.to_degrees()));
            g(51, &format!("{:.6}", s1.to_degrees()));
        }
    }
    for p in &sheet.polygons {
        if p.points.len() < 3 {
            continue;
        }
        let pts = [p.points[0], p.points[1], p.points[2], p.points[2]];
        g(0, "SOLID");
        g(8, p.class.dxf_layer());
        for (i, q) in pts.iter().enumerate() {
            g(10 + i as i32, &f(q.x));
            g(20 + i as i32, &f(q.y));
        }
    }
    for t in &sheet.texts {
        g(0, "TEXT");
        g(8, "DIMENSION");
        g(10, &f(t.position.x));
        g(20, &f(t.position.y));
        g(40, &f(t.height));
        g(1, &dxf_text(&t.text));
        if t.rotation != 0.0 {
            g(50, &format!("{:.6}", t.rotation.to_degrees()));
        }
        let (h, v) = (t.alignment.dxf_horizontal(), t.alignment.dxf_vertical());
        if h != 0 || v != 0 {
            g(72, &h.to_string());
            g(11, &f(t.position.x));
            g(21, &f(t.position.y));
            g(73, &v.to_string());
        }
    }
    g(0, "ENDSEC");
    g(0, "EOF");
    s.into_bytes()
}

#[cfg(test)]
mod sheet_dxf_tests {
    use super::*;
    use crate::sheet::SheetSize;

    #[test]
    fn sheet_dxf_scales_to_model_mm() {
        let mut sheet = DrawingSheet::new(SheetSize::A4);
        sheet.texts.push(crate::RenderedText::new(
            crate::Point2D::new(10.0, 10.0),
            "4× ⌀6",
            3.5,
        ));
        let dxf = String::from_utf8(sheet_to_dxf(&sheet, 2.0)).unwrap();
        // A4 border corner at (287, 200) sheet → (574, 400) model.
        assert!(dxf.contains("574.000000"), "border scaled");
        assert!(dxf.contains("4x %%c6"));
        assert!(dxf.contains("BORDER") && dxf.contains("CENTER"));
        assert!(dxf.trim_end().ends_with("EOF"));
    }
}

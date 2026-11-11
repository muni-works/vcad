//! Presentation colours: `STYLED_ITEM` → surface colour.
//!
//! Every CAD exporter we see (SolidWorks, Fusion, Onshape, FreeCAD/OCCT,
//! KiCad) writes colour the AP214 way:
//!
//! ```text
//! STYLED_ITEM('', (#psa), #item)            item = solid, shell or face
//! PRESENTATION_STYLE_ASSIGNMENT((#ssu, …))
//! SURFACE_STYLE_USAGE(.BOTH., #sss)
//! SURFACE_SIDE_STYLE('', (#fill, …))
//! SURFACE_STYLE_FILL_AREA(#fas)
//! FILL_AREA_STYLE('', (#fasc))
//! FILL_AREA_STYLE_COLOUR('', #c)
//! COLOUR_RGB('', r, g, b)  |  DRAUGHTING_PRE_DEFINED_COLOUR('red')
//! ```
//!
//! `OVER_RIDING_STYLED_ITEM` (a per-occurrence override) has the same first
//! three arguments. Curve styles (edge colours) are skipped: a black edge
//! colour is not the body colour.

use std::collections::{HashMap, HashSet};

use stepperoni::{StepFile, StepValue};

/// `item id → surface colour` (linear 0..1 RGB as written) for every styled
/// item in the file.
pub(crate) fn styled_colors(file: &StepFile) -> HashMap<u64, [f64; 3]> {
    let mut out = HashMap::new();
    for ty in ["STYLED_ITEM", "OVER_RIDING_STYLED_ITEM"] {
        let mut items: Vec<_> = file.entities_of_type(ty);
        // Overrides are applied after plain styles; within a type, file
        // order keeps the result deterministic.
        items.sort_by_key(|e| e.id);
        for e in items {
            let Some(StepValue::EntityRef(item)) = e.args.get(2) else {
                continue;
            };
            let Some(styles) = e.args.get(1) else {
                continue;
            };
            if let Some(c) = resolve(file, styles, 0, &mut HashSet::new()) {
                out.insert(*item, c);
            }
        }
    }
    out
}

/// Depth-first search from a style value to the first surface colour.
fn resolve(
    file: &StepFile,
    v: &StepValue,
    depth: usize,
    seen: &mut HashSet<u64>,
) -> Option<[f64; 3]> {
    if depth > 10 {
        return None;
    }
    match v {
        StepValue::List(xs) => xs.iter().find_map(|x| resolve(file, x, depth + 1, seen)),
        StepValue::Typed { args, .. } => {
            args.iter().find_map(|x| resolve(file, x, depth + 1, seen))
        }
        StepValue::EntityRef(id) => {
            if !seen.insert(*id) {
                return None;
            }
            let e = file.get(*id)?;
            match e.type_name.as_str() {
                "COLOUR_RGB" => {
                    let c: Vec<f64> = e.args.iter().filter_map(|a| a.as_real()).collect();
                    (c.len() >= 3).then(|| [c[c.len() - 3], c[c.len() - 2], c[c.len() - 1]])
                }
                "DRAUGHTING_PRE_DEFINED_COLOUR" => {
                    e.args.first().and_then(|a| a.as_string()).and_then(named)
                }
                // Edge/curve presentation: not the body colour.
                "CURVE_STYLE" | "POINT_STYLE" | "DRAUGHTING_PRE_DEFINED_CURVE_FONT" => None,
                // Transparency etc. carry no colour; everything else is a
                // container to descend through.
                _ => e
                    .args
                    .iter()
                    .find_map(|a| resolve(file, a, depth + 1, seen)),
            }
        }
        _ => None,
    }
}

/// AP214's predefined colour names.
fn named(n: &str) -> Option<[f64; 3]> {
    Some(match n.to_ascii_lowercase().as_str() {
        "black" => [0.0, 0.0, 0.0],
        "red" => [1.0, 0.0, 0.0],
        "green" => [0.0, 1.0, 0.0],
        "blue" => [0.0, 0.0, 1.0],
        "yellow" => [1.0, 1.0, 0.0],
        "magenta" => [1.0, 0.0, 1.0],
        "cyan" => [0.0, 1.0, 1.0],
        "white" => [1.0, 1.0, 1.0],
        _ => return None,
    })
}

/// One colour for a solid: its own style, else its shell's, else the colour
/// covering the most of its faces (KiCad and OCCT style faces, not solids).
pub(crate) fn solid_color(
    colors: &HashMap<u64, [f64; 3]>,
    solid_id: u64,
    shell_ids: &[u64],
    face_ids: &[u64],
) -> Option<[f64; 3]> {
    if let Some(c) = colors.get(&solid_id) {
        return Some(*c);
    }
    if let Some(c) = shell_ids.iter().find_map(|s| colors.get(s)) {
        return Some(*c);
    }
    let mut votes: Vec<([f64; 3], usize)> = Vec::new();
    for c in face_ids.iter().filter_map(|f| colors.get(f)) {
        match votes.iter_mut().find(|(v, _)| v == c) {
            Some((_, n)) => *n += 1,
            None => votes.push((*c, 1)),
        }
    }
    votes.into_iter().max_by_key(|(_, n)| *n).map(|(c, _)| c)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn parse(body: &str) -> StepFile {
        let src = format!(
            "ISO-10303-21;\nHEADER;\nFILE_DESCRIPTION((''),'2;1');\nFILE_NAME('','',(''),(''),'','','');\nFILE_SCHEMA(('AUTOMOTIVE_DESIGN'));\nENDSEC;\nDATA;\n{body}\nENDSEC;\nEND-ISO-10303-21;\n"
        );
        stepperoni::Parser::parse(src.as_bytes()).unwrap()
    }

    #[test]
    fn rgb_chain_resolves_and_skips_curve_style() {
        let f = parse(
            "#1=COLOUR_RGB('',0.1,0.2,0.3);\n\
             #2=FILL_AREA_STYLE_COLOUR('',#1);\n\
             #3=FILL_AREA_STYLE('',(#2));\n\
             #4=SURFACE_STYLE_FILL_AREA(#3);\n\
             #5=SURFACE_SIDE_STYLE('',(#4));\n\
             #6=SURFACE_STYLE_USAGE(.BOTH.,#5);\n\
             #7=COLOUR_RGB('',0.,0.,0.);\n\
             #8=CURVE_STYLE('',#9,POSITIVE_LENGTH_MEASURE(0.1),#7);\n\
             #9=DRAUGHTING_PRE_DEFINED_CURVE_FONT('continuous');\n\
             #10=PRESENTATION_STYLE_ASSIGNMENT((#8,#6));\n\
             #11=STYLED_ITEM('color',(#10),#42);\n\
             #12=DRAUGHTING_PRE_DEFINED_COLOUR('red');\n\
             #13=PRESENTATION_STYLE_ASSIGNMENT((#12));\n\
             #14=OVER_RIDING_STYLED_ITEM('',(#13),#43,#11);",
        );
        let c = styled_colors(&f);
        assert_eq!(c[&42], [0.1, 0.2, 0.3]);
        assert_eq!(c[&43], [1.0, 0.0, 0.0]);
    }

    #[test]
    fn solid_color_falls_back_to_face_majority() {
        let mut c = HashMap::new();
        c.insert(5, [1.0, 0.0, 0.0]);
        c.insert(6, [0.0, 1.0, 0.0]);
        c.insert(7, [0.0, 1.0, 0.0]);
        assert_eq!(
            solid_color(&c, 1, &[2], &[5, 6, 7, 8]),
            Some([0.0, 1.0, 0.0])
        );
        c.insert(2, [0.0, 0.0, 1.0]);
        assert_eq!(solid_color(&c, 1, &[2], &[5, 6, 7]), Some([0.0, 0.0, 1.0]));
        assert_eq!(solid_color(&HashMap::new(), 1, &[], &[]), None);
    }
}

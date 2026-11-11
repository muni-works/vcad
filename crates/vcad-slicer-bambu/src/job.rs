//! Part → printable `.gcode.3mf`: centre on the plate, slice, generate,
//! lint, printcheck, package. Pure (no network). Shared by `vcad print` and
//! the Mac app so both send exactly the same, equally-checked bytes.

use vcad_kernel_tessellate::TriangleMesh;
use vcad_slicer::{PrintStats, SliceSettings};
use vcad_slicer_gcode::{GcodeSettings, LintLimits, LintReport, PrinterProfile};

use crate::threemf::{PrintSettings, ThreeMfModel};

/// What to make.
#[derive(Debug, Clone)]
pub struct JobSpec {
    /// Model name written into the 3MF.
    pub name: String,
    /// Slicer settings.
    pub slice: SliceSettings,
    /// G-code settings (printer, temperatures, fan).
    pub gcode: GcodeSettings,
    /// Filament type (`PLA`, `PETG`, …) written to `slice_info` and matched
    /// against the printer by the gate.
    pub filament_type: String,
    /// Bambu model id the job is sliced for (`C12` = P1S). `None` for
    /// printers with no verified id: the file still exports, but the print
    /// gate will refuse it.
    pub printer_model_id: Option<String>,
}

/// A prepared, checked job.
pub struct Job {
    /// The `.gcode.3mf` bytes.
    pub archive: Vec<u8>,
    /// The plain G-code (also embedded in the archive).
    pub gcode: String,
    /// Linter verdict on the G-code (always clean in a returned Job).
    pub lint: LintReport,
    /// printcheck verdict on the centred part.
    pub printcheck: vcad_printcheck::Report,
    /// Slicer statistics (time, filament, layers, bounds).
    pub stats: PrintStats,
    /// Layer count.
    pub layers: usize,
}

/// Translate in place so the XY bbox centre sits on the plate centre and
/// the lowest point is at Z = 0. Refuses parts that don't fit (10 mm margin
/// each side keeps clear of the purge line).
pub fn center_on_plate(vertices: &mut [f32], printer: &PrinterProfile) -> Result<(), String> {
    if vertices.len() < 9 {
        return Err("empty mesh".into());
    }
    let mut lo = [f32::INFINITY; 3];
    let mut hi = [f32::NEG_INFINITY; 3];
    for v in vertices.as_chunks::<3>().0 {
        for k in 0..3 {
            lo[k] = lo[k].min(v[k]);
            hi[k] = hi[k].max(v[k]);
        }
    }
    let size = [hi[0] - lo[0], hi[1] - lo[1], hi[2] - lo[2]];
    let (bx, by, bz) = (
        printer.bed_x as f32,
        printer.bed_y as f32,
        printer.bed_z as f32,
    );
    if size[0] > bx - 20.0 || size[1] > by - 20.0 || size[2] > bz {
        return Err(format!(
            "part is {:.1} × {:.1} × {:.1} mm; the {} fits {:.0} × {:.0} × {:.0}",
            size[0],
            size[1],
            size[2],
            printer.name,
            bx - 20.0,
            by - 20.0,
            bz
        ));
    }
    let d = [
        bx / 2.0 - (lo[0] + hi[0]) / 2.0,
        by / 2.0 - (lo[1] + hi[1]) / 2.0,
        -lo[2],
    ];
    for v in vertices.as_chunks_mut::<3>().0 {
        for k in 0..3 {
            v[k] += d[k];
        }
    }
    Ok(())
}

fn printcheck(name: &str, vertices: &[f32], indices: &[u32]) -> vcad_printcheck::Report {
    let p = |i: u32| {
        let i = i as usize * 3;
        [
            vertices[i] as f64,
            vertices[i + 1] as f64,
            vertices[i + 2] as f64,
        ]
    };
    let mesh = vcad_printcheck::Mesh {
        tris: indices
            .as_chunks::<3>()
            .0
            .iter()
            .map(|t| [p(t[0]), p(t[1]), p(t[2])])
            .collect(),
    };
    vcad_printcheck::check_mesh(
        &mesh,
        name.to_string(),
        &vcad_printcheck::Options::default(),
    )
}

/// Centre, slice, generate, lint, printcheck and package one part.
pub fn prepare(mut vertices: Vec<f32>, indices: Vec<u32>, spec: &JobSpec) -> Result<Job, String> {
    center_on_plate(&mut vertices, &spec.gcode.printer)?;
    let printcheck = printcheck(&spec.name, &vertices, &indices);
    let mesh = TriangleMesh {
        vertices: vertices.clone(),
        indices: indices.clone(),
        normals: Vec::new(),
        face_kinds: Vec::new(),
        face_ids: Vec::new(),
    };
    let result = vcad_slicer::slice(&mesh, &spec.slice).map_err(|e| e.to_string())?;
    if result.layers.is_empty() {
        return Err("slicer produced no layers".into());
    }
    let limits = LintLimits::for_printer(&spec.gcode.printer);
    let gcode = vcad_slicer_gcode::generate_gcode(&result, spec.gcode.clone());
    let lint = vcad_slicer_gcode::lint(&gcode, &limits);
    if !lint.ok() {
        let shown: Vec<String> = lint
            .findings
            .iter()
            .take(10)
            .map(|f| format!("  line {}: {}", f.line, f.message))
            .collect();
        return Err(format!(
            "G-code linter refused ({} findings):\n{}",
            lint.findings.len(),
            shown.join("\n")
        ));
    }
    let mut model = ThreeMfModel::new(spec.name.clone(), vertices, indices)
        .with_gcode(gcode.clone().into_bytes());
    if let Some(id) = &spec.printer_model_id {
        model = model.with_printer_model(id.clone());
    }
    model.settings = PrintSettings {
        layer_height: spec.slice.layer_height,
        first_layer_height: spec.slice.first_layer_height,
        wall_count: spec.slice.wall_count,
        infill_density: spec.slice.infill_density,
        print_temp: spec.gcode.print_temp,
        bed_temp: spec.gcode.bed_temp,
        filament_type: spec.filament_type.clone(),
    };
    Ok(Job {
        archive: model.to_bytes().map_err(|e| e.to_string())?,
        layers: result.layers.len(),
        stats: result.stats.clone(),
        gcode,
        lint,
        printcheck,
    })
}

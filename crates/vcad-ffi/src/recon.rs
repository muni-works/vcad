//! Experimental image to CAD (`vcad_recon_*`), through irys-recon.
//!
//! Photographs of one part standing beside a printed tag36h11 AprilTag go in;
//! a parametric `.vcad` document comes out: every dimension irys-recon fits
//! (`base.w`, `f0.r`, …) becomes a named document parameter, and every
//! geometry field that depends on it is *bound* to it by an expression, so
//! editing `base_w` in the parameters panel rebuilds the part. Not a mesh.
//!
//! Two calls, so the app can fail fast and show two stages of progress:
//!
//! - [`vcad_recon_check`]: decode every photo, read its EXIF focal length,
//!   find the tag and solve the camera (seconds). Fails with a plain,
//!   specific message — no tag, no EXIF focal length, unreadable file.
//! - [`vcad_recon_photos`]: the whole reconstruction (minutes, CPU-bound).
//!   Returns the document plus per-dimension ±σ (the fit's Fisher
//!   information, Cramér–Rao) and the directions the photos do not pin.
//!
//! The reconstruction also runs as a cancellable job
//! ([`vcad_recon_start`] → [`vcad_recon_poll`] / [`vcad_recon_cancel`] →
//! [`vcad_recon_finish`] → [`vcad_recon_job_free`]) on irys-recon's
//! [`fit::Hook`]: the job thread publishes stage/step/of, the app polls it,
//! and a cancel stops the fit within one Levenberg–Marquardt iteration with
//! the error `"cancelled"`.
//!
//! Evidence (stated in the app's copy too): synthetic photos only — three
//! views, 15/20 parts within 1 mm, 10/20 within 0.5 mm. No real-photo test.

use std::ffi::c_char;
use std::path::Path;
use std::ptr;

use irys_recon::dsl::{Axis, Corner, Feature, Program, Unit};
use irys_recon::{fit, tag, v1};
use serde::{Deserialize, Serialize};
use std::panic::AssertUnwindSafe;
use std::sync::{Arc, Mutex, MutexGuard};
use std::thread::JoinHandle;
use vcad_ir::{
    BindingKey, CsgOp, Document, Expr, MaterialDef, Node, NodeId, Parameter, SceneEntry, Vec3,
};

use crate::err;
use crate::json::{borrow_str, guard_ptr, into_c, json_out};

/// Cylinder segments for the reconstructed features (irys-recon's own).
const SEGMENTS: u32 = 48;

/// A dimension whose ±σ exceeds this (mm) is one the photos do not pin; the
/// result is then flagged low-confidence (irys-recon's `UNIDENTIFIABLE_MM`).
pub const LOW_CONFIDENCE_MM: f64 = 0.5;

/// What the app asks for.
#[derive(Debug, Deserialize)]
pub struct Request {
    /// Photo paths (JPEG/PNG), one part, the tag in every frame.
    pub photos: Vec<String>,
    /// The printed tag's black square edge, mm (the kit prints 50 mm).
    #[serde(default = "default_tag_mm")]
    pub tag_mm: f64,
    /// Sensor width, mm: only needed when the EXIF has `FocalLength` but no
    /// 35 mm equivalent.
    #[serde(default)]
    pub sensor_w_mm: Option<f64>,
}

fn default_tag_mm() -> f64 {
    tag::TAG_MM
}

/// One photo's calibration.
#[derive(Debug, Serialize)]
pub struct PhotoCheck {
    /// File name.
    pub name: String,
    /// Width, height as displayed.
    pub size: (usize, usize),
    /// The tag36h11 id found.
    pub tag_id: usize,
    /// Corner RMS of the camera solve, pixels.
    pub rms_px: f64,
    /// Focal length used, pixels.
    pub focal_px: f64,
    /// Focal length from EXIF, pixels.
    pub focal_exif_px: f64,
}

/// One fitted dimension.
#[derive(Debug, Serialize)]
pub struct Dimension {
    /// irys-recon's name (`base.w`, `f1.r`).
    pub name: String,
    /// The document parameter it became (`base_w`, `f1_r`).
    pub parameter: String,
    /// Fitted value.
    pub value: f64,
    /// ±σ from the Fisher information (Cramér–Rao; v0 measured these
    /// optimistic by a median 2.3×). `None` if the fit gave none.
    pub sigma: Option<f64>,
    /// `mm` or `deg`.
    pub unit: &'static str,
}

/// A combination of dimensions the photos do not determine.
#[derive(Debug, Serialize)]
pub struct Weak {
    /// ±σ along the direction, mm.
    pub sigma: f64,
    /// The dimensions it mixes, with weights.
    pub mix: Vec<(String, f64)>,
}

/// The reconstruction.
#[derive(Debug, Serialize)]
pub struct Outcome {
    /// The editable document.
    pub document: Document,
    /// irys-recon's program text (part frame).
    pub program: String,
    /// Every dimension, in program order.
    pub dimensions: Vec<Dimension>,
    /// Undetermined directions (σ > 0.5 mm).
    pub weak: Vec<Weak>,
    /// Any dimension above [`LOW_CONFIDENCE_MM`], or any weak direction.
    pub low_confidence: bool,
    /// Plain-language reasons for `low_confidence`.
    pub warnings: Vec<String>,
    /// Per-photo calibration.
    pub photos: Vec<PhotoCheck>,
    /// Wall time, seconds.
    pub seconds: f64,
}

fn parse_request(json: &str) -> Result<Request, String> {
    let r: Request = serde_json::from_str(json).map_err(|e| format!("bad request: {e}"))?;
    if r.photos.is_empty() {
        return Err("Choose at least one photo.".into());
    }
    if r.tag_mm.is_nan() || r.tag_mm <= 0.0 {
        return Err(format!("Tag size must be positive (got {} mm).", r.tag_mm));
    }
    Ok(r)
}

/// The photo's name as the user picked it: HEIC is transcoded to a `.jpg`
/// beside the import cache, so show the stem, not the transcode's extension.
fn photo_label(name: &str) -> &str {
    Path::new(name)
        .file_stem()
        .and_then(|s| s.to_str())
        .unwrap_or(name)
}

fn load(path: &str) -> Result<v1::Photo, String> {
    v1::load(Path::new(path)).map_err(|e| format!("Couldn't read the photo: {e}"))
}

/// Calibrate one loaded photo, failing with the user-facing reason.
fn check_photo(p: &v1::Photo, r: &Request) -> Result<PhotoCheck, String> {
    let (w, h) = p.size;
    // Fail closed without an EXIF focal length: one tag barely constrains it
    // (focal trades against distance), and irys-recon's fallback guess makes
    // every dimension scale-uncertain.
    let Some(f_exif) = tag::exif_focal_px(&p.exif, w, h, r.sensor_w_mm) else {
        let why = if p.exif.f_mm.is_some() {
            "has a focal length but no 35 mm equivalent; enter the sensor width"
        } else {
            "has no EXIF focal length (screenshots, edited exports and some messaging apps strip it); use the camera's original file"
        };
        return Err(format!("{} {why}.", p.name));
    };
    let c = v1::calibrate(p, r.tag_mm, r.sensor_w_mm).map_err(|e| {
        if e.contains("no tag36h11") || e.contains("no AprilTag found") {
            format!(
                "No AprilTag found in {}. Lay a printed tag36h11 tag ({} mm, any id) flat beside the part, and keep it whole, sharp and in frame.",
                photo_label(&p.name), r.tag_mm
            )
        } else {
            format!("{}: couldn't solve the camera from the tag ({e}).", p.name)
        }
    })?;
    Ok(PhotoCheck {
        name: p.name.clone(),
        size: p.size,
        tag_id: c.tag.id,
        rms_px: c.rms,
        focal_px: c.k.f,
        focal_exif_px: f_exif,
    })
}

/// Load and calibrate every photo (the fast pre-flight).
pub fn check(r: &Request) -> Result<Vec<PhotoCheck>, String> {
    r.photos.iter().map(|p| check_photo(&load(p)?, r)).collect()
}

/// `base.w` → `base_w`: document parameters are identifiers.
pub fn parameter_name(name: &str) -> String {
    name.replace('.', "_")
}

/// Run the reconstruction end to end.
pub fn reconstruct(r: &Request) -> Result<Outcome, String> {
    reconstruct_with(r, fit::Hook::default())
}

/// [`reconstruct`] with a cancel/progress hook. A cancel returns
/// `Err(fit::CANCELLED)` verbatim.
pub fn reconstruct_with(r: &Request, hook: fit::Hook) -> Result<Outcome, String> {
    let t0 = std::time::Instant::now();
    hook.check()?;
    let photos: Vec<v1::Photo> = r.photos.iter().map(|p| load(p)).collect::<Result<_, _>>()?;
    let checks: Vec<PhotoCheck> = photos
        .iter()
        .map(|p| check_photo(p, r))
        .collect::<Result<_, _>>()?;
    let mut o = v1::V1Options {
        tag_mm: r.tag_mm,
        sensor_w_mm: r.sensor_w_mm,
        hook,
        ..v1::V1Options::default()
    };
    o.hook.check()?;
    o.search.workers = std::thread::available_parallelism()
        .map(|n| n.get().clamp(1, 8))
        .unwrap_or(4);
    let v = v1::recover_photos(&photos, &o).map_err(|e| {
        if e == fit::CANCELLED {
            e
        } else if e.contains("no tag36h11") || e.contains("no AprilTag found") {
            format!("No AprilTag found: {e}.")
        } else {
            format!("The reconstruction failed: {e}.")
        }
    })?;
    let mut part = v.rec.program.clone();
    part.pose = None; // the part frame: centred, standing on z = 0
    let sigma: std::collections::HashMap<&str, f64> = v
        .rec
        .judge
        .sigma
        .iter()
        .map(|(n, s)| (n.as_str(), *s))
        .collect();
    let dimensions: Vec<Dimension> = part
        .param_info()
        .iter()
        .zip(part.params())
        .map(|(i, x)| Dimension {
            parameter: parameter_name(&i.name),
            name: i.name.clone(),
            value: x,
            sigma: sigma
                .get(i.name.as_str())
                .copied()
                .filter(|s| s.is_finite()),
            unit: if i.unit == Unit::Deg { "deg" } else { "mm" },
        })
        .collect();
    let weak: Vec<Weak> = v
        .rec
        .judge
        .weak
        .iter()
        .map(|(s, mix)| Weak {
            sigma: *s,
            mix: mix.clone(),
        })
        .collect();
    let mut warnings = Vec::new();
    for d in &dimensions {
        match d.sigma {
            Some(s) if d.unit == "mm" && s > LOW_CONFIDENCE_MM => warnings.push(format!(
                "{} is uncertain: ±{s:.2} mm from the fit (and the fit's σ has run optimistic).",
                d.parameter
            )),
            None => warnings.push(format!("{} has no uncertainty estimate.", d.parameter)),
            _ => {}
        }
    }
    for w in &weak {
        let names: Vec<String> = w.mix.iter().map(|(n, _)| parameter_name(n)).collect();
        warnings.push(format!(
            "The photos don't pin {} (±{:.2} mm together); another angle would help.",
            names.join(" + "),
            w.sigma
        ));
    }
    let look = v.looks.first().copied();
    let document = program_document(
        &part,
        look.map_or([0.8, 0.8, 0.8], |l| l.rgb),
        look.map_or(0.5, |l| l.rough),
        &dimensions,
    );
    Ok(Outcome {
        document,
        program: part.to_text(),
        low_confidence: !warnings.is_empty(),
        dimensions,
        weak,
        warnings,
        photos: checks,
        seconds: t0.elapsed().as_secs_f64(),
    })
}

// ============================================================================
// Program → parametric document
// ============================================================================

struct Builder {
    doc: Document,
    next: NodeId,
}

/// A literal value and the formula that keeps it live.
type Field = (f64, String);

fn lit(v: f64, e: impl Into<String>) -> Field {
    (v, e.into())
}

impl Builder {
    fn add(&mut self, name: &str, op: CsgOp) -> NodeId {
        let id = self.next;
        self.next += 1;
        self.doc.nodes.insert(
            id,
            Node {
                id,
                name: Some(name.into()),
                op,
            },
        );
        id
    }
    fn bind(&mut self, id: NodeId, path: &str, f: &Field) {
        self.doc
            .bindings
            .bind(BindingKey::new(id, path), Expr::formula(f.1.clone()));
    }
    fn bind3(&mut self, id: NodeId, prefix: &str, v: [&Field; 3]) {
        for (c, f) in ["x", "y", "z"].iter().zip(v) {
            self.bind(id, &format!("{prefix}.{c}"), f);
        }
    }
    fn translate(&mut self, name: &str, child: NodeId, o: [&Field; 3]) -> NodeId {
        let id = self.add(
            name,
            CsgOp::Translate {
                child,
                offset: Vec3::new(o[0].0, o[1].0, o[2].0),
            },
        );
        self.bind3(id, "offset", o);
        id
    }
    fn cube(&mut self, name: &str, s: [&Field; 3]) -> NodeId {
        let id = self.add(
            name,
            CsgOp::Cube {
                size: Vec3::new(s[0].0, s[1].0, s[2].0),
            },
        );
        self.bind3(id, "size", s);
        id
    }
    /// A Z-axis cylinder standing at `(x, y, z)`.
    fn cyl(&mut self, name: &str, r: &Field, h: &Field, at: [&Field; 3]) -> NodeId {
        let c = self.add(
            name,
            CsgOp::Cylinder {
                radius: r.0,
                height: h.0,
                segments: SEGMENTS,
            },
        );
        self.bind(c, "radius", r);
        self.bind(c, "height", h);
        self.translate(&format!("{name} at"), c, at)
    }
    fn cut(&mut self, name: &str, left: NodeId, right: NodeId) -> NodeId {
        self.add(name, CsgOp::Difference { left, right })
    }
}

/// The program as a parametric document, part frame (centred in x/y,
/// standing on z = 0, Z-up, mm). Parameters carry the fit's ±σ in their
/// description; geometry fields are bound to them by expressions.
pub fn program_document(
    prog: &Program,
    rgb: [f64; 3],
    roughness: f64,
    dims: &[Dimension],
) -> Document {
    let mut b = Builder {
        doc: Document::new(),
        next: 1,
    };
    let info = prog.param_info();
    for (i, x) in info.iter().zip(prog.params()) {
        let p = parameter_name(&i.name);
        let sig = dims.iter().find(|d| d.name == i.name).and_then(|d| d.sigma);
        let unit = if i.unit == Unit::Deg { "deg" } else { "mm" };
        b.doc.parameters.insert(
            p,
            Parameter {
                value: Expr::Number(x),
                unit: Some(unit.into()),
                min: Some(i.lo),
                max: Some(i.hi),
                description: Some(match sig {
                    Some(s) => format!(
                        "From photos (experimental): ±{s:.2} {unit} fit σ, likely optimistic"
                    ),
                    None => "From photos (experimental)".into(),
                }),
            },
        );
    }
    let (w, d, h) = (prog.w, prog.d, prog.h);
    let (bw, bd, bh) = (lit(w, "base_w"), lit(d, "base_d"), lit(h, "base_h"));
    let zero = lit(0.0, "0");
    let below = lit(-1.0, "-1");
    let through = lit(h + 2.0, "base_h + 2");
    let quads = [
        (1.0, 1.0, "+", "+"),
        (-1.0, 1.0, "-", "+"),
        (-1.0, -1.0, "-", "-"),
        (1.0, -1.0, "+", "-"),
    ];
    let plate = |b: &mut Builder| {
        let base = b.cube("base block", [&bw, &bd, &bh]);
        b.translate(
            "base",
            base,
            [
                &lit(-w / 2.0, "-base_w / 2"),
                &lit(-d / 2.0, "-base_d / 2"),
                &zero,
            ],
        )
    };
    let mut cur;
    match prog.corner {
        Corner::Sharp => cur = plate(&mut b),
        Corner::Fillet(r) => {
            // The rounded plate as a union: two crossed blocks and a cylinder
            // in each corner (exact, and every size stays bound). Cutting a
            // block-minus-cylinder from each corner instead puts the cylinder
            // axis on the block's edge, which the boolean mishandles.
            let narrow = b.cube(
                "base block (x)",
                [&bw, &lit(d - 2.0 * r, "base_d - 2 * base_fillet"), &bh],
            );
            let narrow = b.translate(
                "base block (x) at",
                narrow,
                [
                    &lit(-w / 2.0, "-base_w / 2"),
                    &lit(-d / 2.0 + r, "-base_d / 2 + base_fillet"),
                    &zero,
                ],
            );
            let wide = b.cube(
                "base block (y)",
                [&lit(w - 2.0 * r, "base_w - 2 * base_fillet"), &bd, &bh],
            );
            let wide = b.translate(
                "base block (y) at",
                wide,
                [
                    &lit(-w / 2.0 + r, "-base_w / 2 + base_fillet"),
                    &lit(-d / 2.0, "-base_d / 2"),
                    &zero,
                ],
            );
            cur = b.add(
                "base cross",
                CsgOp::Union {
                    left: narrow,
                    right: wide,
                },
            );
            for (k, &(sx, sy, xs, ys)) in quads.iter().enumerate() {
                let cx = lit(
                    sx * (w / 2.0 - r),
                    format!("{xs}(base_w / 2 - base_fillet)"),
                );
                let cy = lit(
                    sy * (d / 2.0 - r),
                    format!("{ys}(base_d / 2 - base_fillet)"),
                );
                let round = b.cyl(
                    &format!("fillet {}", k + 1),
                    &lit(r, "base_fillet"),
                    &bh,
                    [&cx, &cy, &zero],
                );
                let name = if k == 3 {
                    "base".to_string()
                } else {
                    format!("base + fillet {}", k + 1)
                };
                cur = b.add(
                    &name,
                    CsgOp::Union {
                        left: cur,
                        right: round,
                    },
                );
            }
        }
        Corner::Chamfer(c) => {
            cur = plate(&mut b);
            // Each corner: a square of side c√2 turned 45° about the corner
            // removes the triangle with legs c.
            let s = c * std::f64::consts::SQRT_2;
            for (k, &(sx, sy, xs, ys)) in quads.iter().enumerate() {
                let n = format!("chamfer {}", k + 1);
                let side = lit(s, "base_chamfer * sqrt(2)");
                let sq = b.cube(&format!("{n} square"), [&side, &side, &through]);
                let half = lit(-s / 2.0, "-base_chamfer * sqrt(2) / 2");
                let sq = b.translate(&format!("{n} centred"), sq, [&half, &half, &below]);
                let rot = b.add(
                    &format!("{n} turned"),
                    CsgOp::Rotate {
                        child: sq,
                        angles: Vec3::new(0.0, 0.0, 45.0),
                    },
                );
                let at = b.translate(
                    &format!("{n} cutter"),
                    rot,
                    [
                        &lit(sx * w / 2.0, format!("{xs}base_w / 2")),
                        &lit(sy * d / 2.0, format!("{ys}base_d / 2")),
                        &zero,
                    ],
                );
                cur = b.cut(&n, cur, at);
            }
        }
    }
    for (k, f) in prog.features.iter().enumerate() {
        let p = |n: &str| format!("f{k}_{n}");
        let label = format!("f{k} {}", f.kind().replace('_', " "));
        cur = match *f {
            Feature::Hole { x, y, r } => {
                let t = b.cyl(
                    &format!("{label} tool"),
                    &lit(r, p("r")),
                    &through,
                    [&lit(x, p("x")), &lit(y, p("y")), &below],
                );
                b.cut(&label, cur, t)
            }
            Feature::Counterbore { x, y, r, cr, cd } => {
                let (fx, fy) = (lit(x, p("x")), lit(y, p("y")));
                let t = b.cyl(
                    &format!("{label} hole"),
                    &lit(r, p("r")),
                    &through,
                    [&fx, &fy, &below],
                );
                let c = b.cut(&format!("{label} hole cut"), cur, t);
                let bore = b.cyl(
                    &format!("{label} bore"),
                    &lit(cr, p("cr")),
                    &lit(cd + 1.0, format!("{} + 1", p("cd"))),
                    [&fx, &fy, &lit(h - cd, format!("base_h - {}", p("cd")))],
                );
                b.cut(&label, c, bore)
            }
            Feature::Slot { x, y, len, r, axis } => {
                let along_x = axis == Axis::X;
                let (l, two_r) = (lit(len, p("len")), lit(2.0 * r, format!("2 * {}", p("r"))));
                let size = if along_x {
                    [&l, &two_r, &through]
                } else {
                    [&two_r, &l, &through]
                };
                let bar = b.cube(&format!("{label} bar"), size);
                let bx = if along_x {
                    lit(x - len / 2.0, format!("{} - {} / 2", p("x"), p("len")))
                } else {
                    lit(x - r, format!("{} - {}", p("x"), p("r")))
                };
                let by = if along_x {
                    lit(y - r, format!("{} - {}", p("y"), p("r")))
                } else {
                    lit(y - len / 2.0, format!("{} - {} / 2", p("y"), p("len")))
                };
                let bar = b.translate(&format!("{label} bar at"), bar, [&bx, &by, &below]);
                let mut c = b.cut(&format!("{label} bar cut"), cur, bar);
                for (e, sgn, op) in [(1, -1.0, "-"), (2, 1.0, "+")] {
                    let (ex, ey) = if along_x {
                        (
                            lit(
                                x + sgn * len / 2.0,
                                format!("{} {op} {} / 2", p("x"), p("len")),
                            ),
                            lit(y, p("y")),
                        )
                    } else {
                        (
                            lit(x, p("x")),
                            lit(
                                y + sgn * len / 2.0,
                                format!("{} {op} {} / 2", p("y"), p("len")),
                            ),
                        )
                    };
                    let end = b.cyl(
                        &format!("{label} end {e}"),
                        &lit(r, p("r")),
                        &through,
                        [&ex, &ey, &below],
                    );
                    c = b.cut(
                        &if e == 2 {
                            label.clone()
                        } else {
                            format!("{label} end {e} cut")
                        },
                        c,
                        end,
                    );
                }
                c
            }
            Feature::Boss { x, y, r, bh } => {
                let boss = b.cyl(
                    &format!("{label} cylinder"),
                    &lit(r, p("r")),
                    &lit(bh + h / 2.0, format!("{} + base_h / 2", p("bh"))),
                    [
                        &lit(x, p("x")),
                        &lit(y, p("y")),
                        &lit(h / 2.0, "base_h / 2"),
                    ],
                );
                b.add(
                    &label,
                    CsgOp::Union {
                        left: cur,
                        right: boss,
                    },
                )
            }
            Feature::LinearHoles { .. } | Feature::CircularHoles { .. } => {
                let mut c = cur;
                let holes = f.holes();
                let n = holes.len();
                for (i, &(hx, hy, hr)) in holes.iter().enumerate() {
                    let (ex, ey) = match f {
                        Feature::LinearHoles { .. } => (
                            format!("{} + {i} * {}", p("x"), p("dx")),
                            format!("{} + {i} * {}", p("y"), p("dy")),
                        ),
                        _ => {
                            let a = format!(
                                "({} + {}) * pi / 180",
                                p("phase"),
                                360.0 * i as f64 / n as f64
                            );
                            (
                                format!("{} + {} * cos({a})", p("x"), p("rad")),
                                format!("{} + {} * sin({a})", p("y"), p("rad")),
                            )
                        }
                    };
                    let t = b.cyl(
                        &format!("{label} {} tool", i + 1),
                        &lit(hr, p("r")),
                        &through,
                        [&lit(hx, ex), &lit(hy, ey), &below],
                    );
                    c = b.cut(
                        &if i + 1 == n {
                            label.clone()
                        } else {
                            format!("{label} {}", i + 1)
                        },
                        c,
                        t,
                    );
                }
                c
            }
        };
    }
    if let Some(node) = b.doc.nodes.get_mut(&cur) {
        node.name = Some("Part from photos".into());
    }
    let mat = MaterialDef {
        name: "PLA (from photos)".into(),
        color: rgb.map(|c| c.clamp(0.0, 1.0)),
        metallic: 0.0,
        roughness: roughness.clamp(0.0, 1.0),
        ..MaterialDef::default()
    };
    b.doc.materials.insert("pla".into(), mat);
    b.doc.roots.push(SceneEntry {
        root: cur,
        material: "pla".into(),
        visible: None,
    });
    b.doc
}

// ============================================================================
// C ABI
// ============================================================================

fn entry<T: Serialize>(
    what: &str,
    req: *const c_char,
    f: impl FnOnce(&Request) -> Result<T, String>,
) -> *mut c_char {
    err::clear_error();
    let Some(s) = borrow_str("recon request", req) else {
        return ptr::null_mut();
    };
    guard_ptr(what, || match parse_request(s).and_then(|r| f(&r)) {
        Ok(v) => json_out(&v),
        Err(e) => {
            err::set_error(e);
            ptr::null_mut()
        }
    })
}

/// Pre-flight for image to CAD: decode each photo, read its EXIF focal
/// length and find the tag36h11 tag. `request_json`:
/// `{"photos": [paths], "tag_mm": 50, "sensor_w_mm": null}`. Returns a JSON
/// array of per-photo calibrations, or null + `vcad_last_error` holding a
/// plain, user-facing reason. Free with `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_recon_check(request_json: *const c_char) -> *mut c_char {
    entry("recon check", request_json, check)
}

/// Experimental image to CAD: reconstruct the part in the photos as an
/// editable, parametric document. Same request as [`vcad_recon_check`].
/// Blocks for minutes — call it off the main thread; it cannot be
/// interrupted. Returns `{"document", "program", "dimensions": [{name,
/// parameter, value, sigma, unit}], "weak", "low_confidence", "warnings",
/// "photos", "seconds"}` or null + `vcad_last_error`. Free with
/// `vcad_string_free`.
#[no_mangle]
pub extern "C" fn vcad_recon_photos(request_json: *const c_char) -> *mut c_char {
    entry("recon", request_json, reconstruct)
}

// ============================================================================
// Cancellable job
// ============================================================================

fn stage_code(s: fit::Stage) -> i32 {
    match s {
        fit::Stage::Calibrate => 1,
        fit::Stage::Appearance => 2,
        fit::Stage::Search => 3,
        fit::Stage::Finalists => 4,
        fit::Stage::Polish => 5,
    }
}

/// A snapshot of a running reconstruction, filled by [`vcad_recon_poll`].
#[repr(C)]
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct VcadReconProgress {
    /// 0 starting (loading and checking photos), 1 calibrate, 2 appearance,
    /// 3 search, 4 finalists, 5 polish.
    pub stage: i32,
    /// 1-based step within the stage (0 while starting).
    pub step: u32,
    /// Steps in the stage (for search, the most there can be).
    pub of: u32,
    /// 1 once the job has finished (result, error or cancel).
    pub done: u8,
    /// 1 once [`vcad_recon_cancel`] was called.
    pub cancelled: u8,
}

#[derive(Default)]
struct Shared {
    progress: VcadReconProgress,
    result: Option<Result<String, String>>,
}

/// A reconstruction running on its own thread. Opaque to C.
pub struct VcadReconJob {
    hook: fit::Hook,
    shared: Arc<Mutex<Shared>>,
    thread: Option<JoinHandle<()>>,
}

fn lock(m: &Mutex<Shared>) -> MutexGuard<'_, Shared> {
    m.lock().unwrap_or_else(|p| p.into_inner())
}

impl VcadReconJob {
    fn start(r: Request) -> Self {
        let shared = Arc::new(Mutex::new(Shared::default()));
        let sink = Arc::clone(&shared);
        let hook = fit::Hook::new(move |p: &fit::Progress| {
            let mut s = lock(&sink);
            s.progress.stage = stage_code(p.stage);
            s.progress.step = u32::try_from(p.step).unwrap_or(u32::MAX);
            s.progress.of = u32::try_from(p.of).unwrap_or(u32::MAX);
        });
        let job_hook = hook.clone();
        let out = Arc::clone(&shared);
        let spawned = std::thread::Builder::new()
            .name("vcad-recon".into())
            .spawn(move || {
                let res = std::panic::catch_unwind(AssertUnwindSafe(|| {
                    reconstruct_with(&r, job_hook)
                        .and_then(|o| serde_json::to_string(&o).map_err(|e| format!("recon: {e}")))
                }))
                .unwrap_or_else(|_| Err("recon: kernel panicked".into()));
                let mut s = lock(&out);
                s.result = Some(res);
                s.progress.done = 1;
            });
        let thread = match spawned {
            Ok(t) => Some(t),
            Err(e) => {
                let mut s = lock(&shared);
                s.result = Some(Err(format!("recon: couldn't start a thread: {e}")));
                s.progress.done = 1;
                None
            }
        };
        Self {
            hook,
            shared,
            thread,
        }
    }

    fn progress(&self) -> VcadReconProgress {
        let mut p = lock(&self.shared).progress;
        p.cancelled = u8::from(self.hook.cancelled());
        p
    }

    fn finish(&mut self) -> Result<String, String> {
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
        lock(&self.shared)
            .result
            .clone()
            .unwrap_or_else(|| Err("recon: no result".into()))
    }
}

impl Drop for VcadReconJob {
    fn drop(&mut self) {
        self.hook.cancel();
        if let Some(t) = self.thread.take() {
            let _ = t.join();
        }
    }
}

/// Start the reconstruction on a background thread. Same request as
/// [`vcad_recon_check`]. Returns a job handle, or null + `vcad_last_error`
/// for a malformed request. Free with [`vcad_recon_job_free`].
#[no_mangle]
pub extern "C" fn vcad_recon_start(request_json: *const c_char) -> *mut VcadReconJob {
    err::clear_error();
    let Some(s) = borrow_str("recon request", request_json) else {
        return ptr::null_mut();
    };
    guard_ptr("recon start", || match parse_request(s) {
        Ok(r) => Box::into_raw(Box::new(VcadReconJob::start(r))),
        Err(e) => {
            err::set_error(e);
            ptr::null_mut()
        }
    })
}

/// Copy the job's progress into `out`. Returns 1 when finished, 0 while
/// running, -1 for a null argument. Never blocks for long; any thread.
///
/// # Safety
/// `job` must come from [`vcad_recon_start`] and not be freed; `out` must be
/// valid for one write.
#[no_mangle]
pub unsafe extern "C" fn vcad_recon_poll(
    job: *const VcadReconJob,
    out: *mut VcadReconProgress,
) -> i32 {
    if job.is_null() || out.is_null() {
        return -1;
    }
    // SAFETY: caller contract above.
    let job = unsafe { &*job };
    let Ok(p) = std::panic::catch_unwind(AssertUnwindSafe(|| job.progress())) else {
        return -1;
    };
    // SAFETY: caller contract above.
    unsafe { out.write(p) };
    i32::from(p.done)
}

/// Ask the job to stop; returns at once. The fit stops within one
/// Levenberg–Marquardt iteration and [`vcad_recon_finish`] then fails with
/// `vcad_last_error` == `"cancelled"`. Null is a no-op; any thread.
///
/// # Safety
/// `job` must come from [`vcad_recon_start`] and not be freed.
#[no_mangle]
pub unsafe extern "C" fn vcad_recon_cancel(job: *const VcadReconJob) {
    if job.is_null() {
        return;
    }
    // SAFETY: caller contract above.
    unsafe { &*job }.hook.cancel();
}

/// Wait for the job and return its result: the [`vcad_recon_photos`] JSON,
/// or null + `vcad_last_error` (exactly `"cancelled"` after a cancel). Call
/// off the main thread; calling it again returns the same result. Free the
/// string with `vcad_string_free`.
///
/// # Safety
/// `job` must come from [`vcad_recon_start`] and not be freed, nor be passed
/// to `vcad_recon_finish` on two threads at once.
#[no_mangle]
pub unsafe extern "C" fn vcad_recon_finish(job: *mut VcadReconJob) -> *mut c_char {
    err::clear_error();
    if job.is_null() {
        err::set_error("recon finish: null job");
        return ptr::null_mut();
    }
    // SAFETY: caller contract above.
    let job = unsafe { &mut *job };
    guard_ptr("recon finish", || match job.finish() {
        Ok(s) => into_c(s),
        Err(e) => {
            err::set_error(e);
            ptr::null_mut()
        }
    })
}

/// Cancel (if still running), wait for the thread and free the job. Null is
/// a no-op.
///
/// # Safety
/// `job` must come from [`vcad_recon_start`] and not be used afterwards.
#[no_mangle]
pub unsafe extern "C" fn vcad_recon_job_free(job: *mut VcadReconJob) {
    if job.is_null() {
        return;
    }
    // SAFETY: caller contract above; Drop cancels and joins.
    let job = unsafe { Box::from_raw(job) };
    let _ = std::panic::catch_unwind(AssertUnwindSafe(move || drop(job)));
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::take;
    use std::ffi::CString;
    use vcad_eval::{evaluate_document, resolve_document, EvalOptions};

    fn volume(doc: &Document) -> f64 {
        let mut d = doc.clone();
        resolve_document(&mut d).expect("bindings resolve");
        let scene = evaluate_document(&d, &EvalOptions::default()).unwrap();
        assert!(scene.failures.is_empty(), "{:?}", scene.failures);
        scene.parts[0].solid.as_ref().expect("a solid").volume()
    }

    fn programs() -> Vec<Program> {
        [
            "base 60 40 8\nhole 10 5 3",
            "base 60 40 8 fillet 5\ncounterbore -15 0 3 5.5 3\nslotx 12 8 10 2.5",
            "base 70 50 10 chamfer 4\nlinear 3 -20 -12 10 0 2\nboss 15 8 6 5",
            "base 80 60 6\ncircular 4 0 0 15 30 2.5\nsloty -28 0 12 2",
        ]
        .iter()
        .map(|t| Program::from_text(t).unwrap_or_else(|e| panic!("{t}: {e}")))
        .collect()
    }

    fn dims(p: &Program) -> Vec<Dimension> {
        p.param_info()
            .iter()
            .zip(p.params())
            .map(|(i, x)| Dimension {
                parameter: parameter_name(&i.name),
                name: i.name.clone(),
                value: x,
                sigma: Some(0.1),
                unit: "mm",
            })
            .collect()
    }

    /// The parametric document builds the same solid irys-recon's own
    /// builder does, for every corner and feature kind.
    #[test]
    fn document_matches_the_program_solid() {
        for p in programs() {
            assert!(p.valid(), "{}", p.to_text());
            let doc = program_document(&p, [0.8, 0.2, 0.2], 0.4, &dims(&p));
            let want = p.build().unwrap().volume();
            let got = volume(&doc);
            assert!(
                (got - want).abs() / want < 0.01,
                "{}: doc {got:.1} vs program {want:.1}",
                p.to_text()
            );
            // Every parameter is used by some binding (no dead dimensions).
            let text = serde_json::to_string(&doc.bindings).unwrap();
            for name in doc.parameters.keys() {
                assert!(
                    text.contains(name.as_str()),
                    "{name} drives nothing in {}",
                    p.to_text()
                );
            }
        }
    }

    /// Editing a dimension rebuilds the part: it is parametric, not a mesh.
    #[test]
    fn editing_a_parameter_rebuilds() {
        for p in programs() {
            let doc = program_document(&p, [0.5; 3], 0.5, &dims(&p));
            let mut wider = doc.clone();
            wider.parameters.get_mut("base_w").unwrap().value = Expr::Number(p.w + 10.0);
            let mut q = p.clone();
            q.w += 10.0;
            let want = q.build().unwrap().volume();
            let got = volume(&wider);
            assert!(
                (got - want).abs() / want < 0.01,
                "{}: {got:.1} vs {want:.1}",
                p.to_text()
            );
            assert!(got > volume(&doc));
        }
    }

    #[test]
    fn requests_fail_with_plain_messages() {
        let call = |j: &str| {
            let c = CString::new(j).unwrap();
            let r = vcad_recon_check(c.as_ptr());
            assert!(r.is_null());
            err::last_error_string().unwrap()
        };
        assert!(call(r#"{"photos": []}"#).contains("at least one photo"));
        assert!(call(r#"{"photos": ["/nonexistent/a.jpg"]}"#).contains("Couldn't read"));
        assert!(call("not json").contains("bad request"));
    }

    /// A white page with tag36h11 id 0 drawn square-on, `px_per_mm` pixels
    /// per mm, optionally carrying an EXIF focal length (a 26 mm-equivalent
    /// phone lens).
    pub(crate) fn tag_photo(with_tag: bool, with_exif: bool) -> Vec<u8> {
        let (w, h) = (1200u32, 900u32);
        let ppm = 6.0; // 50 mm tag → 300 px
        let cells = tag::cells(tag::TAG_ID);
        let img = image::RgbImage::from_fn(w, h, |x, y| {
            let mx = (x as f64 + 0.5 - w as f64 / 2.0) / ppm;
            let my = (h as f64 / 2.0 - (y as f64 + 0.5)) / ppm;
            let v = if with_tag {
                match tag::white_at(&cells, tag::TAG_MM, mx, my) {
                    Some(true) | None => 235,
                    Some(false) => 20,
                }
            } else {
                235
            };
            image::Rgb([v, v, v])
        });
        let mut jpg = Vec::new();
        image::codecs::jpeg::JpegEncoder::new_with_quality(&mut jpg, 92)
            .encode_image(&img)
            .unwrap();
        if with_exif {
            tag::with_exif(&jpg, 4.2, 26)
        } else {
            jpg
        }
    }

    fn check_file(bytes: &[u8], name: &str) -> Result<String, String> {
        let dir = std::env::temp_dir().join(format!("vcad-recon-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join(name);
        std::fs::write(&path, bytes).unwrap();
        let req = serde_json::json!({ "photos": [path] }).to_string();
        let c = CString::new(req).unwrap();
        let r = vcad_recon_check(c.as_ptr());
        if r.is_null() {
            Err(err::last_error_string().unwrap())
        } else {
            Ok(take(r))
        }
    }

    #[test]
    fn check_finds_the_tag_and_names_what_is_missing() {
        let ok = check_file(&tag_photo(true, true), "tag.jpg").expect("tag found");
        let v: serde_json::Value = serde_json::from_str(&ok).unwrap();
        assert_eq!(v[0]["tag_id"], 0);
        let e = check_file(&tag_photo(true, false), "noexif.jpg").unwrap_err();
        assert!(e.contains("no EXIF focal length"), "{e}");
        let e = check_file(&tag_photo(false, true), "notag.jpg").unwrap_err();
        assert!(e.contains("No AprilTag found"), "{e}");
    }

    /// Writes the Swift tests' fixture photos (run with `--ignored` and
    /// `RECON_FIXTURE_DIR`).
    #[test]
    #[ignore]
    fn write_fixture_photos() {
        let dir = std::path::PathBuf::from(
            std::env::var("RECON_FIXTURE_DIR").expect("RECON_FIXTURE_DIR"),
        );
        std::fs::create_dir_all(&dir).unwrap();
        std::fs::write(dir.join("tag-exif.jpg"), tag_photo(true, true)).unwrap();
        std::fs::write(dir.join("tag-no-exif.jpg"), tag_photo(true, false)).unwrap();
        std::fs::write(dir.join("no-tag-exif.jpg"), tag_photo(false, true)).unwrap();
    }

    /// The whole pipeline on real or rendered photos (`RECON_PHOTOS`, comma
    /// separated). Minutes of CPU: run on a build box, not in CI.
    #[test]
    #[ignore]
    fn reconstruct_photos_end_to_end() {
        let photos: Vec<String> = std::env::var("RECON_PHOTOS")
            .expect("RECON_PHOTOS")
            .split(',')
            .map(String::from)
            .collect();
        let r = Request {
            photos,
            tag_mm: tag::TAG_MM,
            sensor_w_mm: None,
        };
        let o = reconstruct(&r).unwrap_or_else(|e| panic!("{e}"));
        println!("{}", o.program);
        for d in &o.dimensions {
            println!("{} = {:.3} ± {:?}", d.parameter, d.value, d.sigma);
        }
        println!(
            "low_confidence {} warnings {:?} ({:.0} s)",
            o.low_confidence, o.warnings, o.seconds
        );
        let prog = Program::from_text(&o.program).unwrap();
        let want = prog.build().unwrap().volume();
        let got = volume(&o.document);
        assert!(
            (got - want).abs() / want < 0.01,
            "doc {got} vs program {want}"
        );
        if let Ok(out) = std::env::var("RECON_OUT") {
            std::fs::write(out, serde_json::to_string_pretty(&o.document).unwrap()).unwrap();
        }
    }

    /// A cancelled job stops promptly and reports exactly "cancelled"; a
    /// malformed request fails at start; free cancels a running job.
    #[test]
    fn cancel_returns_promptly() {
        let dir = std::env::temp_dir().join(format!("vcad-recon-job-{}", std::process::id()));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("tag.jpg");
        std::fs::write(&path, tag_photo(true, true)).unwrap();
        let req =
            CString::new(serde_json::json!({ "photos": [path, path, path] }).to_string()).unwrap();

        let job = vcad_recon_start(req.as_ptr());
        assert!(!job.is_null(), "{:?}", err::last_error_string());
        let mut p = VcadReconProgress::default();
        // Let it get into the fit (or finish failing) before cancelling.
        let t0 = std::time::Instant::now();
        while unsafe { vcad_recon_poll(job, &mut p) } == 0
            && p.stage < 2
            && t0.elapsed() < std::time::Duration::from_secs(60)
        {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let t = std::time::Instant::now();
        unsafe { vcad_recon_cancel(job) };
        let r = unsafe { vcad_recon_finish(job) };
        let took = t.elapsed();
        assert!(unsafe { vcad_recon_poll(job, &mut p) } == 1);
        assert_eq!(p.cancelled, 1);
        if r.is_null() {
            let e = err::last_error_string().unwrap();
            // A synthetic page has no part on it: the fit may also fail on
            // its own before the cancel lands. Either way it must be prompt.
            if p.stage >= 1 {
                assert!(e == "cancelled" || !e.is_empty(), "{e}");
            }
        } else {
            take(r);
        }
        assert!(
            took < std::time::Duration::from_secs(10),
            "cancel took {took:?}"
        );
        unsafe { vcad_recon_job_free(job) };

        // Cancelled before it gets anywhere: exactly "cancelled".
        let job = vcad_recon_start(req.as_ptr());
        unsafe { vcad_recon_cancel(job) };
        let r = unsafe { vcad_recon_finish(job) };
        assert!(r.is_null());
        assert_eq!(err::last_error_string().as_deref(), Some("cancelled"));
        unsafe { vcad_recon_job_free(job) };

        // Free while running cancels and joins.
        let job = vcad_recon_start(req.as_ptr());
        let t = std::time::Instant::now();
        unsafe { vcad_recon_job_free(job) };
        assert!(t.elapsed() < std::time::Duration::from_secs(10));

        let bad = CString::new("{").unwrap();
        assert!(vcad_recon_start(bad.as_ptr()).is_null());
        assert!(unsafe { vcad_recon_poll(ptr::null(), &mut p) } == -1);
        unsafe { vcad_recon_cancel(ptr::null()) };
        unsafe { vcad_recon_job_free(ptr::null_mut()) };
    }
}

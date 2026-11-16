//! Lathe post-processors.
//!
//! Two dialects, and neither emits a word its controller lacks:
//!
//! - [`LatheDialect::LinuxCnc`] — LinuxCNC lathe config: `G18`, `G7`
//!   diameter mode, `G95` feed per revolution, `G96 D… S…` constant surface
//!   speed with a mandatory rpm ceiling, `G97` for drilling, `T… M6 G43`.
//! - [`LatheDialect::GrblEmulated`] — words stock Grbl (and Grbl_ESP32)
//!   accept: `G0 G1 G4 G21 G90 G94 M0 M3 M5 S F T`. Grbl has no diameter
//!   mode, no CSS, no feed-per-rev and no `M6`, so the post scales X itself
//!   (stated in the header), steps the spindle speed per move to emulate CSS
//!   (rpm from the move's larger radius, so surface speed never exceeds the
//!   target), converts mm/rev to mm/min, and pauses with `M0` for tool
//!   changes. grblHAL's lathe words (G7/G96) are build-dependent; until they
//!   are confirmed on a given build, use this dialect there too.
//!
//! Coordinates are the tool's imaginary tip with nose-radius compensation
//! already applied (see [`super::tool`]); never enable G41/G42 on top.

use super::geom::P2;
use super::program::{SpindleSpeed, TurnMove, TurnProgram};
use std::fmt::Write as _;

/// Target controller.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LatheDialect {
    /// LinuxCNC lathe (native G7/G95/G96).
    LinuxCnc,
    /// Stock Grbl words only; lathe features emulated in the post.
    GrblEmulated,
}

/// How X words are scaled.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum XMode {
    /// X = radius.
    Radius,
    /// X = diameter.
    Diameter,
}

/// Lathe post-processor.
#[derive(Debug, Clone)]
pub struct LathePost {
    /// Dialect.
    pub dialect: LatheDialect,
    /// X scaling for [`LatheDialect::GrblEmulated`] (LinuxCNC always posts
    /// diameter under G7). Must match how the machine's X steps are set.
    pub grbl_x_mode: XMode,
    /// Decimal places.
    pub precision: usize,
    /// Tool-change / end position in programmed coordinates `[z, x_radius]`;
    /// `None` = 5 mm beyond the program's extents.
    pub home: Option<P2>,
}

impl LathePost {
    /// LinuxCNC lathe post.
    pub fn linuxcnc() -> Self {
        Self {
            dialect: LatheDialect::LinuxCnc,
            grbl_x_mode: XMode::Diameter,
            precision: 4,
            home: None,
        }
    }

    /// Stock-Grbl post with the given X scaling.
    pub fn grbl(x_mode: XMode) -> Self {
        Self {
            dialect: LatheDialect::GrblEmulated,
            grbl_x_mode: x_mode,
            precision: 4,
            home: None,
        }
    }

    fn x_scale(&self) -> f64 {
        match (self.dialect, self.grbl_x_mode) {
            (LatheDialect::LinuxCnc, _) | (_, XMode::Diameter) => 2.0,
            (_, XMode::Radius) => 1.0,
        }
    }

    fn home_of(&self, prog: &TurnProgram) -> P2 {
        if let Some(h) = self.home {
            return h;
        }
        let mut zmax = 0.0f64;
        let mut xmax = 0.0f64;
        for op in &prog.ops {
            for m in &op.moves {
                if let Some(t) = m.target() {
                    zmax = zmax.max(t[0]);
                    xmax = xmax.max(t[1]);
                }
            }
        }
        [zmax + 5.0, xmax + 5.0]
    }

    fn c(&self, v: f64) -> String {
        let s = format!("{:.p$}", v, p = self.precision);
        if s.starts_with('-') && s[1..].chars().all(|c| c == '0' || c == '.') {
            s[1..].to_string()
        } else {
            s
        }
    }

    /// Post a whole program.
    pub fn post(&self, prog: &TurnProgram) -> String {
        let mut o = String::new();
        let xs = self.x_scale();
        let home = self.home_of(prog);
        let _ = writeln!(o, "(vcad turning: {})", prog.name);
        match self.dialect {
            LatheDialect::LinuxCnc => {
                let _ = writeln!(o, "(post: linuxcnc-lathe; X = diameter under G7)");
                let _ = writeln!(
                    o,
                    "(programmed point = imaginary tool tip, nose radius compensated in CAM: \
                     keep G40, never G41/G42)"
                );
                let _ = writeln!(o, "G18 G21 G90 G40 G80");
                let _ = writeln!(o, "G7");
                let _ = writeln!(o, "G95");
            }
            LatheDialect::GrblEmulated => {
                let mode = match self.grbl_x_mode {
                    XMode::Radius => "radius",
                    XMode::Diameter => "diameter",
                };
                let _ = writeln!(o, "(post: grbl-emulated lathe)");
                let _ = writeln!(o, "(vcad-x-mode: {mode})");
                let _ = writeln!(
                    o,
                    "(X words are {mode}: set the machine's X steps/mm to match. CSS is \
                     stepped per move; feeds are mm/min)"
                );
                let _ = writeln!(
                    o,
                    "(programmed point = imaginary tool tip, nose radius compensated in CAM)"
                );
                let _ = writeln!(o, "G21 G90 G94");
            }
        }
        let mut cur_tool: Option<u32> = None;
        for op in &prog.ops {
            let tool = prog.tool(op.tool);
            let tname = tool.map_or_else(String::new, |t| t.name.clone());
            let _ = writeln!(o, "(op: {} — T{} {})", op.name, op.tool, tname);
            if cur_tool != Some(op.tool) {
                if cur_tool.is_some() {
                    let _ = writeln!(o, "G0 X{} Z{}", self.c(home[1] * xs), self.c(home[0]));
                }
                match self.dialect {
                    LatheDialect::LinuxCnc => {
                        let _ = writeln!(o, "T{} M6 G43", op.tool);
                    }
                    LatheDialect::GrblEmulated => {
                        let _ = writeln!(o, "M5");
                        let _ = writeln!(o, "T{}", op.tool);
                        let _ = writeln!(
                            o,
                            "M0 (load T{}: {} — touch off its imaginary tip)",
                            op.tool, tname
                        );
                    }
                }
                cur_tool = Some(op.tool);
            }
            let mut last_s: Option<f64> = None;
            match (self.dialect, op.speed) {
                (LatheDialect::LinuxCnc, SpindleSpeed::Css { m_per_min, max_rpm }) => {
                    let _ = writeln!(o, "G96 D{:.0} S{:.0} M3", max_rpm, m_per_min);
                }
                (LatheDialect::LinuxCnc, SpindleSpeed::Rpm { rpm }) => {
                    let _ = writeln!(o, "G97 S{:.0} M3", rpm);
                }
                (LatheDialect::GrblEmulated, _) => {
                    // Start at the speed for the op's first target radius.
                    let r0 = op
                        .moves
                        .iter()
                        .find_map(|m| m.target())
                        .map_or(0.0, |t| t[1].abs());
                    let s = grbl_rpm(op.speed, r0);
                    let _ = writeln!(o, "M3 S{:.0}", s);
                    last_s = Some(s);
                }
            }
            let mut last_f: Option<f64> = None;
            let mut pos: Option<P2> = None;
            for m in &op.moves {
                match *m {
                    TurnMove::Dwell { seconds } => {
                        let _ = writeln!(o, "G4 P{:.3}", seconds);
                    }
                    TurnMove::Rapid { to } => {
                        let _ = writeln!(o, "G0 X{} Z{}", self.c(to[1] * xs), self.c(to[0]));
                        pos = Some(to);
                    }
                    TurnMove::Feed { to, feed_scale } => {
                        let f = match self.dialect {
                            LatheDialect::LinuxCnc => op.feed_per_rev * feed_scale,
                            LatheDialect::GrblEmulated => {
                                let r = pos.map_or(to[1].abs(), |p| p[1].abs().max(to[1].abs()));
                                let s = grbl_rpm(op.speed, r);
                                if last_s != Some(s) {
                                    let _ = writeln!(o, "S{:.0}", s);
                                    last_s = Some(s);
                                }
                                op.feed_per_rev * feed_scale * s
                            }
                        };
                        let fw = if last_f.is_none_or(|lf| (lf - f).abs() > 1e-9) {
                            last_f = Some(f);
                            format!(" F{:.4}", f)
                        } else {
                            String::new()
                        };
                        let _ = writeln!(o, "G1 X{} Z{}{}", self.c(to[1] * xs), self.c(to[0]), fw);
                        pos = Some(to);
                    }
                }
            }
        }
        let _ = writeln!(o, "G0 X{} Z{}", self.c(home[1] * xs), self.c(home[0]));
        let _ = writeln!(o, "M5");
        let _ = writeln!(o, "M30");
        o
    }
}

/// Grbl spindle speed for a move whose largest radius is `r`: CSS stepped
/// down to the next 10 rpm so surface speed never exceeds the target and
/// rpm never exceeds the ceiling.
fn grbl_rpm(speed: SpindleSpeed, r: f64) -> f64 {
    let s = speed.rpm_at(r);
    ((s / 10.0).floor() * 10.0).max(10.0)
}

//! Turning programs: stock, spindle modes, operations, moves.

use super::geom::P2;
use super::tool::LatheTool;
use super::TurnError;
use serde::{Deserialize, Serialize};

/// Bar or tube stock, coaxial with the spindle.
///
/// The stock's free end sits at `z = face_allowance` (the part's finished
/// face is `z = 0`) and it runs `length` toward the chuck.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct TurnStock {
    /// Outside diameter, mm.
    pub diameter: f64,
    /// Inside diameter (0 for solid bar), mm.
    pub bore_diameter: f64,
    /// Material beyond the finished face, faced off first, mm.
    pub face_allowance: f64,
    /// Length from the free end to the chuck jaws, mm.
    pub length: f64,
}

impl TurnStock {
    /// Solid round bar.
    pub fn bar(diameter: f64, length: f64, face_allowance: f64) -> Self {
        Self {
            diameter,
            bore_diameter: 0.0,
            face_allowance,
            length,
        }
    }

    /// Outside radius.
    pub fn radius(&self) -> f64 {
        self.diameter / 2.0
    }

    /// `z` of the free end.
    pub fn z_face(&self) -> f64 {
        self.face_allowance
    }

    /// `z` at the jaws.
    pub fn z_chuck(&self) -> f64 {
        self.face_allowance - self.length
    }
}

/// How the spindle speed is commanded.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "mode")]
pub enum SpindleSpeed {
    /// Constant surface speed (G96) with an rpm ceiling.
    Css {
        /// Surface speed, m/min.
        m_per_min: f64,
        /// RPM ceiling (G96 `D`), mandatory: CSS at the axis is unbounded.
        max_rpm: f64,
    },
    /// Fixed RPM (G97).
    Rpm {
        /// Spindle speed.
        rpm: f64,
    },
}

impl SpindleSpeed {
    /// RPM at radius `r` mm.
    pub fn rpm_at(&self, r: f64) -> f64 {
        match *self {
            SpindleSpeed::Rpm { rpm } => rpm,
            SpindleSpeed::Css { m_per_min, max_rpm } => {
                if r <= 1e-9 {
                    max_rpm
                } else {
                    (1000.0 * m_per_min / (std::f64::consts::TAU * r)).min(max_rpm)
                }
            }
        }
    }
}

/// Cutting parameters for one operation.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
pub struct CutParams {
    /// Spindle command.
    pub speed: SpindleSpeed,
    /// Feed, mm per revolution.
    pub feed_per_rev: f64,
    /// Radial depth of cut per pass (roughing / facing), mm.
    pub depth_of_cut: f64,
    /// Clearance for rapids above stock, mm.
    pub clearance: f64,
}

impl CutParams {
    pub(crate) fn validate(&self) -> Result<(), TurnError> {
        if self.feed_per_rev.is_nan() || self.feed_per_rev <= 0.0 {
            return Err(TurnError::BadParams(format!(
                "feed per rev {} must be > 0",
                self.feed_per_rev
            )));
        }
        if self.depth_of_cut.is_nan() || self.depth_of_cut <= 0.0 {
            return Err(TurnError::BadParams(format!(
                "depth of cut {} must be > 0",
                self.depth_of_cut
            )));
        }
        if self.clearance.is_nan() || self.clearance <= 0.0 {
            return Err(TurnError::BadParams("clearance must be > 0".into()));
        }
        match self.speed {
            SpindleSpeed::Css { m_per_min, max_rpm } if m_per_min > 0.0 && max_rpm > 0.0 => {}
            SpindleSpeed::Rpm { rpm } if rpm > 0.0 => {}
            s => return Err(TurnError::BadParams(format!("bad spindle speed {s:?}"))),
        }
        Ok(())
    }
}

/// One move, in **programmed-point** coordinates `[z, x_radius]`.
#[derive(Debug, Clone, Copy, PartialEq, Serialize, Deserialize)]
#[serde(tag = "type")]
pub enum TurnMove {
    /// G0.
    Rapid {
        /// Target.
        to: P2,
    },
    /// G1 at `feed_scale × feed_per_rev`.
    Feed {
        /// Target.
        to: P2,
        /// Multiplier on the operation's feed (e.g. 0.5 near centre).
        feed_scale: f64,
    },
    /// G4 dwell, seconds.
    Dwell {
        /// Seconds.
        seconds: f64,
    },
}

impl TurnMove {
    /// Target of a motion move.
    pub fn target(&self) -> Option<P2> {
        match *self {
            TurnMove::Rapid { to } | TurnMove::Feed { to, .. } => Some(to),
            TurnMove::Dwell { .. } => None,
        }
    }
}

/// One operation: a tool, a spindle mode, a feed, and moves.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnOp {
    /// Operation name (for comments).
    pub name: String,
    /// Tool number.
    pub tool: u32,
    /// Spindle command.
    pub speed: SpindleSpeed,
    /// Feed, mm/rev.
    pub feed_per_rev: f64,
    /// Moves, programmed point.
    pub moves: Vec<TurnMove>,
}

/// A complete turning program.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TurnProgram {
    /// Job name.
    pub name: String,
    /// Tools used (looked up by number).
    pub tools: Vec<LatheTool>,
    /// Operations in order.
    pub ops: Vec<TurnOp>,
}

impl TurnProgram {
    /// Find a tool by number.
    pub fn tool(&self, number: u32) -> Option<&LatheTool> {
        self.tools.iter().find(|t| t.number == number)
    }

    /// Estimated cycle time in seconds (feeds at the rpm the spindle mode
    /// gives at each move's larger radius; rapids at `rapid_mm_min`).
    pub fn estimated_time(&self, rapid_mm_min: f64) -> f64 {
        let mut t = 0.0;
        let mut last: Option<P2> = None;
        for op in &self.ops {
            for m in &op.moves {
                match *m {
                    TurnMove::Dwell { seconds } => t += seconds,
                    TurnMove::Rapid { to } => {
                        if let Some(l) = last {
                            t += super::geom::dist(l, to) / rapid_mm_min * 60.0;
                        }
                        last = Some(to);
                    }
                    TurnMove::Feed { to, feed_scale } => {
                        if let Some(l) = last {
                            let r = l[1].abs().max(to[1].abs());
                            let f = op.feed_per_rev * feed_scale * op.speed.rpm_at(r);
                            t += super::geom::dist(l, to) / f.max(1e-6) * 60.0;
                        }
                        last = Some(to);
                    }
                }
            }
        }
        t
    }
}

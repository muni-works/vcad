//! Turning: lathe operations, a lathe post, and a fail-closed verifier —
//! plus rotary turning on a mill's 4th axis.
//!
//! Everything lives in the half-plane through the spindle axis: `z` along
//! the axis (positive toward the tailstock, finished face at `z = 0`), `x`
//! the radius. A part enters as a [`TurnProfile`] — its exact half-section
//! in lines and arcs (`vcad-kernel` extracts one from a solid of revolution)
//! — and leaves as G-code that has been replayed and checked against it.
//!
//! ```
//! use vcad_kernel_cam::turning::*;
//!
//! // Ø12 × 20 pin with a Ø8 × 8 spigot (R0.3 at the root), from Ø14 bar.
//! // Inside corners need a radius ≥ the finishing insert's nose radius —
//! // the verifier refuses a sharp one rather than pretend it was cut.
//! let profile = ProfileBuilder::start([0.0, 0.0])
//!     .line_to([0.0, 4.0])
//!     .line_to([-7.7, 4.0])
//!     .arc_to([-8.0, 4.3], [-7.7, 4.3], false)
//!     .line_to([-8.0, 6.0])
//!     .line_to([-20.0, 6.0])
//!     .line_to([-20.0, 0.0])
//!     .close()
//!     .unwrap();
//! let stock = TurnStock::bar(14.0, 40.0, 0.5);
//! let rough = LatheTool::cnmg_95(1, 0.4);
//! let finish = LatheTool::dnmg_93(2, 0.2);
//! let part = LatheTool::parting(3, 2.0, 0.1, 10.0);
//! let css = SpindleSpeed::Css { m_per_min: 150.0, max_rpm: 3000.0 };
//! let p = CutParams { speed: css, feed_per_rev: 0.15, depth_of_cut: 1.0, clearance: 1.0 };
//!
//! let mut plan = TurnPlanner::new(profile.clone(), stock).unwrap();
//! plan.face(&rough, &p).unwrap()
//!     .rough_od(&rough, &p, 0.2).unwrap()
//!     .finish_od(&finish, &CutParams { feed_per_rev: 0.08, ..p }).unwrap()
//!     .part_off(&part, &CutParams { feed_per_rev: 0.05, ..p }, 0.2).unwrap();
//! let program = plan.program("pin");
//! let gcode = LathePost::linuxcnc().post(&program);
//! let verdict = verify_gcode(&gcode, &program.tools, &stock, &profile, &VerifyOptions::default());
//! assert!(verdict.ok, "{:#?}", verdict.findings);
//! ```

mod envelope;
pub(crate) mod geom;
mod ops;
mod post;
mod profile;
mod program;
mod rotary;
mod tool;
mod verify;

pub use geom::P2;
pub use ops::TurnPlanner;
pub use post::{LatheDialect, LathePost, XMode};
pub use profile::{ProfileBuilder, ProfileSeg, SegRole, TurnProfile};
pub use program::{CutParams, SpindleSpeed, TurnMove, TurnOp, TurnProgram, TurnStock};
pub use rotary::{
    plan_rotary, verify_rotary_gcode, ATravel, Endmill, RotaryMachine, RotaryMove, RotaryParams,
    RotaryProgram, RotaryVerdict, RotaryVerifyOptions,
};
pub use tool::{LatheTool, LatheToolKind};
pub use verify::{verify_gcode, Finding, TurnVerdict, VerifyOptions};

use thiserror::Error;

/// Errors from turning planning.
#[derive(Debug, Clone, Error, PartialEq)]
pub enum TurnError {
    /// The half-section is malformed.
    #[error("bad profile: {0}")]
    BadProfile(String),
    /// The part cannot be made this way (refused, not skipped).
    #[error("not turnable: {0}")]
    NotTurnable(String),
    /// Stock does not fit the part.
    #[error("bad stock: {0}")]
    BadStock(String),
    /// Tool definition or tool/operation mismatch.
    #[error("bad tool: {0}")]
    BadTool(String),
    /// Invalid cutting parameters.
    #[error("bad parameters: {0}")]
    BadParams(String),
}

impl TurnProgram {
    /// Post with `post` and verify the posted text (fail closed). The
    /// G-code is returned only when the verdict passes.
    pub fn post_verified(
        &self,
        post: &LathePost,
        stock: &TurnStock,
        profile: &TurnProfile,
        opts: &VerifyOptions,
    ) -> Result<String, TurnVerdict> {
        let gcode = post.post(self);
        let mut o = *opts;
        if post.dialect == LatheDialect::GrblEmulated {
            o.x_mode = post.grbl_x_mode;
        }
        let v = verify_gcode(&gcode, &self.tools, stock, profile, &o);
        if v.ok {
            Ok(gcode)
        } else {
            Err(v)
        }
    }
}

//! Live magnet-steered plasmoid (`vcad_kernel_particle::plasmoid`) for the
//! native app's drag demo: the app owns a handle, and every frame hands in
//! where the dragged magnet part is; the kernel integrates the reduced foot
//! model over that frame (fixed RK4 substeps, the magnet pose interpolated
//! from the previous frame's) and hands back foot position, β and lock.
//!
//! Units at this boundary are the document's: millimeters, Z-up, in the
//! *dish frame* (dish centre at the origin, its rim/surface at z = 0) — the
//! app subtracts the dish origin it read from the document.

use std::panic::{catch_unwind, AssertUnwindSafe};
use std::ptr;

use vcad_kernel_particle::field3d::{FieldScene, Pose};
use vcad_kernel_particle::plasmoid::{DiscMagnet, PlasmoidStepper};

use crate::err::{clear_error, set_error};

/// Opaque live-sim handle.
pub struct VcadPlasmoid {
    stepper: PlasmoidStepper,
}

/// Snapshot after a frame.
#[repr(C)]
#[derive(Debug, Clone, Copy)]
pub struct VcadPlasmoidState {
    /// Simulated seconds since create / reset.
    pub t_s: f64,
    /// Foot position in the dish frame, mm.
    pub x_mm: f64,
    /// Foot position in the dish frame, mm.
    pub y_mm: f64,
    /// Electron Hall parameter at the foot (0 with no magnet).
    pub beta: f64,
    /// 1 when the foot is locked under a magnet.
    pub locked: u8,
    /// Index of the magnet it is locked under, -1 when not locked.
    pub locked_magnet: i32,
}

impl Default for VcadPlasmoidState {
    fn default() -> Self {
        Self {
            t_s: 0.0,
            x_mm: 0.0,
            y_mm: 0.0,
            beta: 0.0,
            locked: 0,
            locked_magnet: -1,
        }
    }
}

/// Racing orbit as a fraction of the dish radius (55 mm on the 80 mm
/// reference dish).
const ORBIT_FRAC: f64 = 0.055 / 0.08;
/// Gap the mobility is calibrated at, m (the demo's hold pose).
const CAL_GAP_M: f64 = 10e-3;
/// Mobility margin over the analytic depinning threshold (the demo's).
const MARGIN: f64 = 1.8;

/// Create a live sim for a dish of `dish_radius_mm` with argon at
/// `pressure_torr`, an N52 10 × 5 mm disc magnet, and the racer on its
/// orbit. The pin strength is calibrated once at 1 Torr; changing the
/// pressure later changes only β. Returns NULL on bad input.
#[no_mangle]
pub extern "C" fn vcad_plasmoid_create(
    dish_radius_mm: f64,
    pressure_torr: f64,
) -> *mut VcadPlasmoid {
    clear_error();
    if !(dish_radius_mm.is_finite() && dish_radius_mm > 1.0) {
        set_error("vcad_plasmoid_create: dish radius must be > 1 mm");
        return ptr::null_mut();
    }
    catch_unwind(AssertUnwindSafe(|| {
        let stepper = PlasmoidStepper::demo(
            DiscMagnet::n52_10x5(),
            dish_radius_mm * 1e-3,
            ORBIT_FRAC,
            CAL_GAP_M,
            MARGIN,
            pressure_torr,
        );
        Box::into_raw(Box::new(VcadPlasmoid { stepper }))
    }))
    .unwrap_or_else(|_| {
        set_error("vcad_plasmoid_create: panic in kernel");
        ptr::null_mut()
    })
}

/// Free a handle. Null is a no-op.
#[no_mangle]
pub extern "C" fn vcad_plasmoid_free(sim: *mut VcadPlasmoid) {
    if !sim.is_null() {
        drop(unsafe { Box::from_raw(sim) });
    }
}

/// Foot back to its start on the orbit, t = 0.
#[no_mangle]
pub extern "C" fn vcad_plasmoid_reset(sim: *mut VcadPlasmoid) {
    if let Some(s) = unsafe { sim.as_mut() } {
        s.stepper.reset();
    }
}

/// Set the gas pressure (Torr). Only the collision frequency changes.
#[no_mangle]
pub extern "C" fn vcad_plasmoid_set_pressure(sim: *mut VcadPlasmoid, pressure_torr: f64) {
    if let Some(s) = unsafe { sim.as_mut() } {
        if pressure_torr.is_finite() {
            s.stepper.set_pressure_torr(pressure_torr);
        }
    }
}

/// Racing orbit radius, mm.
#[no_mangle]
pub extern "C" fn vcad_plasmoid_orbit_radius_mm(sim: *const VcadPlasmoid) -> f64 {
    unsafe { sim.as_ref() }.map_or(0.0, |s| s.stepper.params.orbit_radius_m * 1e3)
}

/// Advance one frame of `frame_dt_s` seconds in `substeps` RK4 steps with
/// one magnet: [`vcad_plasmoid_advance_multi`] with `count` 1 (or 0 when
/// `magnet` is NULL).
#[no_mangle]
pub extern "C" fn vcad_plasmoid_advance(
    sim: *mut VcadPlasmoid,
    magnet: *const f64,
    frame_dt_s: f64,
    substeps: u32,
) -> VcadPlasmoidState {
    let count = usize::from(!magnet.is_null());
    vcad_plasmoid_advance_multi(sim, magnet, count, frame_dt_s, substeps)
}

/// Advance one frame of `frame_dt_s` seconds in `substeps` RK4 steps with
/// `count` disc magnets (the handle's N52 10 × 5 mm) superposed.
///
/// `magnets` is `6 × count` doubles — per magnet its centre x, y, z (mm,
/// dish frame) then its magnetization axis x, y, z (north-pole direction,
/// any length); `count` 0 (or NULL) is "no magnet". Each magnet moves
/// linearly from where the previous call left it across the frame, as
/// long as the count is unchanged (a changed count jumps). `substeps` is
/// clamped to 1..=10_000 and a non-positive / non-finite `frame_dt_s`
/// steps nothing. `locked_magnet` in the result indexes `magnets`.
#[no_mangle]
pub extern "C" fn vcad_plasmoid_advance_multi(
    sim: *mut VcadPlasmoid,
    magnets: *const f64,
    count: usize,
    frame_dt_s: f64,
    substeps: u32,
) -> VcadPlasmoidState {
    clear_error();
    let Some(s) = (unsafe { sim.as_mut() }) else {
        set_error("vcad_plasmoid_advance: null handle");
        return VcadPlasmoidState::default();
    };
    let scene = if magnets.is_null() || count == 0 {
        None
    } else {
        if count > 1024 {
            set_error("vcad_plasmoid_advance: more than 1024 magnets");
            return VcadPlasmoidState::default();
        }
        let v = unsafe { std::slice::from_raw_parts(magnets, 6 * count) };
        if !v.iter().all(|x| x.is_finite()) {
            set_error("vcad_plasmoid_advance: non-finite magnet pose");
            return VcadPlasmoidState::default();
        }
        let src = s.stepper.magnet.as_source();
        let mut scene = FieldScene::new();
        for m in v.as_chunks::<6>().0 {
            if m[3] == 0.0 && m[4] == 0.0 && m[5] == 0.0 {
                set_error("vcad_plasmoid_advance: zero magnet axis");
                return VcadPlasmoidState::default();
            }
            scene.push(
                src,
                Pose::from_axis([m[0] * 1e-3, m[1] * 1e-3, m[2] * 1e-3], [m[3], m[4], m[5]]),
            );
        }
        Some(scene)
    };
    let n = substeps.clamp(1, 10_000) as usize;
    let dt = if frame_dt_s.is_finite() && frame_dt_s > 0.0 {
        frame_dt_s / n as f64
    } else {
        0.0
    };
    catch_unwind(AssertUnwindSafe(|| {
        let st = if dt > 0.0 {
            s.stepper.advance_scene(None, scene, dt, n)
        } else {
            s.stepper.state_in(scene.as_ref())
        };
        VcadPlasmoidState {
            t_s: st.t_s,
            x_mm: st.pos_m[0] * 1e3,
            y_mm: st.pos_m[1] * 1e3,
            beta: st.beta,
            locked: st.locked as u8,
            locked_magnet: st.locked_source.map_or(-1, |i| i as i32),
        }
    }))
    .unwrap_or_else(|_| {
        set_error("vcad_plasmoid_advance: panic in kernel");
        VcadPlasmoidState::default()
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn races_then_locks_under_a_held_magnet() {
        let sim = vcad_plasmoid_create(80.0, 1.0);
        assert!(!sim.is_null());
        let r0 = vcad_plasmoid_orbit_radius_mm(sim);
        assert!((r0 - 55.0).abs() < 1e-9);
        // No magnet: it races (moves off its start) and is never locked.
        let s = vcad_plasmoid_advance(sim, ptr::null(), 0.1, 240);
        assert!(s.locked == 0 && s.beta == 0.0 && s.y_mm > 10.0);
        // Magnet held over the orbit, north down, 10 mm gap: within ~1 s the
        // racer comes round and locks.
        let pose = [0.0, -r0, 12.5, 0.0, 0.0, -1.0];
        let mut locked = false;
        for _ in 0..120 {
            let s = vcad_plasmoid_advance(sim, pose.as_ptr(), 1.0 / 60.0, 40);
            locked = s.locked == 1;
        }
        assert!(locked);
        // Raise the pressure to 1 atm: the pin vanishes, the foot races off.
        vcad_plasmoid_set_pressure(sim, 760.0);
        let mut ever = false;
        for _ in 0..60 {
            let s = vcad_plasmoid_advance(sim, pose.as_ptr(), 1.0 / 60.0, 40);
            if s.beta > 0.01 {
                ever = true;
            }
        }
        let s = vcad_plasmoid_advance(sim, pose.as_ptr(), 1.0 / 60.0, 40);
        assert!(!ever && s.locked == 0);
        vcad_plasmoid_free(sim);
    }

    #[test]
    fn rejects_bad_input() {
        assert!(vcad_plasmoid_create(0.0, 1.0).is_null());
        let s = vcad_plasmoid_advance(ptr::null_mut(), ptr::null(), 0.1, 10);
        assert_eq!(s.t_s, 0.0);
    }

    #[test]
    fn two_magnets_report_which_one_holds_the_foot() {
        let sim = vcad_plasmoid_create(80.0, 1.0);
        let r0 = vcad_plasmoid_orbit_radius_mm(sim);
        // Magnet 0 at 180°, magnet 1 at 90°: the CCW racer from 0° meets 1 first.
        let poses = [
            -r0, 0.0, 12.5, 0.0, 0.0, -1.0, 0.0, r0, 12.5, 0.0, 0.0, -1.0,
        ];
        let mut s = VcadPlasmoidState::default();
        for _ in 0..90 {
            s = vcad_plasmoid_advance_multi(sim, poses.as_ptr(), 2, 1.0 / 60.0, 40);
        }
        assert_eq!((s.locked, s.locked_magnet), (1, 1));
        let none = vcad_plasmoid_advance_multi(sim, ptr::null(), 0, 1.0 / 60.0, 40);
        assert_eq!(none.locked_magnet, -1);
        vcad_plasmoid_free(sim);
    }
}

//! Physics solver tools for vcad.app's Analysis → Fields panel
//! (`vcad_solver_run`): charged-particle optics, the electrode optimizer,
//! neutron shielding and bench-measurement comparison — the exact
//! implementation the native MCP server serves (`vcad-solver-tools`), so
//! the panel and the agent can never disagree about a number.

use std::ffi::{c_char, c_void, CString};
use std::ptr;
use std::sync::atomic::{AtomicBool, Ordering};

use crate::err;
use crate::json::{borrow_str, guard_ptr, into_c};

/// Progress hook: `(ctx, fraction 0..1, stage message)`; return false to
/// cancel (the run stops at the next particle / evaluation).
pub type VcadSolverProgressFn =
    extern "C" fn(ctx: *mut c_void, fraction: f64, message: *const c_char) -> bool;

/// Run solver tool `name` on `args_json` (the MCP tool's arguments) and
/// return the FULL result JSON (section data and every optimizer
/// evaluation included). Null on failure with `vcad_last_error` set
/// ("cancelled" when the hook returned false). The hook runs on the
/// calling thread.
#[no_mangle]
pub extern "C" fn vcad_solver_run(
    name: *const c_char,
    args_json: *const c_char,
    progress: Option<VcadSolverProgressFn>,
    ctx: *mut c_void,
) -> *mut c_char {
    err::clear_error();
    let ctx = ctx as usize;
    guard_ptr("vcad_solver_run", || {
        let Some(name) = borrow_str("name", name) else {
            return ptr::null_mut();
        };
        let Some(args) = borrow_str("args", args_json) else {
            return ptr::null_mut();
        };
        let args: serde_json::Value = match serde_json::from_str(args) {
            Ok(v) => v,
            Err(e) => {
                err::set_error(format!("args: {e}"));
                return ptr::null_mut();
            }
        };
        let cancel = AtomicBool::new(false);
        let hook = |f: f64, msg: &str| {
            if let Some(cb) = progress {
                let m = CString::new(msg.replace('\0', " ")).unwrap_or_default();
                if !cb(ctx as *mut c_void, f, m.as_ptr()) {
                    cancel.store(true, Ordering::Relaxed);
                }
            }
        };
        let hooks = vcad_solver_tools::Hooks {
            progress: &hook,
            cancel: &cancel,
        };
        match vcad_solver_tools::run(name, &args, &hooks) {
            Ok(v) => into_c(v.to_string()),
            Err(e) => {
                err::set_error(e.to_string());
                ptr::null_mut()
            }
        }
    })
}

/// `{tools:[{name, description, inputSchema}]}` for the solver tools.
#[no_mangle]
pub extern "C" fn vcad_solver_tools() -> *mut c_char {
    into_c(serde_json::json!({ "tools": vcad_solver_tools::descriptors() }).to_string())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::json::take;

    extern "C" fn count(ctx: *mut c_void, f: f64, _m: *const c_char) -> bool {
        let n = unsafe { &*(ctx as *const std::sync::atomic::AtomicUsize) };
        assert!((0.0..=1.0).contains(&f));
        n.fetch_add(1, Ordering::Relaxed);
        true
    }

    extern "C" fn stop(_ctx: *mut c_void, _f: f64, _m: *const c_char) -> bool {
        false
    }

    fn args() -> CString {
        CString::new(
            r#"{"spec":{"chamber_radius_mm":76,"chamber_half_height_mm":76,
            "rings":[{"ring_radius_mm":19,"z_mm":0,"wire_radius_mm":1,"potential_v":-30000}]},
            "options":{"nr":31,"nz":61,"particles":4,"max_passes":3},"section":true}"#,
        )
        .unwrap()
    }

    #[test]
    fn runs_with_progress_and_returns_the_full_section() {
        let n = std::sync::atomic::AtomicUsize::new(0);
        let name = CString::new("simulate_charged_particles").unwrap();
        let out = take(vcad_solver_run(
            name.as_ptr(),
            args().as_ptr(),
            Some(count),
            &n as *const _ as *mut c_void,
        ));
        let v: serde_json::Value = serde_json::from_str(&out).unwrap();
        assert!(v["section"]["tracks"].as_array().unwrap().len() >= 2);
        assert!(n.load(Ordering::Relaxed) > 3);
    }

    #[test]
    fn hook_returning_false_cancels() {
        let name = CString::new("simulate_charged_particles").unwrap();
        let p = vcad_solver_run(name.as_ptr(), args().as_ptr(), Some(stop), ptr::null_mut());
        assert!(p.is_null());
        assert_eq!(err::last_error_string().as_deref(), Some("cancelled"));
    }

    #[test]
    fn lists_the_tools() {
        let v: serde_json::Value = serde_json::from_str(&take(vcad_solver_tools())).unwrap();
        assert_eq!(v["tools"].as_array().unwrap().len(), 4);
    }
}

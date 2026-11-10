//! Shared string plumbing for the JSON-shaped modeling entry points
//! (`sketch`, `topology`, `features`, `import`).
//!
//! Every owned string these modules return is a NUL-terminated UTF-8 C string
//! allocated by Rust; Swift releases it with [`vcad_string_free`] and nothing
//! else (handing it to `free(3)` is a heap mismatch).

use std::ffi::{c_char, CStr, CString};
use std::ptr;

use crate::err;

/// Borrow a NUL-terminated UTF-8 C string. Records an error and returns `None`
/// on null or invalid UTF-8.
pub(crate) fn borrow_str<'a>(what: &str, s: *const c_char) -> Option<&'a str> {
    if s.is_null() {
        err::set_error(format!("{what}: null pointer"));
        return None;
    }
    match unsafe { CStr::from_ptr(s) }.to_str() {
        Ok(t) => Some(t),
        Err(e) => {
            err::set_error(format!("{what}: not UTF-8: {e}"));
            None
        }
    }
}

/// Hand an owned string across the boundary (null if it contains a NUL, which
/// serde_json output never does).
pub(crate) fn into_c(s: String) -> *mut c_char {
    match CString::new(s) {
        Ok(c) => c.into_raw(),
        Err(e) => {
            err::set_error(format!("string contains NUL: {e}"));
            ptr::null_mut()
        }
    }
}

/// Serialize `v` as JSON and hand it across.
pub(crate) fn json_out<T: serde::Serialize>(v: &T) -> *mut c_char {
    match serde_json::to_string(v) {
        Ok(s) => into_c(s),
        Err(e) => {
            err::set_error(format!("serialize: {e}"));
            ptr::null_mut()
        }
    }
}

/// Run `f` behind a panic guard, mapping a panic to null + a last-error.
pub(crate) fn guard_ptr<T>(what: &str, f: impl FnOnce() -> *mut T) -> *mut T {
    match std::panic::catch_unwind(std::panic::AssertUnwindSafe(f)) {
        Ok(p) => p,
        Err(_) => {
            err::set_error(format!("{what}: kernel panicked"));
            ptr::null_mut()
        }
    }
}

/// Free a string returned by any `vcad_sketch_*`, `vcad_topology_*`,
/// `vcad_feature_*` or `vcad_import_*` entry point. Null is a no-op.
#[no_mangle]
pub extern "C" fn vcad_string_free(s: *mut c_char) {
    if s.is_null() {
        return;
    }
    drop(unsafe { CString::from_raw(s) });
}

#[cfg(test)]
pub(crate) fn take(s: *mut c_char) -> String {
    assert!(
        !s.is_null(),
        "null result; last error: {:?}",
        err::last_error_string()
    );
    let out = unsafe { CStr::from_ptr(s) }.to_str().unwrap().to_string();
    vcad_string_free(s);
    out
}

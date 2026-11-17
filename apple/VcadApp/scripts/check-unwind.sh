#!/usr/bin/env bash
# Fail when a Rust artifact of the Mac app would turn a kernel panic into
# abort() instead of an unwind.
#
#   check-unwind.sh --lib Libs/libvcad_ffi.a [--lib …]   # staticlibs
#   check-unwind.sh --bin dist/vcad.app/Contents/MacOS/vcad
#
# Why this exists: vcad.app links TWO Rust staticlibs (libvcad_ffi.a and
# libtang_compress_ffi.a). Each carries its own copy of std and its own panic
# runtime, and both define the same `__rust_start_panic` symbol; the linker
# keeps one. 1.0.3-beta.5 shipped with libtang_compress_ffi.a built
# `panic = "abort"`, the linker kept ITS runtime, and every catch_unwind in
# vcad-eval / vcad-ffi became dead code: a kernel assertion while opening
# fusor.vcad was SIGABRT instead of a per-part failure. Nothing failed at
# build time. This check is that failure.
#
# --lib: the archive must not carry the panic_abort runtime (one such archive
#        is enough to poison the link, whichever order the linker reads them).
# --bin: the linked executable must import _Unwind_RaiseException, which only
#        the panic_unwind runtime's __rust_start_panic references. It is an
#        undefined (dylib) import, so it survives `strip`.
set -euo pipefail

fail=0
check_lib() {
  local lib="$1"
  if [ ! -f "$lib" ]; then echo "check-unwind: $lib: no such file" >&2; fail=1; return; fi
  # List once into a variable: `ar t | grep -q` under pipefail fails on SIGPIPE.
  local members abort
  members="$(ar t "$lib")"
  abort="$(grep -E '^panic_abort-' <<<"$members" || true)"
  if [ -n "$abort" ]; then
    echo "check-unwind: FAIL $lib carries the panic=abort runtime ($abort)." >&2
    echo "  Linked into vcad.app it can replace the unwind runtime, and a kernel panic" >&2
    echo "  then kills the app. Build it with panic = \"unwind\" (see build-tc-ffi.sh /" >&2
    echo "  tang-compress-ffi/Cargo.toml.in, or [profile.release] in the workspace)." >&2
    fail=1
  elif ! grep -qE '^panic_unwind-' <<<"$members"; then
    echo "check-unwind: FAIL $lib carries no panic_unwind runtime (not a Rust staticlib built with panic=unwind?)." >&2
    fail=1
  else
    echo "check-unwind: ok $lib (panic_unwind)"
  fi
}
check_bin() {
  local bin="$1"
  if [ ! -f "$bin" ]; then echo "check-unwind: $bin: no such file" >&2; fail=1; return; fi
  local imports
  imports="$(nm -u "$bin" 2>/dev/null || true)"
  if grep -q '_Unwind_RaiseException' <<<"$imports"; then
    echo "check-unwind: ok $bin (links the unwind runtime)"
  else
    echo "check-unwind: FAIL $bin does not import _Unwind_RaiseException: it was linked" >&2
    echo "  with Rust's panic=abort runtime, so a kernel panic aborts the app instead of" >&2
    echo "  becoming a per-part failure. Run this script with --lib on every Rust" >&2
    echo "  staticlib in Libs/ to find the one built with panic=abort, and rebuild it." >&2
    fail=1
  fi
}

[ $# -gt 0 ] || { echo "usage: $0 (--lib <archive.a> | --bin <executable>)…" >&2; exit 2; }
while [ $# -gt 0 ]; do
  case "$1" in
    --lib) check_lib "${2:?--lib needs a path}"; shift 2 ;;
    --bin) check_bin "${2:?--bin needs a path}"; shift 2 ;;
    *) echo "usage: $0 (--lib <archive.a> | --bin <executable>)…" >&2; exit 2 ;;
  esac
done
exit "$fail"

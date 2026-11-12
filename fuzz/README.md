# vcad fuzzing

Three layers. Findings and their status: [`docs/qa/fuzz-2026-11-12.md`](../docs/qa/fuzz-2026-11-12.md).

## 1. Document/command property tests (stable, in CI)

`crates/vcad-app/tests/doc_fuzz.rs` (next to `command_algebra.rs`): seeded
random — and deliberately malformed — `DocCommand`/tool sequences through
`DocumentApi` and `vcad_eval`. Invariants: no panic, no hang (60 s per
evaluation), every envelope well-formed, undo-all → initial and redo-all →
final, `save` → `load` exact, corrupt files fail closed, evaluated meshes
finite and well-indexed, solid volumes ≥ 0, cyclic documents refused.

```bash
cargo test -p vcad-app --test doc_fuzz
VCAD_FUZZ_CASES=5000 cargo test -p vcad-app --test doc_fuzz   # long run (mew)
```

## 2. Parser/importer fuzzing (cargo-fuzz, nightly — run on mew)

`fuzz/` is a workspace member (for workspace deps and `[patch]`es) but not a
default member, so plain `cargo build` ignores it. Targets:

| target | entry point |
|---|---|
| `step_parse` / `step_read` | `stepperoni::parse` / `vcad_kernel_step::read_step_from_buffer` |
| `stl` | `vcad_eval::mesh_inline::stl_to_mesh` |
| `loon` | `vcad_loon::eval_vcad` |
| `vcad_json` / `vcad_snapshot` | `DocumentApi::from_ir` / `DocumentApi::load` |
| `kicad_pcb` / `kicad_sch` | `vcad_ecad_symbols::parse_kicad_{pcb,sch}` |
| `eagle_brd` | `parse_eagle_brd` |
| `altium_ascii` / `altium_pcbdoc` | `parse_altium_ascii_pcb` / `parse_altium_pcbdoc` |
| `urdf` | `vcad_kernel_urdf::read_urdf_from_str` |

```bash
cargo install cargo-fuzz
fuzz/seed.sh                                   # corpora from real files in the repo (git ls-files)
cd fuzz
CARGO_TARGET_DIR=$PWD/target cargo +nightly fuzz run step_read corpus/step_read -- \
  -max_total_time=600 -timeout=10 -rss_limit_mb=2048 -max_len=262144
# loon: add -detect_leaks=0 (the interpreter's Rc environments are cyclic by design)
cargo +nightly fuzz run step_read artifacts/step_read/crash-…  # replay a finding
cargo +nightly fuzz tmin step_read artifacts/step_read/crash-… # minimize it
```

A crash becomes a minimized `#[test]` next to the code it broke (see the
`fuzz_regressions` modules); `corpus/` and `artifacts/` are gitignored.
Delete `fuzz/target` afterwards — ASan builds are large.

## 3. Agent-bridge fuzzing (a vcad.app instance you launched)

```bash
open -n apple/VcadApp/dist/vcad.app              # note the new pid in ~/.vcad/instances/
VCAD_APP_PID=<pid> VCAD_FUZZ_N=400 VCAD_FUZZ_SEED=1 python3 -I fuzz/bridge_fuzz.py
```

Raw-HTTP garbage, malformed JSON-RPC, and random/mutated arguments for every
advertised tool; after each request the instance must be alive and answer
`tools/list` within 15 s. Tools with side effects outside the process
(exports, saves, printers, mills) are skipped; `VCAD_FUZZ_SKIP=a,b` skips
more (`ask_user` blocks on a human by design). A fresh bundle id shows the
first-launch agent-install prompt, which blocks the bridge (F10): launch once,
dismiss it, or set `VCAD_SKIP_AGENT_PROMPT=1`. Only ever target your own pid.

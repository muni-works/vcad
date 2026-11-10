<p align="center">
  <img src="assets/mascot.png" width="200" alt="vcad mascot">
</p>

# vcad

Open-source parametric CAD with its own BRep kernel, written in Rust.

vcad is three things in one repository:

- **The Mac app**: a free, native SwiftUI app in [`apple/VcadApp`](apple/VcadApp).
- **The kernel**: a modular Rust workspace in [`crates/`](crates) covering
  BRep topology, booleans, fillets, sketches and constraints, sweeps and
  lofts, STEP, drafting, assemblies, physics, CAM, electronics, and a set of
  analysis solvers.
- **The agent link**: an MCP server, so you can bring your own AI agent
  (Claude Code, Cursor, Codex, and others) and have it model, inspect, and
  export parts.

vcad runs entirely on your machine. There are no accounts, no cloud sync, and
no hosted services: the MCP server runs locally, and the legacy web editor at
[vcad.io](https://vcad.io) is local-only and retiring in favor of the Mac app.

## Prerequisites

vcad depends on two sibling workspaces, `tang` (math) and `phyz` (physics).
Clone both next to vcad before running any `cargo` command; the default build
fails without them:

```bash
git clone git@github.com:ecto/tang.git ../tang
git clone git@github.com:ecto/phyz.git ../phyz
```

## Mac app

Requires macOS 15 and the Xcode command-line tools.

```bash
apple/VcadApp/build-ffi.sh   # build the vcad-ffi kernel static library (release)
apple/VcadApp/bundle.sh      # assemble apple/VcadApp/dist/vcad.app
open apple/VcadApp/dist/vcad.app
```

Release signing, notarization, and auto-update are described in
[docs/mac-release.md](docs/mac-release.md).

### Connect an agent to the running app

The app serves MCP over HTTP on `127.0.0.1` and writes the port to
`~/.vcad/app.json`. Clients that only speak stdio can use the bundled shim:

```json
{ "command": "node", "args": ["/path/to/vcad/apple/VcadApp/scripts/vcad-app-mcp.mjs"] }
```

## MCP server

The standalone server is published to npm as `@vcad/mcp`. It runs locally
over stdio and needs no account:

```json
{ "command": "npx", "args": ["-y", "@vcad/mcp"] }
```

With Claude Code:

```bash
claude mcp add vcad -- npx -y @vcad/mcp
```

The usual loop is `open_document`, author with `create_cad_loon`, look with
`render_view`, measure with `inspect_cad`, and ship with `export_cad`
(STL, GLB, or STEP). Tool packs and protocol details are
documented in [packages/mcp/README.md](packages/mcp/README.md).

## CLI

The kernel and the `vcad` command-line tool are published to crates.io as
`vcad`:

```bash
cargo install vcad
vcad export part.vcad part.stl      # STL, GLB, or STEP; .loon source works too
vcad import part.step part.vcad     # STEP to .vcad
vcad info part.vcad
vcad --help                         # render, route, fab-prep, diff, merge, ...
```

## Rust library

```rust
use vcad::Solid;

// A plate with a hole
let plate = Solid::cube(100.0, 60.0, 20.0);
let hole = Solid::cylinder(10.0, 25.0, 32);
let part = plate - hole;

let mesh = part.to_mesh(32);
```

`vcad` re-exports [`vcad-kernel`](crates/vcad-kernel), the unified kernel API.
Coordinates are Z-up, units are millimeters.

## Repository layout

```
crates/          Rust workspace: kernel crates (vcad-kernel-*), IR, evaluator,
                 loon front end, ECAD (vcad-ecad-*), FFI, CLI, renderer, ...
apple/VcadApp/   native SwiftUI Mac app
packages/        TypeScript workspace: mcp, engine, ir, core, kernel-wasm,
                 app (web app), docs (documentation site), ...
lib/             loon CAD stdlib and DFM rule packs
examples/        sample documents, URDFs, and loon projects
mecheval/        mechanical-design benchmark and graders
docs/            design notes and solver milestone write-ups
```

## Development

```bash
# Rust
cargo test --workspace
cargo clippy --workspace -- -D warnings
cargo fmt --all --check

# TypeScript (workspace packages must be built before dependents)
npm ci
VCAD_WASM_SKIP=1 npm run build --workspaces --if-present
npm test --workspaces --if-present
```

[CLAUDE.md](CLAUDE.md) has the full contributor notes: worktree setup, the
kernel WASM artifact rule, adding IR operations, and the changelog format.

## Export controls

vcad operates no hosted services. It is published open-source software that
you build and run yourself; users must comply with all export laws that apply
to them. Details in
[docs/export-controls.md](docs/export-controls.md).

## License

Copyright © 2026 Municipal Robotics Corporation

Licensed under the [Apache License 2.0](LICENSE). See [NOTICE](NOTICE) for
attributions. Contributions are accepted under the same license per the
[Apache 2.0 Section 5](LICENSE) inbound-equals-outbound rule.

<p align="center">
  <img src="apple/VcadApp/Branding/icon-variants/vcad-icon-v1-1024.png" width="140" alt="vcad app icon">
</p>

<h1 align="center">vcad</h1>

<p align="center">
  <b>Native CAD for the Mac, with a real kernel and room for your agent.</b><br>
  Open the app, ask for a part, and watch every edit land.
</p>

<p align="center">
  <a href="https://vcad.io/desktop/"><b>Download for Mac</b></a>
  &nbsp;·&nbsp;
  <a href="#bring-your-own-agent">Bring your own agent</a>
  &nbsp;·&nbsp;
  <a href="https://muni.works/research/vcad">The paper</a>
  &nbsp;·&nbsp;
  <a href="#build-from-source">Build from source</a>
</p>

<p align="center">
  <img src="assets/readme/mac-app.png" width="880" alt="The vcad Mac app with a Stirling engine assembly open">
</p>

vcad is open-source parametric CAD with its own boundary-representation
kernel, written in Rust. It runs entirely on your machine: no account, no
cloud, no subscription. Requires macOS 15 or later.

## Bring your own agent

vcad speaks MCP, so Claude Code, Claude Desktop, Cursor, Codex, or any MCP
client can model, measure, and export real parts.

**Install the Mac app.** On first launch it finds the agents you have installed
and connects them in one dialog (undo any time in Settings ▸ Agents). The
running app serves MCP on `127.0.0.1`, so your agent drives the document you
are looking at. Each edit it makes is one named undo step.

**Without the app**, the same server is on npm and runs locally:

```bash
claude mcp add vcad -- npx -y @vcad/mcp
```

Or install the Claude Code plugin, which versions the server and the vcad
skill together: `/plugin marketplace add muni-works/vcad`, then
`/plugin install vcad@vcad`. Any other MCP client takes
`{"command": "npx", "args": ["-y", "@vcad/mcp"]}`. Tools and protocol:
[packages/mcp/README.md](packages/mcp/README.md).

## Parts are programs

Agents write parts in **loon**, a small Lisp for geometry. A `.loon` file opens
anywhere a `.vcad` does, and a `defparam` stays editable in the app's
inspector after the agent is done.

```loon
[defparam wall 3.0]
[defparam bore 8.2]

[let plate [extrude 0 0 wall
  [sketch 0 0 0 1 0 0 0 1 0
    #[[polygon #[0 0 60 0 60 40 0 40]
               #[#[8 8 2.2] #[52 8 2.2] #[8 32 2.2] #[52 32 2.2]]]]]]]

[root [pipe plate
  [union [translate 30 20 0 [cylinder 9 20]]]
  [difference [translate 30 20 -1 [cylinder bore 22]]]]
  "aluminum"]
```

A 60 × 40 mm plate with four holes and a bossed bore. Change `bore` and the
part rebuilds.

## Every claim gets a receipt

An agent will state a dimension whether or not it measured one, so vcad gives
it tools to check: `inspect_cad` measures, `render_view` looks,
`check_clearance` and `dfm_check` test the part, and `build_receipt` records
the claims. A receipt fails closed. Each claim is **Holds**, **Stale** (the
geometry changed since it was checked) or **Violated**, and `verify_receipt`
re-checks it after every edit.

## Exact gradients through the kernel

vcad differentiates CAD parameters through its own kernel, fillet radii
included. One fillet radius moves every blend surface at once, and vcad
returns the exact change in volume, centre of mass and inertia, with second
derivatives and a reverse pass, all the way through a rigid-body simulation
with contact. On a five-parameter filleted, drilled part, a gradient costs
85 ms against 860 ms for finite differences. Agents reach it through
`parameter_gradient`.

The method, its limits and every measurement are in
[*Exact Gradients Through a BRep Kernel*](https://muni.works/research/vcad).

## Built with it

[Rose](https://github.com/muni-works/rose), Municipal Robotics' humanoid, is
designed in vcad: every link and bracket is a loon program. The papers'
shared benchmark is one of its shoulder plates.

## What's inside

- **A real BRep kernel** — half-edge topology; planes, cylinders, cones,
  spheres, tori, and NURBS; booleans on exact (Shewchuk) predicates; fillets
  and chamfers; shell; sweep and loft. ([`crates/`](crates))
- **Sketches that solve** — a Levenberg–Marquardt constraint solver for
  coincident, tangent, distance, angle, and the rest.
- **Interchange** — STEP AP214 import and export, STL, GLB, DXF.
- **Assemblies and motion** — mates, joints, forward kinematics, and
  articulated physics with a gym-style interface for RL.
- **Drawings** — orthographic, section, and detail views with dimensions and
  GD&T.
- **Making things** — 2.5D CAM with G-code post and stock simulation, sheet
  metal flat patterns, DFM checks, and PCB layout through fab-ready output.
- **Analysis** — structural FEA, tolerance stackups, thermal, and more, each
  fail-closed: a result says what it was checked against.
- **loon** — a small Lisp for parametric parts; a `.loon` file goes anywhere a
  `.vcad` does.

## Build from source

vcad depends on two sibling workspaces, `tang` (math) and `phyz` (physics).
Clone both next to vcad first; the default build fails without them.

```bash
git clone git@github.com:muni-works/vcad.git
git clone git@github.com:muni-works/tang.git
git clone git@github.com:muni-works/phyz.git
cd vcad
```

**Mac app** (Xcode command-line tools):

```bash
apple/VcadApp/build-ffi.sh   # kernel static library (release)
apple/VcadApp/bundle.sh      # assembles apple/VcadApp/dist/vcad.app
open apple/VcadApp/dist/vcad.app
```

**Kernel and CLI** — published to crates.io as `vcad`:

```bash
cargo install vcad
vcad export part.vcad part.step   # STL, GLB, or STEP; .loon works too
```

**Development:**

```bash
cargo test --workspace
cargo clippy --workspace -- -D warnings
npm ci && VCAD_WASM_SKIP=1 npm run build --workspaces --if-present
```

[CLAUDE.md](CLAUDE.md) has the full contributor notes (worktrees, the kernel
WASM artifact rule, IR operations, changelog). Release signing and updates:
[docs/mac-release.md](docs/mac-release.md). If you had documents on the
retired vcad.io web editor, download them at
[vcad.io/export](https://vcad.io/export).

## License

[Apache License 2.0](LICENSE) — see [NOTICE](NOTICE). Contributions are
accepted under the same license.
Copyright © 2026 Municipal Robotics Corporation. Users are responsible for
complying with export laws that apply to them; see
[docs/export-controls.md](docs/export-controls.md).

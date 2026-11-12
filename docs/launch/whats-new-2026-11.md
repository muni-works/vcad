# What's new: Nov 5–12, 2026

This is a one-page survey of what landed on `main` (through a8a82540f) for vcad 1.0, the free native Mac app plus the open Rust kernel. Each item is labelled **Shippable**, **Shippable, rough edges** or **Experimental**. Labels draw on the PR descriptions, `docs/launch/*` and the hands-on pass in `docs/qa/live-qa-2026-11-11.md`.

## Modeling (Mac app)

| Feature | PRs | Status |
|---|---|---|
| Fusion-style viewport selection, orbit about selection | #1115 | Shippable (QA ✅) |
| Navigation: Compass, Find, Halo, saved views, one-gesture section | #1118, #1128, #1137 | Shippable, rough edges: section caps are hollow on mesh imports and section state is lost when you switch workspace |
| Direct manipulation: press-pull, on-canvas handles, live dimensions, hairline gizmo | #1119 | Shippable (QA ✅) |
| Sketch: inference, smart dimension, solve colours, trim/extend/fillet/mirror/pattern, profile fill | #1129 | Shippable |
| Sketch on face, re-attaching to its named face on rebuild | #1125, #1127 | Shippable |
| History editing: refuse bad reorders, real suppress, rollback marker, broken-reference repair, drag reorder | #1121, #1142 | Shippable |
| Autosave, crash recovery, version history, safe save | #1117 | Shippable (not hands-on QA'd) |

## Output and interop

| Feature | PRs | Status |
|---|---|---|
| Drawings: one click to a dimensioned sheet, section/detail views, BOM, title block, PDF/DXF | #1122 | Shippable, rough edges: annotation collisions on A4. PDF export not verified in QA |
| Print from vcad: Orient → Slice → pair → gated send, pause/stop. No Bambu Studio | #1124, #1137 | Shippable, rough edges: the Orient default was fixed in QA round 2 |
| STEP interop audit: no hangs, colours, assembly placement, NURBS export | #1126, #1123 | Shippable |
| Huge STEP assemblies: Voron 2.4 opens in 22 s (was 30+ min) | #1136 | Shippable |

## Viewport quality

| Feature | PRs | Status |
|---|---|---|
| Antialiased screen-space edges and Fusion-style model edges | #1113, #1132 | Shippable |
| Crease normals, exact kernel normals for BRep, soft studio lighting, crisper grid | #1102, #1114, #1107, #1098 | Shippable |
| One progress system in the header pill | #1135 | Shippable |

## Agents (MCP-native)

| Feature | PRs | Status |
|---|---|---|
| `vcad-mcp` Rust core, `vcad mcp` CLI, one-breath install with a consent dialog | #1099, #1103 | Shippable |
| vcad.app hosts the Rust tool core: one implementation for app and headless | #1140 | Shippable |
| Agent cursor: the agent works in your window | #1101, #1143 | Shippable |
| Live receipt badges (Holds / Stale / Violated) and viewport elicitation | #1104 | Shippable |
| Agent jobs in the header pill, cancellable FEA, live check | #1144 | Shippable |
| Per-instance discovery, explicit target selection | #1133 | Shippable |

## Hero demo

- **Tuned glockenspiel** (#1139). A free-free beam FEM tunes eight bars to within 0.01 cents (predicted). The Mallet strikes a bar and plays its modes, and a Pitch constraint locks each bar to its note. A strike heard by the microphone flips the receipt from *predicted* to *measured*. Fab dry runs produce P1S G-code and mill NC files. **Shippable** as a demo. The measured step needs a physical bar.
- **Modal analysis of any solid** with the Mallet: ships with the demo.

## Experimental

- **Image-to-CAD from photos** (#1081): merged as a draft and gated. Don't market it as finished.
- Electronics workspace: real, but outside the 1.0 headline.

## Known open items (from QA)

- A document opened straight into Drawing never builds the 3D scene. Launches now never restore Drawing (a workaround). The root cause is still open.
- Opening the same file twice spawns two instances.
- Compass/Find/Halo, autosave/versions/recovery, image-to-CAD and the print Slice/Printer steps were not covered by hands-on QA.

## Not running anymore

The hosted web editor, accounts, cloud sync, sharing, ordering and billing are retired (`docs/launch/web-retirement.md`). Former users can download their documents at `/export`.

# vcad: MCP-native (the app is the server)

**One document, one process, one protocol.** The agent and the user work on the
same live object, every claim comes with proof, and the same core runs headless.

## Architecture
- `crates/vcad-mcp` (Rust): the MCP server. Tools declared via `vcad-tool-derive`
  (schema + `outputSchema` from Rust types). Speaks MCP `2026-07-28`
  (stateless, `server/discover`, MRTR, Tasks SEP-2663, `subscriptions/listen`)
  plus legacy `initialize` eras. Reference implementation of the 2026 framing:
  `packages/mcp/src/protocol-2026.ts`.
- `DocumentStore` trait, two impls:
  - `HeadlessStore`: `~/.vcad/mcp-sessions` files (CLI `vcad mcp`, mew, CI).
  - `AppStore`: the live document in vcad.app via `vcad-ffi`.
- Transports: stdio, `127.0.0.1` HTTP, unix socket. `vcad mcp` = stdio shim that
  attaches to a running vcad.app (socket under `~/.vcad/`) or runs headless.
- Strangler: unknown tools proxy to the existing node server
  (`packages/mcp`) until ported. Node retires when the proxy forwards nothing.
- No hosted services. No vcad.io links. "Open in vcad" = `vcad://`.

## MCP feature map
| Feature | Use |
|---|---|
| Resources + subscriptions | `vcad://doc/current`, `/selection`, `/view`; agent edits flow back as updates |
| MCP Apps `ui://` | viewer served from the live doc |
| Tasks | FEA, topopt, routing, fab_prep, prints/mills: progress + cancel |
| Elicitation | "which edge?" answered by clicking in the viewport; confirms |
| Structured output | receipts machine-checkable (`vcad-receipt`) |
| Sampling | local model loops (image-to-CAD refine) without our keys |
| toolsListChanged | context packs (PCB pack only when a board is open) |
| Icons / identity | vcad mark in clients |
Publish `io.vcad/*` extensions (selection, agent cursor, receipts) openly.

## Milestones (1.0 = M0–M3; M4 follow-up arc)
- **M0 One core**: crate, framing, both stores, ~10 core tools (open, loon author,
  inspect, render, export), node proxy, `vcad mcp` CLI.
- **M1 Agent cursor (see)**: core embedded in vcad.app; doc/selection/view
  resources; second labeled cursor in the viewport (v1 animates to what the tool
  touched; v2 real camera/selection control); each agent turn = one named undo step.
- **M2 Proof (prove)**: receipts on every claim, live badges that go stale/red on
  edit; elicitation answered in the viewport.
- **M3 One-breath install**: first launch registers vcad with installed agents
  (Claude Code/Desktop, Codex, Cursor) in one consent dialog; skill bundled;
  stale channels removed. Ships right after M1.
- **M4 Touch atoms**: fab as Tasks (Bambu/Carvera), photo-measure loop
  (image-to-CAD), gradient optimize with sensitivity heatmaps; keep porting tools.

## Seams between parallel streams
- M0 owns `crates/vcad-mcp` and the `DocumentStore` trait — land the trait early.
- M1 owns `apple/VcadApp` viewport/undo/doc plumbing and the FFI to `AppStore`.
- M2 owns receipt badge UI + elicitation UI (coordinate files with M1).
- M3 owns install/registration, `skill/`, docs, channel cleanup.
- M4 owns fab/optimize tools as Tasks.

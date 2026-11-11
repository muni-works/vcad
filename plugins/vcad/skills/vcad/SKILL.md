---
name: vcad
description: Design real, manufacturable parts with vcad — a local parametric CAD kernel your agent drives over MCP. Use when the user asks for a 3D part, bracket, enclosure, mechanism, assembly, PCB-adjacent mechanical work, or anything they want to 3D print, machine, or cut. Author in loon, verify with inspect/render/receipts, and the user watches edits land live in vcad.app.
---

# vcad

vcad is a parametric CAD system with its own BRep kernel. It runs entirely on
the user's machine — no accounts, no cloud, no share links. You drive it through
the `vcad` MCP server; the user watches in **vcad.app**.

## The one flow

1. **Open vcad.** The user launches vcad.app. On first launch it registered
   itself with their agents, so you are already attached — the `vcad` tools are
   in your tool list. (No app? The same server runs as `npx -y @vcad/mcp`.)
2. **Author in loon.** Write the whole part as loon source in one
   `create_cad_loon` call. That returns a `document_id`; every later tool takes it.
3. **Verify before you claim.** Measure with `inspect_cad`, look with
   `render_view`, prove with receipts (below). Never describe a dimension you
   haven't measured.
4. **Edits appear live.** In vcad.app, each authoring call shows up in the
   viewport as one named undo step the user can step back through. Under npx,
   hand the result over with `open_in_app` (a `vcad://` link).
5. **Ship it.** `export_cad` writes STL / GLB / STEP; `save_document` writes a
   `.vcad` (plus the `.loon` source) the user owns.

## Loon in 60 seconds

Lisp-like, square brackets, millimetres, **Z-up**. Operations are
*subject-last*, so they thread through `pipe`.

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

- `[cube x y z]` sits with its corner at the origin; `[cylinder r h]` /
  `[cone rb rt h]` stand on the XY plane along +Z; `[sphere r]` is centred.
- Booleans: `[difference tool subject]`, `[union other subject]`.
- Plates with holes: ONE `[polygon outline #[holes]]` extruded — not a stack
  of difference cutters.
- `[defparam name value]` survives into the document — change it later with
  `set_parameters` instead of re-authoring. `[let …]` does not.
- Never hand-mirror by negating coordinates: `[mirror-x s]`,
  `[mirror-pattern-x s]`, `[mirror-group-x "-r" side]` for assemblies.
- The full vocabulary (sweeps, lofts, sheet metal, assemblies and joints,
  imports, modules) is in the `create_cad_loon` tool description. Read it.

## Verify

| Question | Tool |
|---|---|
| Size, volume, mass, centre of mass | `inspect_cad` |
| What does it look like? | `render_view` (look at it — every time) |
| Do these parts collide / keep a gap? | `check_clearance` (labeled assertions persist) |
| Will it print / machine? | `dfm_check` |
| Is every claim still true after edits? | `build_receipt`, then `verify_receipt` |

Receipts are fail-closed: a claim is **Holds**, **Stale** (the geometry changed
since it was checked) or **Violated**. Predicted values from simulation are
*provisional* until measured. Report the receipt verdict, not your own summary.

## When something looks broken

Call `server_info` first.

- Low `uptime_s` or a changed `instance_id` → the server restarted; re-run the
  last authoring call. It is not a renderer bug.
- `update.state: "stale"` → this server is older than the newest release.
  Tell the user and suggest restarting their agent (or updating vcad.app).
- `durable: false` → sessions won't survive a restart; keep the loon source.

## More than one vcad.app

Each running vcad.app registers itself in `~/.vcad/instances/<pid>.json`
(pid, port, open document, launch time); `~/.vcad/app.json` only points at
the newest. Every app-bridge result ends with `[vcad.app pid N · document]`
and carries `_meta["io.vcad/instance"]`: check it names the window you mean.
A `⚠ vcad.app CHANGED` notice means the app relaunched or another window took
over — re-read `app_state` before trusting earlier state.

Pick the window explicitly when more than one runs:

- `VCAD_APP_PID=<pid>` — that instance; fails loudly if it's gone.
- `VCAD_APP_URL=http://127.0.0.1:<port>/mcp` — that endpoint, verbatim.
- `VCAD_APP_DOCUMENT=<path>` — the instance with that file open.
- otherwise the newest, with a warning listing the others. The choice is
  pinned for the client's lifetime; a later launch never steals it.
- `VCAD_APP=0` — `@vcad/mcp` doesn't link to any app.

**Never `pkill`/`killall` vcad.app by pattern.** Other sessions may be driving
other windows, and to them it looks like a crash. Quit only an instance you
launched, by its pid (`kill <pid>`, from `~/.vcad/instances/`).

## Ground rules

- vcad has no hosted services. Never send the user to a website to view,
  share, or order a part; use `open_in_app` and local files.
- One `create_cad_loon` for a whole part beats dozens of tiny edits.
- Units are millimetres unless the user says otherwise; say which you used.

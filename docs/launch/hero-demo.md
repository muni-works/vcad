# Hero demo — "an agent designs a part" (~60 s)

One continuous screen capture of the native Mac app with Claude connected over
the app's local MCP bridge (`vcad-app`, Help ▸ Connect Claude…). No cuts inside
a beat; cuts between beats are fine. The long (~3 min, hold-up-the-part) version
in the launch plan builds on the same beats once M2c and CNC are filmable.

Status key: **NOW** = filmable on main today (2026-11-07) · **GAP** = waits on
another lane · **WORKAROUND** = filmable with the noted substitute.

## Storyboard

| t | Beat | On screen | Tools | Status |
|---|---|---|---|---|
| 0–4 s | Cold open | Dock click → vcad up in ~0.7 s, empty viewport. Lower-third: "free · open · local". | — | **NOW** (speed-pass: shell at 0.63–0.79 s) |
| 4–8 s | Connect | Claude Desktop/Code beside the app; prompt typed: *"Design a wall bracket for a 2 kg shelf load, 4 mm aluminum, two M5 screws."* | Help ▸ Connect Claude… (already done off-camera) | **NOW** |
| 8–22 s | Agent models | Bracket appears and refines live in the viewport — base, flange, holes, fillet. Feature tree fills in. Named undo steps appear. | `create_cad_loon` or engine `create`/booleans, `set_parameters`, `set_camera`, `frame_part` | **NOW** (M2a write tools, #1045) |
| 22–30 s | DFM | Agent runs DFM; viewport flags a thin wall in orange; agent applies the fix, wall thickens. | `dfm_check`, `dfm_apply_fix`, `annotate` | **NOW** (M2b, #1051) |
| 30–40 s | FEA | Agent fixes the screw faces, loads the shelf face; von Mises field on the part, max stress + safety factor called out. | `inspect_faces`, `analyze_structure`, `annotate` | **NOW** — but see FEA note |
| 40–52 s | PCB in enclosure | Agent opens an enclosure with a board in it, checks fit, board drops in, clearance passes green. | `open_document`, `check_interference` (+ M2c board tools) | **GAP → M2c**. **WORKAROUND:** open the bundled *PCB axial-flux motor* sample and run `check_interference` between board and rotors (opens in 0.06 s from its baked bundle). |
| 52–58 s | Export | STEP + STL written; Finder window with the files; optionally the path-traced beauty shot. | app File ▸ Export (agent has no export tool on the app bridge) | **WORKAROUND:** export by menu on camera, or use `npx @vcad/mcp` `export_cad` on the saved file. **GAP → M4a** for "send to CNC/print". `photoreal` needs `libirys_ffi.dylib` bundled (not in this build). |
| 58–60 s | End card | "vcad 1.0 — free for Mac. vcad.io" + "Kernel paper: differentiable BRep" | — | **NOW** (paper link once the arXiv id exists) |

## Capture plan

- **Machine:** the dev Mac, display at 1440p scaled, ProMotion on. Hide the
  dock and menu-bar clutter; Do Not Disturb on. Light appearance (renders read
  better on the site).
- **Build:** `BAKE_SAMPLES=1 CONFIG=release apple/VcadApp/bundle.sh` so every
  sample opens warm; clear `~/.cache/vcad` *after* a rehearsal so the agent's
  bracket solves live but nothing else stalls.
- **Recorder:** macOS Screenshot ▸ Record Selected Portion (or `screencapture -v
  -R x,y,w,h out.mov`) at 60 fps; record the app and the Claude window side by
  side (app ⅔, Claude ⅓). One take per beat; keep the raw .movs.
- **Agent:** Claude Desktop with `vcad-app` configured by Connect Claude. Keep a
  rehearsed prompt file per beat so takes are repeatable; the agent's tool calls
  stay visible in the transcript pane (that's the point).
- **Camera:** let the agent drive (`set_camera`, `frame_part`) — no manual
  orbiting during agent beats; manual orbit only in the cold open.
- **Speed:** real time for modeling and DFM; FEA solve may be sped up 2× with an
  on-screen "2×" tag — never hidden.
- **Audio:** none recorded; music + captions in post.

## Gaps by lane

| Gap | Lane | Needed for |
|---|---|---|
| Board placement / fit tools on the app bridge, board renders in enclosure | **M2c electronics** | Beat 40–52 s as written |
| Agent-callable export (`export_cad` on the app bridge) | M2a follow-up | Beat 52–58 s without a menu click |
| Send to CNC / print, flat pattern | **M4a fab** | Long version (part in hand) |
| Insert the bracket into an assembly with the shelf | **M4b assembly** | Optional long-version beat |
| Path tracer bundled (`libirys_ffi.dylib`) | release build | Beauty-shot end frame |
| FEA convergence on thin parts | kernel | Credible stress numbers on camera |

**FEA note.** The M2b lane found the lattice FEA doesn't converge on a plain
100×10×10 cantilever at resolution ≤ 48 (stress Δ ~22–25 %, displacement
non-monotonic). Film the colour field and the safety-factor call-out, but don't
put a precise MPa number on screen until that's fixed — or pick a bracket chunky
enough that two resolutions agree within a few percent, and check before the
take.

## Allowed claims on screen

Free, open, local; no account, no cloud. Speed numbers only from
`docs/launch/speed-pass.md`. Nothing about tang beyond the approved H200
attention claim (416 TFLOP/s, beating PyTorch SDPA from 2k tokens) — and it
doesn't belong in this video.

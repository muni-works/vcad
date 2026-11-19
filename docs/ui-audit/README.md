# Mac app UI audit — 2026-11-19 (main e3c92f97e)

Audited with `fusor.loon` open, light and dark, released to the desktop, and windowed at
1600×1000 and 820×620. The script is the `VCAD_UI_AUDIT_DIR` driver
(`apple/VcadApp/Sources/VcadApp/UIAuditDemo.swift`):

```bash
open -n apple/VcadApp/dist/vcad.app --env VCAD_UI_AUDIT_DIR=/tmp/audit --env VCAD_AGENT_BRIDGE=0 \
  --args --vcad-open "$PWD/docs/videos/open-fusor/fusor.loon"
```

In each state it walks:
- every dock workspace, plus the CNC studio (`.manufacture`)
- the ⋯ palette on every tab
- the history strip, the shortcut sheet and the command palette
- a selection with Measurements expanded

It also runs Settings and Welcome. For each state it writes one PNG per visible window
(popovers are separate windows) and appends the camera state to `windows.txt`.

Captures come from the window server's own-window image, so glass is included. When the screen
is locked the window server refuses, and the driver falls back to AppKit's cached display: layout
is intact, but glass and the viewport are missing. Which evidence images are real pixels is noted
below.

## The four reported defects

| # | Report (1.0.3-beta.5) | On current main | Outcome |
|---|---|---|---|
| 1 | ⋯ palette clipped left/bottom, tabs truncated | Not reproducible. All 5 tabs and every row are visible in the captures (`palette-design-main.jpg`). Union/Subtract/Intersect are only *disabled* with nothing selected. | Already fixed on main. Three labels were still truncated ("Check Interfere…") → **fixed** |
| 2 | Smeared rectangle in the top bar | Reproduced in released mode: the `hv-feedthrough` tube refracted by the bar's glass | **Fixed**: 94 % solid backing under released glass. A faint ghost of the tube remains at the glass edge. |
| 3 | View cube is noisy multicolour garbage | Reproduced in released mode: the glass refracts the desktop behind it, and the cube faces are 4–12 % opacity | **Fixed**: same backing. Labels are also sized to fit their faces. |
| 4 | `tee` renders spiky / fan-like | Reproduced with a fresh FFI built from main (includes #1261) | **Deferred → kernel** (`tee-spikes-kernel.jpg`; handoff note in the session notes) |

## Every surface

| Surface | Outcome |
|---|---|
| **3D viewport after visiting Drawing or Electronics** | **Fixed (new, major).** On the way back, a second ARView was built, and it drew the scene wrong: near-black, the model gone except for a sliver, the gizmo filling the window. The model's camera state was intact. Confirmed: `makeNSView` ran again on return and `rebuild` saw correct sizes. Not confirmed: exactly where the new view's camera sat. Fix: one viewport for the window's life, which Drawing/Electronics cover with hit-testing off. Input can't leak into the hidden viewport: their full-area chrome regions stop the pass-through raycast, scroll and middle-mouse. |
| Top bar, windowed (sidebar, title, share, ✦, release, inspector toggles) | OK |
| Top bar, released | **Fixed** (defect 2) |
| Model navigator + filter | OK. Long lists scroll; the last row clips at the scroll edge by design. |
| Inspector: empty state, selection, material row | OK |
| Inspector ▸ Measurements expanded | **Fixed**: "508.4 × 262.0 × 415.3 mm" wrapped mid-number in the 170 pt rail. `KeyValueRow` now goes key-above-value when the value doesn't fit on one line. |
| View cube + ring controls | **Fixed** (defect 3) |
| Dock: Design / Assembly / Drawing / Electronics / Fab, released + windowed | OK |
| ⋯ palette, all 5 tabs | **Fixed** label truncation (two lines) |
| History filmstrip | OK |
| Keyboard shortcut sheet | **Fixed**: the model blurred behind the columns; at 820 pt it clipped on both sides (now 4 columns, or 2×2 with a visible scroller); the "Scroll wheel" key chip wrapped |
| Command palette (⇧⌘P) | **Fixed**: the model blurred behind the list |
| Drawing workspace | OK |
| Electronics empty state | OK |
| Fab: Jobs + Sheet Metal panels | OK |
| CNC studio (Labs) | OK at 1600×1000 and released, viewport intact. **Deferred**: at 820×620 its inspector and transport bar are cramped and clipped. It has its own window-minimum logic and is a Labs workspace. |
| Welcome window | **Fixed**: "Version 1.0" was hard-coded (1.1 shipped). The "New in 1.0" strip copy is a release-notes call → deferred |
| Settings ▸ General, Agents, Agent, Renderer | **Fixed**: wrapped footers were ragged-right (General, Agents, Agent). Renderer OK. |
| Settings ▸ Feedback / Labs | OK by construction (same form style); the tab itself wasn't rendered |
| Dock group menus (▾), workspace picker, title details, connection popover | **Deferred**: `@State`-driven and need real input; computer-use access was declined |
| Material picker, context menus, app menus | **Deferred**: system menus, same reason |
| Analysis / sim panels | **Deferred**: need a run; `VCAD_FIELDS_SNAPSHOT_DIR` already covers the Analysis panel |
| visionOS floating palettes (#1269) | Out of scope: owned by that PR |

## Evidence

Real window-server pixels unless marked *layout-only*.

- `viewport-after-drawing-{before,after}.jpg`
- `compass-released-{before,after}.jpg`: after = 94 % backing
- `topbar-released-after.jpg`: tube under the bar, dark + light, 94 %; before = `before-1.0.3-beta5.webp`
- `shortcuts-blur-{before,after}.jpg`
- `shortcuts-small-before.jpg`; `shortcuts-small-after.jpg` (*layout-only*, offscreen render)
- `command-palette-windowed-after.jpg`, `command-palette-released-after.jpg`
- `palette-labels-{before,after}.jpg`, `palette-design-main.jpg`
- `welcome.jpg` (*layout-only*, shows the bundle version; the dev build is 0.1)
- `measurements-after.jpg` (*layout-only*)
- `tee-spikes-kernel.jpg`

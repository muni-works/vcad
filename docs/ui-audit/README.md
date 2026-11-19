# Mac app UI audit — 2026-11-19 (main e3c92f97e)

Done with `fusor.loon` open, in light and dark, at 1600×1000 and 820×620, windowed and
released to the desktop. Each capture is the window server's image of the app's own window,
so glass is included. The script that produced them is the `VCAD_UI_AUDIT_DIR` driver
(`apple/VcadApp/Sources/VcadApp/UIAuditDemo.swift`):

```bash
open -n apple/VcadApp/dist/vcad.app --env VCAD_UI_AUDIT_DIR=/tmp/audit --env VCAD_AGENT_BRIDGE=0 \
  --args --vcad-open "$PWD/docs/videos/open-fusor/fusor.loon"
```

It writes one PNG per visible window per state (popovers are separate windows), plus
`windows.txt`. That file logs each window's content fitting size against its frame, so an
overflowing popover shows up as a number, and logs the camera state at every step. With the
screen locked the window server refuses the capture; the driver then falls back to AppKit's
cached display, which keeps the layout but drops the glass and the viewport.

## The four reported defects

| # | Report (1.0.3-beta.5) | On current main | Outcome |
|---|---|---|---|
| 1 | ⋯ palette clipped left/bottom, tabs truncated | Not reproducible: content fits 380×494, all 5 tabs and every row visible (`palette-design-main.jpg`). Union/Subtract/Intersect are only *disabled* with nothing selected. | Already fixed. Labels still truncated ("Check Interfere…") → **fixed** |
| 2 | Smeared rectangle in the top bar | Reproduced in released mode. It is the `hv-feedthrough` tube refracted by the bar's glass. | **Fixed**: solid backing under released glass |
| 3 | View cube is noisy multicolour garbage | Reproduced in released mode. The glass refracts the desktop behind it, and the cube's faces are 4–12 % opacity. | **Fixed**: same backing; labels also fit their faces now |
| 4 | `tee` renders spiky / fan-like | Reproduced with a fresh FFI built from main (includes #1261) | **Deferred → kernel** (`tee-spikes-kernel.jpg`, handoff note in the session notes) |

## Every surface

| Surface | Light / dark · large / small | Outcome |
|---|---|---|
| **Viewport after visiting Drawing or Electronics** | all | **Fixed (new, major)**: coming back built a second ARView that rendered from the scene origin, inside the model, black with a giant gizmo. One viewport now lives for the window's life. |
| Top bar, windowed (sidebar, title, share, ✦, release, inspector toggles) | all | OK |
| Top bar, released | both | **Fixed** (defect 2) |
| Model navigator + filter | all | OK (long lists scroll; the last row clips at the scroll edge as intended) |
| Inspector: empty state, selection, Measurements disclosure | all | OK |
| View cube + ring controls | all | **Fixed** (defect 3; labels sized to their faces) |
| Dock: Design / Assembly / Drawing / Electronics / Fab | all | OK |
| ⋯ palette, all 5 tabs | all | **Fixed** label truncation (2 lines). Overflow: none (`windows.txt`) |
| History filmstrip | all | OK |
| Keyboard shortcut sheet | all | **Fixed**: model blurred behind the columns; clipped on both sides in a small window (now 4 columns or 2×2 + scroll); "Scroll wheel" chip wrapped |
| Command palette (⇧⌘P) | all | **Fixed**: model blurred behind the list |
| Drawing workspace (sheet, views, sheet panel) | all | OK |
| Electronics empty state | all | OK |
| Fab: Jobs + Sheet Metal panels | all | OK |
| Welcome window | both | **Fixed**: "Version 1.0" was hard-coded (1.1 shipped). The "New in 1.0" strip copy is a release-notes call → deferred |
| Settings ▸ General | both | **Fixed**: wrapped footers were ragged-right |
| Settings ▸ Feedback / Agents / Agent / Labs / Renderer | — | Deferred: the TabView selection is not drivable offscreen |
| Dock group menus (▾ on cube/fillet/boolean), workspace picker, title details, connection popover | — | Deferred: `@State`-driven, needs real input; computer-use access was declined |
| Material picker, context menus, app menus | — | Deferred: system menus, same reason |
| Analysis / sim panels | — | Deferred: not reachable without a run; `VCAD_FIELDS_SNAPSHOT_DIR` already covers the Analysis panel |
| Release-to-desktop floating palettes (visionOS, #1269) | — | Out of scope: owned by that PR |

## Evidence

- `viewport-after-drawing-{before,after}.jpg`
- `compass-released-{before,after}.jpg`
- `topbar-released-after.jpg`: tube under the bar; before = `before-1.0.3-beta5.webp`
- `shortcuts-blur-{before,after}.jpg`, `shortcuts-small-{before,after}.jpg`
- `palette-labels-{before,after}.jpg`, `palette-design-main.jpg`
- `welcome.jpg`
- `tee-spikes-kernel.jpg`

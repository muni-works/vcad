# vcad for Apple Vision Pro

A SwiftPM visionOS app that compiles the Mac app's whole source tree
(`Sources/VcadVision/App` is a symlink to `../VcadApp/Sources/VcadApp`, minus a short
Mac-only list in `Package.swift`, the same scheme as `../VcadPhone`) and adds the spatial
pieces:

- `DocumentVolume.swift`: the windowed mode. One volumetric window wearing the Mac's own
  chrome on ornaments: `GlassToolbar` on top, `ReleasedLeftRail` (model navigator) and
  `ReleasedRightRail` (inspector) either side, `DockHost` along the bottom.
- `ReleasedSpace.swift` + `ReleasedPanels.swift`: **release to room**, the Vision Pro version
  of the Mac's release to desktop. The document's bodies leave the volume for a mixed
  `ImmersiveSpace` at real scale (1 mm = 1 mm), and the same Mac chrome floats beside them
  as windows. The toolbar's own Release / Open in Window button drives both directions
  (`ReleaseWindowController.setWindowed`, as on the Mac).
- `PlatformShims.swift` (from the iPhone port, visionOS deltas marked),
  `ReleaseControllerVision.swift` (the controller's visionOS half: no 2D screen to
  project to) and `VisionDocumentFlow.swift` (open / save / export via document pickers).

## Build and run (simulator)

```bash
# once: a visionOS 26 simulator (the old Apple-Vision-Pro device type won't pair with it)
xcrun simctl create "Apple Vision Pro" com.apple.CoreSimulator.SimDeviceType.Apple-Vision-Pro-4K com.apple.CoreSimulator.SimRuntime.xrOS-26-5
xcrun simctl boot "Apple Vision Pro"
```

```bash
./build-ffi.sh   # vcad-ffi for aarch64-apple-visionos-sim: nightly + build-std, ~15 min cold
```

```bash
VCAD_OPEN=$PWD/../VcadApp/Tests/Fixtures/web-examples/mascot.vcad ./bundle.sh
```

Headless dev hooks, forwarded by `bundle.sh` into the simulator app:
`VCAD_RELEASE=1` releases into the room on launch, `VCAD_RELEASE_SELECT=<part>` selects a
part, `VCAD_RELEASE_MOVE=<part>:<x>,<y>,<z>` carries it that many mm, and
`VCAD_RELEASE_RETURN=<s>` returns to the window after that many seconds.

## Release to room: ported vs stubbed

| | Vision Pro |
|---|---|
| Bodies dressed like the Mac (studio-softened PBR, own emission, selection lift, entity naming) | ported (shared `ReleasedLook.dressPart` / `dressInstance`) |
| Selection repaint without rebuilds | ported (shared `repaintAll`) |
| Parameter scrub → in-place mesh swap | ported (shared `refreshGeometry`) |
| Parts float over the desktop | ported as a mixed ImmersiveSpace at real scale, with grounding shadows |
| Hover highlight | system gaze highlight (`HoverEffectComponent`); apps can't read gaze |
| Pick a body / add to selection | look and tap / long-press (not yet exercised in the simulator) |
| Move a body | pinch and drag carries it through the room; two-hand pinch scales the release; Reset Layout restores 1:1. Moves are presentation only, not written to the document. The gestures have not been exercised in the simulator yet; screenshots used the `VCAD_RELEASE_MOVE` hook |
| Toolbar, dock, model navigator, inspector | the Mac's own views (`GlassToolbar`, `DockHost`, `ReleasedLeftRail`, `ReleasedRightRail`), on ornaments in the volume and as windows in the room, in visionOS glass |
| Release / return | the toolbar's Release / Open in Window button, as on the Mac |
| Glow / HDR bloom | emissive surfaces ported; the bloom halo (an ARView post-process) is stubbed, and emitters cast a point light instead |
| Feature edges | ribbons (no `CustomMaterial` screen-space line shader on visionOS) |
| Transform gizmo, sketch ink, face/edge picking, direct-manipulation handles | stubbed |
| Agent viewport snapshots and pins, history thumbnails | no ARView to snapshot (empty) |

Known limits:
- Closing the toolbar window with the system close button leaves you in the room without the Open in Window button (exit with the Digital Crown).
- On return, the volume reopens wherever visionOS places it.

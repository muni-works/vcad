# vcad for Apple Vision Pro

A SwiftPM visionOS app that shares the Mac app's editor (`Sources/VcadVision/Shared/*`
are symlinks into `../VcadApp/Sources/VcadApp`) and adds the spatial pieces:

- `DocumentVolume.swift`: the windowed mode. One volumetric window with panels on ornaments.
- `ReleasedSpace.swift` + `ReleasedPanels.swift`: **release to room**, the Vision Pro version
  of the Mac's release to desktop. The document's bodies leave the volume for a mixed
  `ImmersiveSpace` at real scale (1 mm = 1 mm). The Component Palette and the Object
  Inspector float beside them as their own windows, and Return to Window brings the
  volume back.
- `PlatformShims.swift` / `VisionStubs.swift`: AppKit spellings and stand-ins for
  subsystems that only exist on the Mac.

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
| Bodies dressed like the Mac (studio-softened PBR, own emission, selection lift, entity naming) | ported (shared `dressPart` / `dressInstance`) |
| Selection repaint without rebuilds | ported (shared `repaintAll`) |
| Parameter scrub → in-place mesh swap | ported (shared `refreshGeometry`) |
| Parts float over the desktop | ported as a mixed ImmersiveSpace at real scale, with grounding shadows |
| Hover highlight | system gaze highlight (`HoverEffectComponent`); apps can't read gaze |
| Pick a body / add to selection | look and tap / long-press |
| Move a body | pinch and drag carries it through the room; two-hand pinch scales the release; Reset Layout restores 1:1. Moves are presentation only, not written to the document |
| Component Palette, Object Inspector | ported as floating windows (`ObjectInspectorWindow` is the Mac's view) |
| Return to window | ported (Return to Window in the palette) |
| Glow / HDR bloom | emissive surfaces ported; the bloom halo (an ARView post-process) is stubbed, and emitters cast a point light instead |
| Feature edges | ribbons (no `CustomMaterial` screen-space line shader on visionOS) |
| Transform gizmo, sketch ink, face/edge picking, direct-manipulation handles | stubbed |
| Agent bridge, path tracer, loon hot reload (FSEvents), autosave/Recents, CNC/electronics/FEA field views | stubbed (`VisionStubs.swift`) |

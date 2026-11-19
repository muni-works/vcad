// swift-tools-version: 6.0
import PackageDescription

// vcad on iPhone — the full editor, not a viewer.
//
// The app target compiles the WHOLE Mac source tree (Sources/VcadPhone/App is
// a symlink to ../VcadApp/Sources/VcadApp) minus the files below, plus the
// phone's own shell: PlatformShims (AppKit spellings over UIKit), the phone
// viewport + its ReleaseWindowController stand-in, and the phone layout.
//
// SwiftPM has no iOS app product, so bundle.sh drives xcodebuild against the
// iOS Simulator and assembles the .app by hand (the VcadVision trick).
// Libs/VcadFFI.xcframework comes from build-ffi.sh.

/// Mac-only files left out of the iPhone build. Everything else is shared.
let macOnly = [
    "VcadApp.swift",          // the Mac @main (window, menus, dock)
    "Updater.swift",          // Sparkle auto-update (the Mac app ships outside the store)
    "ReleasedDesktop.swift",  // the Mac's borderless editor window + NSView viewport
    "WindowDragRegion.swift", // NSView titlebar drag
    "WelcomeWindow.swift",    // NSWindow welcome
    "FieldsSnapshots.swift",  // headless NSWindow snapshot harness
    "AnalysisSmoke.swift", "NavSmoke.swift", "RuntimeBench.swift",  // Mac dev harnesses
].map { "App/\($0)" }

let package = Package(
    name: "VcadPhone",
    platforms: [.iOS(.v18)],
    targets: [
        // Kernel static lib (device + simulator slices) with its modulemap.
        .binaryTarget(name: "CVcadFFI", path: "Libs/VcadFFI.xcframework"),
        // irys's C types (header only; the path tracer it serves is Mac-only).
        .systemLibrary(name: "CIrys", path: "Sources/CIrys"),
        .executableTarget(
            name: "VcadPhone",
            dependencies: ["CVcadFFI", "CIrys"],
            path: "Sources/VcadPhone",
            exclude: macOnly,
            linkerSettings: [
                .linkedLibrary("c++"),
                .linkedLibrary("lzma"),
            ]
        ),
    ]
)

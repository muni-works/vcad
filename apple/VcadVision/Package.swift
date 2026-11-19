// swift-tools-version: 6.0
import PackageDescription
import Foundation

// visionOS spike. SwiftPM can't express a visionOS app target, so bundle.sh
// drives swift-build with an explicit -target triple + xrsimulator SDK and
// assembles the .app by hand (same trick as ../VcadApp/bundle.sh, one level up
// in ambition). Libs/ holds libvcad_ffi.a built for aarch64-apple-visionos-sim
// by build-ffi.sh.
let ffiLibDir = URL(fileURLWithPath: #filePath)
    .deletingLastPathComponent()
    .appendingPathComponent("Libs")
    .path

/// Mac-only files left out (the iPhone port's list, VcadPhone/Package.swift,
/// minus ReleasedDesktop.swift: visionOS shares its scene half — the AppKit
/// half is behind `#if os(macOS)` — and the room viewport is ReleasedSpace).
let macOnly = [
    "VcadApp.swift",          // the Mac @main (window, menus, dock)
    "Updater.swift",          // Sparkle auto-update
    "WindowDragRegion.swift", // NSView titlebar drag
    "WelcomeWindow.swift",    // NSWindow welcome
    "FieldsSnapshots.swift",  // headless NSWindow snapshot harness
    "AnalysisSmoke.swift", "NavSmoke.swift", "RuntimeBench.swift",  // Mac dev harnesses
].map { "App/\($0)" }

let package = Package(
    name: "VcadVision",
    platforms: [.visionOS(.v2)],
    targets: [
        .systemLibrary(name: "CVcadFFI", path: "Sources/CVcadFFI"),
        // irys's C types (header only; the path tracer it serves is Mac-only).
        .systemLibrary(name: "CIrys", path: "Sources/CIrys"),
        .executableTarget(
            name: "VcadVision",
            dependencies: ["CVcadFFI", "CIrys"],
            path: "Sources/VcadVision",
            exclude: macOnly,
            linkerSettings: [
                .unsafeFlags(["-L", ffiLibDir]),
                .linkedLibrary("c++"),
                .linkedLibrary("lzma"),
            ]
        ),
    ]
)

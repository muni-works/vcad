import XCTest
import SwiftUI
import AppKit
@testable import VcadApp

/// Renders the navigation surfaces offscreen to PNGs for review. Opt-in:
/// VCAD_NAV_SNAPSHOTS=<dir> swift test --filter NavigationSnapshotTests
@MainActor
final class NavigationSnapshotTests: XCTestCase {
    private func write<V: View>(_ view: V, _ name: String, dir: String) {
        let host = NSHostingView(rootView: view.environment(\.colorScheme, .dark))
        host.appearance = NSAppearance(named: .darkAqua)
        let size = host.fittingSize
        host.frame = CGRect(origin: .zero, size: size)
        let win = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        win.contentView = host
        host.layoutSubtreeIfNeeded()
        RunLoop.main.run(until: Date().addingTimeInterval(0.4))
        guard let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds) else { return XCTFail(name) }
        host.cacheDisplay(in: host.bounds, to: rep)
        guard let png = rep.representation(using: .png, properties: [:]) else { return XCTFail(name) }
        try? png.write(to: URL(fileURLWithPath: dir).appendingPathComponent(name + ".png"))
    }

    func testRenderSurfaces() throws {
        guard let dir = ProcessInfo.processInfo.environment["VCAD_NAV_SNAPSHOTS"] else { throw XCTSkip("opt-in") }
        let model = EditorModel()
        let bg = Color(white: 0.16)
        model.azimuth = .pi / 5; model.elevation = .pi / 7
        write(CompassView(model: model).padding(24).background(bg), "compass-iso", dir: dir)
        model.azimuth = 0; model.elevation = 0
        write(CompassView(model: model).padding(24).background(bg), "compass-front", dir: dir)
        NavigationUI.shared.recents = RecentCommands(ids: ["view.0,-1,0", "measure"])
        write(CommandPaletteSheet(model: model).frame(height: 380).background(bg), "find", dir: dir)
        let nav = NavigationUI.shared
        nav.openHalo(at: CGPoint(x: 280, y: 170), model: model, marking: true)
        nav.haloSlot = 2
        write(HaloView().frame(width: 560, height: 340).background(bg), "halo", dir: dir)
        nav.closeHalo()
    }
}

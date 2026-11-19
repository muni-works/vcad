#if os(macOS)
import AppKit
import SwiftUI

// MARK: - Scripted UI audit (VCAD_UI_AUDIT_DIR)

/// `VCAD_UI_AUDIT_DIR=/path open vcad.app --args --vcad-open part.vcad`
/// walks every workspace, the ··· palette on every tab, the history strip,
/// the shortcut sheet and command search, in light and dark, at a small and a
/// large window size, and writes a PNG of every visible app window per state
/// (popovers are their own windows). Each window also logs its content's
/// fitting size against its frame, so a popover whose content overflows is
/// caught as a number, not just by eye. Reproducible without screen-recording
/// rights: it only captures this process's own windows.
@MainActor
enum UIAuditDemo {
    static func runIfRequested(_ m: EditorModel) async {
        guard let dir = ProcessInfo.processInfo.environment["VCAD_UI_AUDIT_DIR"] else { return }
        let url = URL(fileURLWithPath: dir)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        func wait(_ s: Double) async { try? await Task.sleep(nanoseconds: UInt64(s * 1e9)) }
        for _ in 0..<160 where m.docPartMeshes.isEmpty { await wait(0.25) }
        await wait(2)
        let ctl = ReleaseWindowController.shared
        let ui = GlassUI.shared
        var log = ""

        // Offscreen: the shortcut sheet laid out in a large and a small window.
        for (name, width) in [("large", CGFloat(1600)), ("small", CGFloat(820))] {
            let host = NSHostingView(rootView: ShortcutSheet(model: m).frame(width: width))
            host.frame = NSRect(x: 0, y: 0, width: width, height: 900)
            host.frame.size.height = host.fittingSize.height
            let win = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
            win.appearance = NSAppearance(named: .darkAqua)
            win.contentView = host
            host.layoutSubtreeIfNeeded(); host.display()
            if let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds) {
                host.cacheDisplay(in: host.bounds, to: rep)
                try? rep.representation(using: .png, properties: [:])?
                    .write(to: url.appendingPathComponent("offscreen-shortcuts-\(name).png"))
            }
            win.contentView = nil
        }
        if ProcessInfo.processInfo.environment["VCAD_UI_AUDIT_QUICK"] == "1" {
            FileHandle.standardError.write(Data("[VCAD_UI_AUDIT] done → \(dir)\n".utf8))
            return
        }

        func snap(_ name: String) async {
            RenderGate.wake(for: 2)
            await wait(0.9)
            log += String(format: "%@ CAMERA ws=%@ dist=%.3f scale=%.5f pan=(%.3f,%.3f,%.3f) parts=%d\n", name,
                          m.workspace.rawValue, m.distance, m.displayScale,
                          m.panOffset.x, m.panOffset.y, m.panOffset.z, m.docPartMeshes.count)
            for (i, w) in NSApp.windows.enumerated() where w.isVisible && w.frame.width > 2 {
                let tag = "\(name)-w\(i)-\(type(of: w))"
                if let png = capture(w) { try? png.write(to: url.appendingPathComponent(tag + ".png")) }
                let fit = w.contentView?.fittingSize ?? .zero
                let size = w.contentView?.bounds.size ?? .zero
                let over = fit.width > size.width + 0.5 || fit.height > size.height + 0.5
                log += String(format: "%@ frame=%.0fx%.0f fitting=%.0fx%.0f%@\n", tag,
                              size.width, size.height, fit.width, fit.height, over ? " OVERFLOW" : "")
            }
        }

        // Released to the desktop first: chrome floats over whatever is behind.
        ctl.setWindowed(false)
        for (scheme, appearance) in [("dark", NSAppearance.Name.darkAqua), ("light", .aqua)] {
            NSApp.appearance = NSAppearance(named: appearance)
            let d = m.distance
            await snap("released-\(scheme)")
            m.distance = d * 0.32
            await snap("released-\(scheme)-close")
            ui.paletteOpen = true; await snap("released-\(scheme)-palette"); ui.paletteOpen = false
            m.distance = d
        }

        let small = NSSize(width: 820, height: 620), large = NSSize(width: 1600, height: 1000)
        ctl.setWindowed(true)
        for (scheme, appearance) in [("dark", NSAppearance.Name.darkAqua), ("light", .aqua)] {
            NSApp.appearance = NSAppearance(named: appearance)
            for (sizeName, size) in [("large", large), ("small", small)] {
                if let w = ctl.hostWindow {
                    var f = w.frame; f.size = size
                    w.setFrame(f, display: true)
                }
                let p = "\(scheme)-\(sizeName)"
                for space in DockSpace.allCases {
                    ui.enter(space, model: m)
                    await snap("\(p)-\(space.rawValue)")
                }
                ui.enter(.design, model: m)
                for space in DockSpace.allCases {
                    ui.space = space
                    ui.paletteOpen = true
                    await snap("\(p)-palette-\(space.rawValue)")
                    ui.paletteOpen = false
                    await wait(0.3)
                }
                ui.space = .design
                ui.historyOpen = true; await snap("\(p)-history"); ui.historyOpen = false
                ui.shortcutsShown = true; await snap("\(p)-shortcuts"); ui.shortcutsShown = false
                m.showsCommandPalette = true; await snap("\(p)-command-palette"); m.showsCommandPalette = false
                m.selectParts([0]); await snap("\(p)-selected"); m.selectParts([])
            }
        }
        // Settings and the welcome window, in both appearances.
        for (scheme, appearance) in [("dark", NSAppearance.Name.darkAqua), ("light", .aqua)] {
            NSApp.appearance = NSAppearance(named: appearance)
            // Settings in an offscreen window (a background app's
            // showSettingsWindow: opens nothing).
            let host = NSHostingView(rootView: SettingsView())
            host.frame = NSRect(x: 0, y: 0, width: 600, height: 800)
            let fit = host.fittingSize
            host.frame = NSRect(origin: .zero, size: fit)
            let win = NSWindow(contentRect: host.frame, styleMask: [.titled], backing: .buffered, defer: false)
            win.appearance = NSAppearance(named: appearance)
            win.contentView = host
            host.layoutSubtreeIfNeeded(); host.display()
            if let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds) {
                host.cacheDisplay(in: host.bounds, to: rep)
                try? rep.representation(using: .png, properties: [:])?
                    .write(to: url.appendingPathComponent("\(scheme)-settings.png"))
            }
            log += String(format: "%@-settings fitting=%.0fx%.0f\n", scheme, fit.width, fit.height)
            win.contentView = nil
            WelcomeWindowController.shared.show(model: m)
            await snap("\(scheme)-welcome")
        }
        NSApp.appearance = nil
        try? log.write(to: url.appendingPathComponent("windows.txt"), atomically: true, encoding: .utf8)
        FileHandle.standardError.write(Data("[VCAD_UI_AUDIT] done → \(dir)\n".utf8))
    }

    /// The window as the window server composites it (glass included) when
    /// that works for our own window, else AppKit's cached display of the
    /// whole frame view (no glass backdrop, but every control and label).
    private static func capture(_ w: NSWindow) -> Data? {
        typealias Capture = @convention(c) (CGRect, UInt32, UInt32, UInt32) -> Unmanaged<CGImage>?
        if let sym = dlsym(UnsafeMutableRawPointer(bitPattern: -2), "CGWindowListCreateImage") {
            let fn = unsafeBitCast(sym, to: Capture.self)
            if let img = fn(.null, 1 << 3, UInt32(w.windowNumber), 1 << 0)?.takeRetainedValue(),
               img.width > 1, !isBlank(img) {
                return NSBitmapImageRep(cgImage: img).representation(using: .png, properties: [:])
            }
        }
        guard let view = w.contentView?.superview ?? w.contentView,
              let rep = view.bitmapImageRepForCachingDisplay(in: view.bounds) else { return nil }
        view.cacheDisplay(in: view.bounds, to: rep)
        return rep.representation(using: .png, properties: [:])
    }

    /// True when every sampled pixel is fully transparent (a capture the
    /// window server refused without saying so).
    private static func isBlank(_ img: CGImage) -> Bool {
        let W = 32, H = 32
        guard let ctx = CGContext(data: nil, width: W, height: H, bitsPerComponent: 8, bytesPerRow: W * 4,
                                  space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue),
              let data = ctx.data else { return true }
        ctx.draw(img, in: CGRect(x: 0, y: 0, width: W, height: H))
        let px = data.bindMemory(to: UInt8.self, capacity: W * H * 4)
        for i in 0..<(W * H) where px[i * 4 + 3] != 0 { return false }
        return true
    }
}
#endif

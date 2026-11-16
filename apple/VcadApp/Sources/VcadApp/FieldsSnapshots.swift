import AppKit
import SwiftUI

// VCAD_FIELDS_SNAPSHOT_DIR=/path: open a cut-away hobby fusor, run every
// Analysis → Fields screen through the app's own paths (FieldsStore →
// SolverFFI → vcad-solver-tools) and write, per screen:
//   <n>-<name>-panel.png     the Analysis panel, rendered by AppKit
//   <n>-<name>-viewport.png  the live viewport at native resolution (the
//                            section quad on the device's y = 0 plane);
//                            only with VCAD_FIELDS_SNAPSHOT_HALVES=1
//   <n>-<name>.png           a 1920×1080 frame: viewport + panel on the rail
// Reproducible without screen-recording rights — the docs/videos screens.
// The bench readings are EXAMPLE values, labeled as such on screen: nothing
// has been built or measured yet.

@MainActor
enum FieldsSnapshots {
    /// The cut-away device: half a 6" chamber (y ≥ 0 kept, so the y = 0
    /// section is open to the camera), the 4-ring 1.5" grid, the stalk.
    static let fusorLoon = """
    ; Open-fusor hobby device, cut away on the y = 0 section plane.
    [let outer [translate 0 0 -79 [cylinder 79 158]]]
    [let inner [translate 0 0 -76 [cylinder 76 152]]]
    [let front [translate -100 -100 -100 [cube 200 100 200]]]
    [root [difference inner front outer] "steel"]
    [root [translate 0 0 15.371 [torus 11.168 1]] "steel"]
    [root [translate 0 0 5.871 [torus 18.070 1]] "steel"]
    [root [translate 0 0 -5.871 [torus 18.070 1]] "steel"]
    [root [translate 0 0 -15.371 [torus 11.168 1]] "steel"]
    [root [translate 0 0 19 [cylinder 2.5 57]] "steel"]
    """

    static func runIfRequested(_ m: EditorModel) async {
        guard let dir = ProcessInfo.processInfo.environment["VCAD_FIELDS_SNAPSHOT_DIR"] else { return }
        let url = URL(fileURLWithPath: dir)
        // Dark, whatever the desktop is doing: the panel renders dark too.
        NSApp.appearance = NSAppearance(named: .darkAqua)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        func log(_ s: String) { FileHandle.standardError.write(Data("[VCAD_FIELDS] \(s)\n".utf8)) }
        func wait(_ s: Double) async { try? await Task.sleep(nanoseconds: UInt64(s * 1e9)) }

        do {
            let json = try AgentEdits.compileLoon(fusorLoon)
            try AgentEdits.edit(m, name: "Open fusor") { try $0.loadIR(json) }
        } catch { log("load failed: \(AgentEdits.describe(error))") }
        for _ in 0..<120 where m.docPartMeshes.isEmpty { await wait(0.25) }
        await wait(1.5)
        if let w = ReleaseWindowController.shared.arView?.window, ProcessInfo.processInfo.environment["VCAD_FIELDS_KEEP_FRAME"] == nil {
            w.setFrame(NSRect(x: 0, y: 0, width: 1920, height: 1080), display: true)
        }
        m.showsInspector = true
        let analysis = AnalysisStore.shared, fs = FieldsStore.shared
        analysis.shown = true
        analysis.tab = .fields
        analysis.overlayRevision += 1

        var n = 0
        func snap(_ name: String, _ tab: FieldsTab) async {
            n += 1
            let base = "\(n)-\(name)"
            // Re-assert the screen: the window shares a live desktop, and a
            // stray click must not change what the frame shows.
            func pin() { analysis.shown = true; analysis.tab = .fields; fs.tab = tab; m.showsInspector = true }
            pin()
            analysis.overlayRevision += 1
            m.resetCamera()
            RenderGate.wake()
            await wait(1.0)
            // Re-pin right before the capture: the window shares a live screen.
            pin()
            m.resetCamera()
            RenderGate.wake()
            await wait(0.3)
            log(String(format: "camera az %.3f el %.3f dist %.3f pan (%.3f %.3f %.3f)", m.azimuth, m.elevation, m.distance,
                       m.panOffset.x, m.panOffset.y, m.panOffset.z))
            let viewport = await viewportImage()
            let panel = panelImage(m)
            if let viewport, ProcessInfo.processInfo.environment["VCAD_FIELDS_SNAPSHOT_HALVES"] != nil {
                write(viewport, url.appendingPathComponent("\(base)-viewport.png"))
            }
            if let panel { write(panel, url.appendingPathComponent("\(base)-panel.png")) }
            if let frame = compose(viewport: viewport, panel: panel) { write(frame, url.appendingPathComponent("\(base).png")) }
            log("wrote \(base)")
        }

        // 1. Ions: fields + tracks over the section, FoMs, voltage sweep.
        fs.tab = .ions
        let t0 = Date()
        await fs.simulate(sweep: true)
        fs.designExpanded = false
        log(String(format: "simulate %.1fs  %@", Date().timeIntervalSince(t0), fs.particleError ?? "ok"))
        if let p = fs.particle {
            log(String(format: "prediction floor %.3e ceiling %.3e interception %.3f passes %.2f",
                       p.prediction.floor_n_per_s ?? -1, p.prediction.ceiling_n_per_s, p.stats.interception_fraction, p.stats.mean_passes))
        }
        await snap("ions", .ions)
        // The sweep, with the panel's copy of the section folded away.
        fs.sectionInPanel = false
        await snap("sweep", .ions)
        fs.sectionInPanel = true

        // 2. Optimizer over the grid radius.
        fs.tab = .optimize
        fs.optVariables = ["grid_r"]
        await fs.runOptimizer()
        log("optimize live stream \(fs.optLive.count) evaluations parsed vs \(fs.optimize?.evals ?? -1) run")
        log("optimize \(fs.optimize.map { String(format: "best %.3e at grid_r %.2f, %d evals", $0.best_neutron_rate_n_per_s, $0.best_params["grid_r"] ?? -1, $0.evals) } ?? fs.optError ?? "-")")
        await snap("optimize", .optimize)

        // 3. Dose vs distance behind the HDPE castle.
        fs.tab = .dose
        await fs.simulateDose()
        log("dose \(fs.dose.map { "\($0.detectors.count) detectors, budget pass \($0.budget?.pass ?? false)" } ?? fs.doseError ?? "-")")
        await snap("dose", .dose)

        // 4. Bench: EXAMPLE readings near the prediction → Holds.
        fs.tab = .bench
        let floor = fs.particle?.prediction.neutron_rate_n_per_s ?? 2.5e4
        let dose1m = fs.dose?.detectors.first { $0.label == "1 m" }?.dose_usv_per_h ?? 1
        fs.bench.neutronRate = String(format: "%.3g", floor * 1.6)
        fs.bench.neutronSigma = String(format: "%.2g", floor * 0.25)
        fs.bench.neutronInstrument = "EXAMPLE reading — nothing measured yet"
        fs.bench.interception = String(format: "%.2f", min(0.97, (fs.particle?.stats.interception_fraction ?? 0.78) * 0.96))
        fs.bench.dose = String(format: "%.3g", dose1m * 1.2)
        fs.bench.doseSigma = String(format: "%.2g", dose1m * 0.1)
        fs.bench.doseDetector = "1 m"
        fs.bench.doseInstrument = "EXAMPLE reading — nothing measured yet"
        await fs.compare()
        log("compare particle \(fs.compareParticle?.measured_verdict ?? fs.compareError ?? "-") dose \(fs.compareDose?.measured_verdict ?? "-")")
        await snap("bench-holds", .bench)

        // 5. The same screen when the machine under-performs → Violated.
        fs.bench.neutronRate = String(format: "%.3g", floor / 8)
        fs.bench.neutronSigma = String(format: "%.2g", floor / 40)
        await fs.compare()
        log("compare (low) particle \(fs.compareParticle?.measured_verdict ?? fs.compareError ?? "-")")
        await snap("bench-violated", .bench)

        log("done → \(dir)")
        if ProcessInfo.processInfo.environment["VCAD_FIELDS_SNAPSHOT_EXIT"] != nil { exit(0) }
    }

    private static func viewportImage() async -> NSImage? {
        guard let ar = ReleaseWindowController.shared.arView else { return nil }
        return await withCheckedContinuation { (k: CheckedContinuation<NSImage?, Never>) in
            ar.snapshot(saveToHDR: false) { k.resume(returning: $0) }
        }
    }

    /// The Analysis panel rendered by AppKit in an offscreen window (real
    /// controls and charts, unlike ImageRenderer's placeholders).
    private static func panelImage(_ m: EditorModel) -> NSImage? {
        let root = AnalysisPanel(model: m)
            .padding(Theme.Space.s)
            .background(Color(nsColor: .underPageBackgroundColor))
            .environment(\.colorScheme, .dark)
        let host = NSHostingView(rootView: root)
        host.frame = NSRect(x: 0, y: 0, width: Theme.Width.analysisWide + 2 * Theme.Space.s, height: 1200)
        let size = host.fittingSize
        host.frame = NSRect(origin: .zero, size: size)
        let win = NSWindow(contentRect: host.frame, styleMask: [.borderless], backing: .buffered, defer: false)
        win.appearance = NSAppearance(named: .darkAqua)
        win.contentView = host
        host.layoutSubtreeIfNeeded()
        host.display()
        guard let rep = host.bitmapImageRepForCachingDisplay(in: host.bounds) else { return nil }
        host.cacheDisplay(in: host.bounds, to: rep)
        let img = NSImage(size: size)
        img.addRepresentation(rep)
        win.contentView = nil
        return img
    }

    /// 1920×1080: the viewport filling the frame, the panel on the right rail.
    private static func compose(viewport: NSImage?, panel: NSImage?) -> NSImage? {
        let W: CGFloat = 1920, H: CGFloat = 1080
        guard let rep = NSBitmapImageRep(bitmapDataPlanes: nil, pixelsWide: Int(W), pixelsHigh: Int(H), bitsPerSample: 8,
                                         samplesPerPixel: 4, hasAlpha: true, isPlanar: false, colorSpaceName: .deviceRGB,
                                         bytesPerRow: 0, bitsPerPixel: 0) else { return nil }
        NSGraphicsContext.saveGraphicsState()
        NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
        NSColor(calibratedWhite: 0.11, alpha: 1).setFill()
        NSRect(x: 0, y: 0, width: W, height: H).fill()
        if let v = viewport {
            // Aspect-fill the viewport.
            let s = max(W / v.size.width, H / v.size.height)
            let sz = NSSize(width: v.size.width * s, height: v.size.height * s)
            v.draw(in: NSRect(x: (W - sz.width) / 2, y: (H - sz.height) / 2, width: sz.width, height: sz.height))
        }
        if let p = panel {
            let margin: CGFloat = 24
            let s = min(1, (H - 2 * margin) / p.size.height)
            let sz = NSSize(width: p.size.width * s, height: p.size.height * s)
            let r = NSRect(x: W - sz.width - margin, y: H - sz.height - margin, width: sz.width, height: sz.height)
            let path = NSBezierPath(roundedRect: r, xRadius: Theme.Radius.panel, yRadius: Theme.Radius.panel)
            NSGraphicsContext.saveGraphicsState()
            path.addClip()
            p.draw(in: r)
            NSGraphicsContext.restoreGraphicsState()
            NSColor(white: 1, alpha: 0.12).setStroke()
            path.lineWidth = 1
            path.stroke()
        }
        NSGraphicsContext.restoreGraphicsState()
        let img = NSImage(size: NSSize(width: W, height: H))
        img.addRepresentation(rep)
        return img
    }

    private static func write(_ img: NSImage, _ url: URL) {
        guard let tiff = img.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff),
              let png = rep.representation(using: .png, properties: [:]) else { return }
        try? png.write(to: url)
    }
}

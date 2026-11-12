#if os(macOS)
import AppKit
import CVcadFFI
import Foundation
import SwiftUI
import UniformTypeIdentifiers
import simd

// The Welcome window: a left rail of actions, then Recent (returning users),
// examples that each teach something, a "Bring your agent" card with live
// status, and a "New in 1.0" strip. Pure logic lives in WelcomeModel.swift.
//
// Layering: in released-to-desktop mode the model is a screen-sized
// borderless window at the normal level. A plain sibling window loses to it
// whenever that window is ordered front (launch, a click on the model, a
// windowed/released switch), so the Welcome window is attached as a CHILD of
// the editor window: AppKit always keeps a child above its parent.

@MainActor
final class WelcomeWindowController: NSObject, NSWindowDelegate {
    static let shared = WelcomeWindowController()
    private var window: NSWindow?
    private weak var model: EditorModel?
    private var describe: (() -> Void)?

    /// First launch of a scratch instance, outside the headless dev hooks.
    static func shouldShowAtLaunch(model: EditorModel,
                                   defaults: UserDefaults = .standard,
                                   environment: [String: String] = ProcessInfo.processInfo.environment) -> Bool {
        WelcomePrefs.shouldShowAtLaunch(isScratch: model.isDisposableScratch, defaults: defaults,
                                        environment: environment)
    }

    func show(model: EditorModel, describe: (() -> Void)? = nil) {
        self.model = model
        if let describe { self.describe = describe }
        if let window {
            attach(window)
            window.makeKeyAndOrderFront(nil)
            return
        }
        model.refreshRecents()
        let dir = SampleLibrary.directory()
        let samples = dir.map(SampleLibrary.load(from:)) ?? []
        let state = WelcomeState(samples: samples, directory: dir,
                                 recents: WelcomeRecents.list(paths: model.recents.map(\.path)))
        let actions = WelcomeActions(
            newDocument: { [weak self] in self?.close(); AppInstance.open() },
            open: { [weak self] in self?.close(); openPanel(model) },
            importFile: { [weak self] in self?.close(); welcomeImportPanel(model) },
            fromPhotos: { [weak self] in self?.close(); PhotoImportController.shared.show(for: model) },
            describe: IntentEngine.isAvailable ? { [weak self] in
                self?.close(); self?.describe?()
            } : nil,
            shortcuts: { [weak self] in self?.close(); GlassUI.shared.shortcutsShown = true },
            openRecent: { [weak self] url in self?.close(); AppInstance.opening(url, from: model) },
            openSample: { [weak self] sample in self?.openSample(sample) },
            connectAgent: { AgentInstallPrompt.run() },
            whatsNew: { NSWorkspace.shared.open(URL(string: "https://vcad.io/docs/architecture/changelog")!) },
            close: { [weak self] in self?.close() })
        let host = NSHostingController(rootView: WelcomeView(state: state, actions: actions))
        host.sizingOptions = [.preferredContentSize]
        let w = NSWindow(contentViewController: host)
        w.title = "Welcome to vcad"
        w.styleMask = [.titled, .closable, .fullSizeContentView]
        w.titlebarAppearsTransparent = true
        w.titleVisibility = .hidden
        w.isMovableByWindowBackground = true
        w.isReleasedWhenClosed = false
        w.delegate = self
        w.center()
        window = w
        attach(w)
        centerOnScreen(w)
        w.makeKeyAndOrderFront(nil)
    }

    /// Keep the Welcome window above the editor window (the floating model in
    /// released mode): a child window always orders above its parent.
    private func attach(_ w: NSWindow) {
        guard let editor = ReleaseWindowController.shared.window, editor !== w else { return }
        if w.parent !== editor {
            w.parent?.removeChildWindow(w)
            editor.addChildWindow(w, ordered: .above)
        }
        if let screen = w.screen?.visibleFrame, !screen.contains(w.frame) { centerOnScreen(w) }
    }

    /// `center()` runs before the hosting view has its size, which left the
    /// window hanging off the bottom of the screen; centre once it is sized.
    private func centerOnScreen(_ w: NSWindow) {
        if let size = w.contentViewController?.view.fittingSize, size.width > 0 { w.setContentSize(size) }
        guard let screen = (ReleaseWindowController.shared.window?.screen ?? NSScreen.main)?.visibleFrame else { return }
        w.setFrameOrigin(NSPoint(x: screen.midX - w.frame.width / 2,
                                 y: screen.midY - w.frame.height / 2 + screen.height * 0.04))
    }

    /// Called when the editor window appears or switches presentation, so a
    /// Welcome window shown before it existed still ends up on top.
    func reattach() {
        guard let window, window.isVisible else { return }
        attach(window)
        window.makeKeyAndOrderFront(nil)
    }

    var isVisible: Bool { window?.isVisible == true }

    func close() { window?.close() }

    func windowWillClose(_ notification: Notification) {
        guard let w = window else { return }
        w.parent?.removeChildWindow(w)
        window = nil
    }

    private func openSample(_ sample: SampleProject) {
        guard let model, let dir = SampleLibrary.directory() else { return }
        do {
            let url = try SampleLibrary.materialize(sample, from: dir)
            close()
            AppInstance.opening(url, from: model)
            if sample.tutorial == true { NSWorkspace.shared.open(url) }   // the editor, beside the model
        } catch {
            model.loadError = "Could not open \(sample.title): \(error.localizedDescription)"
        }
    }
}

/// Import STEP (opens as a document) or STL (wrapped in a one-line loon file
/// in ~/Documents/vcad/Imports so it is editable like any part).
@MainActor func welcomeImportPanel(_ model: EditorModel) {
    let panel = NSOpenPanel()
    panel.allowedContentTypes = ["step", "stp", "stl"].compactMap { UTType(filenameExtension: $0) }
    panel.prompt = "Import"
    panel.begin { response in
        guard response == .OK, let url = panel.url else { return }
        MainActor.assumeIsolated {
            guard url.pathExtension.lowercased() == "stl" else {
                AppInstance.opening(url, from: model); return
            }
            do {
                let dir = FileManager.default.homeDirectoryForCurrentUser
                    .appendingPathComponent("Documents/vcad/Imports", isDirectory: true)
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                let name = url.deletingPathExtension().lastPathComponent
                let dest = dir.appendingPathComponent(name).appendingPathExtension("loon")
                let quoted = url.path.replacingOccurrences(of: "\\", with: "\\\\")
                    .replacingOccurrences(of: "\"", with: "\\\"")
                let source = "; Imported from \(url.lastPathComponent)\n[root [import-mesh \"\(quoted)\"] \"pla\"]\n"
                try source.write(to: dest, atomically: true, encoding: .utf8)
                AppInstance.opening(dest, from: model)
            } catch {
                model.loadError = "Could not import \(url.lastPathComponent): \(error.localizedDescription)"
            }
        }
    }
}

// MARK: - State

struct WelcomeActions {
    var newDocument: () -> Void
    var open: () -> Void
    var importFile: () -> Void
    var fromPhotos: () -> Void
    var describe: (() -> Void)?
    var shortcuts: () -> Void
    var openRecent: (URL) -> Void
    var openSample: (SampleProject) -> Void
    var connectAgent: () -> Void
    var whatsNew: () -> Void
    var close: () -> Void
}

@MainActor @Observable
final class WelcomeState {
    let samples: [SampleProject]
    let directory: URL?
    let recents: [WelcomeRecent]
    var agent: WelcomeAgentState = .notConnected(detected: [])
    var showAtLaunch: Bool = WelcomePrefs.showAtLaunch() {
        didSet { WelcomePrefs.setShowAtLaunch(showAtLaunch) }
    }

    init(samples: [SampleProject], directory: URL?, recents: [WelcomeRecent]) {
        self.samples = samples
        self.directory = directory
        self.recents = recents
    }

    var mode: WelcomeMode { WelcomeMode(recents: recents) }
    var examples: [SampleProject] { WelcomeExamples.pick(samples) }

    func refreshAgent() {
        let home = AgentInstall.home
        let statuses = AgentKind.allCases.map { (name: $0.label, status: AgentInstall.status($0, home: home)) }
        agent = WelcomeAgentState.from(statuses: statuses, lastCall: AgentBridge.shared.lastCallAt)
    }

    func sampleImage(_ s: SampleProject) -> NSImage? {
        directory.flatMap { NSImage(contentsOf: $0.appendingPathComponent(s.thumbnail)) }
    }
}

// MARK: - Recent thumbnails

/// Recent-document thumbnails: a disk cache (WelcomeThumbnailKey) in front of
/// a small CPU renderer that evaluates the document once, off the main
/// thread, and draws a flat-shaded isometric view. One render at a time.
@MainActor @Observable
final class WelcomeThumbnails {
    static let shared = WelcomeThumbnails()
    private(set) var images: [String: NSImage] = [:]
    private(set) var parts: [String: Int] = [:]
    private var inFlight: Set<String> = []
    nonisolated private static let queue = DispatchQueue(label: "io.vcad.welcome.thumbnails", qos: .utility)

    struct Meta: Codable { var parts: Int }

    func request(_ url: URL) {
        let path = url.path
        guard images[path] == nil, !inFlight.contains(path) else { return }
        let dir = WelcomeThumbnailKey.directory
        let key = WelcomeThumbnailKey.key(for: url)
        let png = dir.appendingPathComponent(key + ".png"), meta = dir.appendingPathComponent(key + ".json")
        if let image = NSImage(contentsOf: png) {
            images[path] = image
            parts[path] = (try? Data(contentsOf: meta)).flatMap { try? JSONDecoder().decode(Meta.self, from: $0) }?.parts
            return
        }
        inFlight.insert(path)
        Self.queue.async {
            let result = WelcomeThumbnailRenderer.render(url: url)
            if let result {
                try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                try? result.png.write(to: png, options: .atomic)
                try? JSONEncoder().encode(Meta(parts: result.parts)).write(to: meta, options: .atomic)
            }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    let shared = WelcomeThumbnails.shared
                    shared.inFlight.remove(path)
                    guard let result else { return }
                    shared.images[path] = NSImage(data: result.png)
                    shared.parts[path] = result.parts
                }
            }
        }
    }
}

enum WelcomeThumbnailRenderer {
    static let size = CGSize(width: 480, height: 320)
    /// Documents above this triangle count are drawn from a stride of their
    /// triangles; a thumbnail doesn't need all of them.
    static let triangleBudget = 250_000

    /// Evaluate `url` and draw it. Nil for files the kernel can't evaluate
    /// (or that produce no geometry), so the card keeps its placeholder.
    nonisolated static func render(url: URL) -> (png: Data, parts: Int)? {
        guard let json = documentJSON(url) else { return nil }
        let dir = Array(url.deletingLastPathComponent().path.utf8)
        let scene: OpaquePointer? = json.withUnsafeBytes { raw in
            dir.withUnsafeBufferPointer { d in
                vcad_scene_from_json_in(raw.bindMemory(to: UInt8.self).baseAddress, json.count, d.baseAddress, d.count)
            }
        }
        guard let scene else { return nil }
        defer { vcad_scene_free(scene) }
        let count = vcad_scene_part_count(scene)
        var meshes: [KernelMesh] = []
        for i in 0..<count {
            let m = KernelMesh.fromView(vcad_scene_part_mesh(scene, i))
            if !m.isEmpty { meshes.append(m) }
        }
        guard !meshes.isEmpty, let png = draw(meshes) else { return nil }
        return (png, count)
    }

    nonisolated static func documentJSON(_ url: URL) -> Data? {
        let ext = url.pathExtension.lowercased()
        let bytes = Array(url.path.utf8)
        if ext == "loon" {
            guard let c = bytes.withUnsafeBufferPointer({ vcad_loon_file_to_json($0.baseAddress, $0.count) }) else { return nil }
            defer { vcad_cam_free(c) }
            return Data(bytes: c, count: strlen(c))
        }
        guard ext == "vcad" else { return nil }
        guard let data = try? Data(contentsOf: url) else { return nil }
        let first = data.first { !($0 == 0x20 || $0 == 0x0A || $0 == 0x0D || $0 == 0x09) }
        if first == UInt8(ascii: "{") { return data }
        guard let c = bytes.withUnsafeBufferPointer({ vcad_document_file_to_json($0.baseAddress, $0.count) }) else { return nil }
        defer { vcad_cam_free(c) }
        return Data(bytes: c, count: strlen(c))
    }

    /// Painter's-algorithm isometric render (Z-up, viewed from +X −Y +Z),
    /// on a transparent background so the card's own surface shows through in
    /// light and dark mode.
    nonisolated static func draw(_ meshes: [KernelMesh]) -> Data? {
        // Isometric basis: screen x, screen y (up), depth (toward viewer).
        let view = simd_normalize(SIMD3<Float>(1, -1, 0.82))
        let right = simd_normalize(simd_cross(SIMD3<Float>(0, 0, 1), view))
        let up = simd_cross(view, right)
        let light = simd_normalize(SIMD3<Float>(0.45, -0.25, 1))
        struct Tri { var p: (CGPoint, CGPoint, CGPoint); var depth: Float; var shade: Float; var hue: Int }
        var tris: [Tri] = []
        var lo = CGPoint(x: CGFloat.infinity, y: .infinity), hi = CGPoint(x: -CGFloat.infinity, y: -.infinity)
        let total = meshes.reduce(0) { $0 + $1.triangleCount }
        let stride = max(1, total / triangleBudget)
        for (mi, m) in meshes.enumerated() {
            var t = 0
            while t < m.triangleCount {
                defer { t += stride }
                let a = m.positions[Int(m.indices[t * 3])], b = m.positions[Int(m.indices[t * 3 + 1])]
                let c = m.positions[Int(m.indices[t * 3 + 2])]
                let n = simd_cross(b - a, c - a)
                let len = simd_length(n)
                guard len > 0 else { continue }
                let nn = n / len
                guard simd_dot(nn, view) > 0 else { continue }      // back face
                func project(_ v: SIMD3<Float>) -> CGPoint { CGPoint(x: CGFloat(simd_dot(v, right)), y: CGFloat(simd_dot(v, up))) }
                let pa = project(a), pb = project(b), pc = project(c)
                for p in [pa, pb, pc] {
                    lo = CGPoint(x: min(lo.x, p.x), y: min(lo.y, p.y)); hi = CGPoint(x: max(hi.x, p.x), y: max(hi.y, p.y))
                }
                let depth = simd_dot((a + b + c) / 3, view)
                let shade = 0.42 + 0.58 * max(0, simd_dot(nn, light))
                tris.append(Tri(p: (pa, pb, pc), depth: depth, shade: shade, hue: mi))
            }
        }
        guard !tris.isEmpty, hi.x > lo.x, hi.y > lo.y else { return nil }
        tris.sort { $0.depth < $1.depth }
        let scale = Float(2)   // render at 2x for Retina
        let W = Int(size.width * CGFloat(scale)), H = Int(size.height * CGFloat(scale))
        guard let ctx = CGContext(data: nil, width: W, height: H, bitsPerComponent: 8, bytesPerRow: 0,
                                  space: CGColorSpace(name: CGColorSpace.sRGB)!,
                                  bitmapInfo: CGImageAlphaInfo.premultipliedLast.rawValue) else { return nil }
        let margin: CGFloat = 0.12
        let fit = min(CGFloat(W) * (1 - 2 * margin) / (hi.x - lo.x), CGFloat(H) * (1 - 2 * margin) / (hi.y - lo.y))
        let ox = (CGFloat(W) - (hi.x - lo.x) * fit) / 2 - lo.x * fit
        let oy = (CGFloat(H) - (hi.y - lo.y) * fit) / 2 - lo.y * fit
        func map(_ p: CGPoint) -> CGPoint { CGPoint(x: p.x * fit + ox, y: p.y * fit + oy) }
        // The vcad drafting blue, with a slight per-part hue walk so
        // assemblies read as separate parts.
        let palette: [(CGFloat, CGFloat, CGFloat)] = [(0.36, 0.55, 0.78), (0.52, 0.62, 0.74), (0.40, 0.66, 0.70), (0.62, 0.58, 0.72)]
        ctx.setLineJoin(.round)
        ctx.setLineWidth(0.6)
        for t in tris {
            let (r, g, b) = palette[t.hue % palette.count]
            let s = CGFloat(t.shade)
            let color = CGColor(srgbRed: r * s, green: g * s, blue: b * s, alpha: 1)
            ctx.setFillColor(color); ctx.setStrokeColor(color)   // stroke hides hairline seams
            ctx.beginPath()
            ctx.move(to: map(t.p.0)); ctx.addLine(to: map(t.p.1)); ctx.addLine(to: map(t.p.2)); ctx.closePath()
            ctx.drawPath(using: .fillStroke)
        }
        guard let image = ctx.makeImage() else { return nil }
        let rep = NSBitmapImageRep(cgImage: image)
        rep.size = size
        return rep.representation(using: .png, properties: [:])
    }
}

// MARK: - View

private enum WelcomeFocus: Hashable {
    case recent(Int)
    case example(Int)
    case agent
}

struct WelcomeView: View {
    @State var state: WelcomeState
    let actions: WelcomeActions
    @State private var tourStep: Int?
    @FocusState private var focus: WelcomeFocus?
    private let thumbs = WelcomeThumbnails.shared
    private let agentTimer = Timer.publish(every: 2, on: .main, in: .common).autoconnect()

    static let columns = 4

    var body: some View {
        HStack(spacing: 0) {
            WelcomeRail(state: state, actions: actions, startTour: { tourStep = 0 })
                .frame(width: 228)
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 22) {
                    header
                    if state.mode == .returning { recentSection }
                    examplesSection
                    agentCard
                    Divider().padding(.top, 2)
                    newStrip
                }
                .padding(.horizontal, 28)
                .padding(.top, 64)
                .padding(.bottom, 24)
            }
            .scrollBounceBehavior(.basedOnSize)
            .frame(width: 700)
            .background(Color(nsColor: .windowBackgroundColor))
        }
        // Run the sidebar, divider and main pane up under the transparent
        // titlebar, edge to edge, the way a full-height sidebar window does.
        .ignoresSafeArea()
        .frame(height: state.mode == .returning ? 664 : 560)
        .overlay {
            if let step = tourStep {
                WelcomeTour(step: Binding(get: { step }, set: { tourStep = $0 }),
                            onFinish: { tourStep = nil },
                            onTryIt: {
                                tourStep = nil
                                if let bracket = state.examples.first { actions.openSample(bracket) }
                            })
                    .transition(.opacity)
            }
        }
        .animation(.easeOut(duration: 0.18), value: tourStep)
        .onAppear {
            state.refreshAgent()
            for r in state.recents { thumbs.request(r.url) }
            focus = state.mode == .returning && !state.recents.isEmpty ? .recent(0)
                : state.examples.isEmpty ? nil : .example(0)
        }
        .onReceive(agentTimer) { _ in state.refreshAgent() }
        .onExitCommand { if tourStep != nil { tourStep = nil } else { actions.close() } }
    }

    private var header: some View {
        VStack(alignment: .leading, spacing: 4) {
            Text(state.mode == .returning ? "Welcome back" : "Welcome to vcad")
                .font(.largeTitle.weight(.bold))
            Text(state.mode == .returning
                 ? "Pick up where you left off, or start something new."
                 : "Parametric CAD you can write, talk to, and check before you make it.")
                .foregroundStyle(.secondary)
        }
    }

    // MARK: sections

    private var recentSection: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            Eyebrow("Recent")
            LazyVGrid(columns: grid, spacing: Theme.Space.m) {
                ForEach(Array(state.recents.enumerated()), id: \.element.id) { i, r in
                    WelcomeCard(title: r.name,
                                detail: WelcomeRecents.context(parts: thumbs.parts[r.url.path], modified: r.modified),
                                image: thumbs.images[r.url.path], fallbackSymbol: symbol(for: r.url),
                                focused: focus == .recent(i)) { actions.openRecent(r.url) }
                        .focusable()
                        .focusEffectDisabled()
                        .focused($focus, equals: .recent(i))
                        .onKeyPress(.return) { actions.openRecent(r.url); return .handled }
                        .onKeyPress(.space) { actions.openRecent(r.url); return .handled }
                        .onMoveCommand { move($0, from: .recent(i)) }
                        .accessibilityLabel("Open recent \(r.name)")
                        .help(r.url.path)
                }
            }
        }
    }

    private var examplesSection: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            if state.mode == .firstLaunch {
                Text(state.mode.examplesTitle).font(.headline)
            } else {
                Eyebrow(state.mode.examplesTitle)
            }
            if state.examples.isEmpty {
                Text("No examples found in this build.").foregroundStyle(.secondary)
            }
            LazyVGrid(columns: grid, spacing: Theme.Space.m) {
                ForEach(Array(state.examples.enumerated()), id: \.element.id) { i, s in
                    WelcomeCard(title: s.title, detail: s.lesson ?? s.subtitle, image: state.sampleImage(s),
                                fallbackSymbol: "cube", focused: focus == .example(i)) { actions.openSample(s) }
                        .focusable()
                        .focusEffectDisabled()
                        .focused($focus, equals: .example(i))
                        .onKeyPress(.return) { actions.openSample(s); return .handled }
                        .onKeyPress(.space) { actions.openSample(s); return .handled }
                        .onMoveCommand { move($0, from: .example(i)) }
                        .accessibilityLabel("Open example \(s.title): \(s.lesson ?? "")")
                        .help(s.subtitle)
                }
            }
        }
    }

    private var agentCard: some View {
        HStack(spacing: Theme.Space.m) {
            Image(systemName: "sparkles")
                .font(.title2.weight(.semibold))
                .foregroundStyle(.white)
                .frame(width: 42, height: 42)
                .background(LinearGradient(colors: [Color(red: 0.36, green: 0.52, blue: 1), Color(red: 0.62, green: 0.36, blue: 0.96)],
                                           startPoint: .topLeading, endPoint: .bottomTrailing),
                            in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                .shadow(color: Color(red: 0.5, green: 0.4, blue: 1).opacity(0.3), radius: 4, y: 2)
            VStack(alignment: .leading, spacing: 3) {
                Text("Bring your agent").font(.headline)
                Text("Claude, Codex or Cursor drive vcad over MCP on this Mac. You watch every edit land.")
                    .font(.callout).foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            Spacer(minLength: Theme.Space.s)
            WelcomeAgentBadge(state: state.agent)
            if !state.agent.isConnected {
                Button("Connect…", action: actions.connectAgent)
                    .buttonStyle(.borderedProminent)
                    .focused($focus, equals: .agent)
            }
        }
        .padding(Theme.Space.l)
        .background(Color(nsColor: .controlBackgroundColor), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: 12, style: .continuous).strokeBorder(.separator, lineWidth: 0.5))
        .accessibilityElement(children: .contain)
    }

    private var newStrip: some View {
        HStack(spacing: Theme.Space.s) {
            Label("New in 1.0", systemImage: "star.fill")
                .font(.callout.weight(.semibold))
                .labelStyle(.titleAndIcon)
                .foregroundStyle(.tint)
            Text("Native Mac app · local MCP agents · DFM & stress checks · receipts")
                .font(.callout).foregroundStyle(.secondary).lineLimit(1).truncationMode(.tail)
            Spacer(minLength: Theme.Space.s)
            Button("What’s new", action: actions.whatsNew).buttonStyle(.link)
        }
    }

    // MARK: helpers

    private var grid: [GridItem] {
        Array(repeating: GridItem(.flexible(), spacing: Theme.Space.m, alignment: .top), count: Self.columns)
    }

    private func symbol(for url: URL) -> String {
        switch url.pathExtension.lowercased() {
        case "step", "stp": "shippingbox"
        case "loon": "chevron.left.forwardslash.chevron.right"
        default: "cube"
        }
    }

    /// Arrow keys move within a grid; off the bottom of Recent lands in
    /// Examples, off the bottom of Examples lands on the agent card.
    private func move(_ direction: MoveCommandDirection, from: WelcomeFocus) {
        let m: WelcomeGridNav.Move
        switch direction {
        case .left: m = .left
        case .right: m = .right
        case .up: m = .up
        case .down: m = .down
        @unknown default: return
        }
        switch from {
        case .recent(let i):
            if let n = WelcomeGridNav.move(i, m, columns: Self.columns, count: state.recents.count) { focus = .recent(n) }
            else if m == .down, !state.examples.isEmpty { focus = .example(min(i % Self.columns, state.examples.count - 1)) }
        case .example(let i):
            if let n = WelcomeGridNav.move(i, m, columns: Self.columns, count: state.examples.count) { focus = .example(n) }
            else if m == .up, state.mode == .returning, !state.recents.isEmpty {
                focus = .recent(min(i % Self.columns, state.recents.count - 1))
            } else if m == .down, !state.agent.isConnected { focus = .agent }
        case .agent:
            break
        }
    }
}

/// The left rail: mark, actions with their shortcuts, then the tour,
/// shortcuts and the "Show at launch" toggle.
private struct WelcomeRail: View {
    @Bindable var state: WelcomeState
    let actions: WelcomeActions
    let startTour: () -> Void

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            VStack(alignment: .leading, spacing: 2) {
                // The bundle's own icon: NSApp.applicationIconImage follows
                // the Dock tile, which mirrors the open model.
                if let icon = Bundle.main.url(forResource: "AppIcon", withExtension: "icns").flatMap(NSImage.init(contentsOf:))
                    ?? NSApp.applicationIconImage {
                    Image(nsImage: icon).resizable().frame(width: 64, height: 64)
                        .shadow(color: .black.opacity(0.18), radius: 6, y: 3)
                        .padding(.bottom, Theme.Space.s)
                }
                Text("vcad").font(.title.weight(.bold))
                Text("Version 1.0").font(.callout).foregroundStyle(.secondary)
            }
            .padding(.leading, 8)
            .padding(.bottom, 22)

            RailButton(title: "New document", symbol: "doc.badge.plus", keys: "⌘N", action: actions.newDocument)
                .keyboardShortcut("n")
            RailButton(title: "Open…", symbol: "folder", keys: "⌘O", action: actions.open)
                .keyboardShortcut("o")
            RailButton(title: "Import STEP/STL", symbol: "square.and.arrow.down", action: actions.importFile)
            RailButton(title: "From photos", symbol: "camera.viewfinder", pill: "beta", action: actions.fromPhotos)
            if let describe = actions.describe {
                RailButton(title: "Describe a part…", symbol: "text.bubble", keys: "⌘K", action: describe)
                    .keyboardShortcut("k")
            }
            Spacer(minLength: Theme.Space.l)
            RailButton(title: "60-second tour", symbol: "play.circle", action: startTour)
            RailButton(title: "Shortcuts", symbol: "keyboard", keys: "⌘/", action: actions.shortcuts)
                .keyboardShortcut("/")
            Toggle("Show at launch", isOn: $state.showAtLaunch)
                .toggleStyle(.checkbox)
                .font(.callout)
                .foregroundStyle(.secondary)
                .padding(.leading, 8).padding(.top, Theme.Space.s)
        }
        .padding(.horizontal, Theme.Space.m)
        .padding(.top, 80)
        .padding(.bottom, Theme.Space.l)
        .frame(maxHeight: .infinity, alignment: .top)
        .background(SidebarMaterial().ignoresSafeArea())
    }
}

/// The system sidebar material (vibrant, behind-window), as Finder and
/// Xcode's welcome window use it.
private struct SidebarMaterial: NSViewRepresentable {
    func makeNSView(context: Context) -> NSVisualEffectView {
        let v = NSVisualEffectView()
        v.material = .sidebar
        v.blendingMode = .behindWindow
        v.state = .followsWindowActiveState
        return v
    }
    func updateNSView(_ v: NSVisualEffectView, context: Context) {}
}

private struct RailButton: View {
    let title: String
    let symbol: String
    var keys: String? = nil
    var pill: String? = nil
    let action: () -> Void
    @State private var hovered = false

    var body: some View {
        Button(action: action) {
            HStack(spacing: Theme.Space.s) {
                Image(systemName: symbol).frame(width: 20).foregroundStyle(.tint)
                    .font(.body.weight(.medium))
                Text(title)
                if let pill {
                    Text(pill.uppercased())
                        .font(.caption2.weight(.bold)).tracking(0.4)
                        .padding(.horizontal, 5).padding(.vertical, 1)
                        .background(Color.orange.opacity(0.18), in: Capsule())
                        .foregroundStyle(.orange)
                }
                Spacer(minLength: Theme.Space.xs)
                if let keys {
                    Text(keys).font(.callout).foregroundStyle(.tertiary)
                }
            }
            .padding(.horizontal, 8)
            .padding(.vertical, 6)
            .contentShape(Rectangle())
            .selectableRow(selected: false, hovered: hovered)
        }
        .buttonStyle(.plain)
        .onHover { hovered = $0 }
    }
}

/// A thumbnail card (Recent and Examples).
private struct WelcomeCard: View {
    let title: String
    let detail: String
    let image: NSImage?
    let fallbackSymbol: String
    let focused: Bool
    let action: () -> Void
    @State private var hovered = false
    @Environment(\.colorScheme) private var scheme

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ZStack {
                // The examples' renders carry this paper tone; recents draw
                // on the same tile so every card reads as one family.
                RoundedRectangle(cornerRadius: 10, style: .continuous)
                    .fill(Color(red: 0.957, green: 0.953, blue: 0.945))
                if let image {
                    Image(nsImage: image).resizable().aspectRatio(contentMode: .fill)
                        .frame(minWidth: 0, maxWidth: .infinity, minHeight: 104, maxHeight: 104)
                        .clipped()
                } else {
                    Image(systemName: fallbackSymbol).font(.title).foregroundStyle(Color.black.opacity(0.22))
                }
            }
            .frame(height: 104)
            .clipShape(RoundedRectangle(cornerRadius: 10, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous)
                .strokeBorder(Color.black.opacity(0.08), lineWidth: 0.5))
            .padding(3)
            .overlay(RoundedRectangle(cornerRadius: 13, style: .continuous)
                .strokeBorder(Color.accentColor, lineWidth: 3).opacity(focused ? 1 : 0))
            .padding(-3)
            .scaleEffect(hovered ? 1.015 : 1)
            .shadow(color: .black.opacity(scheme == .dark ? (hovered ? 0.45 : 0.25) : (hovered ? 0.16 : 0.06)),
                    radius: hovered ? 10 : 3, y: hovered ? 4 : 1)
            Text(title).font(.callout.weight(.semibold)).lineLimit(1).truncationMode(.middle)
            Text(detail).font(.caption).foregroundStyle(.secondary).lineLimit(2)
                .fixedSize(horizontal: false, vertical: true)
        }
        .frame(maxWidth: .infinity, alignment: .leading)
        .contentShape(Rectangle())
        .onTapGesture(perform: action)
        .onHover { hovered = $0 }
        .animation(.easeOut(duration: 0.12), value: hovered)
        .accessibilityAddTraits(.isButton)
        .accessibilityAction(.default, action)
    }
}

private struct WelcomeAgentBadge: View {
    let state: WelcomeAgentState
    var body: some View {
        HStack(spacing: 6) {
            Circle().fill(color).frame(width: 8, height: 8)
                .overlay(Circle().stroke(color.opacity(0.35), lineWidth: 4).opacity(active ? 1 : 0))
            Text(state.title).font(.callout.weight(.medium)).lineLimit(1)
        }
        .padding(.horizontal, 10).padding(.vertical, 5)
        .background(.background.opacity(0.6), in: Capsule())
        .overlay(Capsule().strokeBorder(.separator, lineWidth: 0.5))
        .accessibilityLabel("Agent status: \(state.title)")
    }
    private var active: Bool { if case .active = state { return true }; return false }
    private var color: Color {
        switch state {
        case .notConnected: .secondary
        case .connected: .green
        case .active: .accentColor
        }
    }
}

// MARK: - Tour

/// The 60-second tour: four cards inside the Welcome window — no coach marks
/// to keep in sync with the editor's layout. ←/→ step, Esc closes, and the
/// last card opens the Bracket example to try it.
struct WelcomeTour: View {
    @Binding var step: Int
    let onFinish: () -> Void
    let onTryIt: () -> Void

    struct Page { let symbol: String; let title: String; let body: String; let keys: [String] }
    static let pages: [Page] = [
        Page(symbol: "cube.transparent", title: "Your part lives on the desktop",
             body: "The model floats over your desktop, released from any window. Drag to orbit, scroll to zoom, and press ⌘0 to frame it. Prefer a window? View ▸ Open in Window (⇧⌘D).",
             keys: ["drag", "scroll", "⌘0"]),
        Page(symbol: "slider.horizontal.3", title: "Every number is a parameter",
             body: "Click a part, then scrub any value in the inspector. Or open the .loon source in your own editor: save, and the model rebuilds in place.",
             keys: ["click", "scrub", "⌘S"]),
        Page(symbol: "sparkles", title: "Describe it, or bring your agent",
             body: "Press ⌘K and describe a part. Or connect Claude, Codex or Cursor: they drive vcad over MCP on this Mac, and you watch each edit land.",
             keys: ["⌘K", "⇧⌘K"]),
        Page(symbol: "checkmark.seal", title: "Check it before you make it",
             body: "Measure, run DFM and stress checks in Analyze, strike a bar with the Mallet to hear it, then export STEP or STL with a receipt of what was verified.",
             keys: ["I", "M", "⌘E"]),
    ]

    var body: some View {
        let page = Self.pages[step]
        ZStack {
            Rectangle().fill(.black.opacity(0.4)).onTapGesture(perform: onFinish)
            VStack(spacing: Theme.Space.l) {
                Image(systemName: page.symbol)
                    .font(.system(size: 44, weight: .light))
                    .foregroundStyle(.tint)
                    .frame(height: 64)
                    .id(step)
                    .transition(.asymmetric(insertion: .scale(scale: 0.8).combined(with: .opacity), removal: .opacity))
                VStack(spacing: Theme.Space.s) {
                    Text("\(step + 1) of \(Self.pages.count)").font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                    Text(page.title).font(.title2.weight(.semibold))
                    Text(page.body).foregroundStyle(.secondary).multilineTextAlignment(.center)
                        .fixedSize(horizontal: false, vertical: true)
                }
                HStack(spacing: 6) {
                    ForEach(page.keys, id: \.self) { k in
                        Text(k).font(.callout.monospaced()).padding(.horizontal, 7).padding(.vertical, 2).controlWell()
                    }
                }
                HStack {
                    Button("Skip", action: onFinish).buttonStyle(.borderless).foregroundStyle(.secondary)
                    Spacer()
                    HStack(spacing: 6) {
                        ForEach(0..<Self.pages.count, id: \.self) { i in
                            Circle().fill(i == step ? AnyShapeStyle(Color.accentColor) : AnyShapeStyle(.quaternary))
                                .frame(width: 7, height: 7)
                        }
                    }
                    Spacer()
                    if step > 0 { Button("Back") { step -= 1 } }
                    if step < Self.pages.count - 1 {
                        Button("Next") { step += 1 }.keyboardShortcut(.defaultAction)
                    } else {
                        Button("Try the Bracket", action: onTryIt).keyboardShortcut(.defaultAction)
                    }
                }
            }
            .padding(28)
            .frame(width: 460)
            .background(Color(nsColor: .windowBackgroundColor),
                        in: RoundedRectangle(cornerRadius: Theme.Radius.panel, style: .continuous))
            .overlay(RoundedRectangle(cornerRadius: Theme.Radius.panel, style: .continuous)
                .strokeBorder(.separator, lineWidth: 0.5))
            .shadow(color: .black.opacity(0.3), radius: 24, y: 8)
            .animation(.easeOut(duration: 0.2), value: step)
        }
        .focusable()
        .focusEffectDisabled()
        .onKeyPress(.leftArrow) { if step > 0 { step -= 1 }; return .handled }
        .onKeyPress(.rightArrow) { if step < Self.pages.count - 1 { step += 1 }; return .handled }
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Tour, step \(step + 1) of \(Self.pages.count)")
    }
}
#endif

import SwiftUI
import AppKit
import simd

// vcad's navigation layer, the surfaces: the Compass (orientation widget),
// the Halo (radial, context-aware command ring), saved views and the glide
// that moves the camera between poses. The pure parts — ranking, targets,
// hit-testing, tweening, layout — are in NavigationCore.swift.

// MARK: - Shared state

@MainActor @Observable
final class NavigationUI {
    static let shared = NavigationUI()

    // Halo
    /// Where the Halo is open, in overlay (window content, top-left) coords.
    var haloOrigin: CGPoint?
    /// The slot under the pointer / keyboard.
    var haloSlot: Int?
    /// Opened by a right-drag: releasing the button runs the highlighted slot.
    var haloMarking = false
    var haloSlots: [PaletteCommand] = []

    // Find
    var recents = RecentCommands.load()
    /// Text the next Find opens with (the Halo's "Views" slot seeds "view").
    var findSeed = ""

    // Saved views
    private(set) var savedViews = SavedViewList()
    private var savedViewsPath: String??
    var namingView = false

    // Glide
    @ObservationIgnored private var tween: CameraTween?
    @ObservationIgnored private var tweenStart = Date()
    @ObservationIgnored nonisolated(unsafe) private var tweenTimer: Timer?

    func recordRun(_ id: String) {
        recents.record(id)
        recents.save()
    }

    // MARK: saved views

    func views(for model: EditorModel) -> [SavedView] {
        syncViews(model)
        return savedViews.views
    }
    private func syncViews(_ model: EditorModel) {
        let path = model.documentURL?.path
        if savedViewsPath != .some(path) {
            savedViews = .load(for: path)
            savedViewsPath = .some(path)
        }
    }
    func saveView(named name: String, model: EditorModel) {
        syncViews(model)
        let v = savedViews.save(name, pose: model.cameraPose)
        savedViews.save(for: model.documentURL?.path)
        GlassUI.shared.flash("Saved view \u{201C}\(v.name)\u{201D}", symbol: "camera.viewfinder")
    }
    func deleteView(_ name: String, model: EditorModel) {
        syncViews(model)
        savedViews.remove(name)
        savedViews.save(for: model.documentURL?.path)
    }

    // MARK: glide

    func glide(_ model: EditorModel, to pose: CameraPose, duration: Double = 0.42) {
        model.stopSpin()
        model.cameraUserMoved = true
        let reduce = NSWorkspace.shared.accessibilityDisplayShouldReduceMotion
        tween = CameraTween(from: model.cameraPose, to: pose, duration: reduce ? 0 : duration)
        tweenStart = Date()
        tweenTimer?.invalidate()
        step(model)
        guard tween != nil else { return }
        tweenTimer = Timer.scheduledTimer(withTimeInterval: 1.0 / 120, repeats: true) { [weak self, weak model] _ in
            MainActor.assumeIsolated {
                guard let self, let model else { return }
                self.step(model)
            }
        }
    }
    private func step(_ model: EditorModel) {
        guard let t = tween else { return }
        let elapsed = Date().timeIntervalSince(tweenStart)
        model.cameraPose = t.pose(at: elapsed)
        if t.finished(at: elapsed) {
            tween = nil
            tweenTimer?.invalidate(); tweenTimer = nil
        }
    }
    func cancelGlide() { tween = nil; tweenTimer?.invalidate(); tweenTimer = nil }

    // MARK: halo

    func openHalo(at p: CGPoint, model: EditorModel, marking: Bool) {
        haloSlots = model.haloContext.slots.compactMap { model.navCommand($0) }
        haloOrigin = p
        haloSlot = nil
        haloMarking = marking
    }
    func closeHalo() { haloOrigin = nil; haloSlot = nil; haloMarking = false }
    func runHaloSlot(_ i: Int?) {
        guard let i, haloSlots.indices.contains(i) else { closeHalo(); return }
        let c = haloSlots[i]
        closeHalo()
        guard c.enabled else { return }
        recordRun(c.id)
        DispatchQueue.main.async { c.run() }
    }
}

// MARK: - Model glue

extension EditorModel {
    var cameraPose: CameraPose {
        get { CameraPose(azimuth: azimuth, elevation: elevation, distance: distance, pan: panOffset) }
        set {
            azimuth = newValue.azimuth; elevation = newValue.elevation
            distance = newValue.distance; panOffset = newValue.pan
            pinchBaseline = newValue.distance
        }
    }

    /// Turn to a Compass target. Clicking the face you are already looking at
    /// flips to the one behind it.
    func glide(to target: ViewTarget) {
        var t = target
        if t.kind == .face, ViewTarget.facing(azimuth: azimuth, elevation: elevation) == t,
           abs(elevation - t.elevation) < 0.05 {
            t = t.opposite
        }
        var p = cameraPose
        p.azimuth = t.azimuth; p.elevation = t.elevation
        NavigationUI.shared.glide(self, to: p)
    }
    /// Roll the view a quarter turn about the vertical axis (Compass arrows).
    func turnView(clockwise: Bool) {
        var p = cameraPose
        p.azimuth += (clockwise ? -1 : 1) * .pi / 2
        NavigationUI.shared.glide(self, to: p)
    }

    /// The kernel-frame direction from the model toward the camera.
    var kernelToCamera: SIMD3<Float> { ViewTarget.cameraDirection(azimuth: azimuth, elevation: elevation) }

    /// ⌥⌘X: open a section through the picked face (on its plane) or the
    /// model's centre (on the axis you face most); close it if one is open.
    func toggleSmartSection(throughPickedFace: Bool = true) {
        if section != nil, !throughPickedFace || picking.selectedFaces.isEmpty { section = nil; return }
        var n: SIMD3<Float>?, p: SIMD3<Float>?
        if throughPickedFace, let f = picking.selectedFaces.first, let face = partTopology(f.part)?.face(f.id),
           face.isPlanar, face.normal.count == 3, face.center.count == 3 {
            n = SIMD3(Float(face.normal[0]), Float(face.normal[1]), Float(face.normal[2]))
            p = SIMD3(Float(face.center[0]), Float(face.center[1]), Float(face.center[2]))
        }
        section = .smart(toCamera: kernelToCamera, center: displayCenter, faceNormal: n, facePoint: p)
    }

    var haloContext: HaloContext {
        if sketching { return .sketch }
        if !picking.selectedFaces.isEmpty { return .face }
        if !picking.selectedEdges.isEmpty { return .edge }
        if selectedPartIndex != nil || !multiSelectedParts.isEmpty { return .part }
        return .nothing
    }

    /// Navigation-layer commands (views, saved views, Halo verbs) that the
    /// modeling palette doesn't already carry.
    var navigationCommands: [PaletteCommand] {
        let nav = NavigationUI.shared
        var out: [PaletteCommand] = []
        let named: [(ViewTarget, String, String)] = [
            (.front, "Front", "⌘2"), (.right, "Right", "⌘3"), (.top, "Top", "⌘4"),
            (.back, "Back", ""), (.left, "Left", ""), (.bottom, "Bottom", ""),
        ]
        out.append(.init(id: "view.iso", title: "View: Isometric", symbol: "cube", group: "Camera", shortcut: "⌘1") { [weak self] in
            self?.animateCamera(to: .isometric)
        })
        for (t, name, key) in named {
            out.append(.init(id: "view.\(t.id)", title: "View: \(name)", symbol: "cube.transparent", group: "Camera", shortcut: key) { [weak self] in
                guard let self else { return }
                var p = self.cameraPose; p.azimuth = t.azimuth; p.elevation = t.elevation
                nav.glide(self, to: p)
            })
        }
        for v in nav.views(for: self) {
            out.append(.init(id: "saved.\(v.name)", title: "View: \(v.name)", symbol: "camera.viewfinder", group: "Saved view") { [weak self] in
                guard let self else { return }
                nav.glide(self, to: v.pose)
            })
        }
        out.append(.init(id: "saveView", title: "Save View…", symbol: "camera.badge.ellipsis", group: "Camera", shortcut: "⌥⌘V") {
            nav.namingView = true
        })
        out.append(.init(id: "turnLeft", title: "Turn View Left", symbol: "arrow.counterclockwise", group: "Camera") { [weak self] in
            self?.turnView(clockwise: false)
        })
        out.append(.init(id: "turnRight", title: "Turn View Right", symbol: "arrow.clockwise", group: "Camera") { [weak self] in
            self?.turnView(clockwise: true)
        })
        out.append(.init(id: "frameSel", title: "Frame Selection", symbol: "viewfinder.rectangular", group: "Camera",
                         shortcut: "⌥⌘0", enabled: hasSelection) {
            ReleaseWindowController.shared.frameSelection()
        })
        out.append(.init(id: "halo", title: "Quick Actions (Halo)", symbol: "circle.dashed", group: "Tools", shortcut: "⌥⌘.") { [weak self] in
            guard let self else { return }
            NavigationUI.shared.openHalo(at: Self.pointerInOverlay, model: self, marking: false)
        })
        return out
    }

    /// Every command Find searches: modeling verbs plus navigation.
    var findCommands: [PaletteCommand] { paletteCommands + navigationCommands }

    /// A command by id, for the Halo's slots (with a few Halo-only verbs).
    func navCommand(_ id: String) -> PaletteCommand? {
        let pi = selectedPartIndex
        switch id {
        case "find":
            return .init(id: id, title: "Find", symbol: "magnifyingglass", group: "Tools", shortcut: "⇧⌘P") { [weak self] in
                self?.showsCommandPalette = true
            }
        case "views":
            return .init(id: id, title: "Views", symbol: "camera.viewfinder", group: "Camera") { [weak self] in
                NavigationUI.shared.findSeed = "view: "
                self?.showsCommandPalette = true
            }
        case "section":
            return .init(id: id, title: section == nil ? "Section" : "Close Section", symbol: "square.split.diagonal",
                         group: "Tools", shortcut: "⌥⌘X") { [weak self] in self?.toggleSmartSection(throughPickedFace: false) }
        case "sectionHere":
            return .init(id: id, title: "Section Here", symbol: "square.split.diagonal.fill", group: "Tools") { [weak self] in
                self?.toggleSmartSection(throughPickedFace: true)
            }
        case "hide":
            return .init(id: id, title: "Hide", symbol: "eye.slash", group: "Edit", shortcut: "⇧⌘H",
                         enabled: pi != nil || picking.selectionPart != nil) { [weak self] in
                guard let self else { return }
                if self.selectedPartIndex == nil, let p = self.picking.selectionPart { self.toggleVisibility(part: p) }
                else { self.toggleSelectedVisibility() }
            }
        case "isolate":
            return .init(id: id, title: "Isolate", symbol: "scope", group: "Edit", shortcut: "⇧⌘I", enabled: pi != nil) { [weak self] in
                self?.toggleSelectedIsolate()
            }
        case "combine":
            return .init(id: id, title: "Combine", symbol: "square.on.square", group: "Tools",
                         enabled: usesDocumentTree && pi != nil) { [weak self] in self?.beginCombine(.union) }
        case "showAll":
            return .init(id: id, title: "Show All", symbol: "eye", group: "Edit", enabled: hasHiddenParts) { [weak self] in
                self?.showAllParts()
            }
        case "clearPick":
            return .init(id: id, title: "Clear Picks", symbol: "xmark.circle", group: "Select") { [weak self] in
                self?.picking.clear()
            }
        case "sketch.finish":
            return .init(id: id, title: "Finish Sketch", symbol: "checkmark.circle", group: "Sketch") { [weak self] in
                _ = self?.finishSketch()
            }
        case "sketch":
            return .init(id: id, title: picking.selectedFaces.isEmpty ? "Sketch" : "Sketch on Face",
                         symbol: "scribble.variable", group: "Sketch", shortcut: "⌃S", enabled: usesDocumentTree) { [weak self] in
                self?.enterSketch()
            }
        case "frame":
            return .init(id: id, title: hasSelection ? "Frame Selection" : "Frame All", symbol: "viewfinder", group: "Camera") { [weak self] in
                guard let self else { return }
                if self.hasSelection { ReleaseWindowController.shared.frameSelection() }
                else { self.resetCamera(animated: true) }
            }
        default:
            return findCommands.first { $0.id == id }
        }
    }

    /// The pointer, in the overlay's top-left coordinate space.
    static var pointerInOverlay: CGPoint {
        guard let w = ReleaseWindowController.shared.window, let c = w.contentView else { return .zero }
        let p = c.convert(w.convertPoint(fromScreen: NSEvent.mouseLocation), from: nil)
        return CGPoint(x: p.x, y: c.isFlipped ? p.y : c.bounds.height - p.y)
    }
}

// MARK: - Compass

/// The orientation widget: a live cube that turns with the camera. Faces,
/// edges and corners are all click targets (26 of them); the camera glides
/// there along the shortest arc. Clicking the face you already face flips to
/// the opposite one. A ring of quiet controls sits around it: home, turn left
/// and right, section, saved views.
struct CompassView: View {
    @Bindable var model: EditorModel
    @State private var hovered: ViewTarget?
    private let size: CGFloat = 96

    private var projection: CompassProjection {
        CompassProjection(azimuth: model.azimuth, elevation: model.elevation, scale: Float(size) * 0.25)
    }

    var body: some View {
        VStack(spacing: Theme.Space.xs) {
            ZStack {
                Circle().strokeBorder(.separator, lineWidth: 0.5)
                    .frame(width: size + 18, height: size + 18)
                cube
            }
            HStack(spacing: Theme.Space.xs) {
                ring("arrow.counterclockwise", "Turn view left") { model.turnView(clockwise: false) }
                ring("house", "Home view (⌘1)") { model.animateCamera(to: .isometric) }
                ring("arrow.clockwise", "Turn view right") { model.turnView(clockwise: true) }
                ring(model.section == nil ? "square.split.diagonal" : "square.split.diagonal.fill",
                     model.section == nil ? "Section through the model (⌥⌘X)" : "Close section (⌥⌘X)",
                     active: model.section != nil) { model.toggleSmartSection(throughPickedFace: true) }
                SavedViewsMenu(model: model)
            }
        }
        .padding(Theme.Space.s)
        .panelSurface()
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Orientation")
    }

    private var cube: some View {
        let proj = projection
        let faces = proj.visibleFaces
        let half = size / 2
        return Canvas { ctx, _ in
            func path(_ pts: [SIMD2<Float>]) -> Path {
                var p = Path()
                p.addLines(pts.map { CGPoint(x: CGFloat($0.x) + half, y: CGFloat($0.y) + half) })
                p.closeSubpath()
                return p
            }
            for f in faces {
                let shape = path(f.corners)
                ctx.fill(shape, with: .color(Color.primary.opacity(0.04 + 0.08 * Double(f.facing))))
                ctx.stroke(shape, with: .color(Color.primary.opacity(0.28)), lineWidth: 0.75)
            }
            if let h = hovered {
                for q in proj.highlight(h) {
                    ctx.fill(path(q), with: .color(Color.accentColor.opacity(0.35)))
                }
            }
            for f in faces where f.facing > 0.3 {
                let c = f.corners.reduce(SIMD2<Float>(0, 0), +) / 4
                let label = Text(f.target.label.uppercased())
                    .font(.system(size: 8.5, weight: .semibold))
                    .foregroundStyle(Color.primary.opacity(0.35 + 0.6 * Double(f.facing)))
                ctx.draw(label, at: CGPoint(x: CGFloat(c.x) + half, y: CGFloat(c.y) + half))
            }
            // The world axes, from the cube's bottom-left-front corner.
            let origin = SIMD3<Float>(-1, -1, -1)
            for (axis, color) in [(SIMD3<Float>(1, 0, 0), Color.red), (SIMD3(0, 1, 0), Color.green), (SIMD3(0, 0, 1), Color.blue)] {
                var p = Path()
                let a = proj.project(origin), b = proj.project(origin + axis * 0.7)
                p.move(to: CGPoint(x: CGFloat(a.x) + half, y: CGFloat(a.y) + half))
                p.addLine(to: CGPoint(x: CGFloat(b.x) + half, y: CGFloat(b.y) + half))
                ctx.stroke(p, with: .color(color.opacity(0.85)), lineWidth: 1.5)
            }
        }
        .frame(width: size, height: size)
        .contentShape(Rectangle())
        .onContinuousHover { phase in
            switch phase {
            case .active(let p): hovered = proj.target(at: SIMD2(Float(p.x - half), Float(p.y - half)))
            case .ended: hovered = nil
            }
        }
        .gesture(SpatialTapGesture().onEnded { v in
            if let t = proj.target(at: SIMD2(Float(v.location.x - half), Float(v.location.y - half))) {
                model.glide(to: t)
            }
        })
        .help(hovered?.label ?? "Click a face, edge or corner")
        .accessibilityElement()
        .accessibilityLabel("View cube, facing \(ViewTarget.facing(azimuth: model.azimuth, elevation: model.elevation).label)")
        .accessibilityAction(named: "Front") { model.glide(to: .front) }
        .accessibilityAction(named: "Top") { model.glide(to: .top) }
        .accessibilityAction(named: "Right") { model.glide(to: .right) }
    }

    private func ring(_ symbol: String, _ help: String, active: Bool = false, _ action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Image(systemName: symbol).font(.caption)
                .frame(width: 18, height: 18)
                .foregroundStyle(active ? AnyShapeStyle(Color.accentColor) : AnyShapeStyle(.secondary))
        }
        .buttonStyle(.borderless)
        .help(help)
        .accessibilityLabel(help)
    }
}

/// Saved views as a compact menu on the Compass: recall, save, delete.
struct SavedViewsMenu: View {
    @Bindable var model: EditorModel
    var body: some View {
        let nav = NavigationUI.shared
        Menu {
            let views = nav.views(for: model)
            ForEach(views) { v in
                Button(v.name) { nav.glide(model, to: v.pose) }
            }
            if !views.isEmpty { Divider() }
            Button("Save View…") { nav.namingView = true }
            if !views.isEmpty {
                Menu("Delete") {
                    ForEach(views) { v in Button(v.name) { nav.deleteView(v.name, model: model) } }
                }
            }
        } label: {
            Image(systemName: "camera.viewfinder").font(.caption).foregroundStyle(.secondary)
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.hidden)
        .fixedSize()
        .help("Saved views")
    }
}

/// The name field for Save View (⌥⌘V), hung beneath the toolbar.
struct SaveViewPrompt: View {
    @Bindable var model: EditorModel
    @State private var name = ""
    @FocusState private var focused: Bool
    var body: some View {
        let nav = NavigationUI.shared
        HStack(spacing: Theme.Space.s) {
            Image(systemName: "camera.viewfinder").foregroundStyle(.secondary)
            TextField(nav.views(for: model).isEmpty ? "View 1" : "View name", text: $name)
                .textFieldStyle(.plain).frame(width: 200)
                .focused($focused)
                .onSubmit { nav.saveView(named: name, model: model); nav.namingView = false }
            Text("⏎ save · esc cancel").font(.caption).foregroundStyle(.tertiary)
        }
        .padding(.horizontal, Theme.Space.m).padding(.vertical, Theme.Space.s)
        .onAppear { focused = true }
        .onExitCommand { nav.namingView = false }
    }
}

// MARK: - Halo

/// The radial command ring. Eight slots, clockwise from north: Find, three
/// context verbs, Frame, three more. Opened by a right-drag in the viewport
/// (flick toward a verb and let go) or ⌥⌘. at the pointer (click a verb, or
/// press 1–8, arrows and Return).
struct HaloView: View {
    @Bindable var nav = NavigationUI.shared
    static let radius: CGFloat = 132

    var body: some View {
        if let o = nav.haloOrigin {
            ZStack {
                // Click-away catcher (only in click mode; marking mode ends on release).
                if !nav.haloMarking {
                    Color.black.opacity(0.001).ignoresSafeArea()
                        .onTapGesture { nav.closeHalo() }
                }
                ring.position(o)
            }
            .transition(.scale(scale: 0.85).combined(with: .opacity))
        }
    }

    private var ring: some View {
        let n = nav.haloSlots.count
        return ZStack {
            Circle().fill(.clear).frame(width: 30, height: 30).pillSurface()
            Image(systemName: "circle.dashed").font(.caption2).foregroundStyle(.tertiary)
            ForEach(Array(nav.haloSlots.enumerated()), id: \.offset) { i, c in
                let p = RadialLayout.position(slot: i, of: n, radius: Self.radius, squash: 0.62)
                slot(i, c).offset(x: p.x, y: p.y)
            }
        }
        .frame(width: Self.radius * 2 + 160, height: Self.radius * 1.24 + 60)
        .modifier(ChromeRegion(key: "halo"))
    }

    private func slot(_ i: Int, _ c: PaletteCommand) -> some View {
        let on = nav.haloSlot == i
        return Button { nav.runHaloSlot(i) } label: {
            HStack(spacing: 5) {
                Text("\(i + 1)").font(.caption2.monospacedDigit()).foregroundStyle(.tertiary)
                Image(systemName: c.symbol).font(.callout)
                Text(c.title).font(.callout).lineLimit(1)
            }
            .padding(.horizontal, 10).padding(.vertical, 5)
            .foregroundStyle(c.enabled ? (on ? AnyShapeStyle(Color.white) : AnyShapeStyle(.primary)) : AnyShapeStyle(.tertiary))
            .background(on ? AnyShapeStyle(Color.accentColor) : AnyShapeStyle(Color.primary.opacity(0.07)), in: Capsule())
            .pillSurface()
        }
        .buttonStyle(.plain)
        .disabled(!c.enabled)
        .onHover { if $0 { nav.haloSlot = i } }
        .help(c.shortcut.isEmpty ? c.title : "\(c.title)  \(c.shortcut)")
    }
}

/// Routes right-drags in the viewport to the Halo and its keys while open.
/// A right-click that never moves is replayed, so the ordinary context menu
/// still opens on release.
@MainActor
final class HaloInput {
    static let shared = HaloInput()
    private var monitor: Any?
    private var pendingDown: NSEvent?
    private var replaying = false
    weak var model: EditorModel?

    func install(model: EditorModel) {
        self.model = model
        guard monitor == nil else { return }
        let mask: NSEvent.EventTypeMask = [.rightMouseDown, .rightMouseDragged, .rightMouseUp, .keyDown]
        monitor = NSEvent.addLocalMonitorForEvents(matching: mask) { [weak self] e in
            nonisolated(unsafe) let ev = e
            let consumed = MainActor.assumeIsolated { self?.handle(ev) ?? false }
            return consumed ? nil : e
        }
    }

    private func overlayPoint(_ e: NSEvent) -> CGPoint? {
        guard let w = e.window, w === ReleaseWindowController.shared.window, let c = w.contentView else { return nil }
        let p = c.convert(e.locationInWindow, from: nil)
        return CGPoint(x: p.x, y: c.isFlipped ? p.y : c.bounds.height - p.y)
    }

    /// True when the event was consumed.
    private func handle(_ e: NSEvent) -> Bool {
        let nav = NavigationUI.shared
        switch e.type {
        case .rightMouseDown:
            guard !replaying, let model, !model.showsCommandPalette, let p = overlayPoint(e),
                  !ReleaseWindowController.shared.chromeRects.values.contains(where: { $0.contains(p) })
            else { return false }
            pendingDown = e
            return true
        case .rightMouseDragged:
            guard let down = pendingDown, let model, let a = overlayPoint(down), let b = overlayPoint(e) else { return false }
            let off = CGSize(width: b.x - a.x, height: b.y - a.y)
            if nav.haloOrigin == nil, hypot(off.width, off.height) > 8 {
                withAnimation(Motion.pop) { nav.openHalo(at: a, model: model, marking: true) }
            }
            if nav.haloOrigin != nil {
                nav.haloSlot = RadialLayout.slot(for: off, count: nav.haloSlots.count)
            }
            return true
        case .rightMouseUp:
            guard let down = pendingDown else { return false }
            pendingDown = nil
            if nav.haloMarking {
                withAnimation(Motion.snappy) { nav.runHaloSlot(nav.haloSlot) }
            } else {
                // A plain right-click: replay it so the context menu opens.
                nonisolated(unsafe) let replay = down
                DispatchQueue.main.async { [weak self] in
                    self?.replaying = true
                    replay.window?.sendEvent(replay)
                    self?.replaying = false
                }
            }
            return true
        case .keyDown:
            guard nav.haloOrigin != nil else { return false }
            let n = nav.haloSlots.count
            switch e.keyCode {
            case 53: withAnimation(Motion.snappy) { nav.closeHalo() }               // esc
            case 36, 76: withAnimation(Motion.snappy) { nav.runHaloSlot(nav.haloSlot) } // return
            case 123, 126: nav.haloSlot = ((nav.haloSlot ?? 0) - 1 + n) % max(n, 1)  // ← ↑
            case 124, 125: nav.haloSlot = ((nav.haloSlot ?? -1) + 1) % max(n, 1)     // → ↓
            default:
                if let ch = e.charactersIgnoringModifiers, let d = Int(ch), (1...n).contains(d) {
                    withAnimation(Motion.snappy) { nav.runHaloSlot(d - 1) }
                } else { return false }
            }
            return true
        default:
            return false
        }
    }
}

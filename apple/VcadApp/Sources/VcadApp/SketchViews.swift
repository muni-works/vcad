import SwiftUI
import AppKit
import RealityKit

// Sketch-mode chrome: the sketch bar (the dock while sketching: plane · tools
// · feature · Done/Cancel), the constraint panel (DOF status, constraint glyph
// list, editable driving dimensions), and the 3D ink.

/// The sketch bar. While sketching it takes the dock's place — same glass
/// capsule, same spot: plane | tools | dimension, constrain | feature | Done ✕.
/// The active tool's prompt and the degrees of freedom live in the status
/// field's second line (`EditorModel.sketchStatusLine`).
struct SketchBar: View {
    @Bindable var model: EditorModel
    @State private var planeOpen = false
    @State private var featureOpen = false
    @Namespace private var morph

    /// Always on the bar, in order.
    static let primaryTools: [SketchToolKind] = [.select, .line, .rectangle, .circle, .spline]
    /// Behind the chevron.
    static let overflowTools: [SketchToolKind] = [.polygon, .slot, .arc]
    /// Editing tools, beside Smart Dimension.
    static let editTools: [SketchToolKind] = [.trim, .extend, .fillet]

    /// "Extrude 10 mm", "Revolve 360°", "Sweep"…
    static func featureTitle(_ m: EditorModel) -> String {
        func num(_ v: Double) -> String { v.formatted(.number.precision(.fractionLength(0...2))) }
        switch m.sketchFinish {
        case .extrude, .cut: return "\(m.sketchFinish.label) \(num(m.sketchExtrudeDepth)) mm"
        case .loft: return m.loftProfiles.isEmpty ? "Loft" : "Loft · \(m.loftProfiles.count)"
        case .revolve: return "Revolve \(num(m.sketchRevolveAngle))°"
        case .sweep, .extrudeToFace: return m.sketchFinish.label
        }
    }

    /// The primary button: a loft collects its first section before it can finish.
    static func doneTitle(_ m: EditorModel) -> String {
        m.sketchFinish == .loft && m.loftProfiles.count < 1 ? "Add Section" : "Done"
    }

    var body: some View {
        HStack(spacing: 4) {
            planeChip
            divider
            if let sketch = model.sketch {
                HStack(spacing: 2) {
                    ForEach(Self.primaryTools) { toolButton($0, sketch) }
                    overflowMenu(sketch)
                    if sketch.tool == .polygon {
                        Stepper("\(sketch.polygonSides) sides", value: Binding(get: { sketch.polygonSides }, set: { sketch.polygonSides = $0 }), in: 3...64)
                            .font(.callout).controlSize(.small).fixedSize().padding(.horizontal, 4)
                    }
                }
                divider
                toolButton(.dimension, sketch)
                ForEach(Self.editTools) { toolButton($0, sketch) }
                if sketch.tool == .fillet {
                    TextField("R", value: Binding(get: { sketch.filletRadius }, set: { sketch.filletRadius = max($0, 0.01) }),
                              format: .number.precision(.fractionLength(0...2)))
                        .frame(width: 44).textFieldStyle(.roundedBorder).controlSize(.small)
                        .help("Fillet radius (mm)")
                }
                constrainMenu(sketch)
            }
            divider
            featureButton
            doneButton
            Button { model.exitSketch() } label: {
                Image(systemName: "xmark").font(.system(size: 13, weight: .semibold))
                    .frame(width: 30, height: 32).contentShape(Capsule())
            }
            .help("Cancel sketch (Esc)")
            .accessibilityLabel("Cancel sketch")
        }
        .buttonStyle(.plain)
        .padding(.horizontal, 6).padding(.vertical, 3)
        .glassCapsule()
        .fixedSize()
        .alert("Offset profile", isPresented: $model.sketchOffsetPrompt) {
            TextField("Distance (mm, negative = inward)", value: $model.sketchOffsetDistance, format: .number)
            Button("Offset") { model.sketch?.offsetProfile(by: model.sketchOffsetDistance); model.sketchDirty = true }
            Button("Cancel", role: .cancel) {}
        }
    }

    private var divider: some View { Divider().frame(height: 22).padding(.horizontal, 4) }

    // MARK: plane

    /// "XY ▾" — or "On face" for a sketch on a picked face. Opens upward:
    /// the bar sits at the window's bottom edge.
    @ViewBuilder private var planeChip: some View {
        if model.sketchFace != nil {
            Label("On face", systemImage: "square.on.square")
                .font(.callout.weight(.semibold)).foregroundStyle(.secondary)
                .padding(.horizontal, 10).frame(height: 32)
                .help("Sketching on the picked face")
        } else {
            Button { planeOpen.toggle() } label: {
                HStack(spacing: 4) {
                    Text(model.sketchPlane.label).font(.callout.weight(.semibold))
                    Image(systemName: "chevron.up").font(.caption2.weight(.semibold)).foregroundStyle(.secondary)
                }
                .padding(.horizontal, 10).frame(height: 32).contentShape(Capsule())
            }
            .help("Sketch plane")
            .accessibilityLabel("Sketch plane, \(model.sketchPlane.label)")
            .popover(isPresented: $planeOpen, arrowEdge: .top) {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(SketchPlane.allCases) { p in
                        Button { planeOpen = false; model.setSketchPlane(p) } label: {
                            HStack {
                                Image(systemName: "checkmark").opacity(p == model.sketchPlane ? 1 : 0)
                                Text(p.label)
                                Spacer()
                            }
                            .frame(width: 90).padding(.vertical, 4).contentShape(Rectangle())
                        }.buttonStyle(.plain)
                    }
                }.padding(Theme.Space.s)
            }
        }
    }

    // MARK: tools

    private func toolButton(_ t: SketchToolKind, _ sketch: SketchSession) -> some View {
        let active = sketch.tool == t
        return Button { model.setSketchTool(t) } label: {
            Image(systemName: t.symbol).font(.system(size: 16, weight: .medium))
                .frame(width: 38, height: 32)
                .background {
                    if active {
                        Capsule(style: .continuous).fill(Color.primary.opacity(0.14))
                            .matchedGeometryEffect(id: "active", in: morph)
                    }
                }
                .contentShape(Capsule())
        }
        .help(t.tooltip)
        .accessibilityLabel(t.label)
        .accessibilityAddTraits(active ? .isSelected : [])
    }

    private func overflowMenu(_ sketch: SketchSession) -> some View {
        let picked = Self.overflowTools.first { $0 == sketch.tool }
        return Menu {
            ForEach(Self.overflowTools) { t in
                Button { model.setSketchTool(t) } label: { Label(t.tooltip, systemImage: t.symbol) }
            }
        } label: {
            Image(systemName: picked?.symbol ?? "chevron.down")
                .font(.system(size: picked == nil ? 10 : 16, weight: .semibold))
                .frame(width: picked == nil ? 22 : 38, height: 32)
                .background {
                    if picked != nil { Capsule(style: .continuous).fill(Color.primary.opacity(0.14)) }
                }
        }
        .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
        .help("More tools: Polygon, Slot, Arc")
        .accessibilityLabel("More sketch tools")
    }

    private func constrainMenu(_ sketch: SketchSession) -> some View {
        Menu {
            ForEach(SketchConstraintKind.allCases.filter { !$0.isDimension || $0 == .distance || $0 == .angle }) { k in
                Button("\(k.glyph)  \(k.label)") { _ = sketch.constrainSelection(k); model.sketchDirty = true }
            }
            Divider()
            Button("Delete Selected") { sketch.deleteSelectionOrConstraint(); model.sketchDirty = true }
                .disabled(sketch.selection.isEmpty && sketch.selectedConstraint == nil)
            Button("Toggle Construction  X") { sketch.toggleConstruction(); model.sketchDirty = true }
                .disabled(sketch.selection.isEmpty)
            Button("Mirror (line picked last)") { sketch.mirrorSelection(); model.sketchDirty = true }
                .disabled(sketch.selection.count < 2)
            Menu("Circular Pattern") {
                ForEach([2, 3, 4, 6, 8, 12], id: \.self) { n in
                    Button("\(n)×") { sketch.circularPattern(count: n); model.sketchDirty = true }
                }
            }.disabled(sketch.selection.isEmpty)
            Menu("Linear Pattern (along X)") {
                ForEach([2, 3, 4, 6], id: \.self) { n in
                    Button("\(n)× · 10 mm") { sketch.linearPattern(count: n, step: SIMD2(10, 0)); model.sketchDirty = true }
                }
            }.disabled(sketch.selection.isEmpty)
            Button("Offset Profile…") { model.sketchOffsetPrompt = true }
            Button("Remove Last Constraint") { sketch.popConstraint(); model.sketchDirty = true }
                .disabled(sketch.constraints.isEmpty)
        } label: {
            Image(systemName: "link").font(.system(size: 15, weight: .medium)).frame(width: 34, height: 32)
        }
        .menuStyle(.borderlessButton).menuIndicator(.hidden).fixedSize()
        .help("Constrain the selected entities")
        .accessibilityLabel("Constrain")
    }

    // MARK: feature + finish

    private var featureButton: some View {
        Button { featureOpen.toggle() } label: {
            HStack(spacing: 5) {
                Image(systemName: model.sketchFinish.symbol).font(.system(size: 13, weight: .medium))
                Text(Self.featureTitle(model)).font(.callout).monospacedDigit()
                Image(systemName: "chevron.up").font(.caption2.weight(.semibold)).foregroundStyle(.secondary)
            }
            .padding(.horizontal, 10).frame(height: 32)
            .background(featureOpen ? AnyShapeStyle(.quaternary) : AnyShapeStyle(.clear), in: Capsule(style: .continuous))
            .contentShape(Capsule())
        }
        .help("Feature to create when the sketch is done")
        .accessibilityLabel("Finish as \(Self.featureTitle(model))")
        .popover(isPresented: $featureOpen, arrowEdge: .top) { SketchFeaturePopover(model: model) }
    }

    private var doneButton: some View {
        let why = model.sketchFinishBlocker
        let purpose = "Finish the sketch as \(model.sketchFinish.label)"
        return Button { model.attemptFinishSketch() } label: {
            Text(Self.doneTitle(model)).font(.callout.weight(.semibold))
                .foregroundStyle(.white)
                .padding(.horizontal, 14).frame(height: 28)
                .background(Color.accentColor, in: Capsule(style: .continuous))
                .contentShape(Capsule())
        }
        // Never disabled: hovering or clicking a blocked Done says why, in
        // the status field, and authors nothing.
        .onHover { model.sketchDoneHovered = $0 }
        .help(why ?? purpose)
        .accessibilityLabel(Self.doneTitle(model))
        .accessibilityHint(why.map { "Can't finish: \($0)" } ?? purpose)
        .padding(.leading, 4)
    }
}

/// The feature button's popover: operation, its parameters, snap.
struct SketchFeaturePopover: View {
    @Bindable var model: EditorModel

    /// Grid steps offered under Snap (0 = off).
    static let snapSteps: [Double] = [0, 0.1, 0.5, 1, 5, 10]

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            Picker("Operation", selection: $model.sketchFinish) {
                ForEach(SketchFinish.allCases) { Label($0.label, systemImage: $0.symbol).tag($0) }
            }
            .accessibilityLabel("Finish")
            params
            if let sketch = model.sketch {
                Picker("Snap", selection: Binding(get: { sketch.gridStep }, set: { sketch.gridStep = $0 })) {
                    ForEach(Self.snapSteps, id: \.self) { s in
                        Text(s == 0 ? "Off" : "\(s.formatted()) mm").tag(s)
                    }
                }
                .help("Snap clicks to a grid")
            }
            if !model.loftProfiles.isEmpty {
                Text("\(model.loftProfiles.count) loft section\(model.loftProfiles.count == 1 ? "" : "s")")
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(Theme.Space.m)
        .frame(width: 300)
    }

    @ViewBuilder private var params: some View {
        switch model.sketchFinish {
        case .extrude, .cut, .loft:
            ScrubField(label: model.sketchFinish == .loft ? "Spacing" : "Distance", value: model.sketchExtrudeDepth,
                       sensitivity: 0.1, minValue: 0.1) { v, _ in
                model.sketchExtrudeDepth = v
            }.help(model.sketchFinish == .loft ? "Section spacing" : "Depth")
            if model.sketchFinish == .loft {
                Text("Add Section collects this sketch; two or more sections make the loft.")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        case .revolve:
            ScrubField(label: "Angle", value: model.sketchRevolveAngle, unit: "°", sensitivity: 1, minValue: 1) { v, _ in
                model.sketchRevolveAngle = min(v, 360)
            }.help("Angle (°) about the sketch Y axis")
        case .sweep:
            ScrubField(label: "Turns", value: model.sweepTurns, unit: "", sensitivity: 0.05, minValue: 0) { v, _ in
                model.sweepTurns = v
            }.help("Helix turns. 0 sweeps straight along the sketch normal.")
            ScrubField(label: "Radius", value: model.sweepHelixRadius, sensitivity: 0.1, minValue: 0.1) { v, _ in
                model.sweepHelixRadius = v
            }
            .disabled(model.sweepTurns <= 0)
            .help("Helix radius (mm), axis to path. The profile is centred on the path: its centroid starts at "
                  + "(radius, 0, 0) and climbs world Z. It must stay inside the radius, or the coil self-intersects.")
            ScrubField(label: "Length", value: model.sweepLength, sensitivity: 0.1, minValue: 0.1) { v, _ in
                model.sweepLength = v
            }.help("Total sweep length (mm): the helix height, or the straight distance. Pitch = length ÷ turns.")
        case .extrudeToFace:
            Text("Pick the target face").font(.caption).foregroundStyle(.secondary)
        }
    }
}

/// DOF status + constraint list with editable driving dimensions.
struct SketchConstraintPanel: View {
    @Bindable var model: EditorModel
    var body: some View {
        if let sketch = model.sketch {
            VStack(alignment: .leading, spacing: Theme.Space.s) {
                HStack {
                    Circle().fill(statusColor(sketch)).frame(width: 8, height: 8)
                    Text(sketch.stateLabel).font(.caption.weight(.semibold))
                    Spacer()
                    if !sketch.isFullyConstrained && !sketch.isOverConstrained {
                        Text("\(sketch.dof) DOF").font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                    }
                }
                if sketch.constraints.isEmpty {
                    Text("No constraints").font(.caption).foregroundStyle(.secondary)
                }
                ScrollView {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(sketch.constraints) { c in
                            let culprit = sketch.conflicting.contains(c.index)
                            HStack(spacing: 6) {
                                Text(c.kind.glyph).font(.caption.monospaced()).frame(width: 16)
                                Text(c.kind.label).font(.caption)
                                    .foregroundStyle(culprit ? Color.red : Color.primary)
                                    .help(culprit ? "Conflicts with the rest of the sketch: remove it or change its value" : "")
                                Spacer(minLength: 4)
                                if c.kind.isDimension {
                                    TextField("", value: Binding(get: { c.value },
                                                                 set: { sketch.setDimension(c, value: $0); model.sketchDirty = true }),
                                              format: .number.precision(.fractionLength(0...3)))
                                        .multilineTextAlignment(.trailing).frame(width: 64)
                                        .textFieldStyle(.roundedBorder).controlSize(.small)
                                        // Return commits and hands the keyboard back to
                                        // the canvas, so the next click acts on the sketch.
                                        .onSubmit { Self.returnFocusToCanvas(NSApp.keyWindow) }
                                    Text(c.kind == .angle ? "°" : "mm").font(.caption2).foregroundStyle(.secondary)
                                }
                                Button {
                                    sketch.deleteConstraint(c); model.sketchDirty = true
                                } label: { Image(systemName: "minus.circle") }
                                    .buttonStyle(.borderless).controlSize(.small)
                                    .foregroundStyle(culprit ? Color.red : Color.secondary)
                                    .help("Remove this constraint")
                            }
                        }
                    }
                }.frame(maxHeight: 220)
                if sketch.isOverConstrained, !sketch.conflicting.isEmpty {
                    Text("Remove a red constraint (or change its value) to recover")
                        .font(.caption2).foregroundStyle(.red).lineLimit(2)
                }
                if let err = sketch.lastError {
                    Text(err).font(.caption2).foregroundStyle(.red).lineLimit(2)
                }
                if !model.loftProfiles.isEmpty {
                    Text("\(model.loftProfiles.count) loft section\(model.loftProfiles.count == 1 ? "" : "s")")
                        .font(.caption2).foregroundStyle(.secondary)
                }
            }
            .padding(Theme.Space.m).frame(width: 230).panelSurface()
        }
    }
    /// End a field edit: the window's field editor lets go, so keystrokes
    /// and the next viewport click go to the canvas instead of the field.
    @discardableResult
    static func returnFocusToCanvas(_ window: NSWindow?) -> Bool {
        window?.makeFirstResponder(nil) ?? false
    }
    private func statusColor(_ s: SketchSession) -> Color {
        s.isOverConstrained ? .red : s.isFullyConstrained ? .primary : .blue
    }
}

/// The sketch ink under the kernel frame: committed geometry (pink), live
/// preview (cyan), points, selection (orange) and constraint glyph markers.
@MainActor
func buildSketchRoot(model: EditorModel) -> Entity {
    let root = Entity(); root.name = "sketchRoot"
    guard let sketch = model.sketch else { return root }
    // Solve-state colour: blue while free to move, the foreground colour
    // once fully constrained, red when over-constrained.
    let under = NSColor(srgbRed: 0.24, green: 0.52, blue: 1.0, alpha: 1.0)
    let full = NSColor(white: 0.92, alpha: 1)
    let over = NSColor(srgbRed: 0.95, green: 0.22, blue: 0.18, alpha: 1.0)
    let ink = sketch.isOverConstrained ? over : sketch.isFullyConstrained ? full : under
    let live = NSColor(srgbRed: 0.55, green: 0.92, blue: 1.0, alpha: 1.0)
    let sel = NSColor(red: 1.0, green: 0.62, blue: 0.12, alpha: 1)
    let frame = sketch.frame
    func w(_ p: SIMD2<Double>) -> SIMD3<Float> { SIMD3<Float>(frame.world(p)) }
    let width = max(model.displayedSceneSize, 40) * 0.004

    func ribbon(_ pairs: [SIMD2<Double>], _ color: NSColor, _ scale: Float = 1) {
        guard let m = EdgeOverlay.ribbonResource(segments: pairs.map(w), width: width * scale, name: "ink") else { return }
        root.addChild(ModelEntity(mesh: m, materials: [UnlitMaterial(color: color)]))
    }
    func dot(_ p: SIMD2<Double>, _ color: NSColor, _ r: Float) {
        let e = ModelEntity(mesh: .generateSphere(radius: r), materials: [UnlitMaterial(color: color)])
        e.position = w(p); root.addChild(e)
    }

    // Live profile shading: every closed region, translucent, under the ink.
    let fill = sketch.profileFill()
    if fill.count >= 3 {
        var d = MeshDescriptor(name: "profileFill")
        let lift = SIMD3<Float>(frame.normal) * width * 0.05
        d.positions = MeshBuffers.Positions(fill.map { w($0) - lift })
        let n = UInt32(fill.count)
        // Both windings: visible from either side of the plane.
        d.primitives = .triangles(Array(0..<n) + stride(from: 0, to: n, by: 3).flatMap { [$0, $0 + 2, $0 + 1] })
        if let m = try? MeshResource.generate(from: [d]) {
            var mat = UnlitMaterial(color: NSColor(srgbRed: 0.24, green: 0.52, blue: 1.0, alpha: 1))
            mat.blending = .transparent(opacity: 0.16)
            root.addChild(ModelEntity(mesh: m, materials: [mat]))
        }
    }

    // The sketch-plane grid (every plane, at the snap step) is its own
    // entity — see syncSketchGrid — so a cursor move doesn't rebuild it.

    let (geometry, preview, points) = sketch.ink()
    ribbon(geometry, ink)
    ribbon(sketch.constructionInk(dash: Double(width) * 3), NSColor(white: 0.6, alpha: 1), 0.7)
    ribbon(preview, live)
    for p in points { dot(p, ink, width * 1.2) }

    // Entities of conflicting constraints: re-inked red.
    let culprits = Set(sketch.constraints.filter { sketch.conflicting.contains($0.index) }.flatMap(\.entities))
    func reink(_ h: UInt32, _ color: NSColor, _ scale: Float) {
        guard let e = sketch.entity(h) else { return }
        switch e.kind {
        case .point: if let p = sketch.point(h) { dot(p, color, width * scale) }
        case .line:
            if let a = sketch.point(e.points[0]), let b = sketch.point(e.points[1]) { ribbon([a, b], color, scale) }
        case .circle, .arc:
            if let c = sketch.point(e.points.last ?? 0), let r = sketch.radii[h] {
                ribbon(SketchSession.ring(c, r, 0, 2 * .pi, 64), color, scale)
            }
        }
    }
    for h in culprits { reink(h, over, 1.5) }
    // Selection: re-ink selected entities thicker in orange.
    for h in sketch.selection { reink(h, sel, 2) }
    if let a = sketch.dimensionAnchor { reink(a, sel, 2) }

    // Constraint glyphs: a chip with its symbol beside the constrained
    // entity (dimensions blue, geometric green, conflicting red, picked
    // orange). Click one in Select, then Delete, to remove it.
    for c in sketch.constraints {
        guard let a = sketch.glyphAnchor(c) else { continue }
        let color: NSColor = sketch.selectedConstraint == c.index ? sel
            : sketch.conflicting.contains(c.index) ? over : c.kind.isDimension ? .systemBlue : .systemGreen
        glyphChip(c.kind.glyph, at: w(a), color: color, size: width * 3, normal: SIMD3<Float>(frame.normal), into: root)
    }
    // Live inference glyphs beside the cursor.
    if let inf = sketch.cursorInference, let c = sketch.cursor {
        for (i, g) in inf.inferred.enumerated() {
            let off = SIMD2<Double>(Double(width) * (4 + 3.6 * Double(i)), -Double(width) * 4)
            glyphChip(g.glyph, at: w(inf.point + off), color: .systemGreen, size: width * 3,
                      normal: SIMD3<Float>(frame.normal), into: root)
        }
        if inf.snapped != nil || !inf.inferred.isEmpty { dot(inf.point, .systemGreen, width * 2.2) }
        _ = c
    }

    // Landing marker: snapped point in green, else the cursor.
    if let c = sketch.cursor, sketch.tool.draws, sketch.cursorInference?.inferred.isEmpty ?? true {
        if let h = sketch.snapPoint(c), let p = sketch.point(h) { dot(p, .systemGreen, width * 2.2) }
        else { dot(c, live, width * 1.6) }
    }
    return root
}

/// One constraint glyph: a rounded chip with its symbol in white, facing
/// out of the sketch plane. Text meshes are cached per symbol.
@MainActor
private func glyphChip(_ glyph: String, at p: SIMD3<Float>, color: NSColor, size: Float, normal: SIMD3<Float>, into root: Entity) {
    let chip = ModelEntity(mesh: .generateBox(width: size, height: size, depth: size * 0.2, cornerRadius: size * 0.25),
                           materials: [UnlitMaterial(color: color)])
    chip.position = p
    chip.orientation = simd_quatf(from: SIMD3<Float>(0, 0, 1), to: simd_normalize(normal))
    if let text = GlyphText.mesh(glyph) {
        let t = ModelEntity(mesh: text, materials: [UnlitMaterial(color: .white)])
        let b = text.bounds
        let k = size * 0.7 / max(b.extents.x, b.extents.y, 1e-4)
        t.scale = SIMD3(repeating: k)
        t.position = SIMD3(-b.center.x * k, -b.center.y * k, size * 0.11)
        chip.addChild(t)
    }
    root.addChild(chip)
}

@MainActor
private enum GlyphText {
    static var cache: [String: MeshResource] = [:]
    static func mesh(_ g: String) -> MeshResource? {
        if let m = cache[g] { return m }
        let m = MeshResource.generateText(g, extrusionDepth: 0.01, font: .systemFont(ofSize: 1, weight: .bold))
        cache[g] = m
        return m
    }
}

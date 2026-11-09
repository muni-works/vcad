import SwiftUI
import RealityKit

// Sketch-mode chrome: the toolbar (plane · tools · finish op · Finish/Cancel),
// the constraint panel (DOF status, constraint glyph list, editable driving
// dimensions), the hint pill, and the 3D ink.

/// The sketch toolbar. Replaces the Create/Modify/Combine palette while sketching.
struct SketchPaletteView: View {
    @Bindable var model: EditorModel
    var body: some View {
        HStack(spacing: Theme.Space.m) {
            if model.sketchFace == nil {
                Picker("Sketch plane", selection: Binding(get: { model.sketchPlane }, set: { model.setSketchPlane($0) })) {
                    ForEach(SketchPlane.allCases) { Text($0.label).tag($0) }
                }
                .pickerStyle(.segmented).labelsHidden().controlSize(.small).fixedSize()
            } else {
                Label("On face", systemImage: "square.on.square").font(.caption).foregroundStyle(.secondary)
            }
            Divider().frame(height: 18)
            if let sketch = model.sketch {
                Picker("Sketch tool", selection: Binding(get: { sketch.tool }, set: { model.setSketchTool($0) })) {
                    ForEach(SketchToolKind.allCases) { Label($0.label, systemImage: $0.symbol).tag($0) }
                }
                .pickerStyle(.segmented).labelsHidden().controlSize(.small).fixedSize()
                Toggle("1 mm", isOn: Binding(get: { sketch.gridStep > 0 }, set: { sketch.gridStep = $0 ? 1 : 0 }))
                    .toggleStyle(.button).controlSize(.small).help("Snap clicks to a 1 mm grid")
                if sketch.tool == .polygon {
                    Stepper("\(sketch.polygonSides) sides", value: Binding(get: { sketch.polygonSides }, set: { sketch.polygonSides = $0 }), in: 3...64)
                        .controlSize(.small).fixedSize()
                }
                Menu {
                    ForEach(SketchConstraintKind.allCases.filter { !$0.isDimension || $0 == .distance || $0 == .angle }) { k in
                        Button("\(k.glyph)  \(k.label)") { _ = sketch.constrainSelection(k); model.sketchDirty = true }
                    }
                    Divider()
                    Button("Delete Selected") { sketch.deleteSelection(); model.sketchDirty = true }
                        .disabled(sketch.selection.isEmpty)
                    Button("Offset Profile…") { model.sketchOffsetPrompt = true }
                    Button("Remove Last Constraint") { sketch.popConstraint(); model.sketchDirty = true }
                        .disabled(sketch.constraints.isEmpty)
                } label: { Label("Constrain", systemImage: "link") }
                    .menuStyle(.borderlessButton).fixedSize()
                    .help("Constrain the selected entities")
            }
            Divider().frame(height: 18)
            Picker("Finish", selection: $model.sketchFinish) {
                ForEach(SketchFinish.allCases) { Label($0.label, systemImage: $0.symbol).tag($0) }
            }.labelsHidden().controlSize(.small).fixedSize()
            finishParams
            Button { model.finishSketch() } label: {
                Label(model.sketchFinish == .loft && model.loftProfiles.count < 1 ? "Add Section" : "Finish",
                      systemImage: "checkmark")
            }
            .buttonStyle(.borderedProminent).controlSize(.small)
            .disabled(!model.canFinishSketch)
            Button { model.exitSketch() } label: { Image(systemName: "xmark") }
                .buttonStyle(.bordered).controlSize(.small)
                .help("Cancel sketch (Esc)")
                .accessibilityLabel("Cancel sketch")
        }
        .padding(Theme.Space.s)
        .panelSurface()
        .alert("Offset profile", isPresented: $model.sketchOffsetPrompt) {
            TextField("Distance (mm, negative = inward)", value: $model.sketchOffsetDistance, format: .number)
            Button("Offset") { model.sketch?.offsetProfile(by: model.sketchOffsetDistance); model.sketchDirty = true }
            Button("Cancel", role: .cancel) {}
        }
    }

    @ViewBuilder private var finishParams: some View {
        switch model.sketchFinish {
        case .extrude, .cut, .loft:
            ScrubField(label: "", value: model.sketchExtrudeDepth, sensitivity: 0.1, minValue: 0.1) { v, _ in
                model.sketchExtrudeDepth = v
            }.frame(width: 80).help(model.sketchFinish == .loft ? "Section spacing" : "Depth")
        case .revolve:
            ScrubField(label: "", value: model.sketchRevolveAngle, sensitivity: 1, minValue: 1) { v, _ in
                model.sketchRevolveAngle = min(v, 360)
            }.frame(width: 80).help("Angle (°) about the sketch Y axis")
        case .sweep:
            HStack(spacing: 4) {
                ScrubField(label: "", value: model.sweepTurns, sensitivity: 0.05, minValue: 0) { v, _ in
                    model.sweepTurns = v
                }.frame(width: 60).help("Helix turns (0 = straight along the normal)")
                ScrubField(label: "", value: model.sweepPitch, sensitivity: 0.1, minValue: 0.1) { v, _ in
                    model.sweepPitch = v
                }.frame(width: 60).help("Helix pitch")
            }
        case .extrudeToFace:
            Text("pick target face").font(.caption).foregroundStyle(.secondary)
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
                }
                if sketch.constraints.isEmpty {
                    Text("No constraints").font(.caption).foregroundStyle(.secondary)
                }
                ScrollView {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(sketch.constraints) { c in
                            HStack(spacing: 6) {
                                Text(c.kind.glyph).font(.caption.monospaced()).frame(width: 16)
                                Text(c.kind.label).font(.caption)
                                Spacer(minLength: 4)
                                if c.kind.isDimension {
                                    TextField("", value: Binding(get: { c.value },
                                                                 set: { sketch.setDimension(c, value: $0); model.sketchDirty = true }),
                                              format: .number.precision(.fractionLength(0...3)))
                                        .multilineTextAlignment(.trailing).frame(width: 64)
                                        .textFieldStyle(.roundedBorder).controlSize(.small)
                                    Text(c.kind == .angle ? "°" : "mm").font(.caption2).foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                }.frame(maxHeight: 220)
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
    private func statusColor(_ s: SketchSession) -> Color {
        s.isOverConstrained ? .red : s.isFullyConstrained ? .green : .orange
    }
}

/// One-line prompt for the active tool.
struct SketchHintBar: View {
    let model: EditorModel
    var body: some View {
        HStack(spacing: Theme.Space.m) {
            HStack(spacing: 6) {
                Image(systemName: "hand.point.up.left")
                Text(model.sketch?.tool.hint ?? "")
            }
            if let c = model.sketchCursor {
                Divider().frame(height: 11)
                Text(String(format: "%.1f, %.1f mm", c.x + 0, c.y + 0)).monospacedDigit()
            }
            if let s = model.sketch {
                Divider().frame(height: 11)
                Text(s.stateLabel)
            }
        }
        .font(.subheadline)
        .foregroundStyle(.secondary)
        .padding(.horizontal, 14).padding(.vertical, 7)
        .pillSurface()
    }
}

/// The sketch ink under the kernel frame: committed geometry (pink), live
/// preview (cyan), points, selection (orange) and constraint glyph markers.
@MainActor
func buildSketchRoot(model: EditorModel) -> Entity {
    let root = Entity(); root.name = "sketchRoot"
    guard let sketch = model.sketch else { return root }
    let ink = NSColor(srgbRed: 0.98, green: 0.15, blue: 0.45, alpha: 1.0)
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

    // The floor grid only serves an XY sketch: every other plane gets its own,
    // with the sketch axes through the origin the cursor grid is measured from.
    if model.sketchFace != nil || model.sketchPlane != .xy {
        let spacing = 10.0, half = (Double(max(model.displayedSceneSize, 100)) / spacing).rounded(.up) * spacing
        var lines: [SIMD2<Double>] = []
        for v in stride(from: -half, through: half, by: spacing) where abs(v) > 1e-9 {
            lines += [SIMD2(v, -half), SIMD2(v, half), SIMD2(-half, v), SIMD2(half, v)]
        }
        ribbon(lines, NSColor(white: 0.38, alpha: 1), 0.5)
        ribbon([SIMD2(-half, 0), SIMD2(half, 0)], NSColor(srgbRed: 0.75, green: 0.25, blue: 0.25, alpha: 1))
        ribbon([SIMD2(0, -half), SIMD2(0, half)], NSColor(srgbRed: 0.25, green: 0.65, blue: 0.3, alpha: 1))
    }

    let (geometry, preview, points) = sketch.ink()
    ribbon(geometry, ink)
    ribbon(preview, live)
    for p in points { dot(p, ink, width * 1.2) }

    // Selection: re-ink selected entities thicker in orange.
    for h in sketch.selection {
        guard let e = sketch.entity(h) else { continue }
        switch e.kind {
        case .point: if let p = sketch.point(h) { dot(p, sel, width * 2) }
        case .line:
            if let a = sketch.point(e.points[0]), let b = sketch.point(e.points[1]) { ribbon([a, b], sel, 2) }
        case .circle, .arc:
            if let c = sketch.point(e.points.last ?? 0), let r = sketch.radii[h] {
                ribbon(SketchSession.ring(c, r, 0, 2 * .pi, 64), sel, 2)
            }
        }
    }

    // Constraint glyphs: a small colored chip beside each constrained entity
    // (dimensions blue, geometric green); the panel names them.
    for c in sketch.constraints {
        guard let a = sketch.glyphAnchor(c) else { continue }
        let color: NSColor = c.kind.isDimension ? .systemBlue : .systemGreen
        let chip = ModelEntity(mesh: .generateBox(size: width * 3, cornerRadius: width),
                               materials: [UnlitMaterial(color: color)])
        chip.position = w(a)
        root.addChild(chip)
    }

    // Landing marker: snapped point in green, else the cursor.
    if let c = sketch.cursor, sketch.tool != .select {
        if let h = sketch.snapPoint(c), let p = sketch.point(h) { dot(p, .systemGreen, width * 2.2) }
        else { dot(c, live, width * 1.6) }
    }
    return root
}

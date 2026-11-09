import simd
import SwiftUI

// Feature dialogs for the design workspace: native twins of the web app's
// `packages/app/src/components/dialogs/` (fillet/chamfer, shell, pattern,
// mirror, text) plus scale and cone. Stitch needs an FFI digitizer first. Every op lands through the same
// engine `add_feature` vocabulary the web uses, so a document authored here
// is the document the web would have written. Per-edge blends — which the
// engine vocabulary has no verb for — go through `vcad_feature_append`.

/// One modeling operation a dialog can author.
enum FeatureOp: String, CaseIterable, Identifiable {
    case fillet, chamfer, shell, hole, linearPattern, circularPattern, mirror, scale, text, cone

    var id: String { rawValue }
    var label: String {
        switch self {
        case .fillet: "Fillet"
        case .hole: "Hole"
        case .chamfer: "Chamfer"
        case .shell: "Shell"
        case .linearPattern: "Linear Pattern"
        case .circularPattern: "Circular Pattern"
        case .mirror: "Mirror"
        case .scale: "Scale"
        case .text: "Text"
        case .cone: "Cone"
        }
    }
    var symbol: String {
        switch self {
        case .fillet: "circle.bottomhalf.filled"
        case .hole: "smallcircle.filled.circle"
        case .chamfer: "triangle.bottomhalf.filled"
        case .shell: "square.dashed"
        case .linearPattern: "square.grid.3x1.below.line.grid.1x2"
        case .circularPattern: "circle.grid.cross"
        case .mirror: "arrow.left.and.right.righttriangle.left.righttriangle.right"
        case .scale: "arrow.up.left.and.arrow.down.right"
        case .text: "textformat"
        case .cone: "cone"
        }
    }
    /// Ops that create a new part instead of transforming the selected one.
    var createsPart: Bool { self == .text || self == .cone }
    /// Ops shown on the Modify tab.
    static let modifyOps: [FeatureOp] = [.fillet, .chamfer, .shell, .hole, .linearPattern,
                                         .circularPattern, .mirror, .scale]
    static let createOps: [FeatureOp] = [.cone, .text]
}

/// Axis presets shared by pattern and mirror dialogs.
enum OpAxis: String, CaseIterable, Identifiable {
    case x, y, z
    var id: String { rawValue }
    var label: String { rawValue.uppercased() }
    var vector: [Double] {
        switch self { case .x: [1, 0, 0]; case .y: [0, 1, 0]; case .z: [0, 0, 1] }
    }
    /// Mirror plane normal to this axis, as the engine names it.
    var mirrorPlane: String {
        switch self { case .x: "YZ"; case .y: "XZ"; case .z: "XY" }
    }
}

extension EditorModel {
    /// The selected part's stable feature id, if one is selected.
    var selectedPartFeatureId: String? { selectedPartIndex.flatMap { partId(forPart: $0) } }

    /// Fillet/chamfer: on the picked edges of the selected part when there
    /// are any (one EdgeBlend per edge, chained), else on every edge (the
    /// web dialog's behaviour).
    @discardableResult
    func applyBlend(fillet: Bool, size: Double) -> Bool {
        guard size > 0, let pi = selectedPartIndex ?? picking.selectionPart,
              let id = partId(forPart: pi) else { return false }
        let edges = picking.selectedEdgeIDs(part: pi)
        guard !edges.isEmpty else {
            return addOp(fillet ? .fillet(input: id, radius: size) : .chamfer(input: id, distance: size),
                         name: fillet ? "Fillet" : "Chamfer", materialFrom: id)
        }
        guard let root = rootNodeId(forPart: pi), let topo = partTopology(pi) else { return false }
        let features: [[String: Any]] = edges.enumerated().compactMap { i, eid in
            guard let e = topo.edge(eid) else { return nil }
            // Durable name when the solid carries one, else nearest-edge.
            let query: Any = e.selector?.any ?? [
                "type": "Near",
                "point": ["x": Double(e.midpoint.x), "y": Double(e.midpoint.y), "z": Double(e.midpoint.z)],
            ]
            return ["kind": "edge_blend", "child": i == 0 ? root : -1, "edges": query,
                    "size": size, "shape": fillet ? 1.0 : 0.0,
                    "name": fillet ? "Fillet" : "Chamfer"]
        }
        guard features.count == edges.count else { return false }
        let ok = appendIRFeatures(features, name: fillet ? "Fillet Edges" : "Chamfer Edges")
        if ok { picking.clear() }
        return ok
    }

    /// Shell: with the picked faces of the selected part removed (the
    /// opening — an open-top enclosure), else a closed hollow. Faces travel
    /// as their durable selectors, never the session ids.
    @discardableResult
    func applyShell(thickness: Double) -> Bool {
        guard thickness > 0, let pi = selectedPartIndex ?? picking.selectionPart,
              let id = partId(forPart: pi) else { return false }
        let faces = picking.selectedFaceIDs(part: pi)
        var openFacesJson: String?
        if !faces.isEmpty {
            guard let topo = partTopology(pi) else { return false }
            let refs: [Any] = faces.compactMap { topo.face($0)?.selector?.any }
            guard refs.count == faces.count,
                  let data = try? JSONSerialization.data(withJSONObject: refs),
                  let json = String(data: data, encoding: .utf8) else { return false }
            openFacesJson = json
        }
        let ok = addOp(.shell(input: id, thickness: thickness, openFacesJson: openFacesJson),
                       name: "Shell", materialFrom: id)
        if ok, !faces.isEmpty { picking.clear() }
        return ok
    }
}

/// ISO metric hole sizes: nominal, close-fit clearance and tap drill, mm.
enum MetricHole: String, CaseIterable, Identifiable {
    case m2 = "M2", m25 = "M2.5", m3 = "M3", m4 = "M4", m5 = "M5", m6 = "M6", m8 = "M8", m10 = "M10", m12 = "M12"
    var id: String { rawValue }
    var nominal: Double { Double(rawValue.dropFirst())! }
    var clearance: Double {
        switch self {
        case .m2: 2.2; case .m25: 2.7; case .m3: 3.2; case .m4: 4.3; case .m5: 5.3
        case .m6: 6.4; case .m8: 8.4; case .m10: 10.5; case .m12: 13
        }
    }
    var tapDrill: Double {
        switch self {
        case .m2: 1.6; case .m25: 2.05; case .m3: 2.5; case .m4: 3.3; case .m5: 4.2
        case .m6: 5; case .m8: 6.8; case .m10: 8.5; case .m12: 10.2
        }
    }
}

extension EditorModel {
    /// Longer than any part, so a through hole always clears the far side.
    static let throughAllDepth = 2000.0

    /// Cut a round hole into the picked planar face, optionally counterbored,
    /// as one undo step. `offset` moves it from the face centre along the
    /// face's own axes.
    @discardableResult
    func applyHole(diameter: Double, depth: Double?, offset: SIMD2<Double>,
                   counterbore: (diameter: Double, depth: Double)?,
                   boltCircle: (count: Int, radius: Double)? = nil) -> Bool {
        guard diameter > 0, picking.selectedFaces.count == 1, let f = picking.selectedFaces.first,
              let face = partTopology(f.part)?.face(f.id), face.isPlanar,
              let root = rootNodeId(forPart: f.part) else {
            engineError = "Pick one flat face"; return false
        }
        let frame = SketchFrame.onFace(center: SIMD3(face.center[0], face.center[1], face.center[2]),
                                       normal: SIMD3(face.normal[0], face.normal[1], face.normal[2]))
        // One hole, or one per bolt: evenly spaced around `offset`.
        let centers: [SIMD2<Double>] = boltCircle.map { b in
            (0..<b.count).map { i in
                let t = 2 * Double.pi * Double(i) / Double(b.count)
                return offset + b.radius * SIMD2(cos(t), sin(t))
            }
        } ?? [offset]
        func circle(_ d: Double, at c: SIMD2<Double>) -> [String: Any]? {
            guard let s = SketchSession(frame: frame) else { return nil }
            s.addCircle(center: s.addPoint(c), radius: d / 2)
            s.solve()
            return s.profileJSON()
        }
        func down(_ d: Double) -> [String: Double] {
            let v = frame.normal * -d
            return ["x": v.x, "y": v.y, "z": v.z]
        }
        var cuts: [(profile: [String: Any]?, depth: Double)] = []
        for c in centers {
            if let b = counterbore { cuts.append((circle(b.diameter, at: c), b.depth)) }
            cuts.append((circle(diameter, at: c), depth ?? Self.throughAllDepth))
        }
        var features: [[String: Any]] = []
        for (i, cut) in cuts.enumerated() {
            guard let profile = cut.profile else { engineError = "Couldn't build the hole"; return false }
            features.append(["kind": "cut_extrude", "target": i == 0 ? root : -1, "profile": profile,
                             "direction": down(cut.depth), "name": i == cuts.count - 1 ? "Hole" : "Counterbore"])
        }
        guard appendIRFeatures(features, name: "Hole") else { return false }
        picking.clear()
        selectedFeatureID = featureNodes.last?.id
        return true
    }
}

/// The dialog sheet. Presented from `model.activeFeatureOp`.
struct FeatureOpSheet: View {
    @Bindable var model: EditorModel
    let op: FeatureOp
    /// Hosted as a non-modal command panel: closing clears the active op.
    private func dismiss() { model.activeFeatureOp = nil }

    @State private var size: Double = 2
    @State private var thickness: Double = 1
    @State private var axis: OpAxis = .x
    @State private var count: Int = 3
    @State private var spacing: Double = 20
    @State private var angle: Double = 360
    @State private var origin: [Double] = [0, 0, 0]
    @State private var factor: [Double] = [1, 1, 1]
    @State private var uniform = true
    @State private var text = "vcad"
    @State private var height: Double = 10
    @State private var depth: Double = 2
    @State private var alignment = "left"
    @State private var rBottom: Double = 15
    @State private var rTop: Double = 0
    @State private var coneHeight: Double = 30
    @State private var error: String?
    @State private var holeSize = MetricHole.m3
    @State private var holeTapped = false
    @State private var holeThrough = true
    @State private var holeDepth: Double = 10
    @State private var holeX: Double = 0
    @State private var holeY: Double = 0
    @State private var holeBored = false
    @State private var holeBoreD: Double = 6.5
    @State private var holeBoreDepth: Double = 3.3
    @State private var holeCircle = false
    @State private var holeCount = 4
    @State private var holeRadius: Double = 20
    @State private var priorFilter: PickFilter?

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            Label(op.label, systemImage: op.symbol).font(.headline)
            if op == .hole {
                Text(model.picking.selectedFaces.count == 1 ? "On the picked face" : "Click a flat face.")
                    .foregroundStyle(.secondary)
            } else if !op.createsPart, model.selectedPartFeatureId == nil, model.picking.selectionPart == nil {
                Text("Select a part first.").foregroundStyle(.secondary)
            }
            Form { fields }.formStyle(.columns)
            if let error { Text(error).font(.caption).foregroundStyle(.red) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Apply") { apply() }.keyboardShortcut(.defaultAction).disabled(!canApply)
            }
        }
        .padding(Theme.Space.l).frame(width: 360)
        // The hole tool works on a face: clicks pick faces while it's open.
        .onAppear { if op == .hole { priorFilter = model.picking.filter; model.picking.filter = .face } }
        .onDisappear { if let f = priorFilter { model.picking.filter = f } }
        // A failure message describes the last attempt, not the next pick.
        .onChange(of: model.picking.revision) { _, _ in error = nil }
    }

    private var canApply: Bool {
        if op == .hole { return model.picking.selectedFaces.count == 1 }
        return op.createsPart || model.selectedPartFeatureId != nil || model.picking.selectionPart != nil
    }

    @ViewBuilder private var fields: some View {
        switch op {
        case .fillet, .chamfer:
            num(op == .fillet ? "Radius" : "Distance", $size, unit: "mm")
            let n = model.picking.selectedEdges.count
            Text(n > 0 ? "\(n) picked edge\(n == 1 ? "" : "s")" : "All edges (pick edges with ⌃E to limit)")
                .font(.caption).foregroundStyle(.secondary)
        case .shell:
            num("Thickness", $thickness, unit: "mm")
            let n = model.picking.selectedFaces.count
            Text(n > 0 ? "Opens \(n) picked face\(n == 1 ? "" : "s")" : "Closed shell (pick faces with ⌃F to open them)")
                .font(.caption).foregroundStyle(.secondary)
        case .hole:
            Picker("Size", selection: $holeSize) { ForEach(MetricHole.allCases) { Text($0.rawValue).tag($0) } }
                .onChange(of: holeSize) { _, v in holeBoreD = 2 * v.nominal + 0.5; holeBoreDepth = v.nominal + 0.3 }
            Picker("Fit", selection: $holeTapped) { Text("Clearance").tag(false); Text("Tap drill").tag(true) }
                .pickerStyle(.segmented)
            if holeTapped { Text("Drills \(holeSize.tapDrill.formatted()) mm. The thread itself isn't modeled.").font(.caption).foregroundStyle(.secondary) }
            Toggle("Through all", isOn: $holeThrough)
            if !holeThrough { num("Depth", $holeDepth, unit: "mm") }
            num("Offset X", $holeX, unit: "mm")
            num("Offset Y", $holeY, unit: "mm")
            Toggle("Bolt circle", isOn: $holeCircle)
            if holeCircle {
                Stepper("Holes: \(holeCount)", value: $holeCount, in: 2...64)
                num("Circle radius", $holeRadius, unit: "mm")
            }
            Toggle("Counterbore", isOn: $holeBored)
            if holeBored {
                num("Bore diameter", $holeBoreD, unit: "mm")
                num("Bore depth", $holeBoreDepth, unit: "mm")
            }
        case .linearPattern:
            axisPicker
            Stepper("Count: \(count)", value: $count, in: 2...100)
            num("Spacing", $spacing, unit: "mm")
        case .circularPattern:
            axisPicker
            Stepper("Count: \(count)", value: $count, in: 2...100)
            num("Angle", $angle, unit: "°")
            vec("Axis origin", $origin)
        case .mirror:
            Picker("Plane", selection: $axis) {
                ForEach(OpAxis.allCases) { Text($0.mirrorPlane).tag($0) }
            }.pickerStyle(.segmented)
        case .scale:
            Toggle("Uniform", isOn: $uniform)
            if uniform {
                num("Factor", Binding(get: { factor[0] }, set: { factor = [$0, $0, $0] }), unit: "×")
            } else {
                vec("Factor", $factor)
            }
        case .text:
            TextField("Text", text: $text)
            num("Height", $height, unit: "mm")
            num("Depth", $depth, unit: "mm")
            Picker("Align", selection: $alignment) {
                Text("Left").tag("left"); Text("Center").tag("center"); Text("Right").tag("right")
            }.pickerStyle(.segmented)
        case .cone:
            num("Bottom radius", $rBottom, unit: "mm")
            num("Top radius", $rTop, unit: "mm")
            num("Height", $coneHeight, unit: "mm")
        }
    }

    private var axisPicker: some View {
        Picker("Axis", selection: $axis) { ForEach(OpAxis.allCases) { Text($0.label).tag($0) } }
            .pickerStyle(.segmented)
    }

    private func num(_ label: String, _ v: Binding<Double>, unit: String) -> some View {
        LabeledContent(label) {
            HStack(spacing: 4) {
                TextField(label, value: v, format: .number.precision(.fractionLength(0...3)))
                    .multilineTextAlignment(.trailing).frame(width: 80).labelsHidden()
                Text(unit).foregroundStyle(.secondary)
                // A stepper so these panels work mouse-only (they were type-only).
                Stepper(label, value: v, step: unit == "°" ? 15 : unit == "×" ? 0.1 : 0.5).labelsHidden()
            }
        }
    }

    private func vec(_ label: String, _ v: Binding<[Double]>) -> some View {
        LabeledContent(label) {
            HStack(spacing: 4) {
                ForEach(0..<3, id: \.self) { i in
                    TextField("", value: Binding(get: { v.wrappedValue[i] }, set: { v.wrappedValue[i] = $0 }),
                              format: .number.precision(.fractionLength(0...3)))
                        .multilineTextAlignment(.trailing).frame(width: 56)
                }
            }
        }
    }

    private func apply() {
        let id = model.selectedPartFeatureId
            ?? model.picking.selectionPart.flatMap { model.partId(forPart: $0) }
        var ok = false
        switch op {
        case .fillet, .chamfer:
            ok = model.applyBlend(fillet: op == .fillet, size: size)
        case .shell:
            ok = model.applyShell(thickness: thickness)
        case .hole:
            ok = model.applyHole(diameter: holeTapped ? holeSize.tapDrill : holeSize.clearance,
                                 depth: holeThrough ? nil : holeDepth, offset: SIMD2(holeX, holeY),
                                 counterbore: holeBored ? (holeBoreD, holeBoreDepth) : nil,
                                 boltCircle: holeCircle ? (holeCount, holeRadius) : nil)
        case .linearPattern:
            if let id {
                ok = model.addOp(.linearPattern(input: id, direction: axis.vector, count: count, spacing: spacing),
                                 name: "Linear Pattern", materialFrom: id)
            }
        case .circularPattern:
            if let id {
                ok = model.addOp(.circularPattern(input: id, axisOrigin: origin, axisDir: axis.vector,
                                                  count: count, angleDeg: angle),
                                 name: "Circular Pattern", materialFrom: id)
            }
        case .mirror:
            if let id { ok = model.addOp(.mirror(input: id, plane: axis.mirrorPlane), name: "Mirror", materialFrom: id) }
        case .scale:
            if let id { ok = model.runCommand(.setScale(id: id, factor: factor), name: "Scale") }
        case .text:
            ok = !text.isEmpty && model.addOp(.text(text: text, height: height, depth: depth, alignment: alignment),
                                              name: "Text")
        case .cone:
            ok = model.addOp(.cone(radiusBottom: rBottom, radiusTop: rTop, height: coneHeight, segments: 64),
                             name: "Cone")
        }
        if ok { dismiss() } else { error = model.engineError ?? "\(op.label) failed" }
    }
}

import SwiftUI

// Feature dialogs for the design workspace: native twins of the web app's
// `packages/app/src/components/dialogs/` (fillet/chamfer, shell, pattern,
// mirror, text) plus scale and cone. Stitch needs an FFI digitizer first. Every op lands through the same
// engine `add_feature` vocabulary the web uses, so a document authored here
// is the document the web would have written. Per-edge blends — which the
// engine vocabulary has no verb for — go through `vcad_feature_append`.

/// One modeling operation a dialog can author.
enum FeatureOp: String, CaseIterable, Identifiable {
    case fillet, chamfer, shell, linearPattern, circularPattern, mirror, scale, text, cone

    var id: String { rawValue }
    var label: String {
        switch self {
        case .fillet: "Fillet"
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
    static let modifyOps: [FeatureOp] = [.fillet, .chamfer, .shell, .linearPattern,
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
}

/// The dialog sheet. Presented from `model.activeFeatureOp`.
struct FeatureOpSheet: View {
    @Bindable var model: EditorModel
    let op: FeatureOp
    @Environment(\.dismiss) private var dismiss

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

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            Label(op.label, systemImage: op.symbol).font(.headline)
            if !op.createsPart, model.selectedPartFeatureId == nil, model.picking.selectionPart == nil {
                Text("Select a part first.").foregroundStyle(.secondary)
            }
            Form { fields }.formStyle(.grouped).frame(minHeight: 120)
            if let error { Text(error).font(.caption).foregroundStyle(.red) }
            HStack {
                Spacer()
                Button("Cancel") { dismiss() }.keyboardShortcut(.cancelAction)
                Button("Apply") { apply() }.keyboardShortcut(.defaultAction).disabled(!canApply)
            }
        }
        .padding(Theme.Space.l).frame(width: 360)
    }

    private var canApply: Bool {
        op.createsPart || model.selectedPartFeatureId != nil || model.picking.selectionPart != nil
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
            if let id { ok = model.addOp(.shell(input: id, thickness: thickness), name: "Shell", materialFrom: id) }
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

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
    case fillet, chamfer, shell, hole, linearPattern, circularPattern, mirror, scale, move, rotate, sweepRotate, blend, text, hull, hullPoints, spurGear

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
        case .move: "Move"
        case .rotate: "Rotate"
        case .sweepRotate: "Rotational Sweep"
        case .blend: "Edge Blend"
        case .hull: "Convex Hull"
        case .hullPoints: "Hull from Points"
        case .spurGear: "Spur Gear"
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
        case .move: "arrow.up.and.down.and.arrow.left.and.right"
        case .rotate: "rotate.3d"
        case .sweepRotate: "arrow.trianglehead.2.clockwise.rotate.90"
        case .blend: "circle.bottomrighthalf.pattern.checkered"
        case .hull: "skew"
        case .hullPoints: "point.3.connected.trianglepath.dotted"
        case .spurGear: "gearshape"
        }
    }
    /// Ops that create a new part instead of transforming the selected one.
    var createsPart: Bool { [.text, .hullPoints, .spurGear].contains(self) }
    /// Ops shown on the Modify tab.
    static let modifyOps: [FeatureOp] = [.fillet, .chamfer, .shell, .hole, .linearPattern,
                                         .circularPattern, .mirror, .scale, .move, .rotate, .sweepRotate, .blend, .hull]
    static let createOps: [FeatureOp] = [.spurGear, .text, .hullPoints]
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
    func applyBlend(fillet: Bool, size: Double, shape: Double? = nil) -> Bool {
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
                    "size": size, "shape": shape ?? (fillet ? 1.0 : 0.0),
                    "name": shape != nil ? "Edge Blend" : fillet ? "Fillet" : "Chamfer"]
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
    @State private var error: String?
    @State private var vec3: [Double] = [0, 0, 0]
    @State private var angleLo: Double = 0
    @State private var angleHi: Double = 90
    @State private var clearance: Double = 0
    @State private var blendShape: Double = 0.5
    @State private var pointsText = "0 0 0\n20 0 0\n0 20 0\n0 0 20"
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
    @State private var gear = SpurGearParams()
    @State private var gearMesh = true

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
        .onAppear {
            if op == .hole { priorFilter = model.picking.filter; model.picking.filter = .face }
            if op == .blend { priorFilter = model.picking.filter; model.picking.filter = .edge }
            // Move and Rotate start from where the part already is.
            if op == .move || op == .rotate, let id = model.selectedPartFeatureId,
               let v = model.partVector(id, key: op == .move ? "offset" : "angles") { vec3 = v }
        }
        .onDisappear { if let f = priorFilter { model.picking.filter = f } }
        // A failure message describes the last attempt, not the next pick.
        .onChange(of: model.picking.revision) { _, _ in error = nil }
    }

    private var canApply: Bool {
        if op == .hole { return model.picking.selectedFaces.count == 1 }
        if op == .hull { return model.multiSelectedParts.count >= 2 }
        if op == .blend { return !model.picking.selectedEdges.isEmpty }
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
        case .spurGear:
            gearFields
        case .move:
            vec("Offset", $vec3)
        case .rotate:
            vec("Angles (°)", $vec3)
        case .sweepRotate:
            axisPicker
            vec("Axis origin", $origin)
            num("From", $angleLo, unit: "°")
            num("To", $angleHi, unit: "°")
            num("Clearance", $clearance, unit: "mm")
        case .blend:
            num("Size", $size, unit: "mm")
            LabeledContent("Shape") {
                Slider(value: $blendShape, in: 0...1).frame(width: 140)
            }
            let n = model.picking.selectedEdges.count
            Text(n > 0 ? "\(n) picked edge\(n == 1 ? "" : "s") · 0 is a chamfer, 1 a fillet" : "Click the edges to blend.")
                .font(.caption).foregroundStyle(.secondary)
        case .hullPoints:
            TextEditor(text: $pointsText).font(.body.monospaced()).frame(height: 96)
            Text("One x y z per line, at least four, not flat.").font(.caption).foregroundStyle(.secondary)
        case .hull:
            Text(model.multiSelectedParts.count >= 2 ? "\(model.multiSelectedParts.count) parts selected" : "Select two or more parts with ⌘-click.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    /// The gear the new one would mesh with, when the toggle is on.
    private var gearMate: PlacedSpurGear? { gearMesh ? model.selectedSpurGear : nil }

    @ViewBuilder private var gearFields: some View {
        let mate = model.selectedSpurGear
        if let mate {
            Toggle("Mesh with selected gear", isOn: $gearMesh)
            if gearMesh {
                Text("Matches its module (\(SpurGearMath.fmt(mate.module))) and pressure angle, at center distance "
                     + "\(SpurGearMath.fmt(SpurGearMath.centerDistance(module: mate.module, z1: mate.teeth, z2: gear.teeth))) mm, "
                     + "tooth into gap.")
                    .font(.caption).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
        }
        if gearMate == nil {
            num("Module", $gear.module, unit: "mm")
        }
        LabeledContent("Teeth") {
            Stepper("\(gear.teeth)", value: $gear.teeth, in: 4...400)
        }
        if gearMate == nil {
            Picker("Pressure angle", selection: $gear.pressureAngle) {
                Text("14.5°").tag(14.5); Text("20°").tag(20.0); Text("25°").tag(25.0)
            }.pickerStyle(.segmented)
        }
        num("Face width", $gear.faceWidth, unit: "mm")
        num("Bore", $gear.bore, unit: "mm")
        let shown = gearMate.map { m in
            var g = gear; g.module = m.module; g.pressureAngle = m.pressureAngle; return g
        } ?? gear
        if let why = shown.problem {
            Text(why).font(.caption).foregroundStyle(.red)
        } else {
            Text("Pitch ⌀\(SpurGearMath.fmt(shown.pitchDiameter)) · tip ⌀\(SpurGearMath.fmt(shown.tipDiameter)) mm")
                .font(.caption).foregroundStyle(.secondary)
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

    /// Every number in `text`, in order; a flat `[x, y, z, ...]` when whole triples.
    static func parsePoints(_ text: String) -> [Double] {
        let values = text.split(whereSeparator: { " ,\n\t".contains($0) }).compactMap { Double($0) }
        return values.count % 3 == 0 ? values : []
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
        case .hull:
            ok = model.hullSelected()
        case .hullPoints:
            let pts = FeatureOpSheet.parsePoints(pointsText)
            ok = pts.count >= 12 && model.addOp(.hull(inputs: [], points: pts), name: "Hull")
        case .move:
            if let id { ok = model.runCommand(.setTranslation(id: id, offset: vec3), name: "Move") }
        case .rotate:
            if let id { ok = model.runCommand(.setRotation(id: id, angles: vec3), name: "Rotate") }
        case .sweepRotate:
            if let id {
                ok = model.addOp(.sweepRotate(input: id, axisOrigin: origin, axisDir: axis.vector, angleLoDeg: angleLo,
                                              angleHiDeg: angleHi, clearance: clearance > 0 ? clearance : nil),
                                 name: "Rotational Sweep", materialFrom: id)
            }
        case .blend:
            ok = model.applyBlend(fillet: blendShape >= 0.5, size: size, shape: blendShape)
        case .spurGear:
            ok = model.addSpurGear(gear, meshWith: gearMate)
        }
        if ok { dismiss() } else { error = model.engineError ?? "\(op.label) failed" }
    }
}

/// A convex hull's inspector: which parts it wraps, and its loose points.
struct HullInputsEditor: View {
    @Bindable var model: EditorModel
    let nodeId: Int
    @State private var pointsText: String?

    var body: some View {
        if let hull = model.hullFeature(nodeId: nodeId) {
            let others = model.featureNodes.filter { node in
                guard let pid = node.partIndex.flatMap({ model.partId(forPart: $0) }) else { return false }
                return pid != hull.id && !hull.inputs.contains(pid)
            }
            VStack(alignment: .leading, spacing: 4) {
                ForEach(hull.inputs, id: \.self) { pid in
                    HStack {
                        Text(model.engineState?.parts.first { $0.id == pid }?.name ?? pid).lineLimit(1)
                        Spacer()
                        Button { remove(pid, from: hull) } label: { Image(systemName: "minus.circle") }
                            .buttonStyle(.borderless)
                            .disabled(hull.inputs.count == 1 && hull.points.count < 12)
                            .accessibilityLabel("Remove from hull")
                    }
                }
                Menu("Add Part") {
                    ForEach(others) { node in
                        Button(node.name) {
                            if let pi = node.partIndex, let pid = model.partId(forPart: pi) {
                                model.setHull(id: hull.id, inputs: hull.inputs + [pid], points: hull.points, name: "Add to Hull")
                            }
                        }
                    }
                }
                .disabled(others.isEmpty)
                TextField("Points: x y z per line", text: Binding(
                    get: { pointsText ?? Self.format(hull.points) },
                    set: { pointsText = $0 }), axis: .vertical)
                    .font(.caption.monospaced()).lineLimit(2...6)
                    .onSubmit {
                        guard let t = pointsText else { return }
                        let pts = t.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty ? [] : FeatureOpSheet.parsePoints(t)
                        if !t.isEmpty && pts.isEmpty { return }
                        model.setHull(id: hull.id, inputs: hull.inputs, points: pts, name: "Change Hull Points")
                        pointsText = nil
                    }
            }
        }
    }

    private func remove(_ pid: String, from hull: (id: String, inputs: [String], points: [Double])) {
        model.setHull(id: hull.id, inputs: hull.inputs.filter { $0 != pid }, points: hull.points, name: "Remove from Hull")
    }

    private static func format(_ p: [Double]) -> String {
        stride(from: 0, to: p.count - p.count % 3, by: 3)
            .map { "\(p[$0].formatted()) \(p[$0 + 1].formatted()) \(p[$0 + 2].formatted())" }
            .joined(separator: "\n")
    }
}

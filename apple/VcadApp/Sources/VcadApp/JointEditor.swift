import SwiftUI
import simd

// Joint editor: kind (fixed / revolute / prismatic / cylindrical / ball),
// parent → child, anchors (typed, or taken from the face / edge picked in the
// viewport), axis, limits and authored state.

struct JointEditorSheet: View {
    let model: EditorModel
    let title: String
    @State var spec: JointSpec
    /// Returns false to keep the sheet open (validation refusal).
    let commit: (JointSpec) -> Bool
    @Environment(\.dismiss) private var dismiss
    @State private var limitsOn = false
    @State private var lo = -90.0
    @State private var hi = 90.0
    @State private var pickNote: String?

    var body: some View {
        let ids = model.asmInstances.compactMap { $0["id"] as? String }
        Form {
            TextField("Name", text: Binding(get: { spec.name ?? "" }, set: { spec.name = $0.isEmpty ? nil : $0 }))
            Picker("Kind", selection: $spec.kind) {
                ForEach(JointKindChoice.allCases) { k in Label(k.label, systemImage: k.symbol).tag(k) }
            }
            Picker("Parent", selection: Binding(get: { spec.parentInstanceId ?? "" },
                                                set: { spec.parentInstanceId = $0.isEmpty ? nil : $0 })) {
                Text("World").tag("")
                ForEach(ids, id: \.self) { Text(AsmEdit.instanceName(model.asmJSON, $0)).tag($0) }
            }
            Picker("Child", selection: $spec.childInstanceId) {
                ForEach(ids, id: \.self) { Text(AsmEdit.instanceName(model.asmJSON, $0)).tag($0) }
            }
            Section("Anchors (part-local mm)") {
                anchorRow("Parent anchor", $spec.parentAnchor, instance: spec.parentInstanceId)
                anchorRow("Child anchor", $spec.childAnchor, instance: spec.childInstanceId)
                if let pickNote { Text(pickNote).font(.caption).foregroundStyle(.secondary) }
            }
            if spec.kind.hasAxis {
                Section("Axis") {
                    Vec3Field(label: "Direction", value: $spec.axis)
                    HStack {
                        ForEach(["X", "Y", "Z"], id: \.self) { a in
                            Button(a) {
                                spec.axis = a == "X" ? SIMD3(1, 0, 0) : a == "Y" ? SIMD3(0, 1, 0) : SIMD3(0, 0, 1)
                            }
                        }
                        Button("Picked face normal") { useFaceNormal() }
                    }
                    .controlSize(.small)
                }
            }
            if spec.kind.hasLimits {
                Section("Motion") {
                    Toggle("Limits", isOn: $limitsOn)
                    if limitsOn {
                        HStack {
                            TextField("Min", value: $lo, format: .number)
                            TextField("Max", value: $hi, format: .number)
                            Text(spec.kind.unit).foregroundStyle(.secondary)
                        }
                    }
                    TextField("State (\(spec.kind.unit))", value: $spec.state, format: .number)
                }
            }
            if let m = model.assembly.message { Text(m).font(.caption).foregroundStyle(.orange) }
        }
        .formStyle(.grouped)
        .frame(width: 400)
        .navigationTitle(title)
        .onAppear {
            model.assembly.message = nil
            if let l = spec.limits { limitsOn = true; lo = l.lowerBound; hi = l.upperBound }
        }
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            ToolbarItem(placement: .confirmationAction) {
                Button(title.hasPrefix("Add") ? "Add" : "Apply") {
                    var s = spec
                    s.limits = limitsOn && lo <= hi ? lo...hi : nil
                    if commit(s) { dismiss() }
                }
                .disabled(spec.childInstanceId.isEmpty)
            }
        }
    }

    private func anchorRow(_ label: String, _ v: Binding<SIMD3<Double>>, instance: String?) -> some View {
        HStack {
            Vec3Field(label: label, value: v)
            Button { usePick(v, instance: instance) } label: { Image(systemName: "scope") }
                .buttonStyle(.borderless)
                .help("Use the face or edge picked in the viewport")
                .disabled(instance == nil)
        }
    }

    /// Picked face/edge centre → anchor. Warns (but still applies) when the
    /// pick is on a different part than the instance's part definition.
    private func usePick(_ v: Binding<SIMD3<Double>>, instance: String?) {
        guard let pick = model.pickedAnchor() else {
            pickNote = "Pick a face or edge in the viewport first (face/edge pick filter)."
            return
        }
        v.wrappedValue = pick.point
        let def = model.asmInstances.first { $0["id"] as? String == instance }?["partDefId"] as? String
        pickNote = (pick.partDefId != nil && def != nil && pick.partDefId != def)
            ? "Picked geometry belongs to a different part than this instance."
            : String(format: "Anchor set from pick: (%.2f, %.2f, %.2f)", pick.point.x, pick.point.y, pick.point.z)
    }

    private func useFaceNormal() {
        guard let ref = model.picking.selection.first, ref.kind == .face,
              let f = model.partTopology(ref.part)?.face(ref.id) else {
            pickNote = "Pick a face to take its normal as the axis."
            return
        }
        let n = f.normalF
        spec.axis = SIMD3(Double(n.x), Double(n.y), Double(n.z))
    }
}

/// Three compact number fields.
struct Vec3Field: View {
    let label: String
    @Binding var value: SIMD3<Double>

    var body: some View {
        LabeledContent(label) {
            HStack(spacing: 4) {
                TextField("x", value: $value.x, format: .number).frame(width: 56)
                TextField("y", value: $value.y, format: .number).frame(width: 56)
                TextField("z", value: $value.z, format: .number).frame(width: 56)
            }
            .textFieldStyle(.roundedBorder).controlSize(.small)
        }
    }
}

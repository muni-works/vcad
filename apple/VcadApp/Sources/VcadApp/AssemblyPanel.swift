import SwiftUI
import simd

// The assembly panel: instances (insert / rename / ground / delete), joints
// (add / edit / FK drag / delete) and mates (add / check / delete). Native
// twin of the web's Insert Instance + Add Joint dialogs and the joint
// property rows, plus the mate checker the web doesn't surface.

struct AssemblyPanel: View {
    @Bindable var model: EditorModel
    @State private var tab: Tab = .joints
    @State private var newJoint: JointSpec?
    @State private var newMate: MateSpec?

    enum Tab: String, CaseIterable { case instances = "Instances", joints = "Joints", mates = "Mates" }

    private var ui: AssemblyUIState { model.assembly }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            PanelHeader(title: "Assembly", systemImage: "gearshape.2") { ui.showsPanel = false }
            Picker("", selection: $tab) {
                ForEach(Tab.allCases, id: \.self) { Text($0.rawValue).tag($0) }
            }
            .pickerStyle(.segmented).labelsHidden().controlSize(.small)

            switch tab {
            case .instances: instances
            case .joints: joints
            case .mates: mates
            }
            if let m = ui.message {
                Text(m).font(.caption).foregroundStyle(.orange).lineLimit(3)
            }
            HStack {
                Toggle("Timeline", isOn: Binding(get: { ui.showsTimeline }, set: { ui.showsTimeline = $0 }))
                    .toggleStyle(.button).controlSize(.small)
                    .disabled(model.assemblyInstanceCount == 0)
                Spacer()
            }
        }
        .padding(Theme.Space.l).frame(width: Theme.Width.navigator).panelSurface()
        .sheet(item: Binding(get: { newJoint.map(SheetBox.init) }, set: { newJoint = $0?.spec })) { box in
            JointEditorSheet(model: model, title: "Add Joint", spec: box.spec) { spec in
                model.addJoint(spec) != nil
            }
        }
        .sheet(item: Binding(get: { newMate.map(MateBox.init) }, set: { newMate = $0?.spec })) { box in
            MateEditorSheet(model: model, spec: box.spec)
        }
    }

    // MARK: instances

    private var instances: some View {
        VStack(alignment: .leading, spacing: 6) {
            Menu {
                let defs = AsmEdit.partDefs(model.asmJSON)
                if !defs.isEmpty {
                    Section("Part definitions") {
                        ForEach(defs, id: \.id) { d in
                            Button(d.name) { model.insertInstance(partDefId: d.id) }
                        }
                    }
                }
                if let g = model.documentGraph {
                    let defRoots = Set(defs.map(\.root))
                    let loose = g.visibleRoots.enumerated().filter { !defRoots.contains($0.element.nodeId) }
                    if !loose.isEmpty {
                        Section("Parts") {
                            ForEach(loose, id: \.offset) { i, r in
                                Button(g.nodes[r.nodeId]?.name ?? "Part \(i + 1)") { model.insertInstance(ofPart: i) }
                            }
                        }
                    }
                }
            } label: { Label("Insert Instance", systemImage: "plus.square.on.square") }
            .controlSize(.small)

            ScrollView {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(Array(model.asmInstances.enumerated()), id: \.offset) { _, inst in
                        InstanceRow(model: model, inst: inst)
                    }
                }
            }
            .frame(maxHeight: 260)
        }
    }

    // MARK: joints

    private var joints: some View {
        VStack(alignment: .leading, spacing: 6) {
            Button {
                var s = JointSpec()
                let ids = model.asmInstances.compactMap { $0["id"] as? String }
                // Web convention: first selected = parent, second = child.
                s.childInstanceId = model.selectedInstanceId ?? ids.last ?? ""
                s.parentInstanceId = ids.first { $0 != s.childInstanceId }
                newJoint = s
            } label: { Label("Add Joint", systemImage: "link.badge.plus") }
            .controlSize(.small)
            .disabled(model.asmInstances.isEmpty)

            ScrollView {
                VStack(alignment: .leading, spacing: 6) {
                    ForEach(Array(model.asmJoints.enumerated()), id: \.offset) { _, j in
                        JointDriveRow(model: model, joint: j)
                    }
                }
            }
            .frame(maxHeight: 320)
        }
    }

    // MARK: mates

    private var mates: some View {
        VStack(alignment: .leading, spacing: 6) {
            HStack {
                Button {
                    let ids = model.asmInstances.compactMap { $0["id"] as? String }
                    var s = MateSpec()
                    s.instanceA = ids.first ?? ""
                    s.instanceB = ids.dropFirst().first ?? ""
                    newMate = s
                } label: { Label("Add Mate", systemImage: "plus") }
                .disabled(model.asmInstances.count < 2)
                Button("Check") { model.checkMates() }.disabled(model.asmMates.isEmpty)
            }
            .controlSize(.small)
            if let e = ui.mateError { Text(e).font(.caption).foregroundStyle(.red).lineLimit(3) }
            ForEach(Array(model.asmMates.enumerated()), id: \.offset) { _, m in
                let id = m["id"] as? String ?? ""
                let verdict = ui.mateVerdicts.first { $0.id == id }
                HStack(alignment: .top, spacing: 6) {
                    Image(systemName: verdict == nil ? "circle.dashed"
                          : verdict!.pass ? "checkmark.circle.fill" : "xmark.octagon.fill")
                        .foregroundStyle(verdict == nil ? Color.secondary : verdict!.pass ? Color.green : Color.red)
                    VStack(alignment: .leading, spacing: 1) {
                        Text("\(AsmEdit.instanceName(model.asmJSON, m["instanceA"] as? String)) ↔ \(AsmEdit.instanceName(model.asmJSON, m["instanceB"] as? String))")
                            .font(.caption).lineLimit(1)
                        Text(verdict?.detail ?? ((m["kind"] as? [String: Any])?["type"] as? String ?? ""))
                            .font(.caption2).foregroundStyle(.secondary).lineLimit(2)
                    }
                    Spacer(minLength: 0)
                    Button { model.deleteMate(id) } label: { Image(systemName: "trash") }
                        .buttonStyle(.plain).foregroundStyle(.secondary)
                }
            }
        }
        .onAppear { if !model.asmMates.isEmpty { model.checkMates() } }
    }
}

private struct SheetBox: Identifiable { let spec: JointSpec; var id: String { "joint" } }
private struct MateBox: Identifiable { let spec: MateSpec; var id: String { "mate" } }

private struct InstanceRow: View {
    let model: EditorModel
    let inst: [String: Any]

    var body: some View {
        let id = inst["id"] as? String ?? ""
        let isGround = AsmEdit.groundInstanceId(model.asmJSON) == id
        let selected = model.selectedInstanceId == id
        HStack(spacing: 6) {
            Image(systemName: isGround ? "pin.fill" : "cube").foregroundStyle(isGround ? .orange : .secondary)
            InlineNameField(initial: inst["name"] as? String ?? id) { model.renameInstance(id, $0) }
            Spacer(minLength: 0)
        }
        .padding(.vertical, 2).padding(.horizontal, 4)
        .background(selected ? Color.accentColor.opacity(0.18) : .clear, in: RoundedRectangle(cornerRadius: Theme.Radius.row))
        .contentShape(Rectangle())
        .onTapGesture { model.assembly.selectedInstanceName = inst["name"] as? String ?? id }
        .contextMenu {
            Button(isGround ? "Unset Ground" : "Set as Ground") { model.setGroundInstance(isGround ? nil : id) }
            Button("Delete", role: .destructive) { model.deleteInstance(id) }
        }
    }
}

/// One joint: kind, parent → child, an FK drag slider for driven joints, and
/// edit / delete.
private struct JointDriveRow: View {
    let model: EditorModel
    let joint: [String: Any]
    @State private var editing: JointSpec?

    var body: some View {
        let id = joint["id"] as? String ?? ""
        let spec = AsmEdit.jointSpec(joint)
        let driven = spec.kind.hasLimits
        let range = spec.limits ?? (spec.kind == .revolute ? -180...180 : -200...200)
        let value = model.assembly.dragValues[id] ?? model.currentJointValues()[id] ?? spec.state
        let selected = model.assembly.selectedJointId == id
        VStack(alignment: .leading, spacing: 2) {
            HStack(spacing: 4) {
                Image(systemName: spec.kind.symbol).foregroundStyle(.secondary).frame(width: 14)
                Text(spec.name ?? id).font(.caption.weight(.medium)).lineLimit(1).truncationMode(.middle)
                Spacer(minLength: 2)
                if driven {
                    Text(String(format: "%.1f%@", value, spec.kind.unit))
                        .font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                }
                Button { editing = spec } label: { Image(systemName: "slider.horizontal.3") }
                    .buttonStyle(.plain).foregroundStyle(.secondary).help("Edit joint")
            }
            Text("\(AsmEdit.instanceName(model.asmJSON, spec.parentInstanceId)) → \(AsmEdit.instanceName(model.asmJSON, spec.childInstanceId))")
                .font(.caption2).foregroundStyle(.secondary).lineLimit(1)
            if driven {
                Slider(value: Binding(get: { min(max(value, range.lowerBound), range.upperBound) },
                                      set: { model.dragJoint(id, to: $0) }),
                       in: range) { editingNow in
                    // Release: in a timeline, the drag stays a pending key
                    // value; otherwise it becomes the authored state.
                    if !editingNow, model.timeline == nil { model.commitJointDrag(id) }
                }
                .controlSize(.mini)
                .accessibilityLabel("Drive \(spec.name ?? id)")
            }
        }
        .padding(4)
        .background(selected ? Color.accentColor.opacity(0.14) : .clear, in: RoundedRectangle(cornerRadius: Theme.Radius.row))
        .contentShape(Rectangle())
        .onTapGesture { model.assembly.selectedJointId = id }
        .contextMenu {
            Button("Edit…") { editing = spec }
            if driven {
                Button("Key at Playhead") { model.keyJoint(id) }
            }
            Button("Delete", role: .destructive) { model.deleteJoint(id) }
        }
        .sheet(item: Binding(get: { editing.map { EditBox(id: id, spec: $0) } }, set: { editing = $0?.spec })) { box in
            JointEditorSheet(model: model, title: "Edit Joint", spec: box.spec) { model.updateJoint(box.id, $0) }
        }
    }

    private struct EditBox: Identifiable { let id: String; let spec: JointSpec }
}

/// Mate editor: kind, the two instances, axis, offset.
private struct MateEditorSheet: View {
    let model: EditorModel
    @State var spec: MateSpec
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        let ids = model.asmInstances.compactMap { $0["id"] as? String }
        Form {
            Picker("Kind", selection: $spec.kind) {
                ForEach(MateKindChoice.allCases) { Text($0.label).tag($0) }
            }
            Picker("Instance A", selection: $spec.instanceA) {
                ForEach(ids, id: \.self) { Text(AsmEdit.instanceName(model.asmJSON, $0)).tag($0) }
            }
            Picker("Instance B", selection: $spec.instanceB) {
                ForEach(ids, id: \.self) { Text(AsmEdit.instanceName(model.asmJSON, $0)).tag($0) }
            }
            Vec3Field(label: "Axis", value: $spec.axis)
            if spec.kind == .planarOffset {
                TextField("Offset (mm)", value: $spec.offset, format: .number)
            }
        }
        .formStyle(.grouped)
        .frame(width: 360)
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            ToolbarItem(placement: .confirmationAction) {
                Button("Add") { if model.addMate(spec) != nil { dismiss() } }
            }
        }
    }
}

/// A list-row name field: commits on Return, no auto-focus.
private struct InlineNameField: View {
    let initial: String
    let commit: (String) -> Void
    @State private var text = ""

    var body: some View {
        TextField("", text: $text)
            .textFieldStyle(.plain).font(.caption)
            .onAppear { text = initial }
            .onChange(of: initial) { _, v in text = v }
            .onSubmit { if text != initial { commit(text) } }
            .accessibilityLabel("Instance name")
    }
}

/// Assemble menu: the assembly panel and the keyframe timeline.
struct AssemblyCommands: Commands {
    @Bindable var model: EditorModel

    var body: some Commands {
        CommandMenu("Assemble") {
            Button("Assembly Panel") { model.assembly.showsPanel.toggle() }
                .keyboardShortcut("j", modifiers: [.command, .shift])
            Button("Keyframe Timeline") { model.assembly.showsTimeline.toggle() }
                .keyboardShortcut("k", modifiers: [.command, .option])
                .disabled(model.assemblyInstanceCount == 0)
            Divider()
            Button("Key Pose at Playhead") { model.keyPose() }
                .disabled(model.assemblyInstanceCount == 0)
            Button("Check Mates") { model.assembly.showsPanel = true; model.checkMates() }
                .disabled(model.asmMates.isEmpty)
        }
    }
}

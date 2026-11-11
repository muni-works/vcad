import SwiftUI

// Parameter table: every named document parameter, editable as a number or a
// formula, plus add/remove. Bound fields re-resolve on every change.

struct ParameterTableSheet: View {
    @Bindable var model: EditorModel
    private func dismiss() { GlassUI.shared.closePanel() }
    @State private var newName = ""
    @State private var newValue = "10"

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            if model.docParameters.isEmpty {
                Text("No parameters yet. Add one below, then reference it from any field by name.")
                    .font(.caption).foregroundStyle(.secondary)
            }
            Table(model.docParameters) {
                TableColumn("Name") { p in Text(p.name).font(.callout.monospaced()).help(p.description ?? "") }
                TableColumn("Value / Formula") { p in
                    ParameterValueField(initial: p.formula ?? p.value.map { Self.fmt($0) } ?? "") { text in
                        model.setDocParameter(p.name, value: Self.parse(text), unit: p.unit)
                    }
                }
                TableColumn("Unit") { p in Text(p.unit ?? "").foregroundStyle(.secondary) }.width(48)
                // Imported-from-photos parameters carry their fit ±σ here.
                TableColumn("Note") { p in
                    Text(p.description ?? "").font(.caption).foregroundStyle(.secondary)
                        .lineLimit(1).help(p.description ?? "")
                }
                TableColumn("") { p in
                    Button(role: .destructive) { model.removeDocParameter(p.name) } label: { Image(systemName: "minus.circle") }
                        .buttonStyle(.borderless)
                }.width(28)
            }
            .frame(minHeight: 180)
            HStack {
                TextField("name", text: $newName).frame(width: 120)
                TextField("value or formula", text: $newValue)
                Button("Add") {
                    model.setDocParameter(newName, value: Self.parse(newValue))
                    newName = ""
                }.disabled(newName.trimmingCharacters(in: .whitespaces).isEmpty)
            }
            if let err = model.engineError { Text(err).font(.caption).foregroundStyle(.red).lineLimit(2) }
            HStack { Spacer(); Button("Done") { dismiss() }.keyboardShortcut(.defaultAction) }
        }
        .padding(Theme.Space.l).frame(width: 640)
    }

    /// A plain number stays a literal; anything else is a formula string.
    static func parse(_ s: String) -> Any {
        let t = s.trimmingCharacters(in: .whitespaces)
        return Double(t) ?? t
    }
    static func fmt(_ v: Double) -> String {
        v == v.rounded() ? String(Int(v)) : String(v)
    }
}

/// Commit-on-return text field for one parameter.
private struct ParameterValueField: View {
    let initial: String
    let commit: (String) -> Void
    @State private var text = ""
    var body: some View {
        TextField("", text: $text)
            .onAppear { text = initial }
            .onChange(of: initial) { _, v in text = v }
            .onSubmit { if text != initial { commit(text) } }
            .font(.callout.monospaced())
    }
}

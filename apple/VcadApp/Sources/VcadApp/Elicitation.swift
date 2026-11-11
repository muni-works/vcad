import Foundation
import Observation
import SwiftUI

// MCP elicitation, answered natively (M2). When a tool needs input — "which
// edge?", a confirm before a destructive edit, a material choice — the app is
// the MCP server and the user is sitting in front of it, so the question is
// asked HERE: a pick request arms viewport picking and the answer is a click
// on the model; a form request (MCP `requestedSchema`) shows a native sheet.
// Chat stays a fallback, not the only path: a client that answers an
// `input_required` result over MRTR (SEP-2322) by retrying with
// `inputResponses` short-circuits the UI.
//
// Wire shape follows `elicitation/create`: request {message, requestedSchema},
// response {action: accept|decline|cancel, content?}. vcad's extension
// `x-vcad-pick` ("edge" | "face" | "part", plus `x-vcad-multiple`) asks for a
// viewport selection; the accepted content is `{selection: [...]}` with
// durable selectors (IR EdgeQuery / FaceRef) next to the session ids.

/// What a pick request asks the user to click.
enum ElicitPick: String, Codable {
    case edge, face, part

    var filter: PickFilter {
        switch self { case .edge: .edge; case .face: .face; case .part: .body }
    }
    var noun: String { rawValue }
}

/// One `elicitation/create` request.
struct ElicitRequest: Identifiable {
    let id = UUID()
    var message: String
    /// MCP `requestedSchema` (flat object of primitive properties).
    var schema: [String: Any] = ["type": "object", "properties": [String: Any]()]
    /// Viewport pick instead of (or before) a form.
    var pick: ElicitPick?
    var multiple = false
    /// A destructive confirm: the accept button reads as destructive.
    var destructive = false

    /// Parse MCP `elicitation/create` params (with vcad's `x-vcad-*` extensions).
    init(params: [String: Any]) {
        message = params["message"] as? String ?? "The agent needs your input."
        let s = params["requestedSchema"] as? [String: Any] ?? [:]
        schema = s.isEmpty ? ["type": "object", "properties": [String: Any]()] : s
        pick = (params["x-vcad-pick"] as? String).flatMap(ElicitPick.init(rawValue:))
        multiple = params["x-vcad-multiple"] as? Bool ?? false
        destructive = params["x-vcad-destructive"] as? Bool ?? false
    }

    init(message: String, pick: ElicitPick? = nil, multiple: Bool = false,
         schema: [String: Any]? = nil, destructive: Bool = false) {
        self.message = message
        self.pick = pick
        self.multiple = multiple
        self.destructive = destructive
        if let schema { self.schema = schema }
    }

    /// The MCP params this request goes out as (for MRTR `inputRequests`).
    var params: [String: Any] {
        var p: [String: Any] = ["message": message, "requestedSchema": schema]
        if let pick { p["x-vcad-pick"] = pick.rawValue; p["x-vcad-multiple"] = multiple }
        if destructive { p["x-vcad-destructive"] = true }
        return p
    }

    /// Form fields, in a stable order (`required` first, then by name).
    var fields: [ElicitField] {
        let props = schema["properties"] as? [String: [String: Any]] ?? [:]
        let required = Set(schema["required"] as? [String] ?? [])
        return props.map { ElicitField(name: $0.key, spec: $0.value, required: required.contains($0.key)) }
            .sorted { ($0.required ? 0 : 1, $0.name) < ($1.required ? 0 : 1, $1.name) }
    }
}

/// One primitive property of a `requestedSchema`.
struct ElicitField: Identifiable {
    var id: String { name }
    let name: String
    let spec: [String: Any]
    let required: Bool

    var type: String { spec["type"] as? String ?? "string" }
    var title: String { spec["title"] as? String ?? name.replacingOccurrences(of: "_", with: " ").capitalized }
    var help: String? { spec["description"] as? String }
    var options: [String]? { spec["enum"] as? [String] }
    var optionNames: [String]? { spec["enumNames"] as? [String] }
    var defaultValue: Any? { spec["default"] }
}

enum ElicitAction: String { case accept, decline, cancel }

/// The answer to an `elicitation/create`.
struct ElicitResponse {
    var action: ElicitAction
    var content: [String: Any]? = nil

    var json: [String: Any] {
        var o: [String: Any] = ["action": action.rawValue]
        if let content { o["content"] = content }
        return o
    }

    init(action: ElicitAction, content: [String: Any]? = nil) {
        self.action = action
        self.content = content
    }

    /// Parse an MRTR `inputResponses` entry.
    init?(json: [String: Any]) {
        guard let a = (json["action"] as? String).flatMap(ElicitAction.init(rawValue:)) else { return nil }
        action = a
        content = json["content"] as? [String: Any]
    }
}

@MainActor @Observable
final class ElicitationCenter {
    static let shared = ElicitationCenter()

    /// The request on screen (banner for a pick, sheet for a form).
    private(set) var current: ElicitRequest?
    /// Form values for the current request, by field name.
    var values: [String: Any] = [:]
    /// What the user has picked so far (pick requests).
    private(set) var picked: [TopoRef] = []
    private(set) var pickedParts: [Int] = []

    @ObservationIgnored private var queue: [(ElicitRequest, CheckedContinuation<ElicitResponse, Never>)] = []
    @ObservationIgnored private var continuation: CheckedContinuation<ElicitResponse, Never>?
    @ObservationIgnored private weak var model: EditorModel?
    @ObservationIgnored private var pickHandler: UUID?
    @ObservationIgnored private var previousFilter: PickFilter?

    var showsSheet: Bool { current != nil && current?.pick == nil }

    /// Ask the user. Resolves when they answer (or the request is cancelled).
    func request(_ r: ElicitRequest, model m: EditorModel?) async -> ElicitResponse {
        await withCheckedContinuation { cont in
            queue.append((r, cont))
            model = m ?? model
            if current == nil { presentNext() }
        }
    }

    /// Answer the current request.
    func respond(_ action: ElicitAction) {
        guard let r = current, let cont = continuation else { return }
        var content: [String: Any]?
        if action == .accept {
            if let pick = r.pick { content = ["selection": selectionContent(pick)] }
            else { content = formContent(r) }
        }
        finish(cont, ElicitResponse(action: action, content: content))
    }

    /// Whether Accept is allowed right now (a pick needs something picked;
    /// required form fields need values).
    var canAccept: Bool {
        guard let r = current else { return false }
        if let pick = r.pick { return pick == .part ? !pickedParts.isEmpty : !picked.isEmpty }
        return r.fields.allSatisfy { !$0.required || values[$0.name] != nil }
    }

    /// Cancel everything outstanding (window closing, document switch).
    func cancelAll() {
        let pending = queue
        queue.removeAll()
        if let cont = continuation { finish(cont, ElicitResponse(action: .cancel), advance: false) }
        pending.forEach { $0.1.resume(returning: ElicitResponse(action: .cancel)) }
    }

    // MARK: picking

    /// The user clicked a part (body picks come from the model's selection,
    /// not the topology picker).
    func partSelectionChanged(_ parts: [Int]) {
        guard current?.pick == .part else { return }
        pickedParts = current?.multiple == true ? parts : Array(parts.suffix(1))
    }

    private func presentNext() {
        guard current == nil, !queue.isEmpty else { return }
        let (r, cont) = queue.removeFirst()
        current = r
        continuation = cont
        picked = []; pickedParts = []
        values = Dictionary(uniqueKeysWithValues: r.fields.compactMap { f in f.defaultValue.map { (f.name, $0) } })
        guard let pick = r.pick, let m = model else { return }
        previousFilter = m.picking.filter
        m.picking.filter = pick.filter
        m.picking.clear()
        if pick == .part {
            if let i = m.selectedPartIndex { pickedParts = [i] }
            return
        }
        pickHandler = m.picking.onSelectionChange { [weak self] refs in
            MainActor.assumeIsolated {
                guard let self, let r = self.current, r.pick == pick else { return }
                let wanted = refs.filter { $0.kind == (pick == .edge ? .edge : .face) }
                self.picked = r.multiple ? wanted : Array(wanted.suffix(1))
            }
        }
    }

    private func finish(_ cont: CheckedContinuation<ElicitResponse, Never>, _ resp: ElicitResponse,
                        advance: Bool = true) {
        if let m = model {
            if let h = pickHandler { m.picking.removeHandler(h) }
            if let f = previousFilter { m.picking.filter = f }
        }
        pickHandler = nil; previousFilter = nil
        current = nil; continuation = nil
        picked = []; pickedParts = []; values = [:]
        cont.resume(returning: resp)
        if advance { presentNext() }
    }

    private func selectionContent(_ pick: ElicitPick) -> [[String: Any]] {
        if pick == .part {
            return pickedParts.map { i in
                var o: [String: Any] = ["kind": "part", "part": i]
                if let node = model?.featureNodes.first(where: { $0.partIndex == i }) {
                    o["name"] = node.name; o["node"] = node.nodeId
                }
                return o
            }
        }
        return picked.map { ref in
            var o: [String: Any] = ["kind": ref.kind.rawValue, "part": ref.part, "id": Int(ref.id)]
            if let m = model, let data = AnalysisStore.documentData(m),
               let topo = m.picking.topology(part: ref.part, document: data) {
                let selector: JSONValue? = ref.kind == .edge
                    ? topo.edges.first { $0.id == ref.id }?.selector
                    : topo.faces.first { $0.id == ref.id }?.selector
                if let selector { o["selector"] = selector.any }
            }
            return o
        }
    }

    private func formContent(_ r: ElicitRequest) -> [String: Any] {
        var out: [String: Any] = [:]
        for f in r.fields {
            guard let v = values[f.name] else { continue }
            switch f.type {
            case "integer": out[f.name] = (v as? Int) ?? Int("\(v)") ?? v
            case "number": out[f.name] = (v as? Double) ?? Double("\(v)") ?? v
            default: out[f.name] = v
            }
        }
        return out
    }
}

extension Array {
    subscript(safe i: Int) -> Element? { indices.contains(i) ? self[i] : nil }
}

// MARK: - Views

/// Top-of-viewport banner while a pick elicitation is live: the question, a
/// running count of what's picked, and Use / Decline / Cancel.
struct ElicitationBanner: View {
    @Bindable var center = ElicitationCenter.shared

    var body: some View {
        if let r = center.current, let pick = r.pick {
            let n = pick == .part ? center.pickedParts.count : center.picked.count
            HStack(spacing: Theme.Space.m) {
                Image(systemName: "cursorarrow.click.2").foregroundStyle(.tint)
                VStack(alignment: .leading, spacing: 2) {
                    Text(r.message).font(.callout.weight(.medium))
                    Text(n == 0 ? "Click \(r.multiple ? "one or more" : "a") \(pick.noun)\(r.multiple ? "s" : "") in the viewport"
                               : "\(n) \(pick.noun)\(n == 1 ? "" : "s") picked")
                        .font(.caption).foregroundStyle(.secondary)
                }
                Spacer(minLength: Theme.Space.m)
                Button("Cancel") { center.respond(.cancel) }.keyboardShortcut(.cancelAction)
                Button("Decline") { center.respond(.decline) }
                Button("Use \(pick.noun)\(n == 1 || n == 0 ? "" : "s")") { center.respond(.accept) }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .disabled(!center.canAccept)
            }
            .padding(.horizontal, 14).padding(.vertical, 10)
            .frame(maxWidth: 560)
            .pillSurface()
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Agent request: \(r.message)")
        }
    }
}

/// Native sheet for a form elicitation (confirm, material choice, values).
struct ElicitationSheet: View {
    @Bindable var center = ElicitationCenter.shared

    var body: some View {
        if let r = center.current {
            VStack(alignment: .leading, spacing: Theme.Space.l) {
                Label {
                    Text(r.destructive ? "Confirm agent action" : "The agent is asking").font(.headline)
                } icon: {
                    Image(systemName: r.destructive ? "exclamationmark.triangle.fill" : "questionmark.bubble")
                        .foregroundStyle(r.destructive ? Color.orange : Color.accentColor)
                }
                Text(r.message).fixedSize(horizontal: false, vertical: true)
                ForEach(r.fields) { f in field(f) }
                HStack {
                    Button("Cancel") { center.respond(.cancel) }.keyboardShortcut(.cancelAction)
                    Spacer()
                    Button("Decline") { center.respond(.decline) }
                    Button(r.destructive ? "Do it" : "Answer", role: r.destructive ? .destructive : nil) {
                        center.respond(.accept)
                    }
                    .keyboardShortcut(.defaultAction)
                    .buttonStyle(.borderedProminent)
                    .tint(r.destructive ? .red : .accentColor)
                    .disabled(!center.canAccept)
                }
            }
            .padding(20)
            .frame(width: 400)
        }
    }

    @ViewBuilder private func field(_ f: ElicitField) -> some View {
        VStack(alignment: .leading, spacing: 4) {
            if let opts = f.options {
                Picker(f.title, selection: binding(f, default: opts.first ?? "")) {
                    ForEach(Array(opts.enumerated()), id: \.offset) { i, o in
                        Text(f.optionNames?[safe: i] ?? o).tag(o)
                    }
                }
            } else if f.type == "boolean" {
                Toggle(f.title, isOn: Binding(get: { center.values[f.name] as? Bool ?? false },
                                              set: { center.values[f.name] = $0 }))
            } else {
                TextField(f.title, text: Binding(get: { center.values[f.name].map { "\($0)" } ?? "" },
                                                 set: { center.values[f.name] = $0.isEmpty ? nil : $0 }))
            }
            if let h = f.help { Text(h).font(.caption).foregroundStyle(.secondary) }
        }
    }

    private func binding(_ f: ElicitField, default d: String) -> Binding<String> {
        Binding(get: { center.values[f.name] as? String ?? d }, set: { center.values[f.name] = $0 })
    }
}

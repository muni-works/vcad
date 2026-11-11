import SwiftUI

// MARK: - Feature history (engine `feature_history`)

/// One modeling feature as the engine's history sees it
/// (`vcad_app::history::HistoryRow`).
struct HistoryRow: Decodable, Equatable, Identifiable {
    enum State: String, Decodable { case built, suppressed, skipped, pastMarker = "past_marker", failed }
    struct Candidate: Decodable, Equatable, Identifiable {
        let name: String
        let point: [Double]
        let normal: [Double]
        let score: Double
        var id: String { name }
        /// `n123:top.2` → `top.2`: the scope is a node id, noise to a user.
        var label: String { name.split(separator: ":", maxSplits: 1).last.map(String.init) ?? name }
    }
    struct Broken: Decodable, Equatable {
        let node: Int
        let kind: String
        let reference: String?
        let message: String
        let candidates: [Candidate]
    }
    let id: String
    let index: Int
    let kind: String
    let name: String?
    let state: State
    let inputs: [String]
    var skippedBy: String? = nil
    var marker: Bool = false
    var root: Bool = false
    var node: Int? = nil
    var broken: [Broken] = []

    enum CodingKeys: String, CodingKey {
        case id, index, kind, name, state, inputs, marker, root, node, broken
        case skippedBy = "skipped_by"
    }
    init(from d: Decoder) throws {
        let c = try d.container(keyedBy: CodingKeys.self)
        id = try c.decode(String.self, forKey: .id)
        index = try c.decode(Int.self, forKey: .index)
        kind = try c.decode(String.self, forKey: .kind)
        name = try c.decodeIfPresent(String.self, forKey: .name)
        state = try c.decode(State.self, forKey: .state)
        inputs = try c.decodeIfPresent([String].self, forKey: .inputs) ?? []
        skippedBy = try c.decodeIfPresent(String.self, forKey: .skippedBy)
        marker = try c.decodeIfPresent(Bool.self, forKey: .marker) ?? false
        root = try c.decodeIfPresent(Bool.self, forKey: .root) ?? false
        node = try c.decodeIfPresent(Int.self, forKey: .node)
        broken = try c.decodeIfPresent([Broken].self, forKey: .broken) ?? []
    }

    var isBuilt: Bool { state == .built }
    var displayName: String { name ?? kind.capitalized }
    var symbol: String {
        switch kind {
        case "cube": return "cube"
        case "cylinder": return "cylinder"
        case "sphere": return "circle"
        case "boolean": return "square.on.square"
        case "fillet", "chamfer": return "square.dashed"
        case "ir-subgraph": return "scribble.variable"
        default: return "cube.transparent"
        }
    }
}

/// What the document tree draws, in history order.
enum HistoryTreeItem: Identifiable {
    /// A built feature that is a scene root (the existing part row).
    case node(FeatureNode, HistoryRow?)
    /// A feature that isn't built: suppressed, skipped, past the marker, failed.
    case ghost(HistoryRow)
    /// The rollback marker, after the row it follows.
    case marker(after: String)

    var id: String {
        switch self {
        case .node(let n, _): return n.id
        case .ghost(let h): return "ghost-\(h.id)"
        case .marker(let a): return "marker-\(a)"
        }
    }
}

extension EditorModel {
    /// Re-read the engine's history. Cheap: only parts carrying references
    /// are evaluated (Rust side).
    func refreshHistory() {
        guard usesDocumentTree, let engine,
              let text = try? engine.agentRead("feature_history", [:]),
              let rows = try? JSONDecoder().decode([HistoryRow].self, from: Data(text.utf8))
        else {
            if !history.isEmpty { history = [] }
            return
        }
        if rows != history { history = rows }
    }

    func historyRow(forPart node: FeatureNode) -> HistoryRow? {
        guard let id = engineState?.partId(forRootNode: node.nodeId) else { return nil }
        return history.first { $0.id == id }
    }

    /// The tree in history order: part rows where the history has a built
    /// root, ghost rows for unbuilt features, a marker row after the marker.
    /// Rows the history doesn't know (assembly instances, legacy docs) keep
    /// their place at the end.
    var historyTreeItems: [HistoryTreeItem] {
        guard !history.isEmpty else { return featureNodes.map { .node($0, nil) } }
        var byId: [String: FeatureNode] = [:]
        for n in featureNodes {
            if let id = engineState?.partId(forRootNode: n.nodeId) { byId[id] = n }
        }
        var used = Set<String>()
        var items: [HistoryTreeItem] = []
        for h in history {
            if h.isBuilt {
                if h.root, let n = byId[h.id] { items.append(.node(n, h)); used.insert(n.id) }
            } else {
                items.append(.ghost(h))
            }
            if h.marker { items.append(.marker(after: h.id)) }
        }
        items += featureNodes.filter { !used.contains($0.id) }.map { .node($0, nil) }
        return items
    }

    var isRolledBack: Bool { history.contains { $0.marker } }

    /// Rows with broken references at or under `row` (its inputs,
    /// transitively): a cut's row badges when its tool's sketch face is lost.
    func brokenRows(under row: HistoryRow) -> [HistoryRow] {
        var out: [HistoryRow] = [], seen = Set<String>(), stack = [row.id]
        while let id = stack.popLast() {
            guard seen.insert(id).inserted, let h = history.first(where: { $0.id == id }) else { continue }
            if !h.broken.isEmpty { out.append(h) }
            stack += h.inputs
        }
        return out
    }

    // MARK: suppress

    func isFeatureSuppressed(_ id: String) -> Bool { history.first { $0.id == id }?.state == .suppressed }

    /// Real suppress: the feature is kept but not built; features built on
    /// it are skipped (grey), never errors.
    func setFeatureSuppressed(_ id: String, _ on: Bool) {
        runCommand(.setParam(id: id, key: "suppressed", value: .bool(on)), name: on ? "Suppress" : "Unsuppress")
    }

    // MARK: rollback marker

    /// Move the marker after `id` (nil rolls to the end) as one undo step.
    func rollBack(after id: String?) {
        var cmds: [DocCommand] = history.filter { $0.marker && $0.id != id }
            .map { .setParam(id: $0.id, key: "rollback_marker", value: .bool(false)) }
        if let id { cmds.append(.setParam(id: id, key: "rollback_marker", value: .bool(true))) }
        guard !cmds.isEmpty else { return }
        runCommand(cmds.count == 1 ? cmds[0] : .batch(commands: cmds),
                   name: id == nil ? "Roll to End" : "Roll Back")
    }

    /// Step the marker one built feature earlier (-1) or later (+1).
    func stepMarker(_ delta: Int) {
        let rows = history
        let at = rows.firstIndex { $0.marker } ?? (rows.count - 1)
        let next = at + delta
        if next >= rows.count - 1 { rollBack(after: nil); return }
        guard next >= 0 else { return }
        rollBack(after: rows[next].id)
    }

    // MARK: reorder

    /// Why moving `id` to just before `target` (nil = the end) would break
    /// the history, or nil when the move is legal. Mirrors the engine's
    /// `reorder_violation`, so drop targets can turn red before the drop.
    func reorderProblem(moving id: String, before target: String?) -> String? {
        guard let me = history.first(where: { $0.id == id }) else { return nil }
        let order = history.map(\.id).filter { $0 != id }
        let at = target.flatMap { order.firstIndex(of: $0) } ?? order.count
        let ahead = Set(order[..<at])
        let name = { (fid: String) in self.history.first { $0.id == fid }?.displayName ?? fid }
        if let input = me.inputs.first(where: { !ahead.contains($0) && order.contains($0) }) {
            return "\(me.displayName) is built on \(name(input)), so it has to stay after it"
        }
        if let consumer = history.first(where: { ahead.contains($0.id) && $0.inputs.contains(id) }) {
            return "\(name(consumer.id)) is built on \(me.displayName), so it has to stay before it"
        }
        return nil
    }

    /// Move `id` to just before `target` (nil = the end). The engine
    /// re-checks and refuses an illegal move; the refusal is shown.
    @discardableResult
    func moveFeature(_ id: String, before target: String?) -> Bool {
        if let problem = reorderProblem(moving: id, before: target) {
            engineError = "Can't move there: \(problem)."
            return false
        }
        guard let me = history.first(where: { $0.id == id }) else { return false }
        // `Reorder` indexes all features with the moved one taken out.
        let index: Int
        if let target, let t = history.first(where: { $0.id == target }) {
            index = t.index > me.index ? t.index - 1 : t.index
        } else {
            index = (history.map(\.index).max() ?? me.index)
        }
        guard index != me.index else { return false }
        return runCommand(.reorder(id: id, index: index), name: "Reorder")
    }

    // MARK: broken references

    /// Re-point a broken face reference at `candidate` (one undo step). The
    /// reference keeps the face's point and normal as a fallback hint.
    @discardableResult
    func repairFaceReference(node: Int, with candidate: HistoryRow.Candidate) -> Bool {
        let v = { (a: [Double]) -> [String: Any] in ["x": a[0], "y": a[1], "z": a[2]] }
        let ok = editIR("Re-pick Face") { json in
            guard var nodes = json["nodes"] as? [String: Any],
                  var n = nodes[String(node)] as? [String: Any],
                  var op = n["op"] as? [String: Any] else { return }
            op["face"] = ["name": candidate.name, "point": v(candidate.point), "normal": v(candidate.normal)]
            n["op"] = op
            nodes[String(node)] = n
            json["nodes"] = nodes
        }
        if ok { repairingFeatureID = nil }
        return ok
    }
}

// MARK: - Views

/// A feature that isn't built: suppressed, skipped, past the marker, or
/// failed. Greyed, with the reason, and the actions that bring it back.
struct GhostFeatureRow: View {
    @Bindable var model: EditorModel
    let row: HistoryRow

    private var reason: String {
        switch row.state {
        case .suppressed: return "Suppressed"
        case .skipped:
            let by = row.skippedBy.flatMap { id in model.history.first { $0.id == id }?.displayName }
            return by.map { "Skipped — \($0) isn't built" } ?? "Skipped"
        case .pastMarker: return "After the rollback marker"
        case .failed: return "Couldn't be built: an input is missing"
        case .built: return ""
        }
    }

    var body: some View {
        HStack(spacing: 6) {
            Color.clear.frame(width: 12, height: 12)
            Image(systemName: row.state == .suppressed ? "nosign" : row.symbol)
                .font(.callout).frame(width: 16)
            VStack(alignment: .leading, spacing: 0) {
                Text(row.displayName).font(.callout).lineLimit(1)
                    .strikethrough(row.state == .suppressed)
                Text(reason).font(.caption).foregroundStyle(row.state == .failed ? AnyShapeStyle(.red) : AnyShapeStyle(.tertiary))
                    .lineLimit(1)
            }
            Spacer(minLength: 0)
        }
        .padding(.leading, Theme.Space.xs).padding(.trailing, 5).padding(.vertical, 4)
        .foregroundStyle(.secondary)
        .opacity(row.state == .failed ? 1 : 0.55)
        .contentShape(Rectangle())
        .historyDragAndDrop(model: model, id: row.id)
        .contextMenu {
            if row.state == .suppressed {
                Button { model.setFeatureSuppressed(row.id, false) } label: { Label("Unsuppress", systemImage: "checkmark.circle") }
            }
            if row.state == .pastMarker {
                Button { model.rollBack(after: row.id) } label: { Label("Roll Forward to Here", systemImage: "forward.end") }
                Button { model.rollBack(after: nil) } label: { Label("Roll to End", systemImage: "forward.end.alt") }
            }
        }
        .accessibilityElement(children: .combine)
        .accessibilityLabel("\(row.displayName), \(reason)")
    }
}

/// The rollback marker: the history is built down to here.
struct RollbackMarkerRow: View {
    @Bindable var model: EditorModel
    var body: some View {
        HStack(spacing: 6) {
            Rectangle().fill(Color.accentColor).frame(height: 2)
            Text("Rolled back").font(.caption2.weight(.semibold)).foregroundStyle(Color.accentColor)
            Button { model.stepMarker(-1) } label: { Image(systemName: "chevron.up") }
                .help("Roll back one more feature")
            Button { model.stepMarker(1) } label: { Image(systemName: "chevron.down") }
                .help("Roll forward one feature")
            Button("End") { model.rollBack(after: nil) }
                .help("Roll to the end of the history")
            Rectangle().fill(Color.accentColor).frame(height: 2)
        }
        .buttonStyle(.plain)
        .font(.caption2)
        .padding(.horizontal, Theme.Space.xs).padding(.vertical, 3)
        .accessibilityElement(children: .contain)
        .accessibilityLabel("Rollback marker")
    }
}

/// Red card under a feature whose reference no longer resolves: what was
/// lost, why, and a guided re-pick from the target's faces, best first.
struct BrokenReferenceCard: View {
    @Bindable var model: EditorModel
    /// The features holding broken references (the row itself or its inputs).
    let rows: [HistoryRow]

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            ForEach(Array(rows.flatMap(\.broken).enumerated()), id: \.offset) { _, b in
                Label {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(b.reference.map { "Sketch face \(shortName($0)) is gone" } ?? "Sketch face is gone")
                            .font(.callout.weight(.semibold))
                        Text(b.message).font(.caption).foregroundStyle(.secondary)
                    }
                } icon: {
                    Image(systemName: "exclamationmark.triangle.fill").foregroundStyle(.red)
                }
                if !b.candidates.isEmpty {
                    Text("Re-pick the face it sits on:").font(.caption).foregroundStyle(.secondary)
                    ForEach(b.candidates) { c in
                        Button { model.repairFaceReference(node: b.node, with: c) } label: {
                            HStack {
                                Image(systemName: "square.dashed.inset.filled")
                                Text(c.label).font(.callout.monospaced())
                                Spacer()
                                Text(normalLabel(c.normal)).font(.caption).foregroundStyle(.tertiary)
                            }
                            .contentShape(Rectangle())
                        }
                        .buttonStyle(.plain)
                        .accessibilityLabel("Re-pick face \(c.label)")
                    }
                }
            }
            HStack {
                if let first = rows.first {
                    Button("Suppress \(first.displayName)") { model.setFeatureSuppressed(first.id, true) }
                }
                Button("Undo") { model.undo() }.disabled(model.undoStack.isEmpty)
                Spacer()
                Button("Close") { model.repairingFeatureID = nil }
            }
            .controlSize(.small)
        }
        .padding(Theme.Space.s)
        .background(RoundedRectangle(cornerRadius: 8).fill(Color.red.opacity(0.08)))
        .overlay(RoundedRectangle(cornerRadius: 8).stroke(Color.red.opacity(0.35)))
        .padding(.leading, Theme.Space.l)
    }

    private func shortName(_ n: String) -> String { n.split(separator: ":", maxSplits: 1).last.map(String.init) ?? n }
    private func normalLabel(_ n: [Double]) -> String {
        let axes = [("+X", 0, 1.0), ("−X", 0, -1.0), ("+Y", 1, 1.0), ("−Y", 1, -1.0), ("+Z", 2, 1.0), ("−Z", 2, -1.0)]
        return axes.first { n.count == 3 && abs(n[$0.1] - $0.2) < 1e-6 }.map(\.0) ?? "angled"
    }
}

extension View {
    /// Drag a history row by feature id; drop one onto a row to move it just
    /// before that row. Illegal drops turn the target red with the reason.
    func historyDragAndDrop(model: EditorModel, id: String) -> some View {
        modifier(HistoryDropModifier(model: model, id: id))
    }
}

private struct HistoryDropModifier: ViewModifier {
    @Bindable var model: EditorModel
    let id: String
    @State private var targeted = false

    private var dragged: String? { model.draggingFeatureID }
    private var problem: String? {
        guard let d = dragged, d != id else { return nil }
        return model.reorderProblem(moving: d, before: id)
    }

    func body(content: Content) -> some View {
        content
            .overlay(alignment: .top) {
                if targeted, let d = dragged, d != id {
                    Rectangle().fill(problem == nil ? Color.green : Color.red).frame(height: 2)
                        .help(problem ?? "Move here")
                }
            }
            .draggable(id) {
                Text(model.history.first { $0.id == id }?.displayName ?? "Feature")
                    .padding(4).onAppear { model.draggingFeatureID = id }
            }
            .dropDestination(for: String.self) { items, _ in
                defer { model.draggingFeatureID = nil }
                guard let moving = items.first, moving != id else { return false }
                return model.moveFeature(moving, before: id)
            } isTargeted: { targeted = $0 }
    }
}

/// Drag-and-drop on a part row, when it has a history id.
struct HistoryRowDrag: ViewModifier {
    @Bindable var model: EditorModel
    let id: String?
    @ViewBuilder func body(content: Content) -> some View {
        if let id { content.historyDragAndDrop(model: model, id: id) } else { content }
    }
}

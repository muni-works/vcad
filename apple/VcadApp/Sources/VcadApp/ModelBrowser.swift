import SwiftUI
#if os(macOS)
import AppKit
#endif

// The Model browser (Fusion's model, Apple's manners): the document's NOUNS.
//
// The old tree mirrored the DAG, so every operation became the parent of its
// inputs — fillet a box and the box vanished into a "Fillet" row. Here the top
// level is the parts themselves, named after themselves (DocumentGraph
// .partName), and they stay put whatever is done to them. Repeated assembly
// instances collapse to one "Rotor × 2" row; sketches sit in a quiet section
// at the bottom. History is not drawn here at all.

// MARK: - Model (pure; tested in ModelBrowserTests)

/// One row of the browser.
enum BrowserItem: Identifiable, Equatable {
    /// A root solid of the document, or a single assembly instance.
    case part(FeatureNode)
    /// Several instances of one part def: "Rotor × 2", expandable.
    case group(id: String, name: String, members: [FeatureNode])

    var id: String {
        switch self {
        case .part(let n): return n.id
        case .group(let id, _, _): return id
        }
    }
    var title: String {
        switch self {
        case .part(let n): return n.name
        case .group(_, let name, let members): return ModelBrowser.groupTitle(name, count: members.count)
        }
    }
    /// The rows this item stands for (itself, or every member).
    var nodes: [FeatureNode] {
        switch self {
        case .part(let n): return [n]
        case .group(_, _, let members): return members
        }
    }
    static func == (a: BrowserItem, b: BrowserItem) -> Bool {
        a.id == b.id && a.title == b.title && a.nodes.map(\.id) == b.nodes.map(\.id)
    }
}

enum ModelBrowser {
    /// "Rotor × 2" — never "Rotor:1 / Rotor:2".
    static func groupTitle(_ name: String, count: Int) -> String {
        count > 1 ? "\(name) × \(count)" : name
    }

    /// Browser rows from the document's top-level feature rows: root parts as
    /// they are, assembly instances grouped by part def (in first-seen order).
    static func items(_ roots: [FeatureNode]) -> [BrowserItem] {
        var out: [BrowserItem] = []
        var groupAt: [String: Int] = [:]
        for node in roots {
            guard node.partIndex == nil, let def = node.groupName else {
                out.append(.part(node)); continue
            }
            let key = "\(node.nodeId)|\(def)"
            if let at = groupAt[key], case .group(let id, let name, let members) = out[at] {
                out[at] = .group(id: id, name: name, members: members + [node])
            } else if let at = groupAt[key], case .part(let first) = out[at] {
                out[at] = .group(id: "group:\(key)", name: def, members: [first, node])
            } else {
                groupAt[key] = out.count
                out.append(.part(node))
            }
        }
        return out
    }

    /// Case- and diacritic-insensitive name filter. A group stays when its
    /// own name or any member's matches.
    static func filter(_ items: [BrowserItem], _ query: String) -> [BrowserItem] {
        let q = query.trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return items }
        let hit = { (s: String) in s.range(of: q, options: [.caseInsensitive, .diacriticInsensitive]) != nil }
        return items.filter { item in hit(item.title) || item.nodes.contains { hit($0.name) } }
    }

    /// How many parts the browser shows (a group counts each instance).
    static func partCount(_ items: [BrowserItem]) -> Int { items.reduce(0) { $0 + $1.nodes.count } }
}

// MARK: - Row targets (selection, hover, visibility)

extension EditorModel {
    /// Viewport instances a browser row stands for. A root part of an
    /// assembly document (K1: roots double as part defs) maps to the
    /// instances drawn from it; an instance row maps to itself.
    func browserInstances(_ node: FeatureNode) -> [Int] {
        guard assemblyInstanceCount > 0 else { return [] }
        if node.id.hasPrefix("instance:") {
            return instanceIndex(id: String(node.id.dropFirst("instance:".count))).map { [$0] } ?? []
        }
        return instanceIndices(forNodeId: node.nodeId)
    }

    /// Whether the row can be hidden. Plain document parts can; assembly
    /// instances can't yet (the viewport's assembly path ignores per-instance
    /// visibility — follow-up), so their rows show no eye rather than a lie.
    func canHideBrowserRow(_ nodes: [FeatureNode]) -> Bool {
        nodes.allSatisfy { $0.partIndex != nil && browserInstances($0).isEmpty }
    }

    func isBrowserRowVisible(_ nodes: [FeatureNode]) -> Bool {
        nodes.contains { $0.partIndex.map(isPartVisible) ?? true }
    }

    func toggleBrowserVisibility(_ nodes: [FeatureNode]) {
        guard canHideBrowserRow(nodes) else { return }
        let parts = nodes.compactMap(\.partIndex)
        let hide = parts.contains(where: isPartVisible)
        for p in parts where isPartVisible(p) == hide { toggleVisibility(part: p) }
    }

    /// ⌥-click: show only this part (again to show everything).
    func isolateBrowserRow(_ nodes: [FeatureNode]) {
        guard canHideBrowserRow(nodes), let p = nodes.first?.partIndex else { return }
        isolate(part: p)
    }

    func isBrowserRowIsolated(_ nodes: [FeatureNode]) -> Bool {
        nodes.first?.partIndex.map { isolatedPart == $0 } ?? false
    }

    /// Swatch colour: the row's material, else the colour the viewport
    /// gives an unmaterialed part.
    func browserSwatch(_ node: FeatureNode) -> Color {
        // Same resolution the viewport uses, so the swatch matches the part.
        #if os(macOS)
        if let pi = node.partIndex { return Color(nsColor: resolvedMaterial(forPart: pi).color) }
        if let key = node.material { return Color(nsColor: resolvedMaterial(named: key, fallbackIndex: 0).color) }
        #endif
        return Color.secondary.opacity(0.5)
    }
}

// MARK: - Views

/// The browser body: filter field, parts, then sketches. Lives inside
/// `DesignModelNavigator`'s scroll view.
struct ModelBrowserView: View {
    @Bindable var model: EditorModel
    let items: [BrowserItem]
    let sketches: [FeatureNode]
    let filtering: Bool
    private var hasGroups: Bool { items.contains { if case .group = $0 { true } else { false } } }

    var body: some View {
        LazyVStack(alignment: .leading, spacing: 1) {
            if items.isEmpty && sketches.isEmpty {
                if filtering {
                    Text("No matches").font(.callout).foregroundStyle(.tertiary)
                        .frame(maxWidth: .infinity).padding(.vertical, Theme.Space.m)
                } else {
                    EmptyPanelState(title: "No parts yet", systemImage: "cube.transparent",
                                    detail: "Add a primitive or sketch a profile to start.")
                }
            }
            ForEach(items) { item in
                switch item {
                case .part(let node):
                    BrowserRow(model: model, nodes: [node], title: node.name, depth: 0, reserveDisclosure: hasGroups)
                case .group(let id, _, let members):
                    BrowserRow(model: model, nodes: members, title: item.title, depth: 0, groupID: id)
                    if model.expandedFeatureIDs.contains(id) {
                        ForEach(Array(members.enumerated()), id: \.element.id) { i, m in
                            BrowserRow(model: model, nodes: [m], title: memberTitle(m, i, members), depth: 1, reserveDisclosure: hasGroups)
                        }
                    }
                }
            }
            if !sketches.isEmpty {
                Eyebrow("Sketches")
                    .padding(.horizontal, Theme.Space.xs).padding(.top, Theme.Space.m).padding(.bottom, 2)
                    .foregroundStyle(.tertiary)
                ForEach(sketches) { s in
                    BrowserRow(model: model, nodes: [s], title: s.name, depth: 0, quiet: true, reserveDisclosure: hasGroups)
                }
            }
        }
        .animation(Motion.snappy, value: model.expandedFeatureIDs)
    }

    /// A member keeps its own name when it has one distinct from the group's;
    /// otherwise it is numbered.
    private func memberTitle(_ m: FeatureNode, _ i: Int, _ all: [FeatureNode]) -> String {
        let distinct = Set(all.map(\.name)).count == all.count
        return distinct && m.name != m.groupName ? m.name : "\(m.groupName ?? m.name) \(i + 1)"
    }
}

/// One browser row: swatch, name, and a visibility control that only shows
/// on hover (a hidden part is drawn dimmed instead of carrying an eye).
struct BrowserRow: View {
    @Bindable var model: EditorModel
    /// The feature rows this row stands for (one, or a group's members).
    let nodes: [FeatureNode]
    let title: String
    let depth: Int
    var groupID: String? = nil
    /// Sketches: no swatch, no visibility, secondary text.
    var quiet = false
    /// Keep a chevron's width so names line up when the list has groups.
    var reserveDisclosure = false
    @State private var hovering = false

    private var node: FeatureNode { nodes[0] }
    private var ids: Set<String> { Set(nodes.map(\.id)) }
    private var isInstance: Bool { node.id.hasPrefix("instance:") }
    private var instanceIndices: [Int] { nodes.flatMap(model.browserInstances) }

    private var selected: Bool {
        if let pi = node.partIndex, model.multiSelectedParts.contains(pi) { return true }
        if let si = model.selectedInstanceIndex, isInstance || groupID != nil {
            // A specific instance is selected: light only the row that holds it
            // (a collapsed group holds all of its members).
            return instanceIndices.contains(si) && (groupID == nil || !expanded)
        }
        return model.selectedFeatureID.map(ids.contains) ?? false
    }
    private var hovered: Bool {
        hovering
            || model.hoveredFeatureID.map(ids.contains) == true
            || (node.partIndex != nil && node.partIndex == model.hoveredPartIndex)
            || (isInstance && model.hoveredInstanceIndex.map(instanceIndices.contains) == true)
    }
    private var visible: Bool { quiet || model.isBrowserRowVisible(nodes) }
    private var hideable: Bool { !quiet && model.canHideBrowserRow(nodes) }
    private var expanded: Bool { groupID.map(model.expandedFeatureIDs.contains) ?? false }
    private var renaming: Bool { model.renamingFeatureID == node.id && groupID == nil }
    private var canRename: Bool { groupID == nil && (node.partIndex != nil || isInstance || quiet) }

    var body: some View {
        HStack(spacing: 6) {
            disclosure
            icon
            if renaming {
                RenameField(initial: title, commit: { commitRename($0) },
                            cancel: { model.renamingFeatureID = nil })
            } else {
                Text(title).font(.callout).lineLimit(1).truncationMode(.middle)
            }
            Spacer(minLength: 0)
            if hideable { visibilityButton }
        }
        .padding(.leading, CGFloat(depth) * Theme.Space.m + Theme.Space.xs)
        .padding(.trailing, 4).padding(.vertical, 4)
        .selectableRow(selected: selected, hovered: hovered)
        .foregroundStyle(selected ? AnyShapeStyle(.primary) : quiet ? AnyShapeStyle(.tertiary) : AnyShapeStyle(.secondary))
        .opacity(visible ? 1 : 0.4)
        .contentShape(Rectangle())
        .id(groupID ?? node.id)
        .onHover { inside in
            hovering = inside
            // Row → viewport: light the geometry. Cheap: no re-eval, only
            // hover state the viewport repaints from.
            if inside { model.hoverFeature(node) }
            else if model.hoveredFeatureID.map(ids.contains) == true { model.hoverFeature(nil) }
        }
        .onTapGesture { click() }
        .contextMenu { menu }
        .accessibilityElement(children: .combine)
        .accessibilityLabel(visible ? title : "\(title), hidden")
        .accessibilityAddTraits(selected ? .isSelected : [])
        .accessibilityAction(named: visible ? "Hide" : "Show") { model.toggleBrowserVisibility(nodes) }
    }

    @ViewBuilder private var disclosure: some View {
        if let groupID {
            Button { model.toggleExpanded(groupID) } label: {
                Image(systemName: "chevron.right")
                    .font(.caption2.weight(.semibold))
                    .rotationEffect(.degrees(expanded ? 90 : 0))
                    .foregroundStyle(.tertiary)
                    .frame(width: 10, height: 12).contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            .accessibilityLabel(expanded ? "Collapse" : "Expand")
        } else if reserveDisclosure && depth == 0 {
            Color.clear.frame(width: 10, height: 12)
        }
    }

    @ViewBuilder private var icon: some View {
        if quiet {
            Image(systemName: "pencil.and.outline").font(.caption).frame(width: 14)
        } else {
            // The material swatch is the icon: it tells parts apart at a glance
            // and matches what the viewport draws.
            Circle().fill(model.browserSwatch(node))
                .overlay(Circle().strokeBorder(.primary.opacity(0.18), lineWidth: 0.5))
                .frame(width: 10, height: 10)
                .frame(width: 14)
                .overlay(alignment: .bottomTrailing) {
                    if groupID != nil {
                        Circle().fill(model.browserSwatch(node)).frame(width: 6, height: 6)
                            .overlay(Circle().strokeBorder(.background, lineWidth: 1)).offset(x: 1, y: 2)
                    }
                }
        }
    }

    private var visibilityButton: some View {
        Button { model.toggleBrowserVisibility(nodes) } label: {
            Image(systemName: visible ? "eye" : "eye.slash")
                .font(.caption)
                .frame(width: 16, height: 16).contentShape(Rectangle())
        }
        .buttonStyle(.plain)
        .foregroundStyle(.tertiary)
        // Quiet until the row is hovered; a hidden row keeps its slashed eye
        // so the way back is visible without hunting.
        .opacity(hovering || !visible ? 1 : 0)
        .help(visible ? "Hide (⌥-click the row to isolate)" : "Show")
        .accessibilityHidden(true)
    }

    private func click() {
        guard !renaming else { return }
        #if os(macOS)
        let event = NSApp.currentEvent
        let flags = event?.modifierFlags ?? []
        if event?.clickCount == 2, canRename { model.renamingFeatureID = node.id; return }
        if flags.contains(.option), hideable { model.isolateBrowserRow(nodes); return }
        if flags.contains(.command), let pi = node.partIndex {
            model.toggleMultiSelect(part: pi, featureID: node.id); return
        }
        #endif
        if isInstance, groupID == nil, let i = instanceIndices.first {
            model.selectInstance(i)
        } else {
            model.selectFeature(node.id)
            model.selectionDirty = true
        }
    }

    private func commitRename(_ name: String) {
        defer { model.renamingFeatureID = nil }
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty, trimmed != title else { return }
        if isInstance { model.renameInstance(String(node.id.dropFirst("instance:".count)), trimmed) }
        else if let inst = node.instanceId { model.renameInstance(inst, trimmed) }
        else { model.renameFeature(node.nodeId, to: trimmed) }
    }

    @ViewBuilder private var menu: some View {
        if hideable {
            Button { model.isolateBrowserRow(nodes) } label: {
                Label(model.isBrowserRowIsolated(nodes) ? "Exit Isolate" : "Isolate", systemImage: "scope")
            }
            Button { model.toggleBrowserVisibility(nodes) } label: {
                Label(visible ? "Hide" : "Show", systemImage: visible ? "eye.slash" : "eye")
            }
            if model.hasHiddenParts {
                Button { model.showAllParts() } label: { Label("Show All", systemImage: "eye") }
            }
            Divider()
        }
        if canRename {
            Button { model.renamingFeatureID = node.id } label: { Label("Rename", systemImage: "pencil") }
        }
        Button {
            #if os(macOS)
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(title, forType: .string)
            #else
            UIPasteboard.general.string = title
            #endif
        } label: { Label("Copy Name", systemImage: "doc.on.doc") }
        if let pi = node.partIndex, groupID == nil {
            Divider()
            Button(role: .destructive) { model.deletePart(pi) } label: { Label("Delete Part", systemImage: "trash") }
        }
    }
}

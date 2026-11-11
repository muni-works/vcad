import SwiftUI

// Command palette (⇧⌘P): every modeling command by name, fuzzy-filtered,
// Return runs the top match.

struct PaletteCommand: Identifiable {
    let id: String
    let title: String
    let symbol: String
    /// Where the command lives (menu or tool group), shown beside it in Find.
    var group = ""
    /// Its key equivalent, so Find teaches the menu bar.
    var shortcut = ""
    var enabled = true
    let run: () -> Void
}

extension EditorModel {
    var paletteCommands: [PaletteCommand] {
        var out: [PaletteCommand] = []
        let doc = usesDocumentTree
        let hasPart = selectedPartIndex != nil || picking.selectionPart != nil
        for shape in BaseShape.allCases {
            out.append(.init(id: "add.\(shape.rawValue)", title: "Add \(shape.label)", symbol: shape.symbol, group: "Add Shape",
                             enabled: doc) { [weak self] in self?.arm(shape) })
        }
        for op in FeatureOp.allCases {
            out.append(.init(id: "op.\(op.rawValue)", title: op.label + "…", symbol: op.symbol, group: "Feature",
                             enabled: doc && (op.createsPart || hasPart)) { [weak self] in self?.activeFeatureOp = op })
        }
        out.append(.init(id: "import.photos", title: "Import from Photos (Experimental)…", symbol: "camera.viewfinder", group: "Import") { [weak self] in
            guard let self else { return }
            PhotoImportController.shared.show(for: self)
        })
        out.append(.init(id: "sketch", title: "New Sketch", symbol: "scribble.variable", group: "Sketch", shortcut: "⌃S", enabled: doc) { [weak self] in
            self?.enterSketch()
        })
        for f in SketchFinish.allCases {
            out.append(.init(id: "sketch.\(f.rawValue)", title: "Sketch → \(f.label)", symbol: f.symbol, enabled: doc) { [weak self] in
                self?.sketchFinish = f; self?.enterSketch()
            })
        }
        // Assembly and Fab verbs, so the palette reaches past Design.
        for space in [DockSpace.assembly, .fab] {
            for t in DockTools.tools(space, model: self) {
                out.append(.init(id: "tool.\(t.id)", title: "\(space.label): \(t.label)", symbol: t.symbol, enabled: t.enabled) { [weak self] in
                    guard let self else { return }
                    GlassUI.shared.enter(space, model: self)
                    t.action()
                })
            }
        }
        for b in BooleanOp.allCases {
            out.append(.init(id: "bool.\(b.rawValue)", title: b.label, symbol: b.symbol, group: "Combine",
                             enabled: multiSelectedParts.count == 2 || selectedPartIndex != nil) { [weak self] in self?.beginCombine(b) })
        }
        for f in PickFilter.allCases {
            out.append(.init(id: "pick.\(f.rawValue)", title: "Select \(f.label)", symbol: "cursorarrow.rays", group: "Select") { [weak self] in
                self?.picking.filter = f
            })
        }
        out.append(.init(id: "measure", title: measuring ? "Stop Measuring" : "Measure", symbol: "ruler", group: "Tools", shortcut: "⌃M", enabled: doc) { [weak self] in
            self?.measuring.toggle()
        })
        out.append(.init(id: "section", title: section == nil ? "Section View" : "Close Section View",
                         symbol: "square.split.diagonal", group: "Tools", shortcut: "⌥⌘X") { [weak self] in
            self?.toggleSmartSection()
        })
        out.append(.init(id: "params", title: "Parameters…", symbol: "slider.horizontal.3", group: "Tools", shortcut: "⌥⌘P", enabled: doc) { [weak self] in
            self?.showsParameterTable = true
        })
        out.append(.init(id: "undo", title: "Undo", symbol: "arrow.uturn.backward", group: "Edit", shortcut: "⌘Z", enabled: canUndo) { [weak self] in self?.undo() })
        out.append(.init(id: "redo", title: "Redo", symbol: "arrow.uturn.forward", group: "Edit", shortcut: "⇧⌘Z", enabled: canRedo) { [weak self] in self?.redo() })
        out.append(.init(id: "frame", title: "Frame All", symbol: "viewfinder", group: "Camera", shortcut: "⌘0") { [weak self] in self?.resetCamera(animated: true) })
        return out
    }
}

/// Find (⇧⌘P): fuzzy search over every modeling command, tool, camera view
/// and saved view. Empty query lists recents first; ↑↓ move, Return runs.
struct CommandPaletteSheet: View {
    @Bindable var model: EditorModel
    private func dismiss() { model.showsCommandPalette = false }
    @State private var query = ""
    @State private var cursor = 0
    @FocusState private var focused: Bool

    private var matches: [PaletteCommand] {
        let all = model.findCommands.filter(\.enabled)
        return CommandRanker.rank(all, query: query, recents: NavigationUI.shared.recents.ids,
                                  id: \.id, title: \.title)
    }

    /// Kept for callers that score directly; Find ranks with CommandRanker.
    static func score(_ s: String, _ q: String) -> Int? { CommandRanker.score(s, q) }

    var body: some View {
        let list = matches
        let recents = Set(NavigationUI.shared.recents.ids)
        VStack(spacing: 0) {
            HStack(spacing: Theme.Space.s) {
                Image(systemName: "magnifyingglass").foregroundStyle(.secondary)
                TextField("Find a command, tool or view…", text: $query)
                    .textFieldStyle(.plain).font(.title3)
                    .focused($focused)
                    .onSubmit { run(list.indices.contains(cursor) ? list[cursor] : list.first) }
                    .onKeyPress(.downArrow) { cursor = min(cursor + 1, max(list.count - 1, 0)); return .handled }
                    .onKeyPress(.upArrow) { cursor = max(cursor - 1, 0); return .handled }
                    .onChange(of: query) { _, _ in cursor = 0 }
            }
            .padding(Theme.Space.m)
            Divider()
            ScrollViewReader { proxy in
                ScrollView {
                    LazyVStack(spacing: 1) {
                        ForEach(Array(list.prefix(60).enumerated()), id: \.element.id) { i, c in
                            row(c, selected: i == cursor, recent: query.isEmpty && recents.contains(c.id))
                                .id(i)
                                .onTapGesture { run(c) }
                                .onHover { if $0 { cursor = i } }
                        }
                        if list.isEmpty {
                            Text("No command matches \u{201C}\(query)\u{201D}")
                                .font(.callout).foregroundStyle(.secondary).padding(Theme.Space.l)
                        }
                    }
                    .padding(Theme.Space.xs)
                }
                .onChange(of: cursor) { _, c in proxy.scrollTo(c) }
            }
            .frame(height: 320)
        }
        .frame(width: 520)
        .onAppear {
            query = NavigationUI.shared.findSeed
            NavigationUI.shared.findSeed = ""
            focused = true
        }
        .onExitCommand { dismiss() }
    }

    private func row(_ c: PaletteCommand, selected: Bool, recent: Bool) -> some View {
        HStack(spacing: Theme.Space.s) {
            Image(systemName: c.symbol).frame(width: 18).foregroundStyle(.secondary)
            Text(c.title).lineLimit(1)
            Spacer(minLength: Theme.Space.s)
            if recent {
                Image(systemName: "clock.arrow.circlepath").font(.caption).foregroundStyle(.tertiary)
            }
            if !c.group.isEmpty { Text(c.group).font(.caption).foregroundStyle(.secondary) }
            if !c.shortcut.isEmpty {
                Text(c.shortcut).font(.caption.monospaced()).foregroundStyle(.secondary).frame(minWidth: 36, alignment: .trailing)
            }
        }
        .padding(.horizontal, Theme.Space.s).padding(.vertical, 5)
        .contentShape(Rectangle())
        .selectableRow(selected: selected)
    }

    private func run(_ c: PaletteCommand?) {
        guard let c else { return }
        NavigationUI.shared.recordRun(c.id)
        dismiss()
        DispatchQueue.main.async { c.run() }
    }
}

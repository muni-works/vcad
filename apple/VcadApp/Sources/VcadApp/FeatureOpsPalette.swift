import SwiftUI

// Command palette (⇧⌘P): every modeling command by name, fuzzy-filtered,
// Return runs the top match.

struct PaletteCommand: Identifiable {
    let id: String
    let title: String
    let symbol: String
    var enabled = true
    let run: () -> Void
}

extension EditorModel {
    var paletteCommands: [PaletteCommand] {
        var out: [PaletteCommand] = []
        let doc = usesDocumentTree
        let hasPart = selectedPartIndex != nil || picking.selectionPart != nil
        for shape in BaseShape.allCases {
            out.append(.init(id: "add.\(shape.rawValue)", title: "Add \(shape.label)", symbol: shape.symbol,
                             enabled: doc) { [weak self] in self?.addPrimitive(shape) })
        }
        for op in FeatureOp.allCases {
            out.append(.init(id: "op.\(op.rawValue)", title: op.label + "…", symbol: op.symbol,
                             enabled: doc && (op.createsPart || hasPart)) { [weak self] in self?.activeFeatureOp = op })
        }
        out.append(.init(id: "sketch", title: "New Sketch", symbol: "scribble.variable", enabled: doc) { [weak self] in
            self?.enterSketch()
        })
        for f in SketchFinish.allCases {
            out.append(.init(id: "sketch.\(f.rawValue)", title: "Sketch → \(f.label)", symbol: f.symbol, enabled: doc) { [weak self] in
                self?.sketchFinish = f; self?.enterSketch()
            })
        }
        for b in BooleanOp.allCases {
            out.append(.init(id: "bool.\(b.rawValue)", title: b.label, symbol: b.symbol,
                             enabled: multiSelectedParts.count == 2) { [weak self] in self?.combineSelected(b) })
        }
        for f in PickFilter.allCases {
            out.append(.init(id: "pick.\(f.rawValue)", title: "Select \(f.label)", symbol: "cursorarrow.rays") { [weak self] in
                self?.picking.filter = f
            })
        }
        out.append(.init(id: "measure", title: measuring ? "Stop Measuring" : "Measure", symbol: "ruler", enabled: doc) { [weak self] in
            self?.measuring.toggle()
        })
        out.append(.init(id: "section", title: section == nil ? "Section View" : "Close Section View",
                         symbol: "square.split.diagonal") { [weak self] in
            guard let self else { return }
            self.section = self.section == nil ? SectionPlane() : nil
        })
        out.append(.init(id: "params", title: "Parameters…", symbol: "slider.horizontal.3", enabled: doc) { [weak self] in
            self?.showsParameterTable = true
        })
        out.append(.init(id: "undo", title: "Undo", symbol: "arrow.uturn.backward", enabled: canUndo) { [weak self] in self?.undo() })
        out.append(.init(id: "redo", title: "Redo", symbol: "arrow.uturn.forward", enabled: canRedo) { [weak self] in self?.redo() })
        out.append(.init(id: "frame", title: "Frame All", symbol: "viewfinder") { [weak self] in self?.resetCamera(animated: true) })
        return out
    }
}

struct CommandPaletteSheet: View {
    @Bindable var model: EditorModel
    private func dismiss() { model.showsCommandPalette = false }
    @State private var query = ""
    @FocusState private var focused: Bool

    private var matches: [PaletteCommand] {
        let all = model.paletteCommands.filter(\.enabled)
        let q = query.lowercased().trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return all }
        return all.compactMap { c -> (PaletteCommand, Int)? in
            Self.score(c.title.lowercased(), q).map { (c, $0) }
        }.sorted { $0.1 > $1.1 }.map(\.0)
    }

    /// Subsequence match; contiguous and word-start hits score higher.
    static func score(_ s: String, _ q: String) -> Int? {
        if s.contains(q) { return 1000 - s.count + (s.hasPrefix(q) ? 500 : 0) }
        var si = s.startIndex, score = 0
        for ch in q {
            guard let i = s[si...].firstIndex(of: ch) else { return nil }
            if i == s.startIndex || s[s.index(before: i)] == " " { score += 10 }
            score += 1; si = s.index(after: i)
        }
        return score
    }

    var body: some View {
        VStack(spacing: 0) {
            TextField("Type a command…", text: $query)
                .textFieldStyle(.plain).font(.title3).padding(Theme.Space.m)
                .focused($focused)
                .onSubmit { run(matches.first) }
            Divider()
            List(matches) { c in
                Button { run(c) } label: { Label(c.title, systemImage: c.symbol) }
                    .buttonStyle(.plain)
            }
            .listStyle(.plain).frame(height: 300)
        }
        .frame(width: 460)
        .onAppear { focused = true }
        .onExitCommand { dismiss() }
    }

    private func run(_ c: PaletteCommand?) {
        guard let c else { return }
        dismiss()
        DispatchQueue.main.async { c.run() }
    }
}

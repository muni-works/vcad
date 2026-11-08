import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct NativeElectronicsView: View {
    @Bindable var model: EditorModel
    private var ec: ElectronicsWorkspace { model.electronics }
    @State private var fileError: String?
    @State private var search = ""
    @State private var navigatorTab = "Parts"
    var body: some View {
        @Bindable var ec = ec
        ZStack(alignment: .top) {
            Color(nsColor: .underPageBackgroundColor)
            if ec.board(model).isEmpty {
                // An invitation, not a wall of disabled tools: one action here
                // and the same one first in the dock.
                ContentUnavailableView {
                    Label("Start a circuit board", systemImage: "cpu")
                } description: {
                    Text(ec.message ?? "Create a board in this document, or import one from KiCad or Eagle.")
                } actions: {
                    Button("Create Board") { ec.createBoard(model) }
                    Button("Import KiCad Board…") { ec.tools.importBoard(ec, model) }
                }
                .frame(maxHeight: .infinity)
            } else {
                GeometryReader { geometry in
                    ZStack(alignment: .top) {
                        Group {
                            if ec.view == .spatial { ElectronicsBoardPreview(board: ec.board(model)) }
                            else if ec.view == .schematic, ecRows(ec.schematic(model)["components"]).isEmpty {
                                // A board with no schematic (KiCad .kicad_pcb import) used
                                // to show a blank canvas with no explanation.
                                ContentUnavailableView {
                                    Label("No schematic yet", systemImage: "point.3.connected.trianglepath.dotted")
                                } description: {
                                    Text("This board came without a schematic. Build one from the board's parts and nets, then rearrange it.")
                                } actions: {
                                    Button("Build Schematic from Board") { ec.buildSchematicFromBoard(model) }
                                        .disabled(ecRows(ec.board(model)["footprints"]).isEmpty)
                                }
                            }
                            else { ElectronicsCanvas(model: model) }
                        }
                        .frame(maxWidth: .infinity, maxHeight: .infinity)
                        HStack(alignment: .top, spacing: 0) {
                            if model.showsTree {
                                navigator.padding(Theme.Space.m).frame(width: Theme.Width.navigator)
                                    .frame(maxHeight: min(470, geometry.size.height - Glass.toolbarHeight - 90))
                                    .panelSurface()
                                    .modifier(ChromeRegion(key: "ecNavigator"))
                            }
                            Spacer(minLength: 0)
                            if model.showsInspector {
                                inspector.padding(Theme.Space.m).frame(width: Theme.Width.inspector)
                                    .frame(maxHeight: min(600, geometry.size.height - Glass.toolbarHeight - 90))
                                    .panelSurface()
                                    .modifier(ChromeRegion(key: "ecInspector"))
                            }
                        }
                        .padding(.horizontal, Glass.inset).padding(.top, Glass.toolbarHeight)
                        VStack {
                            Spacer()
                            statusPill.padding(.bottom, Glass.inset + 6)
                        }
                    }
                }
            }
        }
        .frame(maxWidth: .infinity, maxHeight: .infinity, alignment: .top)
        .onChange(of: ec.boardID) { _, _ in ec.selectedRef = nil; ec.selectedTrace = nil; ec.selectedVia = nil; ec.routeStart = nil }
        .onChange(of: model.source) { _, _ in ec.selectedRef = nil; ec.selectedTrace = nil; ec.routeStart = nil; ec.pendingPin = nil }
        .onChange(of: ec.checking) { _, on in
            guard on else { return }
            GlassUI.shared.present("Connectivity", symbol: "link", width: 480, onClose: { ec.checking = false }) {
                VStack(alignment: .leading, spacing: 12) {
                    Text("Checks duplicate references, missing net assignments and trace metadata. This is not a clearance DRC or electrical simulation.").font(.callout).foregroundStyle(.secondary)
                    ForEach(ec.issues.isEmpty ? ["No issues found by these checks."] : ec.issues, id: \.self) { Text($0).font(.callout) }
                }
            }
        }
        .onChange(of: ec.tools.sheet) { _, kind in
            guard let kind else { if GlassUI.shared.panel?.title == Self.title(for: nil) { GlassUI.shared.panel = nil }; return }
            GlassUI.shared.present(Self.title(for: kind), symbol: "cpu", width: 600, onClose: { ec.tools.sheet = nil }) {
                ElectronicsToolSheet(model: model, kind: kind)
            }
        }
        .alert("Document error", isPresented: Binding(get: { fileError != nil }, set: { if !$0 { fileError = nil } })) { Button("OK") { fileError = nil } } message: { Text(fileError ?? "") }
        .background {
            Button("") { ec.routeStart = nil; ec.pendingPin = nil; ec.tool = .select }.keyboardShortcut(.cancelAction).frame(width: 0, height: 0).opacity(0).accessibilityHidden(true)
        }
    }
    static func title(for kind: ElectronicsTools.Sheet?) -> String {
        switch kind {
        case .drc: "Design Rule Check"
        case .length: "Diff Pair / Length Tuning"
        case .stackup: "Layer Stack"
        case .simulate: "Simulate"
        case .enclosure: "Enclosure Fit"
        case nil: ""
        }
    }

    /// One glass pill: what the tool wants, then the board's counts as numbers.
    private var statusPill: some View {
        HStack(spacing: 8) {
            if let busy = ec.tools.busy { ProgressView().controlSize(.mini); Text("\(busy)…") }
            Text(ec.tools.error ?? ec.tools.status ?? ec.message ?? (ec.view == .spatial ? "3D layout preview" : ec.tool == .wire ? "Select two pins to connect their net" : ec.tool == .route ? "Click to route · Esc ends the route" : ec.tool == .place ? "Click to place \(ec.componentKind.lowercased())" : ec.selectedRef ?? "Select a component"))
                .lineLimit(1)
            Text("·").foregroundStyle(.secondary)
            Text("\(ecRows(ec.board(model)["nets"]).count) nets").monospacedDigit()
            if let r = ec.tools.drc {
                let n = Int(ecNumber(r["route_attributable"]))
                Text("·").foregroundStyle(.secondary)
                Text(n == 0 ? "DRC clean" : "DRC \(n)").monospacedDigit()
            }
        }
        .font(.callout)
        .padding(.horizontal, 12).padding(.vertical, 6)
        .glassCapsule()
        .modifier(ChromeRegion(key: "ecStatus"))
    }
    private var navigator: some View {
        @Bindable var ec = ec
        return VStack(alignment: .leading, spacing: 12) {
            Picker("View", selection: $ec.view) { ForEach(ElectronicsView.allCases, id: \.self) { Text($0 == .spatial ? "3D" : $0.rawValue).tag($0) } }
                .pickerStyle(.segmented).labelsHidden()
                .help("Schematic, board, or 3D")
            TextField("Search parts or nets", text: $search).textFieldStyle(.roundedBorder)
            Picker("Navigator", selection: $navigatorTab) {
                ForEach(["Parts", "Nets", "Layers"], id: \.self) { Text($0) }
            }.pickerStyle(.segmented).labelsHidden()
            Divider()
            ScrollView {
                VStack(alignment: .leading, spacing: 4) {
                    if navigatorTab == "Parts" {
                        // The list follows the view: symbols in Schematic, footprints on the board.
                        let parts = ecRows(ec.view == .schematic ? ec.schematic(model)["components"] : ec.board(model)["footprints"])
                        ForEach(parts.indices, id: \.self) { i in
                            let row = parts[i]
                            let ref = row["ref"] as? String ?? "?"
                            let value = row["value"] as? String ?? ""
                            if search.isEmpty || (ref + value).localizedCaseInsensitiveContains(search) {
                                Button { ec.selectedRef = ref; ec.selectedTrace = nil; ec.selectedVia = nil; ec.tool = .select } label: {
                                    HStack { Image(systemName: "cpu"); Text(ref); Spacer(); Text(value).foregroundStyle(.secondary) }
                                        .padding(Theme.Space.s).selectableRow(selected: ec.selectedRef == ref)
                                }.buttonStyle(.plain).accessibilityAddTraits(ec.selectedRef == ref ? .isSelected : [])
                            }
                        }
                    } else if navigatorTab == "Nets" {
                        ForEach(ecRows(ec.board(model)["nets"]), id: \.ecID) { row in
                            let name = row["name"] as? String ?? row.ecID
                            if search.isEmpty || name.localizedCaseInsensitiveContains(search) {
                                Button { ec.activeNet = row.ecID; ec.routeStart = nil } label: {
                                    HStack { Image(systemName: "point.3.connected.trianglepath.dotted"); Text(name); Spacer() }
                                        .padding(Theme.Space.s).selectableRow(selected: ec.activeNet == row.ecID)
                                }.buttonStyle(.plain).accessibilityAddTraits(ec.activeNet == row.ecID ? .isSelected : [])
                            }
                        }
                        if ecRows(ec.board(model)["nets"]).isEmpty {
                            EmptyPanelState(title: "No nets", systemImage: "point.3.connected.trianglepath.dotted",
                                            detail: "Connect schematic pins to create a net.")
                        }
                    } else {
                        Picker("Active copper", selection: $ec.activeLayer) {
                            Text("Front copper").tag("FCu"); Text("Back copper").tag("BCu")
                        }.pickerStyle(.radioGroup)
                    }
                }.font(.callout)
            }
            Divider()
            Button(ec.view == .schematic ? "Show in Board" : "Show in Schematic", systemImage: "arrow.left.arrow.right") {
                ec.view = ec.view == .schematic ? .board : .schematic
            }
            Group {
                if ec.view == .schematic {
                    Text("\(counted(ecRows(ec.schematic(model)["components"]).count, "symbol")) · \(counted((ec.schematic(model)["nets"] as? [String: [String]])?.count ?? 0, "net"))")
                } else {
                    Text("\(counted(ecRows(ec.board(model)["footprints"]).count, "component")) · \(counted(ecRows(ec.board(model)["traces"]).count, "trace"))")
                }
            }
            .font(.caption).foregroundStyle(.secondary)
        }.controlSize(.small)
    }
    private var inspector: some View {
        @Bindable var ec = ec
        return ScrollView {
            VStack(alignment: .leading, spacing: 14) {
                PanelHeader(title: ec.tool == .route ? "Route" : ec.tool == .place ? "Place component"
                            : ec.tool == .wire ? "Connect pins"
                            : ec.selectedRef ?? (ec.view == .schematic ? "Schematic" : "Circuit board"))
                if ec.tool == .select, let ref = ec.selectedRef,
                   let component = ecRows((ec.view == .schematic ? ec.schematic(model)["components"] : ec.board(model)["footprints"])).first(where: { $0["ref"] as? String == ref }) {
                    HStack {
                        Text("Value")
                        TextField("Component value", text: Binding(get: { component["value"] as? String ?? "" }, set: { ec.editComponent(model, key: "value", value: $0) })).textFieldStyle(.roundedBorder)
                    }
                    HStack {
                        Text("Rotation")
                        TextField("Rotation", value: Binding(get: { ecNumber(component["rotation"]) }, set: { v in if v.isFinite { ec.editComponent(model, key: "rotation", value: v.truncatingRemainder(dividingBy: 360)) } }), format: .number).textFieldStyle(.roundedBorder)
                        Text("°")
                    }
                    Text(component["footprintName"] as? String ?? component["footprintId"] as? String ?? "Component").font(.caption).foregroundStyle(.secondary)
                    ForEach(["x", "y"], id: \.self) { axis in
                        HStack {
                            Text(axis.uppercased())
                            TextField(axis.uppercased(), value: Binding(get: { ecNumber((component["position"] as? ECObject)?[axis]) }, set: { v in
                                guard v.isFinite, abs(v) < 100_000 else { return }
                                var pos = component["position"] as? ECObject ?? [:]; pos[axis] = v; ec.editComponent(model, key: "position", value: pos)
                            }), format: .number).textFieldStyle(.roundedBorder)
                            Text("mm").foregroundStyle(.secondary)
                        }
                    }
                    Text("Move coordinates independently in Schematic and Board views.").font(.caption).foregroundStyle(.secondary)
                    Divider()
                }
                if ec.tool == .place {
                Picker("Component", selection: $ec.componentKind) { ForEach(ec.kinds, id: \.self) { Text($0) } }
                Text("Native starter parts use explicit two-pad SMD geometry; verify the footprint against your part before fabrication.").font(.caption).foregroundStyle(.secondary)
                }
                if ec.view == .board && (ec.tool == .route || ec.tool == .via) {
                Picker("Net", selection: $ec.activeNet) {
                    Text("Choose net").tag("")
                    ForEach(ecRows(ec.board(model)["nets"]), id: \.ecID) { row in Text(row["name"] as? String ?? row.ecID).tag(row.ecID) }
                }
                Picker("Copper layer", selection: $ec.activeLayer) { Text("Front copper").tag("FCu"); Text("Back copper").tag("BCu") }
                HStack { Text("Trace width"); TextField("Trace width", value: $ec.traceWidth, format: .number).textFieldStyle(.roundedBorder); Text("mm") }
                Text(ec.routeStart == nil ? "Choose a pad to begin routing." : "Preview · click to commit").font(.caption).foregroundStyle(.secondary)
                Divider()
                Label("Linked schematic", systemImage: "point.3.connected.trianglepath.dotted").font(.headline)
                let pins = (ec.schematic(model)["nets"] as? [String: [String]])?[ec.activeNet] ?? []
                ElectronicsNetPreview(pins: pins, net: ec.activeNet).frame(height: 120)
                Button("Show in Schematic") { ec.view = .schematic }
                }
                Divider()
                Picker("Grid", selection: $ec.grid) { Text("0.25 mm").tag(0.25); Text("0.5 mm").tag(0.5); Text("1 mm").tag(1.0); Text("2.54 mm").tag(2.54) }
                Toggle("Snap to grid", isOn: $ec.snap)
                HStack { Text("Zoom"); Slider(value: $ec.zoom, in: 0.5...3) }
                Button("Delete selected copper", role: .destructive) { ec.removeSelection(model) }.disabled(ec.selectedTrace == nil && ec.selectedVia == nil)
                DisclosureGroup("Editor capabilities") {
                    Text("Imported zones, arcs and custom graphics are preserved but not displayed. Routing previews do not perform clearance checks.").font(.caption).foregroundStyle(.secondary)
                }
                Text("Escape ends routing or pin connection.").font(.caption).foregroundStyle(.secondary)
            }.controlSize(.small)
        }
    }
    /// The same Open panel the File menu uses.
    private func open() { openPanel(model) }
}

extension Dictionary where Key == String, Value == Any {
    var ecID: String { self["id"] as? String ?? "" }
}

extension ElectronicsWorkspace {
    /// Derive a schematic from the board for documents that only have one
    /// (KiCad .kicad_pcb import carries no schematic): a symbol per footprint
    /// with a pin per pad, nets from the pads' net assignments, laid out on a
    /// grid with ICs and connectors first. One undo step.
    func buildSchematicFromBoard(_ model: EditorModel) {
        let footprints = ecRows(board(model)["footprints"])
        guard !footprints.isEmpty else { return }
        func rank(_ ref: String) -> Int {
            switch ref.prefix(while: \.isLetter) {
            case "U", "IC": 0
            case "J", "P", "CN", "USB": 1
            case "Y", "X", "Q", "D", "LED", "SW": 2
            default: 3
            }
        }
        let ordered = footprints.sorted {
            let a = $0["ref"] as? String ?? "", b = $1["ref"] as? String ?? ""
            return (rank(a), a.count, a) < (rank(b), b.count, b)
        }
        var components: [ECObject] = []
        var nets: [String: [String]] = [:]
        var x = 0.0, y = 0.0, rowHeight = 0.0
        let rowWidth = 220.0
        for fp in ordered {
            let ref = fp["ref"] as? String ?? "?"
            var seen = Set<String>()
            var pins: [ECObject] = []
            for pad in ecRows(fp["pads"]) {
                let number = pad["number"] as? String ?? ""
                guard !number.isEmpty, seen.insert(number).inserted else { continue }
                pins.append(["number": number, "name": number, "pin_type": "Passive", "position": ["x": 0.0, "y": 0.0]])
                if let net = pad["net"] as? String, !net.isEmpty { nets[net, default: []].append("\(ref).\(number)") }
            }
            let probe: ECObject = ["pins": pins, "position": ["x": 0.0, "y": 0.0], "rotation": 0.0]
            let body = ElectronicsCanvas.symbolBody(probe)
            let w = body.width + 24, h = body.height + 14
            if x + w > rowWidth { x = 0; y += rowHeight; rowHeight = 0 }
            components.append(["ref": ref, "value": fp["value"] as? String ?? "",
                               "footprintId": fp["footprintName"] as? String ?? "",
                               "position": ["x": x + w / 2, "y": y + h / 2], "rotation": 0.0, "pins": pins])
            x += w; rowHeight = max(rowHeight, h)
        }
        model.editElectronics(name: "Build Schematic from Board") { doc in
            doc["schematic"] = ["title": "Schematic (from board)", "components": components, "wires": [ECObject](),
                                "junctions": [ECObject](), "labels": [ECObject](), "nets": nets]
        }
        view = .schematic
    }
}

struct ElectronicsCanvas: View {
    @Bindable var model: EditorModel
    private var ec: ElectronicsWorkspace { model.electronics }
    @State private var pointer: CGPoint?
    private func transform(_ size: CGSize) -> (CGFloat, CGPoint) {
        // Fit what this view draws: the schematic's symbols, or the board outline.
        // Fitting the schematic to the board outline left real sheets (which run
        // to x ≈ 190 mm) half off-screen.
        let vertices: [CGPoint]
        if ec.view == .schematic, !ecRows(ec.schematic(model)["components"]).isEmpty {
            vertices = ecRows(ec.schematic(model)["components"]).flatMap { c -> [CGPoint] in
                let b = Self.symbolBody(c).insetBy(dx: -6, dy: -6)
                return [CGPoint(x: b.minX, y: b.minY), CGPoint(x: b.maxX, y: b.maxY)].map { Self.world(local: $0, in: c) }
            }
            let minX = vertices.map(\.x).min() ?? 0, maxX = vertices.map(\.x).max() ?? 80
            let minY = vertices.map(\.y).min() ?? 0, maxY = vertices.map(\.y).max() ?? 50
            let scale = max(0.1, min((size.width - 60) / max(1, maxX - minX), (size.height - 160) / max(1, maxY - minY))) * ec.zoom
            return (scale, CGPoint(x: size.width / 2 - (minX + maxX) / 2 * scale, y: size.height / 2 - (minY + maxY) / 2 * scale))
        }
        vertices = ecRows((ec.board(model)["outline"] as? ECObject)?["vertices"]).map { ecPoint($0) }
        let minX = min(0, vertices.map(\.x).min() ?? 0), maxX = max(80, vertices.map(\.x).max() ?? 80)
        let minY = min(0, vertices.map(\.y).min() ?? 0), maxY = max(50, vertices.map(\.y).max() ?? 50)
        let scale = max(0.1, min((size.width - 60) / (maxX - minX), (size.height - 60) / (maxY - minY))) * ec.zoom
        return (scale, CGPoint(x: size.width / 2 - (minX + maxX) / 2 * scale, y: size.height / 2 - (minY + maxY) / 2 * scale))
    }
    var body: some View {
        GeometryReader { geo in
            let (scale, offset) = transform(geo.size)
            Canvas { context, size in
                let spatial = ec.view == .spatial
                func screen(_ p: CGPoint) -> CGPoint {
                    if spatial { return CGPoint(x: size.width / 2 + (p.x - 40 - (p.y - 25) * 0.45) * scale * 0.8, y: size.height / 2 + (p.y - 25) * scale * 0.65) }
                    return CGPoint(x: offset.x + p.x * scale, y: offset.y + p.y * scale)
                }
                func line(_ a: CGPoint, _ b: CGPoint, _ color: Color, _ width: CGFloat = 1) {
                    var path = Path(); path.move(to: screen(a)); path.addLine(to: screen(b)); context.stroke(path, with: .color(color), lineWidth: width)
                }
                let board = ec.board(model)
                let spacing = max(12, ec.grid * scale)
                for x in stride(from: offset.x.truncatingRemainder(dividingBy: spacing), to: size.width, by: spacing) {
                    for y in stride(from: offset.y.truncatingRemainder(dividingBy: spacing), to: size.height, by: spacing) {
                        context.fill(Path(ellipseIn: CGRect(x: x, y: y, width: 1, height: 1)), with: .color(.secondary.opacity(0.22)))
                    }
                }
                if ec.tool == .route, let start = ec.routeStart, let pointer {
                    line(start, ec.snapped(pointer), .yellow.opacity(0.16), max(5, (ec.traceWidth + 0.4) * scale))
                    line(start, ec.snapped(pointer), .yellow, max(1.5, ec.traceWidth * scale))
                }
                if ec.view != .schematic {
                    let vertices = ecRows((board["outline"] as? ECObject)?["vertices"]).map { screen(ecPoint($0)) }
                    var path = Path(); path.addLines(vertices); path.closeSubpath()
                    context.fill(path, with: .color(.green.opacity(0.15)))
                    context.stroke(path, with: .color(.green), lineWidth: 2)
                    for (i, trace) in ecRows(board["traces"]).enumerated() {
                        let layer = trace["layer"] as? String ?? "FCu"
                        line(ecPoint(trace["start"]), ecPoint(trace["end"]), i == ec.selectedTrace ? .yellow : layer == "FCu" ? .orange : .cyan, max(1.5, ecNumber(trace["width"], 0.25) * scale))
                    }
                    for (i, via) in ecRows(board["vias"]).enumerated() {
                        let p = screen(ecPoint(via["position"])), r = max(3, ecNumber(via["diameter"], 0.6) * scale / 2)
                        context.stroke(Path(ellipseIn: CGRect(x: p.x - r, y: p.y - r, width: r * 2, height: r * 2)), with: .color(i == ec.selectedVia ? .yellow : .cyan), lineWidth: 2)
                    }
                } else {
                    for wire in ecRows(ec.schematic(model)["wires"]) { line(ecPoint(wire["start"]), ecPoint(wire["end"]), .green) }
                    // Small nets: wires between nearest pins (a chain, not a star from
                    // the first pin). Power and large nets: a net label at each pin,
                    // the way a schematic draws GND/VBUS, instead of lines across the sheet.
                    let nets = ec.schematic(model)["nets"] as? [String: [String]] ?? [:]
                    let pins = Self.pinTable(ec.schematic(model))
                    for (name, refs) in nets {
                        let points = refs.compactMap { pins[$0] }
                        let color: Color = name == ec.activeNet ? .orange : .green
                        if points.count > 4 || Self.isPowerNet(name) {
                            for (p, side) in points {
                                let tip = CGPoint(x: p.x + side * 3, y: p.y)
                                line(p, tip, color.opacity(0.8), 1)
                                context.draw(Text(name).font(.system(size: 9, weight: .medium)).foregroundStyle(color),
                                             at: screen(tip), anchor: side < 0 ? .trailing : .leading)
                            }
                        } else {
                            var remaining = points.map(\.0)
                            guard var cur = remaining.first else { continue }
                            remaining.removeFirst()
                            while !remaining.isEmpty {
                                let i = remaining.indices.min { hypot(remaining[$0].x - cur.x, remaining[$0].y - cur.y) < hypot(remaining[$1].x - cur.x, remaining[$1].y - cur.y) }!
                                line(cur, remaining[i], color, 1.2)
                                cur = remaining.remove(at: i)
                            }
                        }
                    }
                }
                if ec.view == .schematic {
                    // Symbols: a body sized to the pins, numbered pin stubs, labels above.
                    for f in ecRows(ec.schematic(model)["components"]) {
                        let ref = f["ref"] as? String ?? "?", body = Self.symbolBody(f)
                        let corners = [CGPoint(x: body.minX, y: body.minY), CGPoint(x: body.maxX, y: body.minY),
                                       CGPoint(x: body.maxX, y: body.maxY), CGPoint(x: body.minX, y: body.maxY)]
                            .map { screen(Self.world(local: $0, in: f)) }
                        var outline = Path(); outline.addLines(corners); outline.closeSubpath()
                        let selected = ref == ec.selectedRef
                        context.fill(outline, with: .color(selected ? .accentColor.opacity(0.25) : .secondary.opacity(0.12)))
                        context.stroke(outline, with: .color(selected ? .accentColor : .primary.opacity(0.8)), lineWidth: 1)
                        let top = screen(Self.world(local: CGPoint(x: 0, y: body.minY), in: f))
                        context.draw(Text(ref).font(.caption.weight(.semibold)), at: CGPoint(x: top.x, y: top.y - 14))
                        context.draw(Text(f["value"] as? String ?? "").font(.caption2).foregroundStyle(.secondary),
                                     at: CGPoint(x: top.x, y: top.y - 3), anchor: .bottom)
                        for (pin, local) in Self.symbolPins(f) {
                            let edge = CGPoint(x: local.x < 0 ? max(local.x, body.minX) : min(local.x, body.maxX), y: local.y)
                            line(Self.world(local: edge, in: f), Self.world(local: local, in: f), .green, 1)
                            let pp = screen(Self.world(local: local, in: f)), r = max(2, scale * 0.35)
                            context.fill(Path(ellipseIn: CGRect(x: pp.x - r, y: pp.y - r, width: 2 * r, height: 2 * r)), with: .color(.green))
                            if scale > 6 {
                                context.draw(Text(pin["number"] as? String ?? "").font(.system(size: 9)).foregroundStyle(.secondary),
                                             at: CGPoint(x: pp.x + (local.x < 0 ? 4 : -4), y: pp.y - 6),
                                             anchor: local.x < 0 ? .leading : .trailing)
                            }
                        }
                    }
                }
                let components = ec.view == .schematic ? [] : ecRows(board["footprints"])
                for f in components {
                    let ref = f["ref"] as? String ?? "?", p = ecPoint(f["position"]), center = screen(p)
                    let rect = CGRect(x: center.x - 3 * scale, y: center.y - 1.5 * scale, width: 6 * scale, height: 3 * scale)
                    context.fill(Path(roundedRect: rect, cornerRadius: 2), with: .color(ref == ec.selectedRef ? .accentColor.opacity(0.4) : .secondary.opacity(0.25)))
                    context.stroke(Path(roundedRect: rect, cornerRadius: 2), with: .color(ref == ec.selectedRef ? .accentColor : .primary), lineWidth: 1)
                    context.draw(Text(ref + " · " + (f["value"] as? String ?? "")).font(.subheadline), at: CGPoint(x: center.x, y: center.y - 2.5 * scale))
                    let pads = ecRows(f[ec.view == .schematic ? "pins" : "pads"])
                    for pad in pads {
                        let pp = screen(Self.world(pad, in: f)), r = max(3, scale * 0.6)
                        context.fill(Path(ellipseIn: CGRect(x: pp.x - r, y: pp.y - r, width: 2 * r, height: 2 * r)), with: .color(ec.view == .schematic ? .green : .orange))
                    }
                }
                if let start = ec.routeStart { let p = screen(start); context.stroke(Path(ellipseIn: CGRect(x: p.x - 5, y: p.y - 5, width: 10, height: 10)), with: .color(.accentColor), lineWidth: 2) }
            }
            .onContinuousHover { phase in
                switch phase {
                case .active(let location): pointer = CGPoint(x: (location.x - offset.x) / scale, y: (location.y - offset.y) / scale)
                case .ended: pointer = nil
                }
            }
            .contentShape(Rectangle())
            .gesture(SpatialTapGesture().onEnded { event in
                guard ec.view != .spatial else { return }
                let p = CGPoint(x: (event.location.x - offset.x) / scale, y: (event.location.y - offset.y) / scale)
                switch ec.tool {
                case .place: ec.place(model, at: p)
                case .route:
                    var target = p
                    var onPad = false
                    for component in ecRows(ec.board(model)["footprints"]) {
                        for pad in ecRows(component["pads"]) {
                            let q = Self.world(pad, in: component)
                            if hypot(p.x - q.x, p.y - q.y) < 10 / scale {
                                if ec.routeStart == nil { ec.activeNet = pad["net"] as? String ?? "" }
                                else if pad["net"] as? String != ec.activeNet { ec.message = "This pad belongs to a different net."; return }
                                target = q; onPad = true
                            }
                        }
                    }
                    ec.addTrace(model, to: target, snapToGrid: !onPad)
                case .via: ec.addVia(model, at: p)
                case .wire:
                    if let pin = nearestPin(p, distance: 8 / scale) { ec.connect(model, pin: pin) }
                    else { ec.message = "Choose a component pin." }
                case .select: select(p, tolerance: 8 / scale)
                }
            })
            .simultaneousGesture(DragGesture(minimumDistance: 4).onEnded { event in
                guard ec.view != .spatial, ec.tool == .select else { return }
                let start = CGPoint(x: (event.startLocation.x - offset.x) / scale, y: (event.startLocation.y - offset.y) / scale)
                select(start, tolerance: 8 / scale)
                guard ec.selectedRef != nil else { return }
                let end = ec.snapped(CGPoint(x: (event.location.x - offset.x) / scale, y: (event.location.y - offset.y) / scale))
                ec.editComponent(model, key: "position", value: ecJSON(end))
            })
            .accessibilityLabel(ec.view == .schematic ? "Schematic canvas" : "PCB layout canvas")
        }
    }
    static func world(_ item: ECObject, in component: ECObject) -> CGPoint {
        world(local: ecPoint(item["position"]), in: component)
    }
    static func world(local p: CGPoint, in component: ECObject) -> CGPoint {
        let origin = ecPoint(component["position"]), radians = ecNumber(component["rotation"]) * .pi / 180
        let x = component["mirror"] as? Bool == true ? -p.x : p.x
        return CGPoint(x: origin.x + x * cos(radians) - p.y * sin(radians), y: origin.y + x * sin(radians) + p.y * cos(radians))
    }

    /// A schematic symbol's pins in component-local mm. Uses the pins' own
    /// positions when the sheet carries them; imported and generated sheets
    /// often put every pin at (0, 0), which stacked them on one point and
    /// collapsed every net line, so those get a readable layout instead:
    /// two-pin parts horizontal, everything else split left/right on a
    /// 2.54 mm pitch.
    static func symbolPins(_ component: ECObject) -> [(pin: ECObject, local: CGPoint)] {
        let pins = ecRows(component["pins"])
        if pins.contains(where: { let p = ecPoint($0["position"]); return p.x != 0 || p.y != 0 }) {
            return pins.map { ($0, ecPoint($0["position"])) }
        }
        if pins.count <= 2 {
            return pins.enumerated().map { i, pin in (pin, CGPoint(x: i == 0 ? -3.81 : 3.81, y: 0)) }
        }
        let left = (pins.count + 1) / 2, pitch = 2.54
        return pins.enumerated().map { i, pin in
            let onLeft = i < left
            let row = onLeft ? i : i - left
            let rows = onLeft ? left : pins.count - left
            let y = (CGFloat(row) - CGFloat(rows - 1) / 2) * pitch
            return (pin, CGPoint(x: onLeft ? -10.16 : 10.16, y: y))
        }
    }

    /// "REF.PIN" → (world position, outward side: -1 left / +1 right) for every
    /// pin on the sheet, built once per draw.
    static func pinTable(_ sheet: ECObject) -> [String: (CGPoint, CGFloat)] {
        var out: [String: (CGPoint, CGFloat)] = [:]
        for c in ecRows(sheet["components"]) {
            let ref = c["ref"] as? String ?? ""
            for (pin, local) in symbolPins(c) {
                out["\(ref).\(pin["number"] as? String ?? "")"] = (world(local: local, in: c), local.x < 0 ? -1 : 1)
            }
        }
        return out
    }

    static func isPowerNet(_ name: String) -> Bool {
        let n = name.uppercased()
        return n.contains("GND") || n.hasPrefix("VBUS") || n.hasPrefix("VCC") || n.hasPrefix("VDD")
            || n.hasPrefix("+") || n.hasPrefix("3V3") || n.hasPrefix("5V") || n.hasPrefix("1V")
    }

    /// The symbol body in component-local mm, sized to its pins.
    static func symbolBody(_ component: ECObject) -> CGRect {
        let n = ecRows(component["pins"]).count
        if n <= 2 { return CGRect(x: -2.54, y: -1.0, width: 5.08, height: 2.0) }
        let rows = CGFloat((n + 1) / 2)
        let h = rows * 2.54 + 2.54
        return CGRect(x: -7.62, y: -h / 2, width: 15.24, height: h)
    }

    private func pinPosition(_ ref: String) -> CGPoint? {
        for component in ecRows(ec.schematic(model)["components"]) {
            let cref = component["ref"] as? String ?? ""
            for (pin, local) in Self.symbolPins(component) where "\(cref).\(pin["number"] as? String ?? "")" == ref {
                return Self.world(local: local, in: component)
            }
        }
        return nil
    }
    private func nearestPin(_ p: CGPoint, distance: CGFloat) -> String? {
        for component in ecRows(ec.schematic(model)["components"]) {
            for (pin, local) in Self.symbolPins(component) {
                let q = Self.world(local: local, in: component)
                if hypot(p.x - q.x, p.y - q.y) < distance { return "\(component["ref"] as? String ?? "").\(pin["number"] as? String ?? "")" }
            }
        }
        return nil
    }
    private func select(_ p: CGPoint, tolerance: CGFloat) {
        ec.selectedRef = nil; ec.selectedTrace = nil; ec.selectedVia = nil
        for f in ecRows(ec.view == .schematic ? ec.schematic(model)["components"] : ec.board(model)["footprints"]) {
            let q = ecPoint(f["position"])
            if hypot(p.x - q.x, p.y - q.y) < max(4, tolerance) { ec.selectedRef = f["ref"] as? String; return }
        }
        guard ec.view == .board else { return }
        for (i, via) in ecRows(ec.board(model)["vias"]).enumerated() { let q = ecPoint(via["position"]); if hypot(p.x - q.x, p.y - q.y) < tolerance { ec.selectedVia = i; return } }
        for (i, t) in ecRows(ec.board(model)["traces"]).enumerated() {
            let a = ecPoint(t["start"]), b = ecPoint(t["end"]), dx = b.x - a.x, dy = b.y - a.y
            let u = max(0, min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / max(0.0001, dx * dx + dy * dy)))
            if hypot(p.x - a.x - u * dx, p.y - a.y - u * dy) < tolerance { ec.selectedTrace = i; return }
        }
    }
}

/// Connectivity inset uses document net membership; it does not imply physical routing.
private struct ElectronicsNetPreview: View {
    let pins: [String]
    let net: String
    var body: some View {
        Canvas { context, size in
            guard !pins.isEmpty else {
                context.draw(Text("Select a net to inspect its pins").font(.caption).foregroundStyle(.secondary), at: CGPoint(x: size.width / 2, y: size.height / 2))
                return
            }
            let visible = Array(pins.prefix(6))
            let step = size.height / CGFloat(visible.count + 1)
            var bus = Path()
            bus.move(to: CGPoint(x: size.width - 35, y: step))
            bus.addLine(to: CGPoint(x: size.width - 35, y: step * CGFloat(visible.count)))
            context.stroke(bus, with: .color(.orange), lineWidth: 1.5)
            for (index, pin) in visible.enumerated() {
                let y = step * CGFloat(index + 1)
                context.draw(Text(pin).font(.subheadline.monospaced()), at: CGPoint(x: 8, y: y), anchor: .leading)
                var wire = Path(); wire.move(to: CGPoint(x: 80, y: y)); wire.addLine(to: CGPoint(x: size.width - 35, y: y))
                context.stroke(wire, with: .color(.orange), lineWidth: 1.5)
                context.fill(Path(ellipseIn: CGRect(x: size.width - 38, y: y - 3, width: 6, height: 6)), with: .color(.orange))
            }
        }.accessibilityLabel("Net \(net): \(pins.joined(separator: ", "))")
    }
}

import SwiftUI
import AppKit
import UniformTypeIdentifiers

struct NativeElectronicsView: View {
    @Bindable var model: EditorModel
    private var ec: ElectronicsWorkspace { model.electronics }
    @State private var fileError: String?
    @State private var search = ""
    @State private var navigatorTab = "Parts"
    @State private var newSheetName = ""
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
                            statusPill.padding(.bottom, Glass.dockClearance)
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
        case .erc: "Electrical Rule Check"
        case .footprint: "Footprint Editor"
        case .symbol: "Symbol Editor"
        case .length: "Diff Pair / Length Tuning"
        case .stackup: "Layer Stack"
        case .netClasses: "Net Classes"
        case .simulate: "Simulate"
        case .enclosure: "Enclosure Fit"
        case nil: ""
        }
    }

    /// One glass pill: what the tool wants, then the board's counts as numbers.
    private var statusPill: some View {
        HStack(spacing: 8) {
            if let busy = ec.tools.busy { ProgressView().controlSize(.mini); Text("\(busy)…") }
            Text(ec.tools.error ?? ec.tools.status ?? ec.message ?? (ec.view == .spatial ? "3D layout preview" : ec.tool == .wire ? (ec.view == .schematic ? "Click to draw a wire" : "Select two pins to connect their net") : ec.tool == .label ? "Click a pin or wire to name its net" : ec.tool == .zone ? "Click corners · Return closes the zone" : ec.tool == .route ? "Click to route · Esc ends the route" : ec.tool == .place ? "Click to place \((ec.view == .schematic ? ec.symbolChoice : ec.libraryChoice) ?? ec.componentKind.lowercased())" : ec.selectedRef ?? "Select a component"))
                .lineLimit(1)
            Text("·").foregroundStyle(.secondary)
            if ec.view == .board {
                if let c = ec.cursor {
                    Text(String(format: "%.2f, %.2f mm", c.x, c.y)).monospacedDigit().frame(minWidth: 96)
                    Text("·").foregroundStyle(.secondary)
                }
                Circle().fill(BoardLayers.color(ec.activeLayer)).frame(width: 8, height: 8)
                Text(BoardLayers.name(ec.activeLayer))
                Text("·").foregroundStyle(.secondary)
                let open = ec.openNets(ec.board(model)).count
                Text("\(open) open").monospacedDigit().foregroundStyle(open == 0 ? Color.green : Color.orange)
                Text("·").foregroundStyle(.secondary)
            }
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
                                Button {
                                    ec.selectedRef = ref; ec.selectedTrace = nil; ec.selectedVia = nil; ec.tool = .select
                                    if ec.view == .board { ec.focus(on: ecPoint(row["position"]), minZoom: 1) }
                                } label: {
                                    HStack { Image(systemName: "cpu"); Text(ref); Spacer(); Text(value).foregroundStyle(.secondary) }
                                        .padding(Theme.Space.s).selectableRow(selected: ec.selectedRef == ref)
                                }.buttonStyle(.plain).accessibilityAddTraits(ec.selectedRef == ref ? .isSelected : [])
                            }
                        }
                    } else if navigatorTab == "Nets" {
                        let open = ec.openNets(ec.board(model))
                        if !open.isEmpty { Eyebrow("Open") }
                        ForEach(open.filter { search.isEmpty || $0.name.localizedCaseInsensitiveContains(search) }, id: \.name) { net in
                            Button {
                                ec.activeNet = net.name; ec.highlightNet = net.name; ec.routeStart = nil
                                ec.focus(on: net.at, minZoom: 2)
                            } label: {
                                HStack {
                                    Circle().fill(.orange).frame(width: 7, height: 7)
                                    Text(net.name); Spacer()
                                    Text("\(net.gaps)").monospacedDigit().foregroundStyle(.secondary)
                                }.padding(Theme.Space.s).selectableRow(selected: ec.highlightNet == net.name)
                            }.buttonStyle(.plain)
                        }
                        if !open.isEmpty { Eyebrow("Connected") }
                        ForEach(ecRows(ec.board(model)["nets"]).filter { row in !open.contains { $0.name == row.ecID } }, id: \.ecID) { row in
                            let name = row["name"] as? String ?? row.ecID
                            if search.isEmpty || name.localizedCaseInsensitiveContains(search) {
                                Button { ec.activeNet = row.ecID; ec.highlightNet = row.ecID; ec.routeStart = nil } label: {
                                    HStack { Image(systemName: "point.3.connected.trianglepath.dotted"); Text(name); Spacer() }
                                        .padding(Theme.Space.s).selectableRow(selected: ec.highlightNet == row.ecID)
                                }.buttonStyle(.plain).accessibilityAddTraits(ec.highlightNet == row.ecID ? .isSelected : [])
                            }
                        }
                        if ecRows(ec.board(model)["nets"]).isEmpty {
                            EmptyPanelState(title: "No nets", systemImage: "point.3.connected.trianglepath.dotted",
                                            detail: "Connect schematic pins to create a net.")
                        }
                    } else {
                        ForEach(BoardLayers.copper, id: \.self) { layer in
                            HStack(spacing: 6) {
                                Button { if ec.hiddenLayers.contains(layer) { ec.hiddenLayers.remove(layer) } else { ec.hiddenLayers.insert(layer) } } label: {
                                    Image(systemName: ec.hiddenLayers.contains(layer) ? "eye.slash" : "eye")
                                }.buttonStyle(.plain).accessibilityLabel("Show \(BoardLayers.name(layer))")
                                RoundedRectangle(cornerRadius: 2).fill(BoardLayers.color(layer)).frame(width: 12, height: 12)
                                Button { ec.activeLayer = layer } label: { Text(BoardLayers.name(layer)); Spacer() }.buttonStyle(.plain)
                            }.padding(Theme.Space.xs).selectableRow(selected: ec.activeLayer == layer)
                        }
                        Divider()
                        Toggle("Ratsnest", isOn: $ec.showRatsnest)
                        Toggle("Silkscreen", isOn: $ec.showSilk)
                        Toggle("Courtyards", isOn: $ec.showCourtyard)
                        Toggle("DRC markers", isOn: $ec.showDrcMarkers)
                    }
                }.font(.callout)
            }
            Divider()
            Button(ec.view == .schematic ? "Show in Board" : "Show in Schematic", systemImage: "arrow.left.arrow.right") {
                ec.view = ec.view == .schematic ? .board : .schematic
            }
            if ec.view == .schematic, !ec.sheetNames(model).isEmpty {
                Picker("Sheet", selection: Binding(get: { ec.activeSheet ?? "" }, set: { ec.activeSheet = $0.isEmpty ? nil : $0; ec.selectedRef = nil })) {
                    Text("Root").tag("")
                    ForEach(ec.sheetNames(model), id: \.self) { Text($0).tag($0) }
                }
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
                            : ec.tool == .wire ? (ec.view == .schematic ? "Wire" : "Connect pins")
                            : ec.tool == .label ? "Net label"
                            : ec.tool == .zone ? "Copper zone"
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
                if ec.tool == .select, ec.selectedRef != nil, ec.view != .spatial {
                    Button(ec.view == .schematic ? "Edit Symbol…" : "Edit Footprint…", systemImage: "pencil.and.ruler") {
                        ec.tools.sheet = ec.view == .schematic ? .symbol : .footprint
                    }.controlSize(.small)
                }
                if ec.view == .schematic, ec.tool == .select, ec.activeSheet == nil, ec.selectedRef != nil || !ec.multiRefs.isEmpty {
                    HStack {
                        TextField("New sheet name", text: $newSheetName).textFieldStyle(.roundedBorder)
                        Button("Move") { ec.moveSelectionToNewSheet(model, named: newSheetName); newSheetName = "" }
                    }
                }
                if ec.view == .board, let net = ec.highlightNet {
                    let board = ec.board(model)
                    KeyValueRow(net, String(format: "%.2f mm routed", ec.netLength(net, in: board)))
                    Picker("Class", selection: Binding(get: { ec.netClass(net, in: board) }, set: { ec.setNetClass(model, net, $0) })) {
                        Text("Default").tag("")
                        ForEach(ecRows((board["rules"] as? ECObject)?["classRules"]).compactMap { $0["name"] as? String }, id: \.self) { Text($0).tag($0) }
                    }
                    if Set(ecRows(board["zones"]).filter { $0["net"] as? String == net }.compactMap { $0["layer"] as? String }).count >= 2 {
                        Button("Stitch Zones") { ec.tools.stitch(ec, model, net) }.controlSize(.small)
                    }
                    if let other = ec.pairPartner(of: net, in: board) {
                        KeyValueRow("Pair skew", String(format: "%.3f mm", abs(ec.netLength(net, in: board) - ec.netLength(other, in: board))))
                        Button("Route Pair with \(other)") { ec.tools.routePair(ec, model, net, other) }.controlSize(.small)
                        Button("Match Lengths…") { ec.tools.tuneNets = [net, other]; ec.tools.openLengths(ec, model) }.controlSize(.small)
                    }
                }
                if ec.multiRefs.count >= 2, ec.tool == .select {
                    Eyebrow("\(ec.multiRefs.count) selected")
                    HStack(spacing: 6) {
                        Button { ec.align(model, .left) } label: { Image(systemName: "align.horizontal.left") }.help("Align left")
                        Button { ec.align(model, .right) } label: { Image(systemName: "align.horizontal.right") }.help("Align right")
                        Button { ec.align(model, .top) } label: { Image(systemName: "align.vertical.top") }.help("Align top")
                        Button { ec.align(model, .bottom) } label: { Image(systemName: "align.vertical.bottom") }.help("Align bottom")
                        Button { ec.align(model, .spreadX) } label: { Image(systemName: "distribute.horizontal.center") }.help("Distribute horizontally")
                        Button { ec.align(model, .spreadY) } label: { Image(systemName: "distribute.vertical.center") }.help("Distribute vertically")
                    }.buttonStyle(.borderless).frame(maxWidth: .infinity, alignment: .leading)
                    Divider()
                }
                if ec.tool == .label {
                    TextField("Net name", text: $ec.labelText).textFieldStyle(.roundedBorder)
                    Text("Click a pin or wire. Names like GND and +3V3 draw as power symbols.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                if ec.tool == .wire, ec.view == .schematic {
                    Text("Click to start and add corners; click a pin or wire to finish. Return ends in open space, Esc cancels.")
                        .font(.caption).foregroundStyle(.secondary)
                }
                if ec.tool == .place {
                if ec.view == .board {
                    TextField("Search KiCad footprints", text: $ec.librarySearch).textFieldStyle(.roundedBorder)
                        .onAppear { ec.loadLibrary() }
                    ForEach(ec.libraryMatches(), id: \.self) { id in
                        Button { ec.libraryChoice = id } label: {
                            Text(id).font(.caption).lineLimit(1).truncationMode(.middle).frame(maxWidth: .infinity, alignment: .leading)
                                .padding(Theme.Space.xs).selectableRow(selected: ec.libraryChoice == id)
                        }.buttonStyle(.plain)
                    }
                    if let id = ec.libraryChoice {
                        Text("Click the board to place \(id).").font(.caption).foregroundStyle(.secondary)
                        Button("Use a starter part instead") { ec.libraryChoice = nil }.controlSize(.small)
                    }
                    Divider()
                }
                if ec.view == .schematic {
                    TextField("Search KiCad symbols", text: $ec.symbolSearch).textFieldStyle(.roundedBorder)
                        .onAppear { ec.loadSymbols() }
                    ForEach(ec.symbolMatches(), id: \.self) { id in
                        Button { ec.symbolChoice = id } label: {
                            Text(id).font(.caption).lineLimit(1).truncationMode(.middle).frame(maxWidth: .infinity, alignment: .leading)
                                .padding(Theme.Space.xs).selectableRow(selected: ec.symbolChoice == id)
                        }.buttonStyle(.plain)
                    }
                    if let id = ec.symbolChoice {
                        Text("Click the sheet to place \(id).").font(.caption).foregroundStyle(.secondary)
                        Button("Use a starter part instead") { ec.symbolChoice = nil }.controlSize(.small)
                    }
                    Divider()
                }
                Picker("Component", selection: $ec.componentKind) { ForEach(ec.kinds, id: \.self) { Text($0) } }
                Text("Native starter parts use explicit two-pad SMD geometry; verify the footprint against your part before fabrication.").font(.caption).foregroundStyle(.secondary)
                }
                if ec.view == .board && (ec.tool == .route || ec.tool == .via || ec.tool == .zone) {
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
    @State private var pointerScreen: CGPoint?
    @State private var canvasSize = CGSize.zero
    @State private var panStart: CGSize?
    @State private var monitor: Any?
    private func transform(_ size: CGSize) -> (CGFloat, CGPoint) {
        // Fit what this view draws: the schematic's symbols, or the board outline.
        // Fitting the schematic to the board outline left real sheets (which run
        // to x ≈ 190 mm) half off-screen.
        let vertices: [CGPoint]
        if ec.view == .schematic, !ecRows(ec.schematic(model)["components"]).isEmpty {
            vertices = ecRows(ec.schematic(model)["components"]).flatMap { c -> [CGPoint] in
                let b = Self.symbolBody(c).insetBy(dx: -6, dy: -6)
                return [CGPoint(x: b.minX, y: b.minY), CGPoint(x: b.maxX, y: b.maxY)].map { Self.world(local: $0, in: c) }
            } + (ec.activeSheet == nil ? ec.sheetSymbols(ec.schematic(model)).flatMap { [$0.rect.origin, CGPoint(x: $0.rect.maxX + 6, y: $0.rect.maxY + 6)] } : [])
            let minX = vertices.map(\.x).min() ?? 0, maxX = vertices.map(\.x).max() ?? 80
            let minY = vertices.map(\.y).min() ?? 0, maxY = vertices.map(\.y).max() ?? 50
            let usable = CGSize(width: max(120, size.width - 2 * (Theme.Width.inspector + 2 * Glass.inset) - 40), height: max(120, size.height - Glass.toolbarHeight - 90))
            let scale = max(0.1, min(usable.width / max(1, maxX - minX), usable.height / max(1, maxY - minY))) * ec.zoom
            return (scale, CGPoint(x: size.width / 2 - (minX + maxX) / 2 * scale + ec.pan.width, y: size.height / 2 - (minY + maxY) / 2 * scale + ec.pan.height))
        }
        vertices = ecRows((ec.board(model)["outline"] as? ECObject)?["vertices"]).map { ecPoint($0) }
        let minX = min(0, vertices.map(\.x).min() ?? 0), maxX = max(80, vertices.map(\.x).max() ?? 80)
        let minY = min(0, vertices.map(\.y).min() ?? 0), maxY = max(50, vertices.map(\.y).max() ?? 50)
        // Leave room for the floating navigator/inspector and the toolbar +
        // status pill: the board used to fill the window and run under them.
        let usableW = max(120, size.width - 2 * (Theme.Width.inspector + 2 * Glass.inset) - 40)
        let usableH = max(120, size.height - Glass.toolbarHeight - 90)
        let scale = max(0.1, min(usableW / (maxX - minX), usableH / (maxY - minY))) * ec.zoom
        return (scale, CGPoint(x: size.width / 2 - (minX + maxX) / 2 * scale + ec.pan.width, y: size.height / 2 - (minY + maxY) / 2 * scale + ec.pan.height))
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
                let board = ec.view == .board ? ec.drawBoard(model) : ec.board(model)
                let spacing = max(12, ec.grid * scale)
                for x in stride(from: offset.x.truncatingRemainder(dividingBy: spacing), to: size.width, by: spacing) {
                    for y in stride(from: offset.y.truncatingRemainder(dividingBy: spacing), to: size.height, by: spacing) {
                        context.fill(Path(ellipseIn: CGRect(x: x, y: y, width: 1, height: 1)), with: .color(.secondary.opacity(0.22)))
                    }
                }
                if ec.view != .schematic {
                    let vertices = ecRows((board["outline"] as? ECObject)?["vertices"]).map { screen(ecPoint($0)) }
                    var path = Path(); path.addLines(vertices); path.closeSubpath()
                    context.fill(path, with: .color(Color(red: 0.05, green: 0.14, blue: 0.10)))
                    context.stroke(path, with: .color(.yellow.opacity(0.8)), lineWidth: 1.5)
                    if !spatial { BoardPainter(context: context, ec: ec, board: board, scale: scale, screen: screen).draw() }
                } else {
                    SchematicPainter(context: context, ec: ec, sheet: ec.drawSchematic(model), scale: scale, screen: screen).draw()
                }
                let components = ec.view == .spatial ? ecRows(board["footprints"]) : []
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
                case .active(let location):
                    pointerScreen = location; pointer = CGPoint(x: (location.x - offset.x) / scale, y: (location.y - offset.y) / scale)
                    ec.cursor = pointer
                    if ec.tool == .route, ec.routeStart != nil, let p = pointer {
                        let hit = copperAt(p, tolerance: 10 / scale, net: ec.activeNet)
                        ec.updateRoute(model, to: hit?.at ?? ec.snapped(p), endsOnCopper: hit != nil)
                    }
                case .ended: pointer = nil; pointerScreen = nil; ec.cursor = nil
                }
            }
            .contentShape(Rectangle())
            .gesture(SpatialTapGesture().onEnded { event in
                guard ec.view != .spatial else { return }
                let p = CGPoint(x: (event.location.x - offset.x) / scale, y: (event.location.y - offset.y) / scale)
                switch ec.tool {
                case .place:
                    if let id = ec.libraryChoice, ec.view == .board { ec.placeLibrary(model, id: id, at: p) }
                    else if let id = ec.symbolChoice, ec.view == .schematic { ec.placeSymbol(model, id: id, at: p) }
                    else { ec.place(model, at: p) }
                case .route:
                    if ec.routeStart != nil {
                        let hit = copperAt(p, tolerance: 10 / scale, net: ec.activeNet)
                        ec.commitRoute(model, at: hit?.at ?? ec.snapped(p), endsOnCopper: hit != nil)
                        return
                    }
                    guard let hit = copperAt(p, tolerance: 10 / scale, net: nil), !hit.net.isEmpty else {
                        ec.message = "Start on a pad, via or trace."; return
                    }
                    ec.activeNet = hit.net; ec.highlightNet = hit.net; ec.routeStart = hit.at
                    if let l = hit.layer { ec.activeLayer = l }
                case .via: ec.addVia(model, at: p)
                case .label: ec.placeLabel(model, at: p, tolerance: 10 / scale)
                case .zone: ec.zoneClick(model, at: p, tolerance: 10 / scale)
                case .wire where ec.view == .schematic:
                    ec.wireClick(model, at: p, tolerance: 10 / scale)
                case .wire:
                    if let pin = nearestPin(p, distance: 8 / scale) { ec.connect(model, pin: pin) }
                    else { ec.message = "Choose a component pin." }
                case .select where ec.view == .schematic && ec.activeSheet == nil && ec.sheetSymbols(ec.schematic(model)).contains { $0.rect.contains(p) }:
                    ec.activeSheet = ec.sheetSymbols(ec.schematic(model)).first { $0.rect.contains(p) }?.name
                    ec.zoom = 1; ec.pan = .zero
                case .select:
                    if NSApp.currentEvent?.modifierFlags.contains(.shift) == true, let ref = partAt(p, tolerance: 8 / scale) {
                        if ec.multiRefs.isEmpty, let first = ec.selectedRef { ec.multiRefs.insert(first) }
                        if ec.multiRefs.contains(ref) { ec.multiRefs.remove(ref) } else { ec.multiRefs.insert(ref) }
                        ec.selectedRef = ref
                    } else {
                        ec.multiRefs = []; ec.selectedZone = nil
                        select(p, tolerance: 8 / scale)
                        if ec.view == .board, ec.selectedRef == nil, ec.selectedTrace == nil, ec.selectedVia == nil { ec.selectZone(model, at: p) }
                    }
                }
            })
            .simultaneousGesture(DragGesture(minimumDistance: 4).onChanged { event in
                guard ec.view != .spatial else { return }
                let start = CGPoint(x: (event.startLocation.x - offset.x) / scale, y: (event.startLocation.y - offset.y) / scale)
                // Empty board pans; a part follows the cursor, ratsnest and all.
                if panStart == nil, ec.dragRef == nil {
                    if ec.tool == .select, let ref = partAt(start, tolerance: 8 / scale) {
                        ec.dragRef = ref; ec.selectedRef = ref; ec.selectedTrace = nil; ec.selectedVia = nil
                    } else { panStart = ec.pan }
                }
                if let base = panStart {
                    ec.pan = CGSize(width: base.width + event.translation.width, height: base.height + event.translation.height)
                } else if let ref = ec.dragRef {
                    let rows = ecRows(ec.view == .schematic ? ec.schematic(model)["components"] : ec.board(model)["footprints"])
                    let origin = ecPoint(rows.first { $0["ref"] as? String == ref }?["position"])
                    ec.dragPosition = ec.snapped(CGPoint(x: origin.x + event.translation.width / scale, y: origin.y + event.translation.height / scale))
                }
            }.onEnded { _ in
                defer { panStart = nil; ec.dragRef = nil; ec.dragPosition = nil }
                if let ref = ec.dragRef, let p = ec.dragPosition {
                    if ec.view == .schematic { ec.moveSymbol(model, ref: ref, to: p) }
                    else { ec.editComponent(model, key: "position", value: ecJSON(p)) }
                }
            })
            .onAppear { canvasSize = geo.size; installMonitor() }
            .onDisappear { if let monitor { NSEvent.removeMonitor(monitor) }; monitor = nil }
            .onChange(of: geo.size) { _, new in canvasSize = new }
            .onChange(of: ec.view) { _, view in
                // Cross-probe: whatever is selected stays in view on the other side.
                ec.zoom = 1; ec.pan = .zero
                guard let ref = ec.selectedRef, view != .spatial else { return }
                let rows = ecRows(view == .schematic ? ec.schematic(model)["components"] : ec.board(model)["footprints"])
                if let row = rows.first(where: { $0["ref"] as? String == ref }) { ec.focus(on: ecPoint(row["position"]), minZoom: 1) }
            }
            .onChange(of: ec.focusPoint) { _, p in
                guard let p else { return }
                ec.zoom = max(ec.zoom, ec.focusMinZoom); ec.pan = .zero
                let (s, o) = transform(geo.size)
                ec.pan = CGSize(width: geo.size.width / 2 - (o.x + p.x * s), height: geo.size.height / 2 - (o.y + p.y * s))
            }
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
        let pins = symbolPins(component).map(\.local)
        if pins.count <= 2 { return CGRect(x: -2.54, y: -1.0, width: 5.08, height: 2.0) }
        // Imported symbols carry their own pin ends: the body sits just inside.
        let x0 = pins.map(\.x).min() ?? -7.62, x1 = pins.map(\.x).max() ?? 7.62
        let y0 = pins.map(\.y).min() ?? -7.62, y1 = pins.map(\.y).max() ?? 7.62
        let w = max(x1 - x0, 5.08), h = max(y1 - y0, 5.08)
        let insetX: CGFloat = x1 > x0 ? 2.54 : 0
        let hasTopBottom = pins.contains { ($0.y == y0 || $0.y == y1) && $0.x != x0 && $0.x != x1 }
        return hasTopBottom
            ? CGRect(x: x0 + insetX, y: y0 + 2.54, width: w - 2 * insetX, height: max(2.54, h - 5.08))
            : CGRect(x: x0 + insetX, y: y0 - 1.27, width: w - 2 * insetX, height: h + 2.54)
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
    /// The pad under `p`, nearest first, with its footprint.
    private func padAt(_ p: CGPoint, tolerance: CGFloat) -> (ref: String, pad: ECObject)? {
        var best: (String, ECObject, CGFloat)?
        for f in ecRows(ec.board(model)["footprints"]) {
            for pad in ecRows(f["pads"]) where (pad["layers"] as? [String] ?? []).contains(where: { BoardLayers.copper.contains($0) }) {
                let q = Self.world(pad, in: f), shape = pad["shape"] as? ECObject ?? [:]
                let d = ecNumber(shape["diameter"], 0.6)
                let reach = CGFloat(max(ecNumber(shape["width"], d), ecNumber(shape["height"], d))) / 2
                let dist = hypot(p.x - q.x, p.y - q.y)
                if dist < max(reach, tolerance), dist < (best?.2 ?? .infinity) { best = (f["ref"] as? String ?? "", pad, dist) }
            }
        }
        return best.map { ($0.0, $0.1) }
    }
    /// Copper to start or end a route on: a pad, via or trace, optionally of one net.
    private func copperAt(_ p: CGPoint, tolerance: CGFloat, net: String?) -> (at: CGPoint, net: String, layer: String?)? {
        let board = ec.board(model)
        if ec.routeStart == nil || net != nil {
            for f in ecRows(board["footprints"]) {
                for pad in ecRows(f["pads"]) {
                    let copper = (pad["layers"] as? [String] ?? []).filter { BoardLayers.copper.contains($0) }
                    guard !copper.isEmpty, let n = pad["net"] as? String, net == nil || n == net else { continue }
                    let q = Self.world(pad, in: f), shape = pad["shape"] as? ECObject ?? [:], d = ecNumber(shape["diameter"], 0.6)
                    let reach = CGFloat(max(ecNumber(shape["width"], d), ecNumber(shape["height"], d))) / 2
                    if hypot(p.x - q.x, p.y - q.y) < max(reach, tolerance), q != ec.routeStart {
                        return (q, n, copper.count == 1 ? copper[0] : (copper.contains(ec.activeLayer) ? ec.activeLayer : copper[0]))
                    }
                }
            }
        }
        for v in ecRows(board["vias"]) {
            let q = ecPoint(v["position"])
            guard let n = v["net"] as? String, net == nil || n == net, q != ec.routeStart else { continue }
            if hypot(p.x - q.x, p.y - q.y) < max(CGFloat(ecNumber(v["diameter"], 0.6)) / 2, tolerance) { return (q, n, nil) }
        }
        for t in ecRows(board["traces"]) {
            guard let n = t["net"] as? String, net == nil || n == net else { continue }
            let a = ecPoint(t["start"]), b = ecPoint(t["end"]), dx = b.x - a.x, dy = b.y - a.y
            let u = max(0, min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / max(1e-9, dx * dx + dy * dy)))
            let q = CGPoint(x: a.x + u * dx, y: a.y + u * dy)
            if hypot(p.x - q.x, p.y - q.y) < tolerance, q != ec.routeStart { return (q, n, t["layer"] as? String) }
        }
        return nil
    }
    /// The footprint whose courtyard (or pad extent) contains `p`; smallest wins.
    private func bodyAt(_ p: CGPoint) -> String? {
        var best: (String, CGFloat)?
        for f in ecRows(ec.board(model)["footprints"]) {
            let o = ecPoint(f["position"]), r = -ecNumber(f["rotation"]) * .pi / 180
            let dx = p.x - o.x, dy = p.y - o.y
            let local = CGPoint(x: dx * cos(r) - dy * sin(r), y: dx * sin(r) + dy * cos(r))
            var pts = ecRows(f["graphics"]).filter { ($0["layer"] as? String)?.hasSuffix("CrtYd") == true }
                .flatMap { [ecPoint($0["start"]), ecPoint($0["end"])] }
            if pts.isEmpty { pts = ecRows(f["pads"]).map { ecPoint($0["position"]) } }
            guard let x0 = pts.map(\.x).min(), let x1 = pts.map(\.x).max(), let y0 = pts.map(\.y).min(), let y1 = pts.map(\.y).max() else { continue }
            if local.x >= x0, local.x <= x1, local.y >= y0, local.y <= y1 {
                let area = (x1 - x0) * (y1 - y0)
                if area < (best?.1 ?? .infinity) { best = (f["ref"] as? String ?? "", area) }
            }
        }
        return best?.0
    }
    /// The footprint under `p`: a pad of it, or its body.
    private func partAt(_ p: CGPoint, tolerance: CGFloat) -> String? {
        if ec.view == .schematic {
            for f in ecRows(ec.schematic(model)["components"]) {
                let o = ecPoint(f["position"]), r = -ecNumber(f["rotation"]) * .pi / 180
                let dx = p.x - o.x, dy = p.y - o.y
                var local = CGPoint(x: dx * cos(r) - dy * sin(r), y: dx * sin(r) + dy * cos(r))
                if f["mirror"] as? Bool == true { local.x = -local.x }
                if Self.symbolBody(f).insetBy(dx: -1.5, dy: -1.5).contains(local) { return f["ref"] as? String }
            }
            return nil
        }
        if let hit = padAt(p, tolerance: tolerance) { return hit.ref }
        return bodyAt(p)
    }
    private func select(_ p: CGPoint, tolerance: CGFloat) {
        ec.selectedRef = nil; ec.selectedTrace = nil; ec.selectedVia = nil; ec.highlightNet = nil
        ec.selectedWire = nil; ec.selectedLabel = nil
        if ec.view == .schematic {
            if let pin = nearestPin(p, distance: max(1.4, tolerance)) {
                let nets = ec.schematic(model)["nets"] as? [String: [String]] ?? [:]
                ec.highlightNet = nets.first { $0.value.contains(pin) }?.key
                ec.selectedRef = String(pin.split(separator: ".")[0])
            } else if !ec.selectSheetItem(model, at: p, tolerance: tolerance) {
                ec.selectedRef = partAt(p, tolerance: tolerance)
            }
            return
        }
        let board = ec.board(model)
        if let hit = padAt(p, tolerance: tolerance) {
            ec.selectedRef = hit.ref; ec.highlightNet = hit.pad["net"] as? String; return
        }
        for (i, via) in ecRows(board["vias"]).enumerated() {
            let q = ecPoint(via["position"])
            if hypot(p.x - q.x, p.y - q.y) < max(CGFloat(ecNumber(via["diameter"], 0.6)) / 2, tolerance) {
                ec.selectedVia = i; ec.highlightNet = via["net"] as? String; return
            }
        }
        for (i, trace) in ecRows(board["traces"]).enumerated() where !ec.hiddenLayers.contains(trace["layer"] as? String ?? "") {
            let a = ecPoint(trace["start"]), b = ecPoint(trace["end"]), dx = b.x - a.x, dy = b.y - a.y
            let u = max(0, min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / max(1e-9, dx * dx + dy * dy)))
            if hypot(p.x - a.x - u * dx, p.y - a.y - u * dy) < tolerance {
                ec.selectedTrace = i; ec.highlightNet = trace["net"] as? String; return
            }
        }
        ec.selectedRef = bodyAt(p)
    }
    /// Wheel/pinch zoom about the cursor, two-finger pan, and the editing keys.
    private func installMonitor() {
        guard monitor == nil else { return }
        monitor = NSEvent.addLocalMonitorForEvents(matching: [.scrollWheel, .magnify, .keyDown]) { event in
            guard ec.view != .spatial, model.workspace == .electronics else { return event }
            if event.type == .keyDown {
                if NSApp.keyWindow?.firstResponder is NSTextView { return event }
                if event.keyCode == 53, ec.routeStart != nil { ec.cancelRoute(); return nil }
                if !ec.zonePoints.isEmpty {
                    if event.keyCode == 53 { ec.zonePoints = []; return nil }
                    if event.keyCode == 36 { ec.finishZone(model); return nil }
                }
                if event.keyCode == 51 || event.keyCode == 117, ec.selectedZone != nil { ec.deleteSelectedZone(model); return nil }
                if !ec.wirePoints.isEmpty {
                    if event.keyCode == 53 { ec.wirePoints = []; return nil }
                    if event.keyCode == 36 { ec.finishWire(model); return nil }
                }
                if event.keyCode == 51 || event.keyCode == 117, ec.selectedWire != nil || ec.selectedLabel != nil {
                    ec.deleteSheetSelection(model); return nil
                }
                if event.keyCode == 51 || event.keyCode == 117, ec.selectedTrace != nil || ec.selectedVia != nil {
                    ec.removeSelection(model); return nil
                }
                if ec.routeStart != nil, event.charactersIgnoringModifiers == "v" {
                    ec.nextRouteLayer()
                    if let t = ec.routeTarget { ec.updateRoute(model, to: t, endsOnCopper: ec.routeEndsOnCopper) }
                    return nil
                }
                if event.keyCode == 53, ec.highlightNet != nil || ec.focusPoint != nil { ec.highlightNet = nil; ec.focusPoint = nil; return nil }
                guard event.modifierFlags.intersection([.command, .control, .option]).isEmpty else { return event }
                switch event.charactersIgnoringModifiers {
                case "r" where ec.selectedRef != nil:
                    let rows = ecRows(ec.view == .schematic ? ec.schematic(model)["components"] : ec.board(model)["footprints"])
                    let f = rows.first { $0["ref"] as? String == ec.selectedRef }
                    ec.editComponent(model, key: "rotation", value: (ecNumber(f?["rotation"]) + 90).truncatingRemainder(dividingBy: 360))
                    return nil
                case "f" where ec.selectedRef != nil && ec.view == .board:
                    ec.flipSelected(model); return nil
                case "h": ec.showRatsnest.toggle(); return nil
                case "0": ec.zoom = 1; ec.pan = .zero; ec.focusPoint = nil; return nil
                default: return event
                }
            }
            guard let at = pointerScreen, let world = pointer else { return event }
            if event.type == .scrollWheel, event.hasPreciseScrollingDeltas, !event.modifierFlags.contains(.command) {
                ec.pan.width += event.scrollingDeltaX; ec.pan.height += event.scrollingDeltaY
                return nil
            }
            let factor = event.type == .magnify ? 1 + event.magnification : pow(1.002, event.scrollingDeltaY * (event.hasPreciseScrollingDeltas ? 1 : 12))
            ec.zoom = min(60, max(0.3, ec.zoom * factor))
            ec.pan = .zero
            let (s, o) = transform(canvasSize)
            ec.pan = CGSize(width: at.x - (o.x + world.x * s), height: at.y - (o.y + world.y * s))
            return nil
        }
    }
}

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

import SwiftUI

// The schematic as a schematic: real part glyphs, IC boxes with named pins,
// power symbols, net flags for anything long, right-angle wires for the rest.

private let wireColor = Color(red: 0.36, green: 0.80, blue: 0.48)
private let bodyColor = Color(white: 0.88)

@MainActor struct SchematicPainter {
    let context: GraphicsContext
    let ec: ElectronicsWorkspace
    let sheet: ECObject
    let scale: CGFloat
    let screen: (CGPoint) -> CGPoint

    enum Kind { case resistor, capacitor, inductor, ferrite, diode, led, switch_, box }

    static func kind(_ c: ECObject) -> Kind {
        guard ecRows(c["pins"]).count == 2 else { return .box }
        let ref = (c["ref"] as? String ?? "").uppercased()
        if ref.hasPrefix("FB") { return .ferrite }
        if ref.hasPrefix("SW") { return .switch_ }
        if ref.hasPrefix("LED") { return .led }
        if ref.hasPrefix("DB") || ref.hasPrefix("D") {
            return (c["value"] as? String ?? "").lowercased().contains("green") ? .led : .diode
        }
        switch ref.first {
        case "R": return .resistor
        case "C": return .capacitor
        case "L": return .inductor
        case "T": return .resistor
        default: return .box
        }
    }

    /// Unit direction a pin leaves its symbol, in sheet space.
    static func pinOut(_ local: CGPoint, body: CGRect, in c: ECObject) -> CGPoint {
        let d = [(abs(local.x - body.minX), CGPoint(x: -1, y: 0)), (abs(local.x - body.maxX), CGPoint(x: 1, y: 0)),
                 (abs(local.y - body.minY), CGPoint(x: 0, y: -1)), (abs(local.y - body.maxY), CGPoint(x: 0, y: 1))]
        var v = d.min { $0.0 < $1.0 }!.1
        if c["mirror"] as? Bool == true { v.x = -v.x }
        let r = ecNumber(c["rotation"]) * .pi / 180
        return CGPoint(x: (v.x * cos(r) - v.y * sin(r)).rounded(), y: (v.x * sin(r) + v.y * cos(r)).rounded())
    }

    private struct PinAt { let at: CGPoint; let out: CGPoint }

    private func pinsByKey() -> [String: PinAt] {
        var out: [String: PinAt] = [:]
        for c in ecRows(sheet["components"]) {
            let ref = c["ref"] as? String ?? "", body = ElectronicsCanvas.symbolBody(c)
            for (pin, local) in ElectronicsCanvas.symbolPins(c) {
                out["\(ref).\(pin["number"] as? String ?? "")"] = PinAt(at: ElectronicsCanvas.world(local: local, in: c),
                                                                        out: Self.pinOut(local, body: body, in: c))
            }
        }
        return out
    }

    private func alpha(_ net: String) -> Double { ec.highlightNet == nil || ec.highlightNet == net ? 1 : 0.15 }
    private func tint(_ net: String) -> Color {
        ec.highlightNet == net ? Color.white : wireColor.opacity(alpha(net))
    }
    private func stroke(_ pts: [CGPoint], _ color: Color, _ width: CGFloat = 1.2) {
        var p = Path(); p.addLines(pts.map(screen))
        context.stroke(p, with: .color(color), style: StrokeStyle(lineWidth: width, lineCap: .round, lineJoin: .round))
    }
    private func label(_ text: String, at p: CGPoint, size: CGFloat, color: Color, anchor: UnitPoint = .center, weight: Font.Weight = .regular) {
        context.draw(Text(text).font(.system(size: size, weight: weight)).foregroundStyle(color), at: screen(p), anchor: anchor)
    }
    private var textSize: CGFloat { min(13, max(7.5, scale * 1.15)) }

    func draw() {
        let nets = sheet["nets"] as? [String: [String]] ?? [:]
        let pins = pinsByKey()
        let wires = ecRows(sheet["wires"]).map { (ecPoint($0["start"]), ecPoint($0["end"])) }
        // Pins with a drawn wire or label need no generated stub.
        let labels = ecRows(sheet["labels"])
        func attached(_ p: CGPoint) -> Bool {
            wires.contains { SchematicEdit.on(p, $0.0, $0.1) } || labels.contains { hypot(ecPoint($0["position"]).x - p.x, ecPoint($0["position"]).y - p.y) < 0.05 }
        }
        for (name, refs) in nets {
            let points = refs.compactMap { pins[$0] }.filter { !attached($0.at) }
            guard !points.isEmpty else { continue }
            drawNet(name, points)
        }
        drawWires(wires, nets, pins)
        for l in labels { drawLabel(l) }
        for c in ecRows(sheet["components"]) { drawSymbol(c) }
        drawWirePreview()
    }

    private func drawWires(_ wires: [(CGPoint, CGPoint)], _ nets: [String: [String]], _ pins: [String: PinAt]) {
        // Colour by the net of whatever pin each connected group reaches.
        let groups = SchematicEdit.groups(wires)
        var netOf: [Int: String] = [:]
        for (name, refs) in nets {
            for r in refs {
                guard let p = pins[r] else { continue }
                for (i, w) in wires.enumerated() where SchematicEdit.on(p.at, w.0, w.1) { netOf[groups[i]] = name }
            }
        }
        for (i, w) in wires.enumerated() {
            let net = netOf[groups[i]] ?? ""
            stroke([w.0, w.1], net.isEmpty ? wireColor : tint(net), ec.highlightNet == net && !net.isEmpty ? 2 : 1.3)
        }
        for j in SchematicEdit.junctions(wires) {
            let c = screen(j), r = max(2.2, 0.45 * scale)
            context.fill(Path(ellipseIn: CGRect(x: c.x - r, y: c.y - r, width: 2 * r, height: 2 * r)), with: .color(wireColor))
        }
    }

    private func drawLabel(_ l: ECObject) {
        let text = l["name"] as? String ?? "", at = ecPoint(l["position"])
        let p = PinAt(at: at, out: CGPoint(x: 0, y: -1))
        if text.uppercased().contains("GND") { ground(PinAt(at: at, out: CGPoint(x: 0, y: 1)), text); return }
        if ElectronicsCanvas.isPowerNet(text) { supply(p, text); return }
        let color = tint(text)
        context.fill(Path(ellipseIn: CGRect(x: screen(at).x - 2, y: screen(at).y - 2, width: 4, height: 4)), with: .color(color))
        label(text, at: CGPoint(x: at.x + 0.5, y: at.y - 0.4), size: max(textSize, 9), color: color, anchor: .bottomLeading, weight: .semibold)
    }

    private func drawWirePreview() {
        guard let last = ec.wirePoints.last else { return }
        stroke(ec.wirePoints, .white, 1.5)
        if let c = ec.cursor {
            let end = ec.snapped(c)
            var p = Path(); p.addLines(SchematicEdit.leg(last, end).map(screen))
            context.stroke(p, with: .color(.white.opacity(0.8)), style: StrokeStyle(lineWidth: 1.2, dash: [4, 3]))
        }
        let s = screen(ec.wirePoints[0])
        context.stroke(Path(ellipseIn: CGRect(x: s.x - 4, y: s.y - 4, width: 8, height: 8)), with: .color(.accentColor), lineWidth: 2)
    }

    private func drawNet(_ name: String, _ points: [PinAt]) {
        let upper = name.uppercased()
        if upper.contains("GND") { points.forEach { ground($0, name) }; return }
        if ElectronicsCanvas.isPowerNet(name) { points.forEach { supply($0, name) }; return }
        let xs = points.map(\.at.x), ys = points.map(\.at.y)
        let span = hypot((xs.max() ?? 0) - (xs.min() ?? 0), (ys.max() ?? 0) - (ys.min() ?? 0))
        if points.count > 3 || span > 32 || points.count < 2 { points.forEach { flag($0, name) }; return }
        // Short local net: right-angle wires, chained nearest-first.
        var rest = points, cur = rest.removeFirst()
        while !rest.isEmpty {
            let i = rest.indices.min { hypot(rest[$0].at.x - cur.at.x, rest[$0].at.y - cur.at.y) < hypot(rest[$1].at.x - cur.at.x, rest[$1].at.y - cur.at.y) }!
            let next = rest.remove(at: i)
            let a = CGPoint(x: cur.at.x + cur.out.x * 1.27, y: cur.at.y + cur.out.y * 1.27)
            let b = CGPoint(x: next.at.x + next.out.x * 1.27, y: next.at.y + next.out.y * 1.27)
            let knee = cur.out.x != 0 ? CGPoint(x: b.x, y: a.y) : CGPoint(x: a.x, y: b.y)
            stroke([cur.at, a, knee, b, next.at], tint(name), ec.highlightNet == name ? 2 : 1.2)
            cur = next
        }
        if scale > 5, let p = points.first {
            label(name, at: CGPoint(x: p.at.x + p.out.x * 1.8, y: p.at.y - 0.9), size: 8, color: tint(name), anchor: p.out.x < 0 ? .trailing : .leading)
        }
    }

    /// Stub, then the sign, drawn upright regardless of which way the pin faces.
    private func stub(_ p: PinAt, _ length: CGFloat, _ net: String) -> CGPoint {
        let end = CGPoint(x: p.at.x + p.out.x * length, y: p.at.y + p.out.y * length)
        stroke([p.at, end], tint(net))
        return end
    }
    private func ground(_ p: PinAt, _ net: String) {
        let e = stub(p, 1.5, net), color = tint(net)
        for (i, half) in [1.3, 0.8, 0.3].enumerated() {
            let y = e.y + CGFloat(i) * 0.5
            stroke([CGPoint(x: e.x - half, y: y), CGPoint(x: e.x + half, y: y)], color)
        }
        if net != "GND", scale > 4 { label(net, at: CGPoint(x: e.x, y: e.y + 2.2), size: 8, color: color) }
    }
    private func supply(_ p: PinAt, _ net: String) {
        let e = stub(p, 1.5, net), color = ec.highlightNet == net ? Color.white : Color(red: 0.95, green: 0.55, blue: 0.35).opacity(alpha(net))
        stroke([CGPoint(x: e.x - 1, y: e.y), CGPoint(x: e.x + 1, y: e.y)], color, 1.6)
        if scale > 1.7 { label(net, at: CGPoint(x: e.x, y: e.y - 0.5), size: textSize * 0.9, color: color, anchor: .bottom, weight: .medium) }
    }
    private func flag(_ p: PinAt, _ net: String) {
        let e = stub(p, 1.5, net), color = tint(net), left = p.out.x < 0
        if scale > 1.7 {
            let at = p.out.y == 0 ? CGPoint(x: e.x + (left ? -0.4 : 0.4), y: e.y) : CGPoint(x: e.x, y: e.y + p.out.y * 0.4)
            label(net, at: at, size: textSize * 0.9, color: color, anchor: p.out.y == 0 ? (left ? .trailing : .leading) : (p.out.y < 0 ? .bottom : .top), weight: .medium)
        }
    }

    private func drawSymbol(_ c: ECObject) {
        let ref = c["ref"] as? String ?? "?", value = c["value"] as? String ?? ""
        let selected = ref == ec.selectedRef
        let color: Color = selected ? .accentColor : bodyColor.opacity(ec.highlightNet == nil ? 1 : 0.4)
        let pins = ElectronicsCanvas.symbolPins(c)
        if Self.kind(c) == .box {
            drawBox(c, ref, value, pins, color, selected)
        } else if pins.count == 2 {
            drawGlyph(Self.kind(c), c, pins[0].local, pins[1].local, ref, value, color)
        }
    }

    private func drawBox(_ c: ECObject, _ ref: String, _ value: String, _ pins: [(pin: ECObject, local: CGPoint)], _ color: Color, _ selected: Bool) {
        let body = ElectronicsCanvas.symbolBody(c)
        let corners = [CGPoint(x: body.minX, y: body.minY), CGPoint(x: body.maxX, y: body.minY),
                       CGPoint(x: body.maxX, y: body.maxY), CGPoint(x: body.minX, y: body.maxY)]
            .map { screen(ElectronicsCanvas.world(local: $0, in: c)) }
        var outline = Path(); outline.addLines(corners); outline.closeSubpath()
        context.fill(outline, with: .color(selected ? Color.accentColor.opacity(0.18) : Color(white: 0.5).opacity(0.10)))
        context.stroke(outline, with: .color(color), lineWidth: selected ? 2 : 1.2)
        for (pin, local) in pins {
            let edge = CGPoint(x: min(max(local.x, body.minX), body.maxX), y: min(max(local.y, body.minY), body.maxY))
            stroke([edge, local].map { ElectronicsCanvas.world(local: $0, in: c) }, color)
            guard scale > 3.4 else { continue }
            let at = ElectronicsCanvas.world(local: edge, in: c)
            let onLeft = abs(local.x - body.minX) <= abs(local.x - body.maxX)
            let side: CGFloat = onLeft ? 1 : -1
            let number = pin["number"] as? String ?? "", name = pin["name"] as? String ?? ""
            if !name.isEmpty, name != number, name != "~" {
                label(name, at: CGPoint(x: at.x + side * 0.6, y: at.y), size: 8, color: color.opacity(0.85), anchor: onLeft ? .leading : .trailing)
            }
            label(number, at: CGPoint(x: at.x - side * 0.5, y: at.y - 0.7), size: 7, color: .secondary, anchor: onLeft ? .trailing : .leading)
        }
        let top = ElectronicsCanvas.world(local: CGPoint(x: body.midX, y: body.minY), in: c)
        label(ref, at: CGPoint(x: top.x, y: top.y - 3.2), size: textSize, color: color, anchor: .bottom, weight: .semibold)
        label(value, at: CGPoint(x: top.x, y: top.y - 0.6), size: textSize * 0.85, color: .secondary, anchor: .bottom)
    }

    private func drawGlyph(_ kind: Kind, _ c: ECObject, _ a: CGPoint, _ b: CGPoint, _ ref: String, _ value: String, _ color: Color) {
        let len = max(0.01, hypot(b.x - a.x, b.y - a.y))
        let u = CGPoint(x: (b.x - a.x) / len, y: (b.y - a.y) / len), n = CGPoint(x: -u.y, y: u.x)
        let mid = CGPoint(x: (a.x + b.x) / 2, y: (a.y + b.y) / 2)
        func at(_ t: CGFloat, _ s: CGFloat) -> CGPoint {
            ElectronicsCanvas.world(local: CGPoint(x: mid.x + u.x * t + n.x * s, y: mid.y + u.y * t + n.y * s), in: c)
        }
        let half = len / 2
        func leads(_ inner: CGFloat) { stroke([at(-half, 0), at(-inner, 0)], color); stroke([at(inner, 0), at(half, 0)], color) }
        switch kind {
        case .resistor:
            leads(2.2); stroke([at(-2.2, -0.8), at(2.2, -0.8), at(2.2, 0.8), at(-2.2, 0.8), at(-2.2, -0.8)], color)
        case .capacitor:
            leads(0.45)
            stroke([at(-0.45, -1.8), at(-0.45, 1.8)], color, 1.6); stroke([at(0.45, -1.8), at(0.45, 1.8)], color, 1.6)
        case .inductor:
            leads(2.4)
            var pts: [CGPoint] = []
            for k in 0..<4 {
                let c0 = -1.8 + CGFloat(k) * 1.2
                pts += (0...8).map { i in
                    let th = CGFloat.pi * (1 - CGFloat(i) / 8)
                    return at(c0 + 0.6 * cos(th), -0.9 * sin(th))
                }
            }
            stroke(pts, color)
        case .ferrite:
            leads(2)
            stroke([at(-2, -0.8), at(2, -0.8), at(2, 0.8), at(-2, 0.8), at(-2, -0.8)], color)
            stroke([at(-1.2, 0.8), at(1.2, -0.8)], color)
        case .diode, .led:
            leads(1)
            stroke([at(-1, -1.1), at(1, 0), at(-1, 1.1), at(-1, -1.1)], color); stroke([at(1, -1.1), at(1, 1.1)], color, 1.6)
            if kind == .led { stroke([at(-0.2, -1.5), at(0.6, -2.4)], color); stroke([at(0.8, -1.3), at(1.6, -2.2)], color) }
        case .switch_:
            leads(1.6); stroke([at(-1.6, 0), at(1.4, -1.2)], color)
        case .box: break
        }
        guard scale > 2 else { return }
        let side = abs(u.x) >= abs(u.y)
        let p = side ? at(0, u.x >= 0 ? -2.3 : 2.3) : at(u.y >= 0 ? 2.6 : -2.6, 0)
        let q = side ? at(0, u.x >= 0 ? 2.3 : -2.3) : p
        if side {
            label(ref, at: p, size: textSize, color: color, anchor: .bottom, weight: .semibold)
            label(value, at: q, size: textSize * 0.85, color: .secondary, anchor: .top)
        } else {
            let right = CGPoint(x: mid.x, y: mid.y)
            let w = ElectronicsCanvas.world(local: right, in: c)
            label(ref, at: CGPoint(x: w.x + 2.6, y: w.y - 0.8), size: textSize, color: color, anchor: .leading, weight: .semibold)
            label(value, at: CGPoint(x: w.x + 2.6, y: w.y + 0.8), size: textSize * 0.85, color: .secondary, anchor: .leading)
        }
    }
}

extension ElectronicsWorkspace {
    /// Lay the sheet out by function: each IC with the parts that belong to
    /// it, blocks packed left to right. One undo step.
    func tidySchematic(_ model: EditorModel) {
        let sheet = schematic(model)
        let components = ecRows(sheet["components"])
        let nets = (sheet["nets"] as? [String: [String]] ?? [:]).filter { !ElectronicsCanvas.isPowerNet($0.key) }
        var netsOf: [String: Set<String>] = [:]
        for (net, pins) in nets {
            for p in pins { netsOf[String(p.split(separator: ".")[0]), default: []].insert(net) }
        }
        let refs = components.compactMap { $0["ref"] as? String }
        let count = Dictionary(uniqueKeysWithValues: components.map { ($0["ref"] as? String ?? "", ecRows($0["pins"]).count) })
        let hubs = refs.filter { $0.hasPrefix("U") }.sorted { (count[$0] ?? 0) > (count[$1] ?? 0) }
        var owner: [String: String] = Dictionary(uniqueKeysWithValues: hubs.map { ($0, $0) })
        // Grow outward: a part joins the block it shares the most signal nets with.
        for _ in 0..<4 {
            var changed = false
            for r in refs where owner[r] == nil {
                var score: [String: Int] = [:]
                for other in refs where other != r {
                    if let o = owner[other] { score[o, default: 0] += (netsOf[r] ?? []).intersection(netsOf[other] ?? []).count }
                }
                if let best = score.filter({ $0.value > 0 }).max(by: { ($0.value, $1.key) < ($1.value, $0.key) }) {
                    owner[r] = best.key; changed = true
                }
            }
            if !changed { break }
        }
        var blocks: [String: [String]] = [:]
        for r in refs where !hubs.contains(r) { blocks[owner[r] ?? "misc", default: []].append(r) }
        var placed: [String: CGPoint] = [:]
        let byRef = Dictionary(uniqueKeysWithValues: components.map { ($0["ref"] as? String ?? "", $0) })
        // Room a symbol needs: its body plus space for its net names.
        func cell(_ ref: String) -> CGSize {
            guard let c = byRef[ref] else { return CGSize(width: 18, height: 14) }
            let b = ElectronicsCanvas.symbolBody(c)
            return ecRows(c["pins"]).count <= 2 ? CGSize(width: 18, height: 14) : CGSize(width: b.width + 26, height: b.height + 14)
        }
        struct Block { let hub: String?; let members: [String]; let hubBox: CGRect; let flowW: CGFloat; let size: CGSize }
        func flow(_ members: [String], width: CGFloat) -> (CGFloat, [String: CGPoint]) {
            var x: CGFloat = 0, y: CGFloat = 0, rowH: CGFloat = 0, out: [String: CGPoint] = [:]
            for r in members {
                let c = cell(r)
                if x > 0, x + c.width > width { x = 0; y += rowH; rowH = 0 }
                out[r] = CGPoint(x: x + c.width / 2, y: y + c.height / 2)
                x += c.width; rowH = max(rowH, c.height)
            }
            return (y + rowH, out)
        }
        var packed: [Block] = []
        for hub in hubs + ["misc"] {
            let members = (blocks[hub] ?? []).sorted { $0.localizedStandardCompare($1) == .orderedAscending }
            let isHub = hub != "misc"
            guard isHub || !members.isEmpty else { continue }
            var box = CGRect.zero
            if isHub, let c = byRef[hub] { box = ElectronicsCanvas.symbolBody(c).insetBy(dx: -18, dy: -7) }
            let area = members.reduce(CGFloat(0)) { $0 + cell($1).width * cell($1).height }
            let flowW = members.isEmpty ? 0 : max(54, (area * 1.3).squareRoot())
            let flowH = flow(members, width: flowW).0
            packed.append(Block(hub: isHub ? hub : nil, members: members, hubBox: box, flowW: flowW,
                                size: CGSize(width: box.width + flowW + 12, height: max(box.height, flowH) + 12)))
        }
        let sheetW = max(packed.map(\.size.width).max() ?? 0, (packed.reduce(CGFloat(0)) { $0 + $1.size.width * $1.size.height } * 2.6).squareRoot())
        var x: CGFloat = 0, y: CGFloat = 0, rowH: CGFloat = 0
        for block in packed {
            if x > 0, x + block.size.width > sheetW { x = 0; y += rowH; rowH = 0 }
            if let hub = block.hub { placed[hub] = CGPoint(x: x - block.hubBox.minX, y: y - block.hubBox.minY) }
            for (ref, p) in flow(block.members, width: block.flowW).1 {
                placed[ref] = CGPoint(x: x + block.hubBox.width + p.x, y: y + p.y)
            }
            x += block.size.width; rowH = max(rowH, block.size.height)
        }
        model.editElectronics(name: "Tidy Schematic") { doc in
            var s = doc["schematic"] as? ECObject ?? [:]
            s["components"] = ecRows(s["components"]).map { c -> ECObject in
                var c = c
                if let ref = c["ref"] as? String, let p = placed[ref] { c["position"] = ecJSON(p) }
                return c
            }
            // Parts all moved: drawn wires would dangle. The netlist keeps the
            // connections, and they redraw as stubs and flags.
            s["wires"] = [ECObject](); s["junctions"] = [ECObject](); s["labels"] = [ECObject]()
            doc["schematic"] = s
        }
        zoom = 1; pan = .zero
    }

    /// The sheet as drawn, with any part mid-drag moved.
    func drawSchematic(_ model: EditorModel) -> ECObject {
        var s = schematic(model)
        guard let ref = dragRef, let p = dragPosition else { return s }
        var rows = ecRows(s["components"])
        if let i = rows.firstIndex(where: { $0["ref"] as? String == ref }) { rows[i]["position"] = ecJSON(p); s["components"] = rows }
        return s
    }
}

/// Sheet geometry shared by drawing and editing.
enum SchematicEdit {
    static func on(_ p: CGPoint, _ a: CGPoint, _ b: CGPoint) -> Bool {
        let dx = b.x - a.x, dy = b.y - a.y, l2 = max(1e-9, dx * dx + dy * dy)
        let u = max(0, min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / l2))
        return hypot(p.x - a.x - u * dx, p.y - a.y - u * dy) < 0.05
    }
    static func closest(_ p: CGPoint, _ a: CGPoint, _ b: CGPoint) -> CGPoint {
        let dx = b.x - a.x, dy = b.y - a.y, l2 = max(1e-9, dx * dx + dy * dy)
        let u = max(0, min(1, ((p.x - a.x) * dx + (p.y - a.y) * dy) / l2))
        return CGPoint(x: a.x + u * dx, y: a.y + u * dy)
    }
    /// Right-angle leg from `a` to `b`: horizontal, then vertical.
    static func leg(_ a: CGPoint, _ b: CGPoint) -> [CGPoint] {
        if abs(a.x - b.x) < 1e-6 || abs(a.y - b.y) < 1e-6 { return [a, b] }
        return [a, CGPoint(x: b.x, y: a.y), b]
    }
    /// Group index per wire: wires touching end-to-anywhere share one.
    static func groups(_ wires: [(CGPoint, CGPoint)]) -> [Int] {
        var parent = Array(wires.indices)
        func find(_ x: Int) -> Int { var x = x; while parent[x] != x { parent[x] = parent[parent[x]]; x = parent[x] }; return x }
        for i in wires.indices {
            for j in wires.indices where j > i {
                let (a, b) = (wires[i], wires[j])
                if on(a.0, b.0, b.1) || on(a.1, b.0, b.1) || on(b.0, a.0, a.1) || on(b.1, a.0, a.1) { parent[find(i)] = find(j) }
            }
        }
        return wires.indices.map(find)
    }
    /// Points where three or more wire ends meet, or an end lands mid-wire.
    static func junctions(_ wires: [(CGPoint, CGPoint)]) -> [CGPoint] {
        var out: [CGPoint] = []
        for p in wires.flatMap({ [$0.0, $0.1] }) where !out.contains(where: { hypot($0.x - p.x, $0.y - p.y) < 0.05 }) {
            var degree = 0
            for w in wires where on(p, w.0, w.1) {
                let atEnd = hypot(p.x - w.0.x, p.y - w.0.y) < 0.05 || hypot(p.x - w.1.x, p.y - w.1.y) < 0.05
                degree += atEnd ? 1 : 2
            }
            if degree >= 3 { out.append(p) }
        }
        return out
    }
}

extension ElectronicsWorkspace {
    private func sheetPins(_ model: EditorModel) -> [(key: String, at: CGPoint)] {
        ecRows(schematic(model)["components"]).flatMap { c -> [(key: String, at: CGPoint)] in
            let ref = c["ref"] as? String ?? ""
            return ElectronicsCanvas.symbolPins(c).map { pin, local in
                ("\(ref).\(pin["number"] as? String ?? "")", ElectronicsCanvas.world(local: local, in: c))
            }
        }
    }
    private func sheetWires(_ model: EditorModel) -> [(CGPoint, CGPoint)] {
        ecRows(schematic(model)["wires"]).map { (ecPoint($0["start"]), ecPoint($0["end"])) }
    }
    /// What a click at `p` lands on: a pin, a point on a wire, or the grid.
    func wireSnap(_ model: EditorModel, _ p: CGPoint, tolerance: CGFloat) -> (at: CGPoint, onCopper: Bool) {
        if let pin = sheetPins(model).min(by: { hypot($0.at.x - p.x, $0.at.y - p.y) < hypot($1.at.x - p.x, $1.at.y - p.y) }),
           hypot(pin.at.x - p.x, pin.at.y - p.y) < tolerance { return (pin.at, true) }
        for w in sheetWires(model) {
            let q = SchematicEdit.closest(p, w.0, w.1)
            if hypot(q.x - p.x, q.y - p.y) < tolerance { return (q, true) }
        }
        return (snapped(p), false)
    }
    /// Click with the Wire tool: start, add a corner, or finish on a pin or wire.
    func wireClick(_ model: EditorModel, at p: CGPoint, tolerance: CGFloat) {
        let hit = wireSnap(model, p, tolerance: tolerance)
        guard let last = wirePoints.last else { wirePoints = [hit.at]; return }
        guard hypot(last.x - hit.at.x, last.y - hit.at.y) > 1e-6 else { return }
        wirePoints += SchematicEdit.leg(last, hit.at).dropFirst()
        if hit.onCopper { finishWire(model) }
    }
    /// Commit the wire in progress and join what it touches into one net.
    func finishWire(_ model: EditorModel) {
        guard wirePoints.count >= 2 else { wirePoints = []; return }
        let new = zip(wirePoints, wirePoints.dropFirst()).map { ($0, $1) }
        wirePoints = []
        let before = sheetWires(model), all = before + new
        let groups = SchematicEdit.groups(all)
        let group = groups[before.count]
        let reach = all.indices.filter { groups[$0] == group }.map { all[$0] }
        let pins = sheetPins(model).filter { pin in reach.contains { SchematicEdit.on(pin.at, $0.0, $0.1) } }.map(\.key)
        let labels = ecRows(schematic(model)["labels"]).filter { l in reach.contains { SchematicEdit.on(ecPoint(l["position"]), $0.0, $0.1) } }
            .compactMap { $0["name"] as? String }
        applyToNets(model, name: "Draw Wire", pins: pins, wanted: labels.first) { s in
            s["wires"] = ecRows(s["wires"]) + new.map { ["start": ecJSON($0.0), "end": ecJSON($0.1)] as ECObject }
        }
    }
    /// Place a net label at `p`: whatever it touches joins the named net.
    func placeLabel(_ model: EditorModel, at p: CGPoint, tolerance: CGFloat) {
        let text = labelText.trimmingCharacters(in: .whitespaces)
        guard !text.isEmpty else { message = "Type a net name in the inspector first."; return }
        let hit = wireSnap(model, p, tolerance: tolerance)
        let wires = sheetWires(model), groups = SchematicEdit.groups(wires)
        let touched = Set(wires.indices.filter { SchematicEdit.on(hit.at, wires[$0].0, wires[$0].1) }.map { groups[$0] })
        let reach = wires.indices.filter { touched.contains(groups[$0]) }.map { wires[$0] }
        let pins = sheetPins(model).filter { pin in
            hypot(pin.at.x - hit.at.x, pin.at.y - hit.at.y) < 0.05 || reach.contains { SchematicEdit.on(pin.at, $0.0, $0.1) }
        }.map(\.key)
        applyToNets(model, name: "Place Label", pins: pins, wanted: text) { s in
            s["labels"] = ecRows(s["labels"]) + [["name": text, "position": ecJSON(hit.at), "rotation": 0, "scope": "Local"] as ECObject]
        }
    }
    /// Join `pins` into one net — `wanted`, or the best name they already
    /// have — keep the board in step, and make the whole edit one undo.
    private func applyToNets(_ model: EditorModel, name: String, pins: [String], wanted: String?, _ edit: (inout ECObject) -> Void) {
        let undoBefore = model.undoStack
        let before = model.documentJSON.flatMap { DocEdit.serialize($0) }
        var nets = schematic(model)["nets"] as? [String: [String]] ?? [:]
        let matching = nets.keys.filter { key in nets[key]!.contains { pins.contains($0) } }
        let isAuto = { (n: String) in n.first == "N" && n.dropFirst().allSatisfy(\.isNumber) }
        var next = 1
        while nets["N\(next)"] != nil { next += 1 }
        let target = wanted ?? matching.filter { !isAuto($0) }.sorted().first ?? matching.sorted().first ?? "N\(next)"
        let joined = matching.filter { $0 != target && !isAuto($0) }
        var refs = Set(pins).union(nets[target] ?? [])
        for key in matching where key != target { refs.formUnion(nets.removeValue(forKey: key) ?? []) }
        if !refs.isEmpty { nets[target] = refs.sorted() }
        model.editElectronics(name: name) { doc in
            var s = doc["schematic"] as? ECObject ?? [:]
            edit(&s); s["nets"] = nets; doc["schematic"] = s
        }
        if !refs.isEmpty { syncBoardNets(model, nets: nets, merged: matching.filter { $0 != target }, target: target) }
        if let before { model.undoStack = Array((undoBefore + [UndoEntry(data: before, name: name)]).suffix(64)) }
        message = joined.isEmpty ? nil : "Joined \(joined.joined(separator: ", ")) into \(target)"
        activeNet = target
    }
}

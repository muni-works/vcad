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
        for (name, refs) in nets {
            let points = refs.compactMap { pins[$0] }
            guard !points.isEmpty else { continue }
            drawNet(name, points)
        }
        for c in ecRows(sheet["components"]) { drawSymbol(c) }
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

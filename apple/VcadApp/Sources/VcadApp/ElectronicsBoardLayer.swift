import SwiftUI

// The board as a PCB tool draws it: real pad shapes and footprint art, per-
// layer copper, zones, a live ratsnest, net highlighting and DRC markers.

enum BoardLayers {
    static let copper = ["FCu", "In1Cu", "In2Cu", "BCu"]
    static func name(_ l: String) -> String {
        switch l {
        case "FCu": "Front"
        case "BCu": "Back"
        case "In1Cu": "Inner 1"
        case "In2Cu": "Inner 2"
        default: l
        }
    }
    static func color(_ l: String) -> Color {
        switch l {
        case "FCu": Color(red: 0.86, green: 0.24, blue: 0.22)
        case "BCu": Color(red: 0.27, green: 0.52, blue: 0.95)
        case "In1Cu": Color(red: 0.90, green: 0.70, blue: 0.20)
        case "In2Cu": Color(red: 0.32, green: 0.72, blue: 0.42)
        default: .gray
        }
    }
}

/// One airwire: the shortest gap between two unconnected islands of a net.
struct Airwire { let a: CGPoint, b: CGPoint, net: String }

@MainActor enum BoardRatsnest {
    private struct Node { let p: CGPoint; let reach: CGFloat; let layers: Set<String>; let isPad: Bool }

    /// Group each net's pads, traces and vias into copper islands, then span
    /// the islands with their shortest gaps. A net with a plane joins every
    /// via and through pad that reaches the plane's layer.
    static func compute(_ board: ECObject) -> [Airwire] {
        var nodes: [String: [Node]] = [:]
        var links: [String: [(Int, Int)]] = [:]
        for f in ecRows(board["footprints"]) {
            for pad in ecRows(f["pads"]) {
                guard let net = pad["net"] as? String, !net.isEmpty else { continue }
                let layers = Set((pad["layers"] as? [String] ?? []).filter { BoardLayers.copper.contains($0) })
                guard !layers.isEmpty else { continue }
                let shape = pad["shape"] as? ECObject ?? [:]
                let reach = max(ecNumber(shape["width"], ecNumber(shape["diameter"], 0.5)),
                                ecNumber(shape["height"], ecNumber(shape["diameter"], 0.5))) / 2
                nodes[net, default: []].append(Node(p: ElectronicsCanvas.world(pad, in: f), reach: CGFloat(reach), layers: layers, isPad: true))
            }
        }
        for t in ecRows(board["traces"]) {
            guard let net = t["net"] as? String, let layer = t["layer"] as? String else { continue }
            let i = nodes[net]?.count ?? 0
            nodes[net, default: []].append(Node(p: ecPoint(t["start"]), reach: 0.02, layers: [layer], isPad: false))
            nodes[net, default: []].append(Node(p: ecPoint(t["end"]), reach: 0.02, layers: [layer], isPad: false))
            links[net, default: []].append((i, i + 1))
        }
        for v in ecRows(board["vias"]) {
            guard let net = v["net"] as? String else { continue }
            nodes[net, default: []].append(Node(p: ecPoint(v["position"]), reach: CGFloat(ecNumber(v["diameter"], 0.6) / 2),
                                                layers: Set(BoardLayers.copper), isPad: false))
        }
        var planes: [String: Set<String>] = [:]
        for z in ecRows(board["zones"]) {
            if let net = z["net"] as? String, let layer = z["layer"] as? String { planes[net, default: []].insert(layer) }
        }

        var out: [Airwire] = []
        for (net, list) in nodes {
            var parent = Array(list.indices)
            func find(_ x: Int) -> Int { var x = x; while parent[x] != x { parent[x] = parent[parent[x]]; x = parent[x] }; return x }
            func union(_ a: Int, _ b: Int) { parent[find(a)] = find(b) }
            for (a, b) in links[net] ?? [] { union(a, b) }
            for i in list.indices {
                for j in list.indices where j > i && !list[i].layers.isDisjoint(with: list[j].layers) {
                    if hypot(list[i].p.x - list[j].p.x, list[i].p.y - list[j].p.y) <= list[i].reach + list[j].reach { union(i, j) }
                }
            }
            if let plane = planes[net] {
                let onPlane = list.indices.filter { !list[$0].layers.isDisjoint(with: plane) }
                for i in onPlane.dropFirst() { union(onPlane[0], i) }
            }
            // Prim over islands, using the closest pair between them.
            var islands = Dictionary(grouping: list.indices.filter { list[$0].isPad || true }, by: find).values.map { $0 }
            islands = islands.filter { $0.contains { list[$0].isPad } }
            guard islands.count > 1 else { continue }
            var tree = islands.removeFirst()
            while !islands.isEmpty {
                var best = (d: CGFloat.infinity, i: 0, a: CGPoint.zero, b: CGPoint.zero)
                for (k, island) in islands.enumerated() {
                    for a in tree {
                        for b in island {
                            let d = hypot(list[a].p.x - list[b].p.x, list[a].p.y - list[b].p.y)
                            if d < best.d { best = (d, k, list[a].p, list[b].p) }
                        }
                    }
                }
                out.append(Airwire(a: best.a, b: best.b, net: net))
                tree += islands.remove(at: best.i)
            }
        }
        return out
    }
}

@MainActor struct BoardPainter {
    let context: GraphicsContext
    let ec: ElectronicsWorkspace
    let board: ECObject
    let scale: CGFloat
    let screen: (CGPoint) -> CGPoint

    private func dim(_ net: String?) -> Double {
        guard let h = ec.highlightNet else { return 1 }
        return net == h ? 1 : 0.14
    }
    private func tint(_ c: Color, _ net: String?) -> Color {
        if let h = ec.highlightNet, net == h { return c.mix(with: .white, by: 0.35) }
        return c.opacity(dim(net))
    }

    func draw() {
        let hidden = ec.hiddenLayers
        // Bottom-up, with the active layer last so it reads on top.
        let order = BoardLayers.copper.reversed().filter { $0 != ec.activeLayer } + [ec.activeLayer]
        for z in ecRows(board["zones"]) where !hidden.contains(z["layer"] as? String ?? "") {
            let pts = ecRows(z["outline"]).map { screen(ecPoint($0)) }
            guard pts.count > 2 else { continue }
            var path = Path(); path.addLines(pts); path.closeSubpath()
            let color = BoardLayers.color(z["layer"] as? String ?? "")
            context.fill(path, with: .color(color.opacity(0.10 * dim(z["net"] as? String))))
        }
        let traces = ecRows(board["traces"])
        for layer in order where !hidden.contains(layer) {
            let alpha = layer == ec.activeLayer ? 1.0 : 0.72
            for (i, t) in traces.enumerated() where t["layer"] as? String == layer {
                let net = t["net"] as? String
                var path = Path(); path.move(to: screen(ecPoint(t["start"]))); path.addLine(to: screen(ecPoint(t["end"])))
                let color = i == ec.selectedTrace ? Color.white : tint(BoardLayers.color(layer).opacity(alpha), net)
                context.stroke(path, with: .color(color), style: StrokeStyle(lineWidth: max(1, ecNumber(t["width"], 0.25) * scale), lineCap: .round))
            }
        }
        for f in ecRows(board["footprints"]) { drawArt(f) }
        for f in ecRows(board["footprints"]) { drawPads(f) }
        for (i, v) in ecRows(board["vias"]).enumerated() {
            let net = v["net"] as? String, p = screen(ecPoint(v["position"]))
            let r = max(2, ecNumber(v["diameter"], 0.6) * scale / 2), d = max(0.8, ecNumber(v["drill"], 0.3) * scale / 2)
            context.fill(Path(ellipseIn: CGRect(x: p.x - r, y: p.y - r, width: 2 * r, height: 2 * r)),
                         with: .color(i == ec.selectedVia ? .white : tint(Color(white: 0.72), net)))
            context.fill(Path(ellipseIn: CGRect(x: p.x - d, y: p.y - d, width: 2 * d, height: 2 * d)),
                         with: .color(Color.black.opacity(0.75 * dim(net))))
        }
        for f in ecRows(board["footprints"]) { drawLabel(f) }
        if ec.showRatsnest { drawRatsnest() }
        drawMarkers()
        drawRoutePreview()
    }

    private func drawRoutePreview() {
        guard let start = ec.routeStart else { return }
        let preview = ec.routePreview
        if preview?["status"] as? String == "routed", let path = preview?["path"] as? [[Any]] {
            let w = max(1.5, ecNumber(preview?["width"], 0.25) * scale)
            for seg in path where seg.count == 3 {
                var p = Path(); p.move(to: screen(ecPoint(seg[0]))); p.addLine(to: screen(ecPoint(seg[1])))
                context.stroke(p, with: .color(BoardLayers.color(seg[2] as? String ?? "").mix(with: .white, by: 0.3)),
                               style: StrokeStyle(lineWidth: w, lineCap: .round))
            }
            for via in preview?["vias"] as? [Any] ?? [] {
                let c = screen(ecPoint(via)), r = max(3, 0.3 * scale)
                context.stroke(Path(ellipseIn: CGRect(x: c.x - r, y: c.y - r, width: 2 * r, height: 2 * r)), with: .color(.white), lineWidth: 1.5)
            }
        } else if let target = ec.routeTarget, preview != nil {
            var p = Path(); p.move(to: screen(start)); p.addLine(to: screen(target))
            context.stroke(p, with: .color(.red.opacity(0.9)), style: StrokeStyle(lineWidth: 1.5, dash: [5, 4]))
        }
        if let target = ec.routeTarget, ec.routeEndsOnCopper {
            let c = screen(target)
            context.stroke(Path(ellipseIn: CGRect(x: c.x - 8, y: c.y - 8, width: 16, height: 16)), with: .color(.green), lineWidth: 2)
        }
        let c = screen(start)
        context.stroke(Path(ellipseIn: CGRect(x: c.x - 6, y: c.y - 6, width: 12, height: 12)), with: .color(.accentColor), lineWidth: 2)
    }

    private func artVisible(_ layer: String) -> Bool {
        if layer.hasSuffix("SilkS") { return ec.showSilk && !ec.hiddenLayers.contains(layer.hasPrefix("F") ? "FCu" : "BCu") }
        if layer.hasSuffix("CrtYd") { return ec.showCourtyard }
        return false
    }

    private func drawArt(_ f: ECObject) {
        let selected = f["ref"] as? String == ec.selectedRef
        for g in ecRows(f["graphics"]) {
            let layer = g["layer"] as? String ?? ""
            guard selected || artVisible(layer) else { continue }
            guard layer.hasSuffix("SilkS") || layer.hasSuffix("CrtYd") else { continue }
            var path = Path()
            switch g["type"] as? String {
            case "Line":
                path.move(to: screen(ElectronicsCanvas.world(local: ecPoint(g["start"]), in: f)))
                path.addLine(to: screen(ElectronicsCanvas.world(local: ecPoint(g["end"]), in: f)))
            case "Polygon":
                path.addLines(ecRows(g["vertices"]).map { screen(ElectronicsCanvas.world(local: ecPoint($0), in: f)) })
                path.closeSubpath()
            case "Circle":
                let c = screen(ElectronicsCanvas.world(local: ecPoint(g["center"]), in: f)), r = ecNumber(g["radius"]) * scale
                path.addEllipse(in: CGRect(x: c.x - r, y: c.y - r, width: 2 * r, height: 2 * r))
            default: continue
            }
            let base: Color = selected ? .white : layer.hasSuffix("SilkS") ? Color(white: 0.85).opacity(0.75) : Color.pink.opacity(0.35)
            context.stroke(path, with: .color(ec.highlightNet == nil || selected ? base : base.opacity(0.25)), lineWidth: max(0.6, ecNumber(g["width"], 0.1) * scale))
        }
    }

    private func drawPads(_ f: ECObject) {
        let comp = ecNumber(f["rotation"]), selected = f["ref"] as? String == ec.selectedRef
        for pad in ecRows(f["pads"]) {
            let layers = (pad["layers"] as? [String] ?? []).filter { BoardLayers.copper.contains($0) }
            guard !layers.isEmpty, layers.contains(where: { !ec.hiddenLayers.contains($0) }) else { continue }
            let net = pad["net"] as? String, through = layers.count > 1
            let c = screen(ElectronicsCanvas.world(pad, in: f))
            let shape = pad["shape"] as? ECObject ?? [:], kind = shape["type"] as? String ?? "Circle"
            let d = ecNumber(shape["diameter"], 0.6)
            let w = CGFloat(ecNumber(shape["width"], d)) * scale, h = CGFloat(ecNumber(shape["height"], d)) * scale
            let rect = CGRect(x: -w / 2, y: -h / 2, width: w, height: h)
            let path: Path
            switch kind {
            case "Circle": path = Path(ellipseIn: rect)
            case "Oval": path = Path(roundedRect: rect, cornerRadius: min(w, h) / 2)
            case "RoundRect": path = Path(roundedRect: rect, cornerRadius: CGFloat(ecNumber(shape["cornerRatio"], 0.25)) * min(w, h))
            default: path = Path(rect)
            }
            var ctx = context
            ctx.translateBy(x: c.x, y: c.y)
            ctx.rotate(by: .degrees(ecNumber(pad["rotation"]) + comp))
            let base = through ? Color(red: 0.85, green: 0.70, blue: 0.30) : BoardLayers.color(layers[0])
            ctx.fill(path, with: .color(tint(base, net)))
            if selected { ctx.stroke(path, with: .color(.white), lineWidth: 1) }
            if through, let drill = (pad["drill"] as? ECObject).map({ ecNumber($0["diameter"], 0.8) }) ?? (pad["drill"] as? NSNumber)?.doubleValue {
                let r = max(1, drill * scale / 2)
                ctx.fill(Path(ellipseIn: CGRect(x: -r, y: -r, width: 2 * r, height: 2 * r)), with: .color(Color.black.opacity(0.8 * dim(net))))
            }
            if scale > 14, let n = pad["number"] as? String, !n.isEmpty {
                context.draw(Text(n).font(.system(size: 9, weight: .semibold)).foregroundStyle(.white.opacity(dim(net))), at: c)
            }
            if scale > 22, let net {
                context.draw(Text(net).font(.system(size: 8)).foregroundStyle(.white.opacity(0.8 * dim(net))), at: CGPoint(x: c.x, y: c.y + 9))
            }
        }
    }

    private func drawLabel(_ f: ECObject) {
        guard scale > 4 || f["ref"] as? String == ec.selectedRef else { return }
        let c = screen(ecPoint(f["position"]))
        let color: Color = f["ref"] as? String == ec.selectedRef ? .white : Color.white.opacity(ec.highlightNet == nil ? 0.8 : 0.25)
        context.draw(Text(f["ref"] as? String ?? "?").font(.system(size: min(13, max(9, scale * 0.9)), weight: .medium)).foregroundStyle(color), at: c)
    }

    private func drawRatsnest() {
        for w in ec.airwires(board) {
            var path = Path(); path.move(to: screen(w.a)); path.addLine(to: screen(w.b))
            let on = ec.highlightNet == nil || ec.highlightNet == w.net
            context.stroke(path, with: .color(Color.white.opacity(on ? 0.75 : 0.08)), lineWidth: ec.highlightNet == w.net ? 1.5 : 0.8)
        }
    }

    private func drawMarkers() {
        guard ec.showDrcMarkers else { return }
        for f in ecRows(ec.tools.drc?["findings"]) {
            let rule = f["rule"] as? String ?? ""
            guard !rule.contains("Unconnected"), !rule.contains("NetIslands") else { continue }
            let p = screen(CGPoint(x: ecNumber(f["x"]), y: ecNumber(f["y"])))
            var x = Path(); x.move(to: CGPoint(x: p.x - 5, y: p.y - 5)); x.addLine(to: CGPoint(x: p.x + 5, y: p.y + 5))
            x.move(to: CGPoint(x: p.x + 5, y: p.y - 5)); x.addLine(to: CGPoint(x: p.x - 5, y: p.y + 5))
            context.stroke(x, with: .color(.yellow), lineWidth: 2)
        }
        if let focus = ec.focusPoint {
            let p = screen(focus)
            context.stroke(Path(ellipseIn: CGRect(x: p.x - 16, y: p.y - 16, width: 32, height: 32)), with: .color(.yellow), lineWidth: 2)
        }
    }
}

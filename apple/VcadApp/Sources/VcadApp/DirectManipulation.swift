import Foundation
import simd
import SwiftUI
#if canImport(AppKit)
import AppKit
#endif

// Direct manipulation on the canvas: press-pull a planar face, drag on-canvas
// handles for a feature's driving value (extrude/cut depth, fillet/chamfer
// size, shell thickness), ⌥-drag a part to move it, all with snapping and a
// live dimension label you can click to type an exact value.
//
// Every drag is ONE named undo step: the engine is snapshotted at the grab,
// each move re-loads the pre-drag IR with the new value (`irEdit` with
// `gestureStart: false`), and an exact value typed into the label afterwards
// rewrites the same step instead of adding one.
//
// The handles live inside the gizmo root (`buildGizmo`), so the viewport's
// existing hover/drag routing — which keys off entity names through
// `gizmoHandle(for:)` — drives them with no viewport changes.

/// The on-canvas dimension: what the drag currently means, and where.
struct ManipLabel: Equatable {
    var text: String
    /// Anchor in kernel coordinates (mm, Z-up).
    var anchor: SIMD3<Float>
    /// The drag that produced it can be re-applied at an exact value.
    var editable: Bool
    /// The value the label shows (mm or degrees), for the edit field.
    var value: Double?
    /// The drag has ended; the label lingers for a click.
    var settled = false
    /// Snapped to geometry (shown as a tick).
    var snappedTo: String?
}

/// What a direct-manipulation drag is editing.
enum ManipKind {
    /// Press-pull on a planar face of a primitive cube: the size along that
    /// axis (and the translate, for a negative-side face).
    case cubeFace(sizeNode: Int, axis: Int, positive: Bool, translateNode: Int?, base: Double, baseOffset: Double)
    /// Press-pull on an extrude's end cap: the extrude depth.
    case extrudeCap(node: Int, dir: SIMD3<Double>)
    /// Press-pull on any other planar face: a new boss (outward, Union) or
    /// pocket (inward, Difference) built from the face's boundary.
    case boss(part: Int, root: Int, profile: [String: Any], center: SIMD3<Double>, normal: SIMD3<Double>)
    /// A feature's scalar (`key` on `node`).
    case scalar(node: Int, key: String, base: Double, min: Double)
}

/// Drag bookkeeping (not observed: the viewport never redraws from it).
struct DirectManipState {
    var kind: ManipKind?
    var name = "Edit"
    /// IR before the drag: every move edits a copy of this.
    var base: [String: Any]?
    /// Drag axis (kernel) and its origin; the value is the ray's parameter
    /// along it relative to the grab.
    var axisOrigin: SIMD3<Float> = .zero
    var axisDir: SIMD3<Float> = .zero
    var t0: Float = 0
    /// Value change per mm of drag along the axis.
    var gain: Double = 1
    /// Plane offsets (along `axisDir`) the value snaps to, mm.
    var snapTargets: [(value: Double, label: String)] = []
    /// Last applied value.
    var value: Double = 0
    /// The gizmo handle currently grabbed (hides the others).
    var activeName: String?
    /// After a drag: reapply its IR edit at an exact value (same undo step).
    var reapply: ((Double) -> Void)?
    var fadeToken = 0
}

extension EditorModel {
    // MARK: handles

    /// The planar face picked on the selected part, if exactly one is.
    var pullFace: (part: Int, face: TopoFace)? {
        guard usesDocumentTree, !sketching else { return nil }
        let faces = picking.selection.filter { $0.kind == .face }
        guard faces.count == 1, let ref = faces.first,
              let topo = partTopology(ref.part), let f = topo.face(ref.id), f.isPlanar else { return nil }
        return (ref.part, f)
    }

    /// The selected part's editable feature handle: where it sits and which
    /// way it drags (kernel), and what it edits.
    struct FeatureHandle {
        var name: String
        var point: SIMD3<Float>
        var dir: SIMD3<Float>
        var label: String
    }

    func featureHandles() -> [FeatureHandle] {
        guard let pi = selectedPartIndex, let json = documentJSON, pullFace == nil,
              let root = visibleRootNode(json, pi) else { return [] }
        let m = pi < docPartMeshes.count ? (lo: docPartMeshes[pi].lo, hi: docPartMeshes[pi].hi)
                                         : (lo: SIMD3<Float>.zero, hi: SIMD3<Float>.zero)
        let top = Self.unwrapTransforms(json, root)
        guard let op = DocEdit.op(json, nodeId: top), let type = op["type"] as? String else { return [] }
        let corner = SIMD3<Float>(m.hi.x, m.lo.y, m.hi.z)
        let diag = simd_normalize(SIMD3<Float>(1, -1, 1))
        switch type {
        case "Fillet": return [FeatureHandle(name: "fh:radius", point: corner, dir: diag, label: "Fillet")]
        case "Chamfer": return [FeatureHandle(name: "fh:distance", point: corner, dir: diag, label: "Chamfer")]
        case "EdgeBlend": return [FeatureHandle(name: "fh:size", point: corner, dir: diag, label: "Blend")]
        case "Shell":
            let p = SIMD3<Float>((m.lo.x + m.hi.x) / 2, m.lo.y, (m.lo.z + m.hi.z) / 2)
            return [FeatureHandle(name: "fh:thickness", point: p, dir: SIMD3(0, -1, 0), label: "Shell")]
        case "Union", "Difference":
            guard let r = (op["right"] as? NSNumber)?.intValue,
                  let cap = extrudeCap(json, Self.unwrapTransforms(json, r)) else { return [] }
            return [FeatureHandle(name: "fh:extrude", point: cap.point, dir: cap.dir,
                                  label: type == "Union" ? "Extrude" : "Cut")]
        case "Extrude":
            guard let cap = extrudeCap(json, top) else { return [] }
            return [FeatureHandle(name: "fh:extrude", point: cap.point, dir: cap.dir, label: "Extrude")]
        default: return []
        }
    }

    /// Names the gizmo builder hangs and the viewport routes as handles.
    func manipHandle(for name: String) -> GizmoHandle? {
        if name == "pull", pullFace != nil { return .axis(SIMD3(0, 0, 1)) }
        if name.hasPrefix("fh:") { return .axis(SIMD3(0, 0, 1)) }
        // ⌥-drag the selected part itself: move it across the ground plane.
        #if os(macOS)
        if name.hasPrefix("part"), let i = Int(name.dropFirst(4)), i == selectedPartIndex,
           NSEvent.modifierFlags.contains(.option), pullFace == nil {
            return .plane(SIMD3(1, 0, 0), SIMD3(0, 1, 0))
        }
        #endif
        return nil
    }

    // MARK: begin / drag / end

    /// Start a press-pull or feature-handle drag. False → not ours.
    func beginManip(handle name: String, ray: (o: SIMD3<Float>, d: SIMD3<Float>)) -> Bool {
        guard name == "pull" || name.hasPrefix("fh:"), let engine, let json = documentJSON else { return false }
        manip = DirectManipState()
        manip.activeName = name
        manip.base = json
        if name == "pull" {
            guard let (pi, f) = pullFace, let root = visibleRootNode(json, pi) else { return false }
            let n = simd_normalize(f.normalF), c = f.centerF
            manip.axisOrigin = c; manip.axisDir = n
            manip.snapTargets = planeTargets(normal: n, through: c, excluding: f.id, part: pi)
            if let k = cubeFaceKind(json, root: root, normal: n) {
                manip.kind = k; manip.name = "Press Pull"
            } else if let k = extrudeCapKind(json, root: root, face: f) {
                manip.kind = k; manip.name = "Press Pull"
            } else if let prof = faceProfile(part: pi, face: f) {
                manip.kind = .boss(part: pi, root: root, profile: prof,
                                   center: SIMD3<Double>(c), normal: SIMD3<Double>(n))
                manip.name = "Press Pull"
            } else { return false }
        } else {
            guard let h = featureHandles().first(where: { $0.name == name }),
                  let pi = selectedPartIndex, let root = visibleRootNode(json, pi) else { return false }
            manip.axisOrigin = h.point; manip.axisDir = h.dir
            let top = Self.unwrapTransforms(json, root)
            if name == "fh:extrude" {
                var node = top
                if let op = DocEdit.op(json, nodeId: top), (op["type"] as? String) != "Extrude",
                   let r = (op["right"] as? NSNumber)?.intValue { node = Self.unwrapTransforms(json, r) }
                guard let d = Self.vec3(DocEdit.op(json, nodeId: node)?["direction"]) else { return false }
                manip.kind = .extrudeCap(node: node, dir: d)
                manip.name = h.label == "Cut" ? "Cut Depth" : "Extrude Depth"
            } else {
                let key = String(name.dropFirst(3))
                let base = (DocEdit.op(json, nodeId: top)?[key] as? NSNumber)?.doubleValue ?? 1
                manip.kind = .scalar(node: top, key: key, base: base, min: 0.05)
                // Blends and walls are small: a long drag for a fine value.
                manip.gain = 0.35
                manip.name = "\(h.label) \(key == "thickness" ? "Thickness" : "Size")"
            }
        }
        manip.t0 = Self.axisParam(rayO: ray.o, rayD: ray.d, center: manip.axisOrigin, axis: manip.axisDir)
        manip.value = 0
        pushUndo(.snapshot(engine.save()), name: manip.name)
        return true
    }

    /// Continue a direct-manipulation drag. False → not ours.
    func manipDragTo(ray: (o: SIMD3<Float>, d: SIMD3<Float>)) -> Bool {
        guard manip.kind != nil else { return false }
        let t = Self.axisParam(rayO: ray.o, rayD: ray.d, center: manip.axisOrigin, axis: manip.axisDir)
        var d = Double(t - manip.t0) * manip.gain
        var snapped: String?
        if manip.gain == 1, let hit = snapToTargets(d) { d = hit.value; snapped = hit.label }
        else { d = snapValue(d, fine: manip.gain < 1) }
        manip.value = d
        applyManip(d, gestureStart: false)
        let anchor = manip.axisOrigin + manip.axisDir * Float(d / manip.gain)
        showManipLabel(manipText(d), at: anchor, value: displayedValue(d), snappedTo: snapped)
        return true
    }

    /// Finish a direct-manipulation drag. False → not ours.
    @discardableResult
    func endManip() -> Bool {
        guard manip.kind != nil else { return false }
        manip.activeName = nil
        let base = manip.base, kind = manip.kind, gain = manip.gain
        manip.kind = nil
        if abs(manip.value) < 1e-9 {
            // A click on the handle, not a drag: take the empty step back.
            if let last = undoStack.last, last.name == manip.name { undo(); redoStack.removeAll() }
            manipLabel = nil
            gizmoDirty = true
            return true
        }
        markReeval(.rebuild)
        if let failed = newEvaluationErrors(), !failed.isEmpty {
            undo(); redoStack.removeAll()
            engineError = "\(manip.name) couldn't be built, so it was undone:\n\(failed)"
            manipLabel = nil
            return true
        }
        picking.clear()
        // Typing an exact value rewrites this drag's step (same snapshot).
        manip.reapply = { [weak self] v in
            guard let self, let base, let kind else { return }
            self.manip.base = base; self.manip.kind = kind; self.manip.gain = gain
            self.applyManip(self.rawValue(fromDisplayed: v), gestureStart: false)
            self.manip.kind = nil
            self.markReeval(.rebuild)
        }
        fadeManipLabel()
        gizmoDirty = true
        return true
    }

    /// Re-apply the last drag at an exact value typed into the label.
    func commitManipValue(_ v: Double) {
        manip.reapply?(v)
        manipLabel = nil
        manip.reapply = nil
    }

    // MARK: applying

    private func applyManip(_ d: Double, gestureStart: Bool) {
        guard var json = manip.base, let kind = manip.kind else { return }
        switch kind {
        case let .cubeFace(sizeNode, axis, positive, tn, base, baseOffset):
            let ax = ["x", "y", "z"][axis]
            let size = max(0.05, base + d)
            DocEdit.setVecComponent(&json, nodeId: sizeNode, key: "size", axis: ax, value: size)
            if !positive, let tn {
                DocEdit.setVecComponent(&json, nodeId: tn, key: "offset", axis: ax, value: baseOffset - (size - base))
            }
        case let .extrudeCap(node, dir):
            let len = simd_length(dir)
            guard len > 1e-9 else { return }
            let nl = max(0.05, len + d)
            let v = dir / len * nl
            for (i, ax) in ["x", "y", "z"].enumerated() {
                DocEdit.setVecComponent(&json, nodeId: node, key: "direction", axis: ax, value: v[i])
            }
        case let .boss(part, root, profile, center, normal):
            guard abs(d) > 1e-6 else { break }
            Self.appendBoss(&json, part: part, root: root, profile: profile,
                            center: center, normal: normal, distance: d)
        case let .scalar(node, key, base, lo):
            DocEdit.setScalar(&json, nodeId: node, key: key, value: max(lo, base + d))
        }
        irEdit(manip.name, gestureStart: gestureStart, reeval: .inPlace, base: json) { _ in }
    }

    /// Union a prism (outward) or subtract a pocket (inward) shaped like the
    /// face, as real Sketch2D → Extrude → boolean nodes on the part's root.
    static func appendBoss(_ json: inout [String: Any], part: Int, root: Int, profile: [String: Any],
                           center: SIMD3<Double>, normal n: SIMD3<Double>, distance d: Double) {
        guard var nodes = json["nodes"] as? [String: Any] else { return }
        let next = (nodes.values.compactMap { ($0 as? [String: Any])?["id"] as? NSNumber }
            .map(\.intValue).max() ?? 0) + 1
        let eps = 0.01
        // Start a hair inside the solid (boss) or outside it (pocket) so the
        // tool overlaps the face instead of kissing it.
        let o = d > 0 ? center - n * eps : center + n * eps
        let dir = n * (d > 0 ? d + eps : d - eps)
        var sketch = profile
        sketch["origin"] = ["x": o.x, "y": o.y, "z": o.z]
        let s = next, e = next + 1, b = next + 2
        nodes[String(s)] = ["id": s, "name": "Press Pull Profile", "op": sketch]
        nodes[String(e)] = ["id": e, "name": "Press Pull",
                            "op": ["type": "Extrude", "sketch": s,
                                   "direction": ["x": dir.x, "y": dir.y, "z": dir.z]]]
        nodes[String(b)] = ["id": b, "name": d > 0 ? "Boss" : "Pocket",
                            "op": ["type": d > 0 ? "Union" : "Difference", "left": root, "right": e]]
        json["nodes"] = nodes
        guard var roots = json["roots"] as? [[String: Any]] else { return }
        var vis = 0
        for (i, r) in roots.enumerated() where (r["visible"] as? Bool) ?? true {
            if vis == part { roots[i]["root"] = b; break }
            vis += 1
        }
        json["roots"] = roots
    }

    // MARK: classification

    /// Root through Translate/Rotate wrappers to the shape node.
    static func unwrapTransforms(_ json: [String: Any], _ id: Int) -> Int {
        var cur = id
        for _ in 0..<16 {
            guard let op = DocEdit.op(json, nodeId: cur), let t = op["type"] as? String,
                  ["Translate", "Rotate", "Scale"].contains(t),
                  let c = (op["child"] as? NSNumber)?.intValue else { return cur }
            cur = c
        }
        return cur
    }

    func visibleRootNode(_ json: [String: Any], _ part: Int) -> Int? {
        guard let roots = json["roots"] as? [[String: Any]] else { return nil }
        var vis = 0
        for r in roots where (r["visible"] as? Bool) ?? true {
            if vis == part { return (r["root"] as? NSNumber)?.intValue }
            vis += 1
        }
        return nil
    }

    static func vec3(_ any: Any?) -> SIMD3<Double>? {
        guard let v = any as? [String: Any] else { return nil }
        return SIMD3((v["x"] as? NSNumber)?.doubleValue ?? 0,
                     (v["y"] as? NSNumber)?.doubleValue ?? 0,
                     (v["z"] as? NSNumber)?.doubleValue ?? 0)
    }

    /// A cube under (at most) one Translate, face normal on a world axis →
    /// a parametric size edit. Rotated cubes fall through to a boss.
    private func cubeFaceKind(_ json: [String: Any], root: Int, normal n: SIMD3<Float>) -> ManipKind? {
        // Walk the part's transform chain: only identity rotates/scales may
        // sit between the root and the cube (the engine wraps every part in
        // Translate·Rotate·Scale), so world axes are the cube's axes.
        var node = root, translate: Int?
        for _ in 0..<8 {
            guard let op = DocEdit.op(json, nodeId: node), let t = op["type"] as? String,
                  let c = (op["child"] as? NSNumber)?.intValue else { break }
            if t == "Translate" { if translate == nil { translate = node } }
            else if t == "Rotate" { guard Self.vec3(op["angles"]).map({ simd_length($0) < 1e-9 }) ?? true else { return nil } }
            else if t == "Scale" { guard Self.vec3(op["factor"]).map({ simd_length($0 - 1) < 1e-9 }) ?? true else { return nil } }
            else { break }
            node = c
        }
        guard let op = DocEdit.op(json, nodeId: node), (op["type"] as? String) == "Cube",
              let size = Self.vec3(op["size"]) else { return nil }
        guard let axis = (0..<3).first(where: { abs(n[$0]) > 0.999 }) else { return nil }
        let positive = n[axis] > 0
        if !positive && translate == nil {
            // Wrap the cube in a Translate so the -side face can move.
            return nil
        }
        let off = translate.flatMap { Self.vec3(DocEdit.op(json, nodeId: $0)?["offset"]) } ?? .zero
        return .cubeFace(sizeNode: node, axis: axis, positive: positive, translateNode: translate,
                         base: size[axis], baseOffset: off[axis])
    }

    /// A face that is the far cap of an Extrude (alone, or the tool of the
    /// top boolean) → edit that extrude's depth.
    private func extrudeCapKind(_ json: [String: Any], root: Int, face f: TopoFace) -> ManipKind? {
        let top = Self.unwrapTransforms(json, root)
        var cands = [top]
        if let op = DocEdit.op(json, nodeId: top), let r = (op["right"] as? NSNumber)?.intValue {
            cands.append(Self.unwrapTransforms(json, r))
        }
        for node in cands {
            guard let cap = extrudeCap(json, node),
                  let dir = Self.vec3(DocEdit.op(json, nodeId: node)?["direction"]) else { continue }
            let n = SIMD3<Double>(f.normalF), c = SIMD3<Double>(f.centerF)
            let dn = simd_normalize(dir)
            // Only when the root has no transform (so kernel == part coords).
            guard node == top || Self.unwrapTransforms(json, root) == top,
                  abs(simd_dot(n, dn)) > 0.999,
                  abs(simd_dot(c - SIMD3<Double>(cap.point), dn)) < 1e-3 else { continue }
            // Pulling the cap outward lengthens a boss; for a cut, the cap
            // faces back into the part — same math, sign from the normal.
            let sgn = simd_dot(n, dn) > 0 ? 1.0 : -1.0
            return .extrudeCap(node: node, dir: dir * sgn)
        }
        return nil
    }

    /// The end cap of an Extrude node: profile centroid + direction.
    func extrudeCap(_ json: [String: Any], _ node: Int) -> (point: SIMD3<Float>, dir: SIMD3<Float>)? {
        guard let op = DocEdit.op(json, nodeId: node), (op["type"] as? String) == "Extrude",
              let dir = Self.vec3(op["direction"]), simd_length(dir) > 1e-9,
              let sk = (op["sketch"] as? NSNumber)?.intValue,
              let s = DocEdit.op(json, nodeId: sk),
              let o = Self.vec3(s["origin"]), let xd = Self.vec3(s["x_dir"]), let yd = Self.vec3(s["y_dir"]),
              let segs = s["segments"] as? [[String: Any]], !segs.isEmpty else { return nil }
        var cx = 0.0, cy = 0.0
        for seg in segs {
            let p = seg["start"] as? [String: Any]
            cx += (p?["x"] as? NSNumber)?.doubleValue ?? 0
            cy += (p?["y"] as? NSNumber)?.doubleValue ?? 0
        }
        cx /= Double(segs.count); cy /= Double(segs.count)
        let p = o + xd * cx + yd * cy + dir
        return (SIMD3<Float>(p), SIMD3<Float>(simd_normalize(dir)))
    }

    /// Sketch2D op (origin filled in later) tracing a planar face's boundary
    /// loops in the face's own frame; the largest loop is the outline, the
    /// rest are holes.
    func faceProfile(part: Int, face f: TopoFace) -> [String: Any]? {
        guard let topo = partTopology(part) else { return nil }
        let n = SIMD3<Double>(f.normalF)
        let c = SIMD3<Double>(f.centerF)
        let (u, v) = Self.planeBasis(n)
        var segs: [(SIMD2<Double>, SIMD2<Double>)] = []
        for e in topo.edges where e.faces.contains(f.id) {
            let pts = e.segmentPoints.map { SIMD3<Double>($0) - c }
            guard pts.count >= 2 else { continue }
            for i in 0..<(pts.count - 1) {
                let a = SIMD2(simd_dot(pts[i], u), simd_dot(pts[i], v))
                let b = SIMD2(simd_dot(pts[i + 1], u), simd_dot(pts[i + 1], v))
                if simd_distance(a, b) > 1e-7 { segs.append((a, b)) }
            }
        }
        var loops = Self.chainLoops(segs)
        if loops.isEmpty, let sel = f.selector?.any as? [String: Any],
           let pt = Self.vec3(sel["point"]) {
            // A circular cap: its edge carries no polyline, but the selector
            // point sits on the rim. Trace the circle.
            let r = simd_length(pt - c)
            guard r > 1e-6 else { return nil }
            loops = [(0..<96).map { i in
                let a = Double(i) / 96 * 2 * .pi
                return SIMD2(r * cos(a), r * sin(a))
            }]
        }
        guard !loops.isEmpty else { return nil }
        let sorted = loops.sorted { abs(Self.area($0)) > abs(Self.area($1)) }
        func lines(_ loop: [SIMD2<Double>]) -> [[String: Any]] {
            (0..<loop.count).map { i in
                let a = loop[i], b = loop[(i + 1) % loop.count]
                return ["type": "Line", "start": ["x": a.x, "y": a.y], "end": ["x": b.x, "y": b.y]]
            }
        }
        var op: [String: Any] = ["type": "Sketch2D",
                                 "origin": ["x": c.x, "y": c.y, "z": c.z],
                                 "x_dir": ["x": u.x, "y": u.y, "z": u.z],
                                 "y_dir": ["x": v.x, "y": v.y, "z": v.z],
                                 "segments": lines(Self.ccw(sorted[0], true))]
        if sorted.count > 1 { op["holes"] = sorted.dropFirst().map { lines(Self.ccw($0, false)) } }
        return op
    }

    static func planeBasis(_ n: SIMD3<Double>) -> (SIMD3<Double>, SIMD3<Double>) {
        let ref: SIMD3<Double> = abs(n.z) < 0.9 ? [0, 0, 1] : [1, 0, 0]
        let u = simd_normalize(simd_cross(ref, n))
        return (u, simd_cross(n, u))
    }

    /// Join unordered segments into closed loops by endpoint matching.
    static func chainLoops(_ segs: [(SIMD2<Double>, SIMD2<Double>)]) -> [[SIMD2<Double>]] {
        let tol = 1e-4
        var remaining = segs
        var loops: [[SIMD2<Double>]] = []
        while let first = remaining.popLast() {
            var loop = [first.0, first.1]
            var closed = false
            while !closed {
                guard let tail = loop.last else { break }
                if let i = remaining.firstIndex(where: { simd_distance($0.0, tail) < tol }) {
                    loop.append(remaining.remove(at: i).1)
                } else if let i = remaining.firstIndex(where: { simd_distance($0.1, tail) < tol }) {
                    loop.append(remaining.remove(at: i).0)
                } else { break }
                if simd_distance(loop.last!, loop[0]) < tol { loop.removeLast(); closed = true }
            }
            if closed, loop.count >= 3 { loops.append(loop) }
        }
        return loops
    }

    static func area(_ p: [SIMD2<Double>]) -> Double {
        var a = 0.0
        for i in 0..<p.count { let q = p[(i + 1) % p.count]; a += p[i].x * q.y - q.x * p[i].y }
        return a / 2
    }

    static func ccw(_ p: [SIMD2<Double>], _ want: Bool) -> [SIMD2<Double>] {
        (area(p) > 0) == want ? p : p.reversed()
    }

    // MARK: snapping

    /// Offsets (along `normal`, from `through`) of every other planar face
    /// parallel to it — so a pulled face lands flush with existing geometry.
    private func planeTargets(normal n: SIMD3<Float>, through c: SIMD3<Float>,
                              excluding fid: UInt32, part: Int) -> [(value: Double, label: String)] {
        var out: [(Double, String)] = [(0, "start")]
        for p in 0..<partCount where isPartVisible(p) {
            guard let topo = partTopology(p) else { continue }
            for f in topo.faces where f.isPlanar && !(p == part && f.id == fid) {
                guard abs(simd_dot(simd_normalize(f.normalF), n)) > 0.999 else { continue }
                let off = Double(simd_dot(f.centerF - c, n))
                if abs(off) > 1e-4 { out.append((off, p == part ? "flush" : "flush with part")) }
            }
        }
        return out
    }

    private func snapToTargets(_ d: Double) -> (value: Double, label: String)? {
        let tol = Double(mmPerPixel() * 8)
        let best = manip.snapTargets.min { abs($0.value - d) < abs($1.value - d) }
        if let best, abs(best.value - d) < tol, best.label != "start" { return best }
        return nil
    }

    /// World mm under one screen pixel at the orbit target.
    func mmPerPixel() -> Float {
        let h = Float(max(viewSize.height, 300))
        return 2 * tan(Float(30) * .pi / 180) * distance / max(displayScale, 1e-6) / h
    }

    /// Round a dragged length to a step that's a few pixels on screen —
    /// 0.1, 0.5, 1, 5, 10 … mm. ⌘ (or `fine`) drags at a tenth of the step.
    func snapValue(_ v: Double, fine: Bool = false) -> Double {
        var step = niceStep(Double(mmPerPixel()) * 6)
        #if os(macOS)
        if NSEvent.modifierFlags.contains(.command) || fine { step /= 10 }
        #endif
        return (v / step).rounded() * step
    }

    func niceStep(_ raw: Double) -> Double {
        let ladder: [Double] = [0.01, 0.05, 0.1, 0.5, 1, 5, 10, 50, 100, 500, 1000]
        return ladder.first { $0 >= raw } ?? 1000
    }

    /// Rotations snap to 5° (⌘: 1°, free below).
    func snapAngle(_ deg: Double) -> Double {
        #if os(macOS)
        if NSEvent.modifierFlags.contains(.command) { return deg.rounded() }
        #endif
        return (deg / 5).rounded() * 5
    }

    /// Translate/plane drags: grid-round each component, then align the
    /// part's bounding box (min/center/max) with any other part's along the
    /// moved axes when within a few pixels.
    func snapMove(_ off: SIMD3<Float>, part: Int, handle: GizmoHandle) -> SIMD3<Float> {
        var o = off
        var axes: [Int] = []
        switch handle {
        case .axis(let a): axes = (0..<3).filter { abs(a[$0]) > 0.5 }
        case .plane(let a, let b): axes = (0..<3).filter { abs(a[$0]) > 0.5 || abs(b[$0]) > 0.5 }
        default: return off
        }
        let me = part < docPartMeshes.count ? (docPartMeshes[part].lo, docPartMeshes[part].hi) : nil
        let others = (0..<docPartMeshes.count).filter { $0 != part && isPartVisible($0) }
            .map { (docPartMeshes[$0].lo, docPartMeshes[$0].hi) }
        var snapped = false
        for ax in axes {
            if let me, let d = Self.alignDelta(off[ax], axis: ax, mine: me, others: others, tol: mmPerPixel() * 8) {
                o[ax] = off[ax] + d; snapped = true
            } else { o[ax] = Float(snapValue(Double(off[ax]))) }
        }
        manip.snapTargets = snapped ? [(0, "aligned")] : []
        return o
    }

    /// Smallest correction that lines a moved box's min/center/max up with
    /// any other box's min/center/max along `axis`, within `tol`.
    static func alignDelta(_ off: Float, axis ax: Int, mine: (SIMD3<Float>, SIMD3<Float>),
                           others: [(SIMD3<Float>, SIMD3<Float>)], tol: Float) -> Float? {
        let xs = [mine.0[ax], (mine.0[ax] + mine.1[ax]) / 2, mine.1[ax]].map { $0 + off }
        var best: Float?
        for m in others {
            for t in [m.0[ax], (m.0[ax] + m.1[ax]) / 2, m.1[ax]] {
                for x in xs where abs(t - x) < tol {
                    if best == nil || abs(t - x) < abs(best!) { best = t - x }
                }
            }
        }
        return best
    }

    // MARK: label

    private func displayedValue(_ d: Double) -> Double {
        guard let kind = manip.kind else { return d }
        switch kind {
        case let .cubeFace(_, _, _, _, base, _): return max(0.05, base + d)
        case let .extrudeCap(_, dir): return max(0.05, simd_length(dir) + d)
        case .boss: return d
        case let .scalar(_, _, base, lo): return max(lo, base + d)
        }
    }

    private func rawValue(fromDisplayed v: Double) -> Double {
        guard let kind = manip.kind else { return v }
        switch kind {
        case let .cubeFace(_, _, _, _, base, _): return v - base
        case let .extrudeCap(_, dir): return v - simd_length(dir)
        case .boss: return v
        case let .scalar(_, _, base, _): return v - base
        }
    }

    private func manipText(_ d: Double) -> String {
        let v = displayedValue(d)
        if case .boss = manip.kind { return (d >= 0 ? "+" : "−") + Self.mm(abs(v)) }
        return Self.mm(v)
    }

    static func mm(_ v: Double) -> String {
        let s = abs(v - v.rounded()) < 1e-6 ? String(format: "%.0f", v) : String(format: "%.2f", v)
            .replacingOccurrences(of: #"0+$"#, with: "", options: .regularExpression)
        return s + " mm"
    }

    func moveLabel(_ off: SIMD3<Float>) -> String {
        let comps = (0..<3).filter { abs(off[$0]) > 1e-4 }
        if comps.isEmpty { return "0 mm" }
        if comps.count == 1 { return ["X ", "Y ", "Z "][comps[0]] + Self.mm(Double(off[comps[0]])) }
        return comps.map { ["X ", "Y ", "Z "][$0] + Self.mm(Double(off[$0])) }.joined(separator: "  ")
    }

    func showManipLabel(_ text: String, at k: SIMD3<Float>, value: Double? = nil, snappedTo: String? = nil) {
        let snap = snappedTo ?? (manip.snapTargets.first?.label == "aligned" ? "aligned" : nil)
        manipLabel = ManipLabel(text: text, anchor: k, editable: value != nil, value: value,
                                settled: false, snappedTo: snap)
    }

    /// After a drag the label lingers (clickable) and then fades.
    func fadeManipLabel() {
        guard manipLabel != nil else { return }
        manipLabel?.settled = true
        manip.fadeToken += 1
        let token = manip.fadeToken
        Task { @MainActor [weak self] in
            try? await Task.sleep(nanoseconds: 4_000_000_000)
            guard let self, self.manip.fadeToken == token, self.manipLabel?.settled == true else { return }
            withAnimation(.easeOut(duration: 0.25)) { self.manipLabel = nil }
            self.manip.reapply = nil
        }
    }

    /// Keep a settled label up while the user is typing into it.
    func holdManipLabel() { manip.fadeToken += 1 }
}

// MARK: - On-canvas dimension

/// The live dimension pill: follows the dragged point, shows the value (and a
/// snap tick), and after the drag turns into a field for an exact value.
struct DimensionLabelOverlay: View {
    let model: EditorModel
    var project: (SIMD3<Float>) -> CGPoint?
    @State private var editing = false
    @State private var text = ""
    @FocusState private var focused: Bool

    var body: some View {
        GeometryReader { _ in
            if let l = model.manipLabel, let p = project(l.anchor) {
                pill(l)
                    .position(x: p.x + 46, y: p.y - 22)
                    .transition(.opacity)
            }
        }
        .allowsHitTesting(model.manipLabel?.settled == true)
        .onChange(of: model.manipLabel?.settled) { _, settled in
            if settled != true { editing = false }
        }
    }

    @ViewBuilder private func pill(_ l: ManipLabel) -> some View {
        HStack(spacing: 5) {
            if l.snappedTo != nil {
                Image(systemName: "scope").font(.system(size: 9, weight: .semibold))
                    .foregroundStyle(Color.accentColor)
            }
            if editing {
                TextField("", text: $text)
                    .textFieldStyle(.plain)
                    .font(.system(size: 12, weight: .medium, design: .monospaced))
                    .frame(width: 64)
                    .focused($focused)
                    .onSubmit {
                        if let v = Double(text.filter { "0123456789.-".contains($0) }) {
                            model.commitManipValue(v)
                        }
                        editing = false
                    }
                    .onExitCommand { editing = false; model.fadeManipLabel() }
            } else {
                Text(l.text)
                    .font(.system(size: 12, weight: .medium, design: .monospaced))
                    .monospacedDigit()
            }
        }
        .padding(.horizontal, 8)
        .padding(.vertical, 4)
        .background(.ultraThinMaterial, in: Capsule())
        .overlay(Capsule().strokeBorder(Color.primary.opacity(l.settled && l.editable ? 0.25 : 0.1),
                                        lineWidth: 0.5))
        .shadow(color: .black.opacity(0.18), radius: 4, y: 1)
        .onTapGesture {
            guard l.settled, l.editable else { return }
            model.holdManipLabel()
            text = l.value.map { String(format: "%g", $0) } ?? ""
            editing = true
            focused = true
        }
        .help(l.settled && l.editable ? "Click to type an exact value" : "")
    }
}

// MARK: - Scripted demo / screenshots (VCAD_SNAPSHOT_DIR)

/// `VCAD_SNAPSHOT_DIR=/path open vcad.app --args --vcad-open cube.vcad`
/// walks the direct-manipulation states on the live viewport and writes a
/// PNG per state plus numbered frames of a press-pull drag — the PR's
/// screenshots and recording, reproducible without screen-recording rights.
@MainActor
enum DirectManipDemo {
    static func runIfRequested(_ m: EditorModel) async {
        guard let dir = ProcessInfo.processInfo.environment["VCAD_SNAPSHOT_DIR"] else { return }
        let url = URL(fileURLWithPath: dir)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        func wait(_ s: Double) async { try? await Task.sleep(nanoseconds: UInt64(s * 1e9)) }
        for _ in 0..<120 where m.docPartMeshes.isEmpty { await wait(0.25) }
        await wait(2)
        let ctl = ReleaseWindowController.shared
        func snap(_ name: String) async {
            ctl.rebuildGizmo(model: m)
            RenderGate.wake()
            await wait(0.6)
            guard let ar = ctl.arView else { return }
            await withCheckedContinuation { (k: CheckedContinuation<Void, Never>) in
                ar.snapshot(saveToHDR: false) { img in
                    if let img, let tiff = img.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff),
                       let png = rep.representation(using: .png, properties: [:]) {
                        try? png.write(to: url.appendingPathComponent(name + ".png"))
                    }
                    k.resume()
                }
            }
        }
        await snap("1-idle")
        m.selectParts([0]); await wait(0.5)
        await snap("2-gizmo")
        m.hoveredGizmoHandle = "gizmoX"; await snap("3-hover-x")
        m.hoveredGizmoHandle = "rotZ"; await snap("4-hover-rotz")
        m.hoveredGizmoHandle = nil
        guard let topo = m.partTopology(0),
              let f = topo.faces.filter({ $0.isPlanar && $0.normalF.z > 0.99 }).max(by: { $0.centerF.z < $1.centerF.z })
        else { return }
        m.picking.select(.face(f.id, part: 0), additive: false)
        await snap("5-pull-arrow")
        let c = f.centerF
        func ray(_ dz: Float) -> (o: SIMD3<Float>, d: SIMD3<Float>) { (SIMD3(c.x + 400, c.y, c.z + dz), SIMD3(-1, 0, 0)) }
        m.beginGizmoDrag(handle: "pull", ray: ray(0))
        for (i, dz) in stride(from: Float(0), through: 15, by: 1).enumerated() {
            m.gizmoDragTo(ray: ray(dz))
            ctl.refreshGeometry(model: m, collisions: false)
            await snap(String(format: "frame-%03d", i))
        }
        await snap("6-pulling")
        m.endGizmoDrag()
        ctl.refreshGeometry(model: m, collisions: true)
        await snap("7-settled-label")
        m.selectParts([0]); m.picking.clear()
        await snap("8-after")
        FileHandle.standardError.write(Data("[VCAD_SNAPSHOT] done → \(dir)\n".utf8))
    }
}

// MARK: - Big-assembly open screenshots (VCAD_ASSEMBLY_SNAPSHOT_DIR)

/// `VCAD_ASSEMBLY_SNAPSHOT_DIR=/path VCAD_OPEN=big.step vcad` snaps the
/// viewport at fixed times while a large assembly opens
/// (`VCAD_ASSEMBLY_SNAPSHOT_AT`, seconds, default `20,45,80,200`) — parts
/// streaming in, the preview LOD, the full-quality refine — with the real
/// seconds since launch in each file name. Reproducible without screen-recording rights.
@MainActor
enum AssemblyOpenDemo {
    static func runIfRequested(_ m: EditorModel) async {
        guard let dir = ProcessInfo.processInfo.environment["VCAD_ASSEMBLY_SNAPSHOT_DIR"] else { return }
        let url = URL(fileURLWithPath: dir)
        try? FileManager.default.createDirectory(at: url, withIntermediateDirectories: true)
        let t0 = Date()
        func wait(_ s: Double) async { try? await Task.sleep(nanoseconds: UInt64(s * 1e9)) }
        func snap(_ name: String) async {
            let ctl = ReleaseWindowController.shared
            RenderGate.wake()
            await wait(0.5)
            guard let ar = ctl.arView else { return }
            let label = String(format: "%@-%.1fs", name, Date().timeIntervalSince(t0))
            await withCheckedContinuation { (k: CheckedContinuation<Void, Never>) in
                ar.snapshot(saveToHDR: false) { img in
                    if let img, let tiff = img.tiffRepresentation, let rep = NSBitmapImageRep(data: tiff),
                       let png = rep.representation(using: .png, properties: [:]) {
                        try? png.write(to: url.appendingPathComponent(label + ".png"))
                    }
                    k.resume()
                }
            }
            FileHandle.standardError.write(Data("[ASSEMBLY_SNAPSHOT] \(label) solved=\(m.solveProgress.done)/\(m.solveProgress.total)\n".utf8))
        }
        // Timed, so a busy main thread can't stall the trigger: the file
        // names carry the real time each frame was taken.
        let at = (ProcessInfo.processInfo.environment["VCAD_ASSEMBLY_SNAPSHOT_AT"] ?? "20,45,80,200")
            .split(separator: ",").compactMap { Double($0) }
        for (i, t) in at.enumerated() {
            let left = t - Date().timeIntervalSince(t0)
            if left > 0 { await wait(left) }
            await snap("\(i + 1)")
        }
        FileHandle.standardError.write(Data("[ASSEMBLY_SNAPSHOT] done → \(dir)\n".utf8))
    }
}

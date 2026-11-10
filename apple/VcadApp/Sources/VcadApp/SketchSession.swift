import Foundation
import simd
import CVcadFFI
import Observation

// Constraint-driven sketching on the vcad-kernel-constraints solver
// (vcad_sketch_* in crates/vcad-ffi/src/sketch.rs).
//
// The session owns one VcadSketch on a plane (a principal plane or a picked
// face). Tools add points / lines / arcs / circles; consecutive lines share
// point handles, so connectivity needs no coincident constraints. Every edit
// re-solves, and the session republishes solved positions, DOF and the
// under/fully/over-constrained state for the UI.

/// What Finish does with a closed sketch profile.
enum SketchFinish: String, CaseIterable, Identifiable {
    case extrude, cut, extrudeToFace, revolve, sweep, loft
    var id: String { rawValue }
    var label: String {
        switch self {
        case .extrude: "Extrude"
        case .cut: "Cut Extrude"
        case .extrudeToFace: "Extrude to Face"
        case .revolve: "Revolve"
        case .sweep: "Sweep"
        case .loft: "Loft"
        }
    }
    var symbol: String {
        switch self {
        case .extrude: "arrow.up.to.line"
        case .cut: "scissors"
        case .extrudeToFace: "arrow.up.to.line.compact"
        case .revolve: "arrow.triangle.2.circlepath"
        case .sweep: "point.topleft.down.to.point.bottomright.curvepath"
        case .loft: "square.stack.3d.up"
        }
    }
}

extension Comparable {
    func clamped(to r: ClosedRange<Self>) -> Self { min(max(self, r.lowerBound), r.upperBound) }
}

/// What a sketch click does.
enum SketchToolKind: String, CaseIterable, Identifiable {
    case select, line, rectangle, circle, arc, spline, dimension
    var id: String { rawValue }
    var label: String {
        switch self {
        case .select: "Select"
        case .line: "Line"
        case .rectangle: "Rectangle"
        case .circle: "Circle"
        case .arc: "Arc"
        case .spline: "Spline"
        case .dimension: "Dimension"
        }
    }
    var symbol: String {
        switch self {
        case .select: "cursorarrow"
        case .line: "line.diagonal"
        case .rectangle: "rectangle"
        case .circle: "circle"
        case .arc: "circle.lefthalf.striped.horizontal"
        case .spline: "point.bottomleft.forward.to.point.topright.scurvepath"
        case .dimension: "ruler"
        }
    }
    var hint: String {
        switch self {
        case .select: "Click entities to select · ⌘-click to add"
        case .line: "Click points · click the first point to close"
        case .rectangle: "Click two opposite corners"
        case .circle: "Click the center, then the radius"
        case .arc: "Click the center, the start, then the end"
        case .spline: "Click control points · Return to finish"
        case .dimension: "Click a line for its length or a circle/arc for its radius"
        }
    }
}

/// A geometric constraint the user can add between selected entities.
enum SketchConstraintKind: UInt32, CaseIterable, Identifiable {
    case coincident = 0, horizontal, vertical, parallel, perpendicular, tangent,
         distance, length, radius, angle, equalLength, equalRadius, fixed,
         pointOnLine, concentric, pointOnCircle, midpoint,
         horizontalDistance, verticalDistance, diameter, pointLineDistance, symmetric
    var id: UInt32 { rawValue }
    var label: String {
        switch self {
        case .coincident: "Coincident"
        case .horizontal: "Horizontal"
        case .vertical: "Vertical"
        case .parallel: "Parallel"
        case .perpendicular: "Perpendicular"
        case .tangent: "Tangent"
        case .distance: "Distance"
        case .length: "Length"
        case .radius: "Radius"
        case .angle: "Angle"
        case .equalLength: "Equal Length"
        case .equalRadius: "Equal Radius"
        case .fixed: "Fixed"
        case .pointOnLine: "Point on Line"
        case .concentric: "Concentric"
        case .pointOnCircle: "Point on Circle"
        case .midpoint: "Midpoint"
        case .horizontalDistance: "Horizontal Distance"
        case .verticalDistance: "Vertical Distance"
        case .diameter: "Diameter"
        case .pointLineDistance: "Point–Line Distance"
        case .symmetric: "Symmetric"
        }
    }
    /// Short glyph drawn in the constraint list and next to the geometry.
    var glyph: String {
        switch self {
        case .coincident: "●"
        case .horizontal: "H"
        case .vertical: "V"
        case .parallel: "∥"
        case .perpendicular: "⊥"
        case .tangent: "T"
        case .distance, .horizontalDistance, .verticalDistance, .pointLineDistance: "↔"
        case .length: "L"
        case .radius: "R"
        case .diameter: "⌀"
        case .angle: "∠"
        case .equalLength, .equalRadius: "="
        case .fixed: "⚓"
        case .pointOnLine, .pointOnCircle: "∈"
        case .concentric: "◎"
        case .midpoint: "M"
        case .symmetric: "⇋"
        }
    }
    /// Driving dimensions carry an editable value.
    var isDimension: Bool {
        switch self {
        case .distance, .length, .radius, .angle, .horizontalDistance, .verticalDistance,
             .diameter, .pointLineDistance: true
        default: false
        }
    }
}

/// One sketch entity, mirrored from the solver.
struct SketchEntity: Identifiable, Hashable {
    enum Kind: Hashable { case point, line, arc, circle }
    var handle: UInt32
    var kind: Kind
    /// Point handles this entity references (line: start,end; arc: start,end,center; circle: center).
    var points: [UInt32] = []
    /// Construction geometry (spline control points) never enters the profile.
    var construction = false
    var id: UInt32 { handle }
}

/// One constraint, mirrored from the solver (index = solver constraint index).
struct SketchConstraintRow: Identifiable, Hashable {
    var index: Int
    var kind: SketchConstraintKind
    var entities: [UInt32]
    var value: Double
    var id: Int { index }
}

/// A sketch plane in kernel space.
struct SketchFrame: Equatable {
    var origin: SIMD3<Double>
    var xDir: SIMD3<Double>
    var yDir: SIMD3<Double>
    var normal: SIMD3<Double> { simd_normalize(simd_cross(xDir, yDir)) }

    static func principal(_ p: SketchPlane) -> SketchFrame {
        .init(origin: .zero, xDir: SIMD3(p.xDir.0, p.xDir.1, p.xDir.2), yDir: SIMD3(p.yDir.0, p.yDir.1, p.yDir.2))
    }
    /// A frame on a planar face: origin at its center, normal outward, x
    /// along the face's most horizontal in-plane direction.
    static func onFace(center: SIMD3<Double>, normal n0: SIMD3<Double>) -> SketchFrame {
        let n = simd_normalize(n0)
        let ref: SIMD3<Double> = abs(n.z) < 0.9 ? [0, 0, 1] : [0, 1, 0]
        let x = simd_normalize(simd_cross(ref, n))
        let y = simd_cross(n, x)
        return .init(origin: center, xDir: x, yDir: y)
    }

    func world(_ p: SIMD2<Double>) -> SIMD3<Double> { origin + p.x * xDir + p.y * yDir }
    func local(_ w: SIMD3<Double>) -> SIMD2<Double> {
        let d = w - origin
        return SIMD2(simd_dot(d, xDir), simd_dot(d, yDir))
    }
    /// Ray → 2D plane coords.
    func intersect(origin o: SIMD3<Double>, dir d: SIMD3<Double>) -> SIMD2<Double>? {
        let n = normal
        let denom = simd_dot(d, n)
        guard abs(denom) > 1e-9 else { return nil }
        let t = simd_dot(origin - o, n) / denom
        guard t > 0 else { return nil }
        return local(o + t * d)
    }
    var json: [String: Any] {
        func v(_ a: SIMD3<Double>) -> [String: Double] { ["x": a.x, "y": a.y, "z": a.z] }
        return ["origin": v(origin), "x_dir": v(xDir), "y_dir": v(yDir)]
    }
}

@MainActor
@Observable
final class SketchSession {
    let frame: SketchFrame
    @ObservationIgnored nonisolated(unsafe) private let raw: OpaquePointer

    private(set) var entities: [SketchEntity] = []
    private(set) var constraints: [SketchConstraintRow] = []
    /// Solved 2D point positions by handle.
    private(set) var positions: [UInt32: SIMD2<Double>] = [:]
    private(set) var radii: [UInt32: Double] = [:]
    private(set) var dof: Int = 0
    private(set) var state: UInt8 = UInt8(VCAD_SKETCH_UNDER)
    private(set) var converged = true
    private(set) var lastError: String?

    var tool: SketchToolKind = .line { didSet { pending = [] } }
    /// Selected entity handles (select tool), in click order.
    var selection: [UInt32] = []
    /// Clicks the active tool has collected so far (plane coords).
    private(set) var pending: [SIMD2<Double>] = []
    /// Point handles of the line tool's open chain.
    private var chain: [UInt32] = []
    /// Live cursor (plane coords).
    var cursor: SIMD2<Double>?
    /// Bumped on every change: the viewport redraws the ink.
    private(set) var revision = 0
    /// Snap radius in plane mm.
    var snap: Double = 2.5

    init?(frame: SketchFrame) {
        var o = [frame.origin.x, frame.origin.y, frame.origin.z]
        var x = [frame.xDir.x, frame.xDir.y, frame.xDir.z]
        var y = [frame.yDir.x, frame.yDir.y, frame.yDir.z]
        guard let s = vcad_sketch_new_on_plane(&o, &x, &y) else { return nil }
        self.raw = s
        self.frame = frame
    }
    deinit { vcad_sketch_free(raw) }

    // MARK: state readout

    var stateLabel: String {
        if !converged { return "Not solved" }
        switch Int32(state) {
        case VCAD_SKETCH_FULL: return "Fully constrained"
        case VCAD_SKETCH_OVER: return "Over-constrained"
        default: return dof == 1 ? "1 degree of freedom" : "\(dof) degrees of freedom"
        }
    }
    var isFullyConstrained: Bool { Int32(state) == VCAD_SKETCH_FULL && converged }
    var isOverConstrained: Bool { Int32(state) == VCAD_SKETCH_OVER }
    var isEmpty: Bool { entities.allSatisfy { $0.kind == .point } }

    func point(_ h: UInt32) -> SIMD2<Double>? { positions[h] }
    func entity(_ h: UInt32) -> SketchEntity? { entities.first { $0.handle == h } }

    // MARK: primitives

    @discardableResult
    func addPoint(_ p: SIMD2<Double>) -> UInt32 {
        let h = vcad_sketch_add_point(raw, p.x, p.y)
        if h != 0 { entities.append(.init(handle: h, kind: .point)); positions[h] = p }
        return h
    }
    @discardableResult
    func addLine(_ a: UInt32, _ b: UInt32) -> UInt32 {
        let h = vcad_sketch_add_line(raw, a, b)
        if h != 0 { entities.append(.init(handle: h, kind: .line, points: [a, b])) }
        return h
    }
    @discardableResult
    func addArc(start: UInt32, end: UInt32, center: UInt32, ccw: Bool = true) -> UInt32 {
        let h = vcad_sketch_add_arc(raw, start, end, center, ccw)
        if h != 0 { entities.append(.init(handle: h, kind: .arc, points: [start, end, center])) }
        return h
    }
    @discardableResult
    func addCircle(center: UInt32, radius: Double) -> UInt32 {
        let h = vcad_sketch_add_circle(raw, center, radius)
        if h != 0 { entities.append(.init(handle: h, kind: .circle, points: [center])); radii[h] = radius }
        return h
    }

    /// Add a constraint. Entity refs use VCAD_SKETCH_REF_* selectors.
    @discardableResult
    func constrain(_ kind: SketchConstraintKind,
                   _ e0: UInt32, _ r0: UInt32 = VCAD_SKETCH_REF_SELF,
                   _ e1: UInt32 = 0, _ r1: UInt32 = VCAD_SKETCH_REF_SELF,
                   _ e2: UInt32 = 0, _ r2: UInt32 = VCAD_SKETCH_REF_SELF,
                   value: Double = 0, value2: Double = 0, solveNow: Bool = true) -> Bool {
        var c = VcadSketchConstraint(kind: kind.rawValue, e0: e0, r0: r0, e1: e1, r1: r1,
                                     e2: e2, r2: r2, value: value, value2: value2)
        let idx = vcad_sketch_add_constraint(raw, &c)
        guard idx >= 0 else { lastError = SimError.pending() ?? "Constraint refused"; return false }
        constraints.append(.init(index: Int(idx), kind: kind,
                                 entities: [e0, e1, e2].filter { $0 != 0 }, value: value))
        if solveNow { solve() }
        return true
    }

    /// Edit a driving dimension in place and re-solve.
    func setDimension(_ row: SketchConstraintRow, value: Double) {
        guard vcad_sketch_set_dimension(raw, UInt32(row.index), value, 0) else {
            lastError = SimError.pending() ?? "Dimension refused"; return
        }
        if let i = constraints.firstIndex(of: row) { constraints[i].value = value }
        solve()
    }

    /// Remove the most recent constraint (the solver's stack is LIFO).
    func popConstraint() {
        guard !constraints.isEmpty, vcad_sketch_pop_constraint(raw) else { return }
        constraints.removeLast()
        solve()
    }

    /// Re-solve and refresh every solved position.
    func solve() {
        let r = vcad_sketch_solve(raw)
        converged = r.converged != 0 || r.status == 3 || r.status == 4
        state = r.state
        dof = Int(r.dof)
        refresh()
    }

    private func refresh() {
        var xy = [0.0, 0.0]
        for e in entities {
            switch e.kind {
            case .point:
                if vcad_sketch_point(raw, e.handle, VCAD_SKETCH_REF_SELF, &xy) { positions[e.handle] = SIMD2(xy[0], xy[1]) }
            case .circle, .arc:
                let r = vcad_sketch_radius(raw, e.handle)
                if r.isFinite, r > 0 { radii[e.handle] = r }
            case .line:
                break
            }
        }
        revision &+= 1
    }

    // MARK: tools

    /// Nearest existing point within snap.
    func snapPoint(_ p: SIMD2<Double>) -> UInt32? {
        entities.filter { $0.kind == .point }
            .compactMap { e in positions[e.handle].map { (e.handle, simd_distance($0, p)) } }
            .filter { $0.1 < snap }.min { $0.1 < $1.1 }?.0
    }

    /// Point handle at `p`: an existing one within snap, else a new point.
    private func pointAt(_ p: SIMD2<Double>) -> UInt32 { snapPoint(p) ?? addPoint(p) }

    /// Constrain a fresh line horizontal/vertical when it is within ~3° of an axis.
    private func autoAxis(_ line: UInt32, _ a: SIMD2<Double>, _ b: SIMD2<Double>) {
        let d = b - a
        guard simd_length(d) > 1e-6 else { return }
        let ang = abs(atan2(d.y, d.x))
        if ang < 0.05 || abs(ang - .pi) < 0.05 { constrain(.horizontal, line, solveNow: false) }
        else if abs(ang - .pi / 2) < 0.05 { constrain(.vertical, line, solveNow: false) }
    }

    /// A click on the plane.
    func tap(_ p: SIMD2<Double>, additive: Bool = false) {
        switch tool {
        case .select:
            let hit = nearestEntity(p)
            if additive { if let hit { if let i = selection.firstIndex(of: hit) { selection.remove(at: i) } else { selection.append(hit) } } }
            else { selection = hit.map { [$0] } ?? [] }
        case .line:
            let h = pointAt(p)
            if let last = chain.last, last != h {
                let line = addLine(last, h)
                if let a = positions[last], let b = positions[h] { autoAxis(line, a, b) }
                if h == chain.first { chain = [] }      // closed the loop
                else { chain.append(h) }
            } else if chain.isEmpty {
                chain = [h]
            }
        case .rectangle:
            if let a = pending.first {
                rectangle(a, p); pending = []
            } else { pending = [p] }
        case .circle:
            if let c = pending.first {
                let ch = pointAt(c)
                addCircle(center: ch, radius: max(simd_distance(c, p), 0.1)); pending = []
            } else { pending = [p] }
        case .arc:
            pending.append(p)
            if pending.count == 3 {
                let c = pending[0], s = pending[1]
                let r = simd_distance(c, s)
                // Project the end onto the circle so the arc is consistent.
                let dir = pending[2] - c
                let e = c + (simd_length(dir) > 1e-9 ? simd_normalize(dir) * r : SIMD2(r, 0))
                let ch = pointAt(c), sh = pointAt(s), eh = pointAt(e)
                addArc(start: sh, end: eh, center: ch, ccw: true)
                pending = []
            }
        case .spline:
            pending.append(p)
        case .dimension:
            guard let h = nearestEntity(p), let e = entity(h) else { break }
            switch e.kind {
            case .line:
                if let a = positions[e.points[0]], let b = positions[e.points[1]] {
                    constrain(.length, h, value: simd_distance(a, b), solveNow: false)
                }
            case .circle, .arc:
                constrain(.radius, h, value: radii[h] ?? 1, solveNow: false)
            case .point:
                if let q = positions[h] { constrain(.fixed, h, value: q.x, value2: q.y, solveNow: false) }
            }
        }
        solve()
    }

    /// Finish a multi-click tool (spline: sample the control polygon).
    func commitPending() {
        if tool == .spline, pending.count >= 2 {
            let pts = Self.catmullRom(pending, samplesPerSpan: 8)
            // Interior samples are closer than the snap radius: only the
            // endpoints may join existing geometry.
            var prev = pointAt(pts[0])
            for (i, q) in pts.enumerated().dropFirst() {
                let h = i == pts.count - 1 ? pointAt(q) : addPoint(q)
                if h != prev { addLine(prev, h) }
                prev = h
            }
        }
        pending = []
        chain = []
        solve()
    }

    /// Escape: drop in-progress clicks.
    func cancelPending() { pending = []; chain = []; revision &+= 1 }

    private func rectangle(_ a: SIMD2<Double>, _ b: SIMD2<Double>) {
        let c = [a, SIMD2(b.x, a.y), b, SIMD2(a.x, b.y)].map { pointAt($0) }
        let l = (0..<4).map { addLine(c[$0], c[($0 + 1) % 4]) }
        constrain(.horizontal, l[0], solveNow: false)
        constrain(.vertical, l[1], solveNow: false)
        constrain(.horizontal, l[2], solveNow: false)
        constrain(.vertical, l[3], solveNow: false)
    }

    /// Apply a constraint to the current selection, inferring refs from kinds.
    @discardableResult
    func constrainSelection(_ kind: SketchConstraintKind) -> Bool {
        let sel = selection.compactMap { entity($0) }
        defer { solve() }
        switch (kind, sel.count) {
        case (.horizontal, 1), (.vertical, 1):
            guard sel[0].kind == .line else { return false }
            return constrain(kind, sel[0].handle, solveNow: false)
        case (.parallel, 2), (.perpendicular, 2), (.equalLength, 2):
            guard sel.allSatisfy({ $0.kind == .line }) else { return false }
            return constrain(kind, sel[0].handle, VCAD_SKETCH_REF_SELF, sel[1].handle, solveNow: false)
        case (.equalRadius, 2), (.concentric, 2):
            guard sel.allSatisfy({ $0.kind == .circle || $0.kind == .arc }) else { return false }
            return constrain(kind, sel[0].handle, VCAD_SKETCH_REF_SELF, sel[1].handle, solveNow: false)
        case (.tangent, 2):
            return constrain(kind, sel[0].handle, VCAD_SKETCH_REF_SELF, sel[1].handle, solveNow: false)
        case (.coincident, 2):
            guard sel.allSatisfy({ $0.kind == .point }) else { return false }
            return constrain(kind, sel[0].handle, VCAD_SKETCH_REF_SELF, sel[1].handle, solveNow: false)
        case (.pointOnLine, 2):
            guard let pt = sel.first(where: { $0.kind == .point }), let ln = sel.first(where: { $0.kind == .line })
            else { return false }
            return constrain(kind, pt.handle, VCAD_SKETCH_REF_SELF, ln.handle, solveNow: false)
        case (.midpoint, 2):
            guard let pt = sel.first(where: { $0.kind == .point }), let ln = sel.first(where: { $0.kind == .line })
            else { return false }
            return constrain(kind, pt.handle, VCAD_SKETCH_REF_SELF, ln.handle, solveNow: false)
        case (.fixed, 1):
            guard sel[0].kind == .point, let q = positions[sel[0].handle] else { return false }
            return constrain(kind, sel[0].handle, value: q.x, value2: q.y, solveNow: false)
        case (.distance, 2):
            guard sel.allSatisfy({ $0.kind == .point }),
                  let a = positions[sel[0].handle], let b = positions[sel[1].handle] else { return false }
            return constrain(kind, sel[0].handle, VCAD_SKETCH_REF_SELF, sel[1].handle,
                             value: simd_distance(a, b), solveNow: false)
        case (.angle, 2):
            guard sel.allSatisfy({ $0.kind == .line }),
                  let a0 = positions[sel[0].points[0]], let a1 = positions[sel[0].points[1]],
                  let b0 = positions[sel[1].points[0]], let b1 = positions[sel[1].points[1]] else { return false }
            let da = simd_normalize(a1 - a0), db = simd_normalize(b1 - b0)
            let deg = acos(max(-1, min(1, simd_dot(da, db)))) * 180 / .pi
            return constrain(kind, sel[0].handle, VCAD_SKETCH_REF_SELF, sel[1].handle, value: deg, solveNow: false)
        default:
            lastError = "\(kind.label) needs a different selection"
            return false
        }
    }

    /// Nearest entity to a plane point (lines by segment distance, circles by ring).
    func nearestEntity(_ p: SIMD2<Double>) -> UInt32? {
        var best: (UInt32, Double)?
        func consider(_ h: UInt32, _ d: Double) { if d < snap * 1.5, d < (best?.1 ?? .infinity) { best = (h, d) } }
        for e in entities {
            switch e.kind {
            case .point:
                if let q = positions[e.handle] { consider(e.handle, simd_distance(q, p) * 0.8) }
            case .line:
                if let a = positions[e.points[0]], let b = positions[e.points[1]] { consider(e.handle, Self.segDist(p, a, b)) }
            case .circle, .arc:
                if let c = positions[e.points.last ?? 0], let r = radii[e.handle] {
                    consider(e.handle, abs(simd_distance(c, p) - r))
                }
            }
        }
        return best?.0
    }

    // MARK: offset

    /// Offset every closed loop outward (positive) or inward by `d`, adding
    /// the result as new geometry: lines offset with mitred corners, circles
    /// and arcs by radius.
    func offsetProfile(by d: Double) {
        guard let prof = profileSegments() else { return }
        let lines = prof.compactMap { s -> (SIMD2<Double>, SIMD2<Double>)? in
            if case .line(let a, let b) = s { return (a, b) }; return nil
        }
        // Circles (two half-arcs share a center) → one offset circle.
        for e in entities where e.kind == .circle {
            if let c = positions[e.points[0]], let r = radii[e.handle], r + d > 0 {
                addCircle(center: addPoint(c), radius: r + d)
            }
        }
        guard lines.count >= 3, lines.count == prof.count else { solve(); return }
        // Polygon offset with orientation-aware normals.
        let pts = lines.map(\.0)
        let area = zip(pts, pts.dropFirst() + [pts[0]]).reduce(0.0) { $0 + ($1.0.x * $1.1.y - $1.1.x * $1.0.y) }
        let sign: Double = area > 0 ? -1 : 1        // outward for CCW is the right-hand normal
        let offLines = lines.map { a, b -> (SIMD2<Double>, SIMD2<Double>) in
            let t = simd_normalize(b - a)
            let n = SIMD2(t.y, -t.x) * sign * d
            return (a - n, b - n)
        }
        var corners: [SIMD2<Double>] = []
        for i in 0..<offLines.count {
            let p = offLines[(i + offLines.count - 1) % offLines.count], q = offLines[i]
            corners.append(Self.intersect(p.0, p.1, q.0, q.1) ?? q.0)
        }
        let hs = corners.map { addPoint($0) }
        for i in 0..<hs.count { addLine(hs[i], hs[(i + 1) % hs.count]) }
        solve()
    }

    // MARK: profile

    enum Seg { case line(SIMD2<Double>, SIMD2<Double>); case arc(SIMD2<Double>, SIMD2<Double>, SIMD2<Double>, Bool) }

    /// The closed profile as JSON `{origin,x_dir,y_dir,segments}` — exactly
    /// the `profile` object `vcad_feature_append` takes. Nil when open.
    func profileJSON() -> [String: Any]? {
        guard let r = vcad_sketch_profile_json(raw) else {
            lastError = SimError.pending() ?? "Profile is not closed"; return nil
        }
        defer { vcad_string_free(r) }
        return try? JSONSerialization.jsonObject(with: Data(String(cString: r).utf8)) as? [String: Any]
    }

    func profileSegments() -> [Seg]? {
        guard let segs = profileJSON()?["segments"] as? [[String: Any]] else { return nil }
        func v(_ a: Any?) -> SIMD2<Double> {
            let d = a as? [String: Any]
            return SIMD2((d?["x"] as? NSNumber)?.doubleValue ?? 0, (d?["y"] as? NSNumber)?.doubleValue ?? 0)
        }
        return segs.map { s in
            s["type"] as? String == "Arc"
                ? .arc(v(s["start"]), v(s["end"]), v(s["center"]), (s["ccw"] as? Bool) ?? true)
                : .line(v(s["start"]), v(s["end"]))
        }
    }

    var canFinish: Bool { !isEmpty && vcad_sketch_profile_json(raw).map { vcad_string_free($0); return true } ?? false }

    // MARK: drawing

    /// Ink as plane-space segment pairs: committed geometry, then the live preview.
    func ink(circleSegments n: Int = 64) -> (geometry: [SIMD2<Double>], preview: [SIMD2<Double>], points: [SIMD2<Double>]) {
        var g: [SIMD2<Double>] = []
        for e in entities {
            switch e.kind {
            case .point: break
            case .line:
                if let a = positions[e.points[0]], let b = positions[e.points[1]] { g += [a, b] }
            case .circle:
                if let c = positions[e.points[0]], let r = radii[e.handle] { g += Self.ring(c, r, 0, 2 * .pi, n) }
            case .arc:
                if let s = positions[e.points[0]], let en = positions[e.points[1]], let c = positions[e.points[2]] {
                    let r = simd_distance(c, s)
                    let a0 = atan2(s.y - c.y, s.x - c.x)
                    var a1 = atan2(en.y - c.y, en.x - c.x)
                    if a1 <= a0 { a1 += 2 * .pi }
                    g += Self.ring(c, r, a0, a1, max(8, Int(Double(n) * (a1 - a0) / (2 * .pi))))
                }
            }
        }
        var pv: [SIMD2<Double>] = []
        if let c = cursor {
            switch tool {
            case .line:
                if let last = chain.last, let a = positions[last] { pv += [a, c] }
            case .rectangle:
                if let a = pending.first {
                    let k = [a, SIMD2(c.x, a.y), c, SIMD2(a.x, c.y)]
                    for i in 0..<4 { pv += [k[i], k[(i + 1) % 4]] }
                }
            case .circle:
                if let a = pending.first { pv += Self.ring(a, simd_distance(a, c), 0, 2 * .pi, n) }
            case .arc:
                if pending.count == 1 { pv += [pending[0], c] }
                if pending.count == 2 {
                    let ctr = pending[0], r = simd_distance(ctr, pending[1])
                    let a0 = atan2(pending[1].y - ctr.y, pending[1].x - ctr.x)
                    var a1 = atan2(c.y - ctr.y, c.x - ctr.x)
                    if a1 <= a0 { a1 += 2 * .pi }
                    pv += Self.ring(ctr, r, a0, a1, 48)
                }
            case .spline:
                let pts = Self.catmullRom(pending + [c], samplesPerSpan: 8)
                for i in 0..<max(0, pts.count - 1) { pv += [pts[i], pts[i + 1]] }
            case .select, .dimension:
                break
            }
        }
        let pts = entities.filter { $0.kind == .point }.compactMap { positions[$0.handle] } + pending
        return (g, pv, pts)
    }

    /// Anchor point (plane coords) for a constraint's glyph.
    func glyphAnchor(_ c: SketchConstraintRow) -> SIMD2<Double>? {
        guard let h = c.entities.first, let e = entity(h) else { return nil }
        switch e.kind {
        case .point: return positions[h].map { $0 + SIMD2(1.5, 1.5) }
        case .line:
            guard let a = positions[e.points[0]], let b = positions[e.points[1]] else { return nil }
            let t = b - a, l = simd_length(t)
            let n = l > 1e-9 ? SIMD2(-t.y, t.x) / l : .zero
            return (a + b) / 2 + n * 2.5
        case .circle, .arc:
            guard let c0 = positions[e.points.last ?? 0], let r = radii[h] else { return nil }
            return c0 + SIMD2(r * 0.7, r * 0.7)
        }
    }

    // MARK: geometry helpers

    static func ring(_ c: SIMD2<Double>, _ r: Double, _ a0: Double, _ a1: Double, _ n: Int) -> [SIMD2<Double>] {
        var out: [SIMD2<Double>] = []
        let steps = max(n, 2)
        for i in 0..<steps {
            let t0 = a0 + (a1 - a0) * Double(i) / Double(steps)
            let t1 = a0 + (a1 - a0) * Double(i + 1) / Double(steps)
            out += [c + r * SIMD2(cos(t0), sin(t0)), c + r * SIMD2(cos(t1), sin(t1))]
        }
        return out
    }

    static func segDist(_ p: SIMD2<Double>, _ a: SIMD2<Double>, _ b: SIMD2<Double>) -> Double {
        let ab = b - a, l2 = simd_length_squared(ab)
        guard l2 > 1e-12 else { return simd_distance(p, a) }
        let t = max(0, min(1, simd_dot(p - a, ab) / l2))
        return simd_distance(p, a + t * ab)
    }

    static func intersect(_ a0: SIMD2<Double>, _ a1: SIMD2<Double>,
                          _ b0: SIMD2<Double>, _ b1: SIMD2<Double>) -> SIMD2<Double>? {
        let r = a1 - a0, s = b1 - b0
        let den = r.x * s.y - r.y * s.x
        guard abs(den) > 1e-12 else { return nil }
        let t = ((b0.x - a0.x) * s.y - (b0.y - a0.y) * s.x) / den
        return a0 + t * r
    }

    /// Catmull-Rom through the control points (endpoints duplicated).
    static func catmullRom(_ p: [SIMD2<Double>], samplesPerSpan n: Int) -> [SIMD2<Double>] {
        guard p.count >= 2 else { return p }
        var out: [SIMD2<Double>] = [p[0]]
        for i in 0..<(p.count - 1) {
            let p0 = p[max(i - 1, 0)], p1 = p[i], p2 = p[i + 1], p3 = p[min(i + 2, p.count - 1)]
            for k in 1...n {
                let t: Double = Double(k) / Double(n)
                let t2: Double = t * t
                let t3: Double = t2 * t
                let a: SIMD2<Double> = 2 * p1
                let b: SIMD2<Double> = (p2 - p0) * t
                let c2: SIMD2<Double> = 2 * p0 - 5 * p1
                let c: SIMD2<Double> = (c2 + 4 * p2 - p3) * t2
                let d2: SIMD2<Double> = 3 * p1 - 3 * p2
                let d: SIMD2<Double> = (d2 - p0 + p3) * t3
                out.append(0.5 * (a + b + c + d))
            }
        }
        return out
    }
}

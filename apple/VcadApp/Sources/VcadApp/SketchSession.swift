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
    case select, line, rectangle, polygon, slot, circle, arc, spline, dimension, trim, extend, fillet
    var id: String { rawValue }
    var label: String {
        switch self {
        case .select: "Select"
        case .line: "Line"
        case .rectangle: "Rectangle"
        case .polygon: "Polygon"
        case .slot: "Slot"
        case .circle: "Circle"
        case .arc: "Arc"
        case .spline: "Spline"
        case .dimension: "Smart Dimension"
        case .trim: "Trim"
        case .extend: "Extend"
        case .fillet: "Sketch Fillet"
        }
    }
    /// Bare-letter key while sketching (Esc also falls back to Select).
    var shortcut: Character? {
        switch self {
        case .select: "s"
        case .line: "l"
        case .rectangle: "r"
        case .polygon: "p"
        case .slot: "o"
        case .circle: "c"
        case .arc: "a"
        case .spline: "n"
        case .dimension: "d"
        case .trim: "t"
        case .extend: "e"
        case .fillet: "f"
        }
    }
    /// Places geometry by clicking (the status line shows the cursor).
    var draws: Bool { ![.select, .dimension, .trim, .extend, .fillet].contains(self) }
    /// The prompt cut to fit beside the cursor coordinates.
    var shortHint: String {
        switch self {
        case .select: "Click to select"
        case .line: "Click points"
        case .rectangle: "Two corners"
        case .polygon: "Center, then corner"
        case .slot: "Ends, then width"
        case .circle: "Center, then radius"
        case .arc: "Center, start, end"
        case .spline: "Points · Return ends"
        case .dimension: "Pick to size · ⌘ pairs"
        case .trim: "Click a piece to cut"
        case .extend: "Click near a line end"
        case .fillet: "Click a corner"
        }
    }
    /// "Line  L".
    var tooltip: String { shortcut.map { "\(label)  \(String($0).uppercased())" } ?? label }
    var symbol: String {
        switch self {
        case .select: "cursorarrow"
        case .line: "line.diagonal"
        case .rectangle: "rectangle"
        case .polygon: "hexagon"
        case .slot: "capsule"
        case .circle: "circle"
        case .arc: "circle.lefthalf.striped.horizontal"
        case .spline: "point.bottomleft.forward.to.point.topright.scurvepath"
        case .dimension: "ruler"
        case .trim: "scissors"
        case .extend: "arrow.right.to.line"
        case .fillet: "arrow.turn.up.right"
        }
    }
    var hint: String {
        switch self {
        case .select: "Click entities to select · ⌘-click to add"
        case .line: "Click points · click the first point to close"
        case .rectangle: "Click two opposite corners"
        case .polygon: "Click the center, then a corner"
        case .slot: "Click both end centers, then the width"
        case .circle: "Click the center, then the radius"
        case .arc: "Click the center, the start, then the end"
        case .spline: "Click control points · Return to finish"
        case .dimension: "Click a line or circle to size it · a point, then another, for the distance · ⌘-click a second line for the angle"
        case .trim: "Click the piece of a line to cut back to its nearest crossings"
        case .extend: "Click near a line end to grow it to the next line"
        case .fillet: "Click the corner where two lines meet"
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
    var ccw = true
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
    @ObservationIgnored nonisolated(unsafe) private var raw: OpaquePointer

    private(set) var entities: [SketchEntity] = []
    private(set) var constraints: [SketchConstraintRow] = []
    /// Solved 2D point positions by handle.
    private(set) var positions: [UInt32: SIMD2<Double>] = [:]
    private(set) var radii: [UInt32: Double] = [:]
    private(set) var dof: Int = 0
    private(set) var state: UInt8 = UInt8(VCAD_SKETCH_UNDER)
    private(set) var converged = true
    private(set) var lastError: String?
    /// Every applied constraint as sent to the solver, for rebuilding after a delete.
    private var recorded: [VcadSketchConstraint] = []

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
    var snap: Double = 1.2
    /// Plane mm per screen point at the last click, set by the viewport so
    /// picking works in screen space at any zoom. Nil: fixed mm radii.
    var pickScale: Double?
    /// The first point of a point-to-point dimension.
    private(set) var dimensionAnchor: UInt32?
    /// Clicks round to this many mm unless they land on a point. 0 turns it off.
    var gridStep: Double = 1
    /// Side count of the polygon tool.
    var polygonSides = 6

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
        if h != 0 { entities.append(.init(handle: h, kind: .arc, points: [start, end, center], ccw: ccw)) }
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
        // A zero handle is an entity the solver refused (e.g. a zero-length
        // line): never hand it a constraint.
        let known = Set(entities.map(\.handle))
        guard e0 != 0, known.contains(e0), e1 == 0 || known.contains(e1), e2 == 0 || known.contains(e2) else {
            lastError = "\(kind.label) can't be added to that geometry"; return false
        }
        var c = VcadSketchConstraint(kind: kind.rawValue, e0: e0, r0: r0, e1: e1, r1: r1,
                                     e2: e2, r2: r2, value: value, value2: value2)
        let idx = vcad_sketch_add_constraint(raw, &c)
        // The solver's message is for developers (handles, function names).
        guard idx >= 0 else { _ = SimError.pending(); lastError = "\(kind.label) can't be added to that geometry"; return false }
        recorded.append(c)
        constraints.append(.init(index: Int(idx), kind: kind,
                                 entities: [e0, e1, e2].filter { $0 != 0 }, value: value))
        if solveNow { solve() }
        return true
    }

    /// Edit a driving dimension in place and re-solve.
    func setDimension(_ row: SketchConstraintRow, value: Double) { recording { setDimensionNow(row, value: value) } }
    private func setDimensionNow(_ row: SketchConstraintRow, value: Double) {
        guard vcad_sketch_set_dimension(raw, UInt32(row.index), value, 0) else {
            _ = SimError.pending(); lastError = "That dimension can't be applied"; return
        }
        if let i = constraints.firstIndex(of: row) { constraints[i].value = value }
        if recorded.indices.contains(row.index) { recorded[row.index].value = value }
        solve()
    }

    /// Remove the most recent constraint (the solver's stack is LIFO).
    func popConstraint() { recording { popConstraintNow() } }
    private func popConstraintNow() {
        guard !constraints.isEmpty, vcad_sketch_pop_constraint(raw) else { return }
        constraints.removeLast()
        recorded.removeLast()
        solve()
    }

    /// Delete the selected entities, plus any line or arc built on a deleted
    /// point and any point left unused. The solver can't remove entities, so
    /// this rebuilds it from what remains, keeping solved positions.
    func deleteSelection() {
        guard !selection.isEmpty else { return }
        recording { removeEntities(Set(selection)) }
    }

    // MARK: history

    /// Everything the sketch is made of, solver-independent. Undo/redo
    /// rebuild the solver from one of these, since it can't remove entities.
    private struct Snapshot {
        var entities: [SketchEntity]
        var constraints: [SketchConstraintRow]
        var recorded: [VcadSketchConstraint]
        var positions: [UInt32: SIMD2<Double>]
        var radii: [UInt32: Double]
    }
    private var undoStack: [Snapshot] = []
    private var redoStack: [Snapshot] = []
    private var recordingDepth = 0

    /// The sketch's own history: Cmd-Z inside a sketch walks this, never the document.
    var canUndo: Bool { hasInProgress || !undoStack.isEmpty }
    var canRedo: Bool { !redoStack.isEmpty }

    private func snapshot() -> Snapshot {
        Snapshot(entities: entities, constraints: constraints, recorded: recorded, positions: positions, radii: radii)
    }
    /// Run a user action; it becomes one undo step when it changed geometry or constraints.
    private func recording(_ body: () -> Void) {
        recordingDepth += 1
        let before = snapshot()
        body()
        recordingDepth -= 1
        guard recordingDepth == 0,
              before.entities != entities || before.constraints != constraints else { return }
        undoStack.append(before)
        redoStack = []
    }
    /// Undo: first drop in-progress clicks, else step back one action.
    @discardableResult
    func undo() -> Bool {
        if hasInProgress { cancelPending(); return true }
        guard let s = undoStack.popLast() else { return false }
        redoStack.append(snapshot())
        restore(s)
        return true
    }
    @discardableResult
    func redo() -> Bool {
        guard let s = redoStack.popLast() else { return false }
        undoStack.append(snapshot())
        restore(s)
        return true
    }

    /// A solver rebuilt from a snapshot, plus the handle map old -> new.
    private struct Built {
        var raw: OpaquePointer
        var map: [UInt32: UInt32]
        var entities: [SketchEntity]
        var rows: [SketchConstraintRow]
        var recorded: [VcadSketchConstraint]
    }
    /// Build a fresh solver from `snap`, leaving out the constraint at list
    /// position `skipping`. Constraints whose entities are gone are dropped.
    private func build(_ snap: Snapshot, skipping: Int? = nil) -> Built? {
        var o = [frame.origin.x, frame.origin.y, frame.origin.z]
        var x = [frame.xDir.x, frame.xDir.y, frame.xDir.z]
        var y = [frame.yDir.x, frame.yDir.y, frame.yDir.z]
        guard let fresh = vcad_sketch_new_on_plane(&o, &x, &y) else { return nil }
        var map: [UInt32: UInt32] = [:]
        var rebuilt: [SketchEntity] = []
        for e in snap.entities {
            var n = e
            n.points = e.points.compactMap { map[$0] }
            guard n.points.count == e.points.count else { continue }
            switch e.kind {
            case .point:
                let q = snap.positions[e.handle] ?? .zero
                n.handle = vcad_sketch_add_point(fresh, q.x, q.y)
            case .line:
                n.handle = vcad_sketch_add_line(fresh, n.points[0], n.points[1])
            case .arc:
                n.handle = vcad_sketch_add_arc(fresh, n.points[0], n.points[1], n.points[2], e.ccw)
            case .circle:
                n.handle = vcad_sketch_add_circle(fresh, n.points[0], snap.radii[e.handle] ?? 1)
            }
            guard n.handle != 0 else { continue }
            map[e.handle] = n.handle
            rebuilt.append(n)
        }
        var rows: [SketchConstraintRow] = []
        var replayed: [VcadSketchConstraint] = []
        for (i, (row, source)) in zip(snap.constraints, snap.recorded).enumerated() where i != skipping {
            guard row.entities.allSatisfy({ map[$0] != nil }) else { continue }
            var c = source
            c.e0 = map[c.e0] ?? 0
            c.e1 = map[c.e1] ?? 0
            c.e2 = map[c.e2] ?? 0
            let index = vcad_sketch_add_constraint(fresh, &c)
            guard index >= 0 else { _ = SimError.pending(); continue }
            rows.append(.init(index: Int(index), kind: row.kind, entities: row.entities.compactMap { map[$0] }, value: row.value))
            replayed.append(c)
        }
        return Built(raw: fresh, map: map, entities: rebuilt, rows: rows, recorded: replayed)
    }

    /// Delete one constraint row (the panel's remove button): rebuild the
    /// solver without it.
    func deleteConstraint(_ row: SketchConstraintRow) {
        guard let i = constraints.firstIndex(of: row) else { return }
        recording { restore(snapshot(), skipping: i) }
    }

    /// Rebuild the solver from a snapshot, keeping its solved positions.
    /// Constraints whose entities are gone are dropped.
    private func restore(_ snap: Snapshot, skipping: Int? = nil) {
        guard let b = build(snap, skipping: skipping) else { lastError = "Sketch solver unavailable"; return }
        let fresh = b.raw, map = b.map, rebuilt = b.entities, rows = b.rows, replayed = b.recorded
        var moved: [UInt32: SIMD2<Double>] = [:]
        var sizes: [UInt32: Double] = [:]
        for (old, new) in map {
            if let p = snap.positions[old] { moved[new] = p }
            if let r = snap.radii[old] { sizes[new] = r }
        }
        vcad_sketch_free(raw)
        raw = fresh
        entities = rebuilt
        constraints = rows
        recorded = replayed
        positions = moved
        radii = sizes
        selection = []
        pending = []
        chain = []
        dimensionAnchor = nil
        lastError = nil
        solve()
    }

    /// Re-solve and refresh every solved position.
    func solve() {
        let r = vcad_sketch_solve(raw)
        converged = r.converged != 0 || r.status == 3 || r.status == 4
        state = r.state
        dof = Int(r.dof)
        conflicting = isOverConstrained ? findConflicts() : []
        refresh()
    }

    /// Row indices (`SketchConstraintRow.index`) of the constraints that make
    /// an over-constrained sketch fail: each is one whose removal alone lets
    /// the rest solve. Empty when the sketch solves.
    private(set) var conflicting: Set<Int> = []

    /// Trial-solve without each constraint in turn (the solver reports only
    /// equation counts, not which equation is redundant).
    private func findConflicts() -> Set<Int> {
        let snap = snapshot()
        guard snap.constraints.count <= 200 else { return [] }
        var out: Set<Int> = []
        for (i, row) in snap.constraints.enumerated() {
            guard let b = build(snap, skipping: i) else { continue }
            let r = vcad_sketch_solve(b.raw)
            vcad_sketch_free(b.raw)
            let ok = r.converged != 0 || r.status == 3 || r.status == 4
            if ok, Int32(r.state) != VCAD_SKETCH_OVER { out.insert(row.index) }
        }
        // Any one would do (a plain duplicate): point at the newest.
        if out.count == snap.constraints.count, let last = snap.constraints.last { return [last.index] }
        return out
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

    /// A click on the plane.
    /// Where a click at `p` lands: an existing point, else the nearest grid crossing.
    func gridded(_ p: SIMD2<Double>) -> SIMD2<Double> {
        guard gridStep > 0, tool.draws, snapPoint(p) == nil else { return p }
        return (p / gridStep).rounded(.toNearestOrAwayFromZero) * gridStep
    }

    func tap(_ raw: SIMD2<Double>, additive: Bool = false) { recording { tapNow(raw, additive: additive) } }
    private func tapNow(_ raw: SIMD2<Double>, additive: Bool) {
        if tool != .select { lastError = nil }
        let p = gridded(raw)
        switch tool {
        case .select:
            if !additive, let c = glyphAt(p) {
                selection = []; selectedConstraint = c; revision &+= 1; break
            }
            selectedConstraint = nil
            let hit = nearestEntity(p)
            if additive { if let hit { if let i = selection.firstIndex(of: hit) { selection.remove(at: i) } else { selection.append(hit) } } }
            else { selection = hit.map { [$0] } ?? [] }
        case .line:
            lineTap(raw)
        case .rectangle:
            if let a = pending.first {
                // Corners sharing a row or column make a zero-area box (the
                // corners would snap together into a degenerate line): keep
                // waiting for a real second corner.
                guard Self.isRectangle(a, p, snap: snap) else {
                    lastError = "A rectangle needs width and height. Pick the opposite corner off this line."
                    break
                }
                rectangle(a, p); pending = []
            } else { pending = [p] }
        case .polygon:
            if let c = pending.first {
                polygon(center: c, corner: p); pending = []
            } else { pending = [p] }
        case .slot:
            pending.append(p)
            if pending.count == 3 { slot(pending[0], pending[1], through: p); pending = [] }
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
            dimensionTap(p, additive: additive)
        case .trim:
            trimTap(p)
        case .extend:
            extendTap(p)
        case .fillet:
            filletTap(p)
        }
        solve()
    }

    /// The Smart Dimension pick for an entity.
    func dimensionPick(_ h: UInt32) -> SketchPick? {
        guard let e = entity(h) else { return nil }
        switch e.kind {
        case .point: return positions[h].map { .point(h, $0) }
        case .line:
            guard let a = positions[e.points[0]], let b = positions[e.points[1]] else { return nil }
            return .line(h, a, b)
        case .circle, .arc:
            guard let ch = e.points.last, let c = positions[ch], let r = radii[h] else { return nil }
            return .curve(h, center: ch, c, radius: r, full: e.kind == .circle)
        }
    }

    /// Smart Dimension. A line or curve click sizes it at once (length,
    /// radius, diameter). A point click arms it: the next click on a point,
    /// curve or line measures the distance between. ⌘-click a second entity
    /// right after sizing a line to dimension the pair instead — the angle
    /// between two lines, a point–line distance — replacing the size just
    /// added. It never fixes a point.
    private func dimensionTap(_ p: SIMD2<Double>, additive: Bool) {
        let hit = nearestEntity(p)
        if let first = dimensionAnchor {
            dimensionAnchor = nil
            guard let hit, hit != first else { return }
            // A point on the line it's paired with (its own end, say): the
            // pair measures nothing, so size the line instead.
            if case .point(_, let q) = dimensionPick(first), case .line(_, let a, let b)? = dimensionPick(hit),
               SmartDimension.lineDistance(q, a, b) < 1e-6 {
                addDimension(dimensionPick(hit), nil); return
            }
            addDimension(dimensionPick(first), dimensionPick(hit))
            return
        }
        guard let hit, let pk = dimensionPick(hit) else { lastSized = nil; return }
        if additive, let prev = lastSized, prev != hit, let prevPick = dimensionPick(prev),
           SmartDimension.plan(prevPick, pk) != nil {
            if let last = constraints.last, last.entities == [prev],
               [.length, .radius, .diameter].contains(last.kind), vcad_sketch_pop_constraint(raw) {
                constraints.removeLast(); recorded.removeLast()
            }
            lastSized = nil
            addDimension(prevPick, pk)
            return
        }
        if case .point = pk { dimensionAnchor = hit; lastSized = nil; revision &+= 1; return }
        lastSized = hit
        addDimension(pk, nil)
    }

    /// The entity the last dimension click sized (for ⌘-click pairing).
    @ObservationIgnored private var lastSized: UInt32?

    private func addDimension(_ a: SketchPick?, _ b: SketchPick?) {
        guard let a, let plan = SmartDimension.plan(a, b) else {
            lastError = "Those two can't be dimensioned together"; return
        }
        let sizes: Set<SketchConstraintKind> = [.length, .radius, .diameter]
        // A second size dimension on the same curve only over-constrains it.
        if sizes.contains(plan.kind),
           constraints.contains(where: { sizes.contains($0.kind) && $0.entities == [plan.e0] }) {
            lastError = "That \(entity(plan.e0)?.kind == .line ? "line" : "curve") already has a dimension — edit it in the list"
            return
        }
        constrain(plan.kind, plan.e0, VCAD_SKETCH_REF_SELF, plan.e1, value: plan.value, solveNow: false)
    }

    // MARK: inference

    /// The segment the line tool drew last (for ⊥ / ∥ / = inference).
    private(set) var lastSegment: UInt32?

    /// Snap reach in plane mm.
    var inferenceReach: Double { pickScale != nil ? pickRadii.point * 1.4 : snap }

    func inferenceModel() -> SketchInference {
        var m = SketchInference()
        m.reach = inferenceReach
        for e in entities {
            switch e.kind {
            case .point:
                if let q = positions[e.handle] { m.points.append((e.handle, q)) }
            case .line:
                if let a = positions[e.points[0]], let b = positions[e.points[1]] {
                    m.lines.append(.init(handle: e.handle, a: a, b: b, aH: e.points[0], bH: e.points[1]))
                }
            case .circle, .arc:
                if let ch = e.points.last, let c = positions[ch], let r = radii[e.handle] {
                    m.curves.append(.init(handle: e.handle, center: c, radius: r, ends: e.kind == .arc ? Array(e.points.prefix(2)) : []))
                }
            }
        }
        return m
    }

    /// Where a line-tool click at `p` would land, and what it would constrain.
    func inferLine(_ p: SIMD2<Double>) -> SketchInference.Result {
        let anchorH = chain.last
        return inferenceModel().infer(p, anchor: anchorH.flatMap { positions[$0] }, anchorH: anchorH, previous: lastSegment)
    }

    /// The live inference under the cursor (line tool only), for the glyphs.
    var cursorInference: SketchInference.Result? {
        guard tool == .line, let c = cursor else { return nil }
        return inferLine(c)
    }

    /// When true, the next line click ignores inference (⌘ held).
    var suppressInference = false

    private func lineTap(_ click: SIMD2<Double>) {
        let res = suppressInference ? .init(point: click, inferred: []) : inferLine(click)
        let q = res.inferred.isEmpty ? gridded(click) : res.point
        let before = constraints.count
        let h = res.snapped ?? (snapPoint(q) ?? addPoint(q))
        for i in res.inferred {
            switch i {
            case .midpoint(let l): constrain(.midpoint, h, VCAD_SKETCH_REF_SELF, l, solveNow: false)
            case .onLine(let l): constrain(.pointOnLine, h, VCAD_SKETCH_REF_SELF, l, solveNow: false)
            case .onCircle(let c): constrain(.pointOnCircle, h, VCAD_SKETCH_REF_SELF, c, solveNow: false)
            default: break
            }
        }
        if let last = chain.last, last != h {
            let line = addLine(last, h)
            for i in res.inferred {
                switch i {
                case .horizontal: constrain(.horizontal, line, solveNow: false)
                case .vertical: constrain(.vertical, line, solveNow: false)
                case .perpendicular(let l): constrain(.perpendicular, line, VCAD_SKETCH_REF_SELF, l, solveNow: false)
                case .parallel(let l): constrain(.parallel, line, VCAD_SKETCH_REF_SELF, l, solveNow: false)
                case .equal(let l): constrain(.equalLength, line, VCAD_SKETCH_REF_SELF, l, solveNow: false)
                case .tangent(let c): constrain(.tangent, line, VCAD_SKETCH_REF_SELF, c, VCAD_SKETCH_REF_SELF, last, solveNow: false)
                default: break
                }
            }
            lastSegment = line
            if h == chain.first { chain = []; lastSegment = nil }      // closed the loop
            else { chain.append(h) }
        } else if chain.isEmpty {
            chain = [h]; lastSegment = nil
        }
        // An inference that would over-constrain is dropped, newest first.
        solve()
        while isOverConstrained || !converged, constraints.count > before, vcad_sketch_pop_constraint(raw) {
            constraints.removeLast(); recorded.removeLast(); solve()
        }
    }

    // MARK: construction, glyphs, drag

    /// Toggle construction on the selected curves: dashed, still
    /// constrainable, never part of a profile.
    func toggleConstruction() {
        recording {
            let sel = Set(selection)
            let curves = entities.indices.filter { sel.contains(entities[$0].handle) && entities[$0].kind != .point }
            guard !curves.isEmpty else { lastError = "Select lines, arcs or circles to make construction"; return }
            let on = !curves.allSatisfy { entities[$0].construction }
            for i in curves { entities[i].construction = on }
            revision &+= 1
        }
    }
    var hasConstruction: Bool { entities.contains { $0.construction && $0.kind != .point } }

    /// The constraint whose glyph was clicked (Select tool); Delete removes it.
    var selectedConstraint: Int?

    /// Constraint (row index) whose glyph sits under `p`.
    func glyphAt(_ p: SIMD2<Double>) -> Int? {
        let reach = pickRadii.point * 1.2
        return constraints.compactMap { c in glyphAnchor(c).map { (c.index, simd_distance($0, p)) } }
            .filter { $0.1 <= reach }.min { $0.1 < $1.1 }?.0
    }

    /// Delete the selected glyph's constraint, else the selected entities.
    func deleteSelectionOrConstraint() {
        if let i = selectedConstraint, let row = constraints.first(where: { $0.index == i }) {
            selectedConstraint = nil
            deleteConstraint(row)
        } else {
            deleteSelection()
        }
    }

    @ObservationIgnored private var dragPoint: UInt32?
    @ObservationIgnored private var dragBefore: Snapshot?
    var isDragging: Bool { dragPoint != nil }

    /// Start dragging the point under `p` (Select tool). The point follows
    /// the cursor and the solver keeps every constraint as it moves.
    func beginDrag(_ p: SIMD2<Double>) -> Bool {
        guard tool == .select, let h = nearestEntity(p), entity(h)?.kind == .point else { return false }
        dragPoint = h; dragBefore = snapshot(); selection = [h]
        return true
    }
    func dragTo(_ p: SIMD2<Double>) {
        guard let h = dragPoint else { return }
        pin(h, at: p)
    }
    func endDrag() {
        defer { dragPoint = nil; dragBefore = nil }
        guard let before = dragBefore, before.positions != positions else { return }
        undoStack.append(before); redoStack = []
    }

    /// Solve once with `h` pinned at `p`, then drop the pin: positions keep
    /// the pinned solution, the constraint list is unchanged.
    private func pin(_ h: UInt32, at p: SIMD2<Double>) {
        var c = VcadSketchConstraint(kind: SketchConstraintKind.fixed.rawValue, e0: h, r0: VCAD_SKETCH_REF_SELF,
                                     e1: 0, r1: VCAD_SKETCH_REF_SELF, e2: 0, r2: VCAD_SKETCH_REF_SELF,
                                     value: p.x, value2: p.y)
        guard vcad_sketch_add_constraint(raw, &c) >= 0 else { _ = SimError.pending(); return }
        _ = vcad_sketch_solve(raw)
        _ = vcad_sketch_pop_constraint(raw)
        solve()
    }

    // MARK: trim / extend / fillet / mirror / pattern

    /// Straight segments other than `h`, for trim and extend.
    private func cutters(excluding h: UInt32) -> [(SIMD2<Double>, SIMD2<Double>)] {
        entities.filter { $0.kind == .line && $0.handle != h }.compactMap { e in
            guard let a = positions[e.points[0]], let b = positions[e.points[1]] else { return nil }
            return (a, b)
        }
    }

    /// Axis constraints a replaced line should hand on to its pieces.
    private func axisKinds(of h: UInt32) -> [SketchConstraintKind] {
        constraints.filter { ($0.kind == .horizontal || $0.kind == .vertical) && $0.entities == [h] }.map(\.kind)
    }

    /// Remove entities (plus curves built on removed points and points left
    /// unused) by rebuilding the solver. Handles are renumbered.
    private func removeEntities(_ start: Set<UInt32>) {
        var gone = start
        for e in entities where e.kind != .point && e.points.contains(where: gone.contains) { gone.insert(e.handle) }
        let kept = entities.filter { !gone.contains($0.handle) }
        let used = Set(kept.filter { $0.kind != .point }.flatMap(\.points))
        for e in entities where e.kind != .point && gone.contains(e.handle) {
            for p in e.points where !used.contains(p) { gone.insert(p) }
        }
        var snap = snapshot()
        snap.entities = entities.filter { !gone.contains($0.handle) }
        restore(snap)
    }

    private func trimTap(_ p: SIMD2<Double>) {
        guard let h = nearestEntity(p), let e = entity(h), e.kind == .line,
              let a = positions[e.points[0]], let b = positions[e.points[1]] else {
            lastError = "Trim works on lines: click the piece to cut"; return
        }
        let ab = b - a
        let t = (simd_dot(p - a, ab) / max(simd_length_squared(ab), 1e-12)).clamped(to: 0...1)
        let keep = SketchEdit.trim(a, b, at: t, cutters: cutters(excluding: h))
        let axes = axisKinds(of: h)
        for r in keep {
            let s = r.lowerBound == 0 ? e.points[0] : addPoint(a + r.lowerBound * ab)
            let f = r.upperBound == 1 ? e.points[1] : addPoint(a + r.upperBound * ab)
            let piece = addLine(s, f)
            for k in axes { constrain(k, piece, solveNow: false) }
        }
        removeEntities([h])
    }

    private func extendTap(_ p: SIMD2<Double>) {
        guard let h = nearestEntity(p), let e = entity(h), e.kind == .line,
              let a = positions[e.points[0]], let b = positions[e.points[1]] else {
            lastError = "Extend works on lines: click near the end to grow"; return
        }
        guard let target = SketchEdit.extend(a, b, near: p, cutters: cutters(excluding: h)) else {
            lastError = "Nothing to extend to in that direction"; return
        }
        let end = simd_distance(p, b) <= simd_distance(p, a) ? e.points[1] : e.points[0]
        let shared = entities.filter { $0.kind != .point && $0.points.contains(end) }.count
        guard shared == 1 else { lastError = "That end is joined to another curve"; return }
        pin(end, at: target)
    }

    /// Radius the Fillet tool rounds corners with.
    var filletRadius: Double = 2

    private func filletTap(_ p: SIMD2<Double>) {
        guard let h = snapPoint(p) ?? nearestEntity(p).flatMap({ entity($0)?.kind == .point ? $0 : nil }),
              let c = positions[h] else { lastError = "Click the corner point where two lines meet"; return }
        let lines = entities.filter { $0.kind == .line && $0.points.contains(h) }
        let others = entities.filter { $0.kind != .point && $0.kind != .line && $0.points.contains(h) }
        guard lines.count == 2, others.isEmpty else { lastError = "A fillet needs exactly two lines meeting at the corner"; return }
        let far = lines.map { $0.points[0] == h ? $0.points[1] : $0.points[0] }
        guard let a = positions[far[0]], let b = positions[far[1]],
              let f = SketchEdit.fillet(corner: c, a: a, b: b, radius: filletRadius) else {
            lastError = "Radius \(filletRadius.formatted()) mm doesn't fit that corner"; return
        }
        let axesA = axisKinds(of: lines[0].handle), axesB = axisKinds(of: lines[1].handle)
        let pa = addPoint(f.onA), pb = addPoint(f.onB), pc = addPoint(f.center)
        let la = addLine(far[0], pa), lb = addLine(pb, far[1])
        let arc = addArc(start: f.ccw ? pa : pb, end: f.ccw ? pb : pa, center: pc, ccw: true)
        for k in axesA { constrain(k, la, solveNow: false) }
        for k in axesB { constrain(k, lb, solveNow: false) }
        constrain(.tangent, la, VCAD_SKETCH_REF_SELF, arc, VCAD_SKETCH_REF_SELF, pa, solveNow: false)
        constrain(.tangent, lb, VCAD_SKETCH_REF_SELF, arc, VCAD_SKETCH_REF_SELF, pb, solveNow: false)
        constrain(.radius, arc, value: filletRadius, solveNow: false)
        removeEntities(Set(lines.map(\.handle) + [h]))
    }

    /// Copy the selected curves through `map`; arcs reverse when `flips`.
    /// Returns old point handle -> new point handle.
    @discardableResult
    private func copySelection(_ map: (SIMD2<Double>) -> SIMD2<Double>, flips: Bool, curves: [SketchEntity]) -> [UInt32: UInt32] {
        var pts: [UInt32: UInt32] = [:]
        func pt(_ h: UInt32) -> UInt32 {
            if let n = pts[h] { return n }
            let n = addPoint(map(positions[h] ?? .zero)); pts[h] = n; return n
        }
        for e in curves {
            switch e.kind {
            case .point: _ = pt(e.handle)
            case .line: addLine(pt(e.points[0]), pt(e.points[1]))
            case .circle: addCircle(center: pt(e.points[0]), radius: radii[e.handle] ?? 1)
            case .arc:
                let (s, f, c) = (pt(e.points[0]), pt(e.points[1]), pt(e.points[2]))
                addArc(start: flips ? f : s, end: flips ? s : f, center: c, ccw: e.ccw)
            }
        }
        return pts
    }

    /// Mirror the selection about the last selected line, tying each copied
    /// point to its original with a symmetric constraint.
    func mirrorSelection() {
        recording {
            let sel = selection.compactMap { entity($0) }
            guard let axis = sel.last(where: { $0.kind == .line }),
                  let a = positions[axis.points[0]], let b = positions[axis.points[1]] else {
                lastError = "Select the curves to mirror, then the mirror line last"; return
            }
            let curves = sel.filter { $0.handle != axis.handle }
            guard !curves.isEmpty else { lastError = "Select curves to mirror as well as the mirror line"; return }
            let onAxis = Set(axis.points)
            let pairs = copySelection({ SketchEdit.mirror($0, a, b) }, flips: true, curves: curves)
            for (old, new) in pairs.sorted(by: { $0.key < $1.key }) where !onAxis.contains(old) {
                constrain(.symmetric, old, VCAD_SKETCH_REF_SELF, new, VCAD_SKETCH_REF_SELF, axis.handle, solveNow: false)
            }
            solve()
        }
    }

    /// Circular pattern: `count` copies in all (original included) about the
    /// selected point (last), else the sketch origin.
    func circularPattern(count: Int) {
        recording {
            let sel = selection.compactMap { entity($0) }
            let centerH = sel.last(where: { $0.kind == .point })?.handle
            let c = centerH.flatMap { positions[$0] } ?? .zero
            let curves = sel.filter { $0.handle != centerH && $0.kind != .point }
            guard count >= 2, !curves.isEmpty else { lastError = "Select curves to pattern"; return }
            for k in 1..<count {
                let ang = 2 * .pi * Double(k) / Double(count)
                copySelection({ SketchEdit.rotate($0, about: c, by: ang) }, flips: false, curves: curves)
            }
            solve()
        }
    }

    /// Linear pattern: `count` copies in all, each `step` further along.
    func linearPattern(count: Int, step: SIMD2<Double>) {
        recording {
            let curves = selection.compactMap { entity($0) }.filter { $0.kind != .point }
            guard count >= 2, !curves.isEmpty, simd_length(step) > 1e-9 else { lastError = "Select curves to pattern"; return }
            for k in 1..<count { copySelection({ $0 + Double(k) * step }, flips: false, curves: curves) }
            solve()
        }
    }

    /// Finish a multi-click tool (spline: sample the control polygon).
    func commitPending() { recording { commitPendingNow() } }
    private func commitPendingNow() {
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
        lastSegment = nil
        solve()
    }

    var hasInProgress: Bool { !pending.isEmpty || !chain.isEmpty || dimensionAnchor != nil }

    /// Escape: drop in-progress clicks.
    func cancelPending() { pending = []; chain = []; lastSegment = nil; dimensionAnchor = nil; revision &+= 1 }

    /// Two clicks span a real rectangle: both sides longer than the snap radius,
    /// so no two corners merge.
    static func isRectangle(_ a: SIMD2<Double>, _ b: SIMD2<Double>, snap: Double) -> Bool {
        abs(b.x - a.x) >= max(snap, 1e-6) && abs(b.y - a.y) >= max(snap, 1e-6)
    }
    private func rectangle(_ a: SIMD2<Double>, _ b: SIMD2<Double>) {
        let c = [a, SIMD2(b.x, a.y), b, SIMD2(a.x, b.y)].map { pointAt($0) }
        let l = (0..<4).map { addLine(c[$0], c[($0 + 1) % 4]) }
        constrain(.horizontal, l[0], solveNow: false)
        constrain(.vertical, l[1], solveNow: false)
        constrain(.horizontal, l[2], solveNow: false)
        constrain(.vertical, l[3], solveNow: false)
    }

    /// The corners a polygon would have: shared with the live preview.
    static func polygonCorners(center c: SIMD2<Double>, corner p: SIMD2<Double>, sides n: Int) -> [SIMD2<Double>] {
        let d = p - c, r = simd_length(d), a0 = atan2(d.y, d.x)
        return (0..<n).map { c + r * SIMD2(cos(a0 + Double($0) * 2 * .pi / Double(n)), sin(a0 + Double($0) * 2 * .pi / Double(n))) }
    }
    private func polygon(center: SIMD2<Double>, corner: SIMD2<Double>) {
        guard simd_distance(center, corner) > 0.1 else { return }
        let c = Self.polygonCorners(center: center, corner: corner, sides: polygonSides).map { pointAt($0) }
        let l = c.indices.map { addLine(c[$0], c[($0 + 1) % c.count]) }
        for i in l.indices.dropFirst() { constrain(.equalLength, l[0], VCAD_SKETCH_REF_SELF, l[i], solveNow: false) }
    }
    /// A stadium: `a`→`b` is the centerline, `p` sets the half-width.
    static func slotOffset(_ a: SIMD2<Double>, _ b: SIMD2<Double>, through p: SIMD2<Double>) -> SIMD2<Double>? {
        let d = b - a
        guard simd_length(d) > 0.1 else { return nil }
        let u = simd_normalize(d), n = SIMD2(-u.y, u.x)
        let r = abs(simd_dot(p - a, n))
        return r > 0.05 ? n * r : nil
    }
    private func slot(_ a: SIMD2<Double>, _ b: SIMD2<Double>, through p: SIMD2<Double>) {
        guard let n = Self.slotOffset(a, b, through: p) else { return }
        let ca = addPoint(a), cb = addPoint(b)
        let a0 = addPoint(a + n), a1 = addPoint(a - n), b0 = addPoint(b + n), b1 = addPoint(b - n)
        let top = addLine(b0, a0), bottom = addLine(a1, b1)
        addArc(start: a0, end: a1, center: ca, ccw: true)
        addArc(start: b1, end: b0, center: cb, ccw: true)
        constrain(.parallel, top, VCAD_SKETCH_REF_SELF, bottom, solveNow: false)
    }

    /// Apply a constraint to the current selection, inferring refs from kinds.
    @discardableResult
    func constrainSelection(_ kind: SketchConstraintKind) -> Bool {
        var ok = false
        recording { ok = constrainSelectionNow(kind) }
        return ok
    }
    private func constrainSelectionNow(_ kind: SketchConstraintKind) -> Bool {
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
            // Signed, as the solver measures it: an unsigned value flips one line.
            let deg = SmartDimension.signedAngle(a1 - a0, b1 - b0)
            return constrain(kind, sel[0].handle, VCAD_SKETCH_REF_SELF, sel[1].handle, value: deg, solveNow: false)
        default:
            lastError = "\(kind.label) needs a different selection"
            return false
        }
    }

    /// Pick radii in plane mm: a point must be clicked squarely, a curve
    /// anywhere within a few screen points of its ink.
    var pickRadii: (point: Double, curve: Double) {
        guard let k = pickScale, k.isFinite, k > 0 else { return (snap * 0.6, snap * 1.5) }
        return (5 * k, 8 * k)
    }

    /// Entity at a plane point. A point wins only when the click is really on
    /// it; otherwise the nearest line (segment distance) or circle (ring).
    func nearestEntity(_ p: SIMD2<Double>) -> UInt32? {
        let reach = pickRadii
        let point = entities.filter { $0.kind == .point }
            .compactMap { e in positions[e.handle].map { (e.handle, simd_distance($0, p)) } }
            .filter { $0.1 <= reach.point }.min { $0.1 < $1.1 }
        if let point { return point.0 }
        var best: (UInt32, Double)?
        func consider(_ h: UInt32, _ d: Double) { if d <= reach.curve, d < (best?.1 ?? .infinity) { best = (h, d) } }
        for e in entities {
            switch e.kind {
            case .point:
                break
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
    func offsetProfile(by d: Double) { recording { offsetProfileNow(by: d) } }
    private func offsetProfileNow(by d: Double) {
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
        if hasConstruction { return singleRegion() }
        guard let r = vcad_sketch_profile_json(raw) else {
            lastError = SimError.pending() ?? "Profile is not closed"; return nil
        }
        defer { vcad_string_free(r) }
        return try? JSONSerialization.jsonObject(with: Data(String(cString: r).utf8)) as? [String: Any]
    }

    /// Why a sketch's loops can't become regions — shown on the disabled
    /// Finish button and in the sketch status line.
    struct RegionProblem: Error, Equatable { let reason: String }

    /// The closed loops grouped into regions, for a sketch the single-loop
    /// export refuses: a bore and its bolt circle, a row of slots, a ring (an
    /// outer loop with an inner one — a lip). A loop inside another becomes a
    /// hole in it (`holes` on the profile; the kernel orients outer CCW and
    /// holes CW); a loop inside a hole is an island, a region of its own.
    /// Fails, with the reason, when a curve is left open or two loops cross.
    func loopRegions() -> Result<[[String: Any]], RegionProblem> {
        let curves = entities.filter { $0.kind != .point && !$0.construction }
        guard !curves.isEmpty else { return .failure(.init(reason: "Draw a closed profile to finish")) }
        let open = RegionProblem(reason: "A curve is left open — close every loop to finish")
        var parent = Dictionary(curves.flatMap { $0.points.prefix(2) }.map { ($0, $0) }) { first, _ in first }
        func root(_ h: UInt32) -> UInt32 { var r = h; while let n = parent[r], n != r { r = n }; return r }
        for e in curves where e.kind != .circle { parent[root(e.points[0])] = root(e.points[1]) }
        let groups = Dictionary(grouping: curves) { root($0.points[0]) }.values
        func xy(_ p: SIMD2<Double>) -> [String: Double] { ["x": p.x, "y": p.y] }
        func xyz(_ v: SIMD3<Double>) -> [String: Double] { ["x": v.x, "y": v.y, "z": v.z] }

        struct Loop {
            var segments: [[String: Any]]
            var lo: SIMD2<Double>, hi: SIMD2<Double>
            var edges: [(SIMD2<Double>, SIMD2<Double>)]   // straight edges, for the crossing test
            var area: Double { (hi.x - lo.x) * (hi.y - lo.y) }
            func contains(_ o: Loop) -> Bool {
                lo.x <= o.lo.x && lo.y <= o.lo.y && hi.x >= o.hi.x && hi.y >= o.hi.y && area > o.area
            }
            func overlaps(_ o: Loop) -> Bool { lo.x <= o.hi.x && o.lo.x <= hi.x && lo.y <= o.hi.y && o.lo.y <= hi.y }
        }
        var loops: [Loop] = []
        for group in groups {
            var segments: [[String: Any]] = []
            var extent: [SIMD2<Double>] = []
            var edges: [(SIMD2<Double>, SIMD2<Double>)] = []
            if group.count == 1, group[0].kind == .circle {
                guard let c = positions[group[0].points[0]], let r = radii[group[0].handle] else { return .failure(open) }
                let a = c + SIMD2(r, 0), b = c - SIMD2(r, 0)
                segments = [["type": "Arc", "start": xy(a), "end": xy(b), "center": xy(c), "ccw": true],
                            ["type": "Arc", "start": xy(b), "end": xy(a), "center": xy(c), "ccw": true]]
                extent = [c - SIMD2(r, r), c + SIMD2(r, r)]
            } else {
                guard group.allSatisfy({ $0.kind != .circle }) else {
                    return .failure(.init(reason: "A circle shares a point with other curves — loops must not touch"))
                }
                // Every vertex of a closed loop joins exactly two curves.
                var degree: [UInt32: Int] = [:]
                for e in group { degree[e.points[0], default: 0] += 1; degree[e.points[1], default: 0] += 1 }
                guard degree.values.allSatisfy({ $0 == 2 }) else {
                    return .failure(degree.values.contains { $0 > 2 }
                        ? .init(reason: "Curves branch at a shared point — loops must not cross or overlap")
                        : open)
                }
                var rest = group
                let start = rest[0].points[0]
                var at = start
                while !rest.isEmpty {
                    guard let i = rest.firstIndex(where: { $0.points[0] == at || $0.points[1] == at }) else { return .failure(open) }
                    let e = rest.remove(at: i)
                    let forward = e.points[0] == at
                    let to = e.points[forward ? 1 : 0]
                    guard let p = positions[at], let q = positions[to] else { return .failure(open) }
                    if e.kind == .line {
                        segments.append(["type": "Line", "start": xy(p), "end": xy(q)])
                        edges.append((p, q))
                    } else {
                        guard let c = positions[e.points[2]] else { return .failure(open) }
                        segments.append(["type": "Arc", "start": xy(p), "end": xy(q), "center": xy(c), "ccw": forward])
                        let r = simd_distance(c, p)
                        extent += [c - SIMD2(r, r), c + SIMD2(r, r)]
                    }
                    extent += [p, q]
                    at = to
                }
                guard at == start else { return .failure(open) }
            }
            loops.append(Loop(segments: segments, lo: extent.reduce(extent[0], pointwiseMin),
                              hi: extent.reduce(extent[0], pointwiseMax), edges: edges))
        }
        // Loops may nest but never cross. Containment is by bounding box
        // (plus an exact straight-edge crossing test), which is exact for the
        // rectangles, slots and circles the toolbar draws.
        func cross(_ a: (SIMD2<Double>, SIMD2<Double>), _ b: (SIMD2<Double>, SIMD2<Double>)) -> Bool {
            func o(_ p: SIMD2<Double>, _ q: SIMD2<Double>, _ r: SIMD2<Double>) -> Double {
                (q.x - p.x) * (r.y - p.y) - (q.y - p.y) * (r.x - p.x)
            }
            let d1 = o(b.0, b.1, a.0), d2 = o(b.0, b.1, a.1), d3 = o(a.0, a.1, b.0), d4 = o(a.0, a.1, b.1)
            return d1 * d2 <= 0 && d3 * d4 <= 0
        }
        let crossing = RegionProblem(reason: "Two loops cross or touch — a loop must lie fully inside or outside another")
        for i in loops.indices {
            for j in loops.indices where i < j {
                let (a, b) = (loops[i], loops[j])
                guard a.overlaps(b) else { continue }
                if !a.contains(b) && !b.contains(a) { return .failure(crossing) }
                for ea in a.edges { for eb in b.edges where cross(ea, eb) { return .failure(crossing) } }
            }
        }
        // Depth = how many loops enclose it: even depths are regions, odd
        // depths are holes in their innermost enclosing loop.
        let containers = loops.indices.map { i in loops.indices.filter { $0 != i && loops[$0].contains(loops[i]) } }
        var holes: [Int: [[[String: Any]]]] = [:]
        for i in loops.indices where containers[i].count % 2 == 1 {
            guard let owner = containers[i].min(by: { loops[$0].area < loops[$1].area }) else { continue }
            holes[owner, default: []].append(loops[i].segments)
        }
        let profiles: [[String: Any]] = loops.indices.filter { containers[$0].count % 2 == 0 }.map { i in
            var p: [String: Any] = ["origin": xyz(frame.origin), "x_dir": xyz(frame.xDir), "y_dir": xyz(frame.yDir),
                                    "segments": loops[i].segments]
            if let h = holes[i] { p["holes"] = h }
            return p
        }
        return .success(profiles)
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

    var canFinish: Bool {
        if hasConstruction { return singleRegion() != nil }
        return !isEmpty && vcad_sketch_profile_json(raw).map { vcad_string_free($0); return true } ?? false
    }

    /// With construction geometry the kernel export would include it, so a
    /// sketch with construction finishes from its one hole-free loop region.
    private func singleRegion() -> [String: Any]? {
        guard case .success(let r) = loopRegions(), r.count == 1, r[0]["holes"] == nil else {
            lastError = "Profile is not closed"; return nil
        }
        return r[0]
    }

    // MARK: drawing

    /// Ink as plane-space segment pairs: committed geometry, then the live preview.
    func ink(circleSegments n: Int = 64) -> (geometry: [SIMD2<Double>], preview: [SIMD2<Double>], points: [SIMD2<Double>]) {
        var g: [SIMD2<Double>] = []
        for e in entities where !e.construction {
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
                if let last = chain.last, let a = positions[last] { pv += [a, cursorInference?.point ?? c] }
            case .rectangle:
                if let a = pending.first {
                    let k = [a, SIMD2(c.x, a.y), c, SIMD2(a.x, c.y)]
                    for i in 0..<4 { pv += [k[i], k[(i + 1) % 4]] }
                }
            case .polygon:
                if let a = pending.first, simd_distance(a, c) > 0.01 {
                    let v = Self.polygonCorners(center: a, corner: c, sides: polygonSides)
                    for k in v.indices { pv += [v[k], v[(k + 1) % v.count]] }
                }
            case .slot:
                if pending.count == 1 { pv += [pending[0], c] }
                if pending.count == 2, let w = Self.slotOffset(pending[0], pending[1], through: c) {
                    let (a, b) = (pending[0], pending[1]), r = simd_length(w), t = atan2(w.y, w.x)
                    pv += [a + w, b + w, a - w, b - w]
                    pv += Self.ring(a, r, t, t + .pi, n) + Self.ring(b, r, t - .pi, t, n)
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
            case .select, .dimension, .trim, .extend, .fillet:
                break
            }
        }
        let pts = entities.filter { $0.kind == .point }.compactMap { positions[$0.handle] } + pending
        return (g, pv, pts)
    }

    /// Construction curves as dash pairs.
    func constructionInk(dash: Double, circleSegments n: Int = 64) -> [SIMD2<Double>] {
        var solid: [SIMD2<Double>] = []
        for e in entities where e.construction {
            switch e.kind {
            case .point: break
            case .line:
                if let a = positions[e.points[0]], let b = positions[e.points[1]] { solid += [a, b] }
            case .circle:
                if let c = positions[e.points[0]], let r = radii[e.handle] { solid += Self.ring(c, r, 0, 2 * .pi, n) }
            case .arc:
                if let st = positions[e.points[0]], let en = positions[e.points[1]], let c = positions[e.points[2]] {
                    let a0 = atan2(st.y - c.y, st.x - c.x)
                    var a1 = atan2(en.y - c.y, en.x - c.x)
                    if a1 <= a0 { a1 += 2 * .pi }
                    solid += Self.ring(c, simd_distance(c, st), a0, a1, 32)
                }
            }
        }
        return Self.dashed(solid, dash: dash)
    }

    /// Split segment pairs into dash pairs (dash on, dash off).
    static func dashed(_ pairs: [SIMD2<Double>], dash: Double) -> [SIMD2<Double>] {
        guard dash > 0 else { return pairs }
        var out: [SIMD2<Double>] = []
        var phase = 0.0
        for i in stride(from: 0, to: pairs.count - 1, by: 2) {
            let a = pairs[i], b = pairs[i + 1], l = simd_distance(a, b)
            guard l > 1e-12 else { continue }
            var t = 0.0
            while t < l {
                let inDash = phase.truncatingRemainder(dividingBy: 2 * dash) < dash
                let left = dash - phase.truncatingRemainder(dividingBy: dash)
                let step = min(left, l - t)
                if inDash { out += [a + (b - a) * (t / l), a + (b - a) * ((t + step) / l)] }
                t += step; phase += step
            }
        }
        return out
    }

    /// Closed regions as triangles (flat vertex list, three per triangle),
    /// for the live profile shading. Empty while any loop is open.
    func profileFill(arcSteps: Int = 24) -> [SIMD2<Double>] {
        guard case .success(let regions) = loopRegions() else { return [] }
        var out: [SIMD2<Double>] = []
        for r in regions {
            guard let segs = r["segments"] as? [[String: Any]] else { continue }
            let outer = Self.polyline(segs, arcSteps: arcSteps)
            let holes = ((r["holes"] as? [[[String: Any]]]) ?? []).map { Self.polyline($0, arcSteps: arcSteps) }
            let poly = SketchFill.bridge(outer: outer, holes: holes)
            for (a, b, c) in SketchFill.triangulate(poly) { out += [poly[a], poly[b], poly[c]] }
        }
        return out
    }

    /// A profile's segments (`loopRegions` JSON) as a closed polygon.
    static func polyline(_ segs: [[String: Any]], arcSteps: Int) -> [SIMD2<Double>] {
        func v(_ a: Any?) -> SIMD2<Double> {
            let d = a as? [String: Any]
            return SIMD2((d?["x"] as? NSNumber)?.doubleValue ?? 0, (d?["y"] as? NSNumber)?.doubleValue ?? 0)
        }
        var out: [SIMD2<Double>] = []
        for sgm in segs {
            let st = v(sgm["start"]), en = v(sgm["end"])
            if (sgm["type"] as? String) == "Arc" {
                let c = v(sgm["center"]), ccw = (sgm["ccw"] as? Bool) ?? true
                let rr = simd_distance(c, st)
                let a0 = atan2(st.y - c.y, st.x - c.x)
                var a1 = atan2(en.y - c.y, en.x - c.x)
                if ccw { if a1 <= a0 { a1 += 2 * .pi } } else { if a1 >= a0 { a1 -= 2 * .pi } }
                for k in 0..<arcSteps {
                    let t = a0 + (a1 - a0) * Double(k) / Double(arcSteps)
                    out.append(c + rr * SIMD2(cos(t), sin(t)))
                }
            } else {
                out.append(st)
            }
        }
        return out
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
            // Several glyphs on one line sit side by side along it.
            let k = Double(constraints.prefix { $0.index != c.index }.filter { $0.entities.first == h }.count)
            let u = l > 1e-9 ? t / l : .zero
            return (a + b) / 2 + n * 2.5 + u * (k * 3)
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

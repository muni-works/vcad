import XCTest
import simd
@testable import VcadApp

/// Pure sketch logic: inference while drawing, Smart Dimension, profile
/// fill, and the trim / extend / fillet / mirror geometry.
final class SketchInferenceTests: XCTestCase {
    private func model() -> SketchInference {
        var m = SketchInference()
        m.reach = 1
        // A horizontal line (handle 10) from (0,0) [1] to (10,0) [2].
        m.points = [(1, V2(0, 0)), (2, V2(10, 0))]
        m.lines = [.init(handle: 10, a: V2(0, 0), b: V2(10, 0), aH: 1, bH: 2)]
        return m
    }

    // MARK: inference

    func testHorizontalAndVerticalSnapFromAnchor() {
        let m = model()
        let h = m.infer(V2(20, 0.4), anchor: V2(10, 0), anchorH: 2)
        XCTAssertEqual(h.inferred, [.horizontal])
        XCTAssertEqual(h.point.y, 0, accuracy: 1e-9)
        let v = m.infer(V2(10.3, 8), anchor: V2(10, 0), anchorH: 2)
        XCTAssertEqual(v.inferred.first, .vertical)
        XCTAssertEqual(v.point.x, 10, accuracy: 1e-9)
    }

    func testCoincidentBeatsEverythingAndKeepsThePoint() {
        let m = model()
        let r = m.infer(V2(0.3, 0.2))
        XCTAssertEqual(r.snapped, 1)
        XCTAssertEqual(r.point, V2(0, 0))
    }

    func testMidpointSnap() {
        let r = model().infer(V2(5.2, 0.3))
        XCTAssertEqual(r.inferred, [.midpoint(10)])
        XCTAssertEqual(r.point, V2(5, 0))
    }

    func testOnLineWhenNothingStronger() {
        let r = model().infer(V2(7.5, 0.4))
        XCTAssertEqual(r.inferred, [.onLine(10)])
        XCTAssertEqual(r.point.y, 0, accuracy: 1e-9)
    }

    func testPerpendicularAndParallelToPreviousSegment() {
        var m = SketchInference()
        m.reach = 0.5
        // A 45° segment (handle 20) from (0,0) to (10,10).
        m.points = [(1, V2(0, 0)), (2, V2(10, 10))]
        m.lines = [.init(handle: 20, a: V2(0, 0), b: V2(10, 10), aH: 1, bH: 2)]
        let perp = m.infer(V2(15.1, 5), anchor: V2(10, 10), anchorH: 2, previous: 20)
        XCTAssertTrue(perp.inferred.contains(.perpendicular(20)), "\(perp.inferred)")
        XCTAssertEqual(simd_dot(perp.point - V2(10, 10), V2(1, 1)), 0, accuracy: 1e-9)
        let par = m.infer(V2(15, 15.1), anchor: V2(10, 10), anchorH: 2, previous: 20)
        XCTAssertTrue(par.inferred.contains(.parallel(20)), "\(par.inferred)")
    }

    func testEqualLengthToPreviousSegment() {
        let m = model()
        // Up from (10,0), roughly 10 long: vertical and equal to line 10.
        let r = m.infer(V2(10.1, 9.7), anchor: V2(10, 0), anchorH: 2, previous: 10)
        XCTAssertEqual(r.inferred, [.vertical, .equal(10)])
        XCTAssertEqual(r.point, V2(10, 10))
    }

    func testTangentFromArcEnd() {
        var m = SketchInference()
        m.reach = 0.5
        // Arc (handle 30) centred at the origin, radius 5, ending at (5,0) [3].
        m.points = [(3, V2(5, 0))]
        m.curves = [.init(handle: 30, center: .zero, radius: 5, ends: [4, 3])]
        let r = m.infer(V2(5.1, -1 + 8), anchor: V2(5, 0), anchorH: 3)
        // Straight up from (5,0) is both vertical and tangent: vertical wins
        // the direction, so check an off-axis tangent with a rotated arc.
        XCTAssertEqual(r.inferred.first, .vertical)
        m.points = [(3, V2(3, 4))]
        let t = m.infer(V2(3, 4) + V2(-4, 3) * 2 + V2(0.02, 0), anchor: V2(3, 4), anchorH: 3)
        XCTAssertTrue(t.inferred.contains(.tangent(30)), "\(t.inferred)")
    }

    // MARK: smart dimension

    func testSmartDimensionKinds() {
        let line = SketchPick.line(10, V2(0, 0), V2(3, 4))
        XCTAssertEqual(SmartDimension.plan(line), .init(kind: .length, e0: 10, value: 5))
        let circle = SketchPick.curve(11, center: 5, V2(0, 0), radius: 2, full: true)
        XCTAssertEqual(SmartDimension.plan(circle)?.kind, .diameter)
        XCTAssertEqual(SmartDimension.plan(circle)?.value, 4)
        let arc = SketchPick.curve(12, center: 6, V2(9, 9), radius: 2, full: false)
        XCTAssertEqual(SmartDimension.plan(arc)?.kind, .radius)
        XCTAssertNil(SmartDimension.plan(.point(1, .zero)))

        let d = SmartDimension.plan(.point(1, V2(0, 0)), .point(2, V2(6, 8)))
        XCTAssertEqual(d, .init(kind: .distance, e0: 1, e1: 2, value: 10))
        let pl = SmartDimension.plan(.point(1, V2(0, 5)), .line(10, V2(-1, 0), V2(1, 0)))
        XCTAssertEqual(pl?.kind, .pointLineDistance); XCTAssertEqual(pl?.value ?? 0, 5, accuracy: 1e-12)
        let ang = SmartDimension.plan(.line(10, V2(0, 0), V2(1, 0)), .line(13, V2(0, 0), V2(1, 1)))
        XCTAssertEqual(ang?.kind, .angle); XCTAssertEqual(ang?.value ?? 0, 45, accuracy: 1e-9)
        XCTAssertNil(SmartDimension.plan(.line(10, V2(0, 0), V2(1, 0)), .line(13, V2(0, 1), V2(1, 1))), "parallel")
        let cc = SmartDimension.plan(circle, arc)
        XCTAssertEqual(cc?.kind, .distance); XCTAssertEqual(cc?.e0, 5); XCTAssertEqual(cc?.e1, 6)
        XCTAssertEqual(SmartDimension.plan(line, line)?.kind, .length, "same entity twice = alone")
    }

    // MARK: fill

    func testTriangulationCoversTheArea() {
        let sq = [V2(0, 0), V2(10, 0), V2(10, 10), V2(0, 10)]
        XCTAssertEqual(triArea(sq), 100, accuracy: 1e-9)
        let l = [V2(0, 0), V2(10, 0), V2(10, 4), V2(4, 4), V2(4, 10), V2(0, 10)]
        XCTAssertEqual(triArea(l), 64, accuracy: 1e-9)
        XCTAssertEqual(triArea(l.reversed()), 64, accuracy: 1e-9, "either winding")
    }

    func testTriangulationWithAHole() {
        let outer = [V2(0, 0), V2(10, 0), V2(10, 10), V2(0, 10)]
        let hole = [V2(3, 3), V2(7, 3), V2(7, 7), V2(3, 7)]
        let poly = SketchFill.bridge(outer: outer, holes: [hole])
        XCTAssertEqual(triArea(poly), 100 - 16, accuracy: 1e-6)
    }

    private func triArea(_ poly: [V2]) -> Double {
        SketchFill.triangulate(poly).reduce(0) { acc, t in
            acc + abs(SketchFill.area([poly[t.0], poly[t.1], poly[t.2]]))
        }
    }

    // MARK: editing

    func testTrimKeepsTheOutsidePieces() {
        let cutters = [(V2(3, -1), V2(3, 1)), (V2(7, -1), V2(7, 1))]
        let keep = SketchEdit.trim(V2(0, 0), V2(10, 0), at: 0.5, cutters: cutters)
        XCTAssertEqual(keep.count, 2)
        XCTAssertEqual(keep[0].upperBound, 0.3, accuracy: 1e-9)
        XCTAssertEqual(keep[1].lowerBound, 0.7, accuracy: 1e-9)
        let end = SketchEdit.trim(V2(0, 0), V2(10, 0), at: 0.9, cutters: cutters)
        XCTAssertEqual(end.count, 1); XCTAssertEqual(end[0].upperBound, 0.7, accuracy: 1e-9)
        XCTAssertTrue(SketchEdit.trim(V2(0, 0), V2(10, 0), at: 0.5, cutters: []).isEmpty, "no crossings: whole line goes")
    }

    func testExtendToNextLine() {
        let wall = [(V2(20, -5), V2(20, 5))]
        let to = SketchEdit.extend(V2(0, 0), V2(10, 0), near: V2(9, 0), cutters: wall)
        XCTAssertEqual(to, V2(20, 0))
        XCTAssertNil(SketchEdit.extend(V2(0, 0), V2(10, 0), near: V2(1, 0), cutters: wall), "nothing behind")
    }

    func testFilletOfARightAngle() throws {
        let f = try XCTUnwrap(SketchEdit.fillet(corner: V2(0, 0), a: V2(10, 0), b: V2(0, 10), radius: 2))
        XCTAssertEqual(f.onA.x, 2, accuracy: 1e-9); XCTAssertEqual(f.onA.y, 0, accuracy: 1e-9)
        XCTAssertEqual(f.onB.x, 0, accuracy: 1e-9); XCTAssertEqual(f.onB.y, 2, accuracy: 1e-9)
        XCTAssertEqual(f.center.x, 2, accuracy: 1e-9); XCTAssertEqual(f.center.y, 2, accuracy: 1e-9)
        XCTAssertNil(SketchEdit.fillet(corner: .zero, a: V2(1, 0), b: V2(0, 1), radius: 5), "too big")
    }

    func testMirrorAndRotate() {
        XCTAssertEqual(SketchEdit.mirror(V2(3, 1), V2(0, 0), V2(0, 10)), V2(-3, 1))
        let r = SketchEdit.rotate(V2(1, 0), about: .zero, by: .pi / 2)
        XCTAssertEqual(r.x, 0, accuracy: 1e-12); XCTAssertEqual(r.y, 1, accuracy: 1e-12)
    }
}

/// The same pieces through a live solver session.
@MainActor
final class SketchToolsSessionTests: XCTestCase {
    private func session() throws -> SketchSession {
        let s = try XCTUnwrap(SketchSession(frame: .principal(.xy)))
        s.gridStep = 0
        return s
    }

    func testLineToolInfersAxesAndClosesARectangleUnderConstrainedNotOver() throws {
        let s = try session()
        s.tool = .line
        for p in [V2(0, 0), V2(20, 0.3), V2(20.2, 10), V2(0.1, 10.2), V2(0, 0)] { s.tap(p) }
        let kinds = s.constraints.map(\.kind)
        XCTAssertEqual(kinds.filter { $0 == .horizontal }.count, 2, "\(kinds)")
        XCTAssertEqual(kinds.filter { $0 == .vertical }.count, 2, "\(kinds)")
        XCTAssertFalse(s.isOverConstrained)
        XCTAssertEqual(s.profileFill().count, 6, "a closed rectangle shades as two triangles")
    }

    func testSmartDimensionPairsWithCommandClick() throws {
        let s = try session()
        s.tool = .rectangle; s.tap([0, 0]); s.tap([20, 10])
        // A slanted line from the top-right corner.
        let l = s.addLine(s.addPoint([30, 20]), s.addPoint([40, 20]))
        s.solve(); _ = l
        s.tool = .dimension
        s.tap([10, 0]); XCTAssertEqual(s.constraints.last?.kind, .length)
        let before = s.constraints.count
        s.tap([35, 20], additive: true)
        XCTAssertEqual(s.constraints.count, before + 1, "parallel lines don't pair: the second is sized")
        XCTAssertEqual(s.constraints.last?.kind, .length)
        s.tap([0, 5]); XCTAssertEqual(s.constraints.last?.kind, .length)
        s.tap([10, 10], additive: true)
        XCTAssertEqual(s.constraints.last?.kind, .angle)
        XCTAssertEqual(s.constraints.last?.value ?? 0, 90, accuracy: 1e-6)
        XCTAssertEqual(s.point(s.entities.first { $0.kind == .point }!.handle)?.y ?? 99, 0, accuracy: 1e-6, "nothing flipped")
        // Point, then line: point–line distance.
        s.tap([40, 20]); XCTAssertNotNil(s.dimensionAnchor, s.lastError ?? "")
        s.tap([10, 0])
        XCTAssertEqual(s.constraints.last?.kind, .pointLineDistance, s.lastError ?? "no error")
        XCTAssertEqual(s.constraints.last?.value ?? 0, 20, accuracy: 1e-6)
    }

    func testConstructionLeavesTheProfile() throws {
        let s = try session()
        s.tool = .rectangle; s.tap([0, 0]); s.tap([20, 10])
        s.tool = .line; s.suppressInference = true; s.tap([5, 5]); s.tap([15, 5]); s.commitPending()
        XCTAssertFalse(s.canFinish, "an open line blocks the profile")
        s.tool = .select; s.suppressInference = false
        s.tap([10, 5]); XCTAssertEqual(s.selection.count, 1)
        s.toggleConstruction()
        XCTAssertTrue(s.hasConstruction)
        XCTAssertTrue(s.canFinish, "construction never enters the profile")
        XCTAssertNotNil(s.profileJSON())
    }

    func testTrimSplitsALineAtCrossings() throws {
        let s = try session()
        let a = s.addPoint([0, 0]), b = s.addPoint([10, 0])
        s.addLine(a, b)
        for x in [3.0, 7.0] { s.addLine(s.addPoint([x, -2]), s.addPoint([x, 2])) }
        s.solve()
        s.tool = .trim
        s.tap([5, 0])
        let horizontal = s.entities.filter { e in
            e.kind == .line && e.points.compactMap { s.point($0)?.y }.allSatisfy { abs($0) < 1e-9 }
        }
        XCTAssertEqual(horizontal.count, 2)
    }

    func testFilletRoundsARectangleCorner() throws {
        let s = try session()
        s.tool = .rectangle; s.tap([0, 0]); s.tap([20, 10])
        s.tool = .fillet; s.filletRadius = 2
        s.tap([20, 10])
        XCTAssertEqual(s.entities.filter { $0.kind == .arc }.count, 1, s.lastError ?? "")
        XCTAssertFalse(s.isOverConstrained, s.stateLabel)
        XCTAssertTrue(s.canFinish || (try? s.loopRegions().get()) != nil)
    }

    func testDragKeepsConstraints() throws {
        let s = try session()
        s.tool = .rectangle; s.tap([0, 0]); s.tap([20, 10])
        s.tool = .select
        XCTAssertTrue(s.beginDrag([20, 10]))
        s.dragTo([30, 15])
        s.endDrag()
        let ys = s.entities.filter { $0.kind == .point }.compactMap { s.point($0.handle)?.y }
        // Still a rectangle: two distinct y values.
        XCTAssertEqual(Set(ys.map { ($0 * 1000).rounded() }).count, 2)
        XCTAssertTrue(s.undo())
    }

    func testMirrorAddsSymmetricCopies() throws {
        let s = try session()
        let axis = s.addLine(s.addPoint([0, -10]), s.addPoint([0, 10]))
        let l = s.addLine(s.addPoint([2, 0]), s.addPoint([5, 3]))
        s.solve()
        s.selection = [l, axis]
        s.mirrorSelection()
        XCTAssertEqual(s.constraints.filter { $0.kind == .symmetric }.count, 2)
        XCTAssertTrue(s.entities.contains { $0.kind == .point && s.point($0.handle).map { simd_distance($0, [-5, 3]) < 1e-6 } == true })
    }
}

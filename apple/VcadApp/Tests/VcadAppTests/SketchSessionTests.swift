import XCTest
import CVcadFFI
import simd
@testable import VcadApp

/// The constraint sketcher: tools build solver geometry, constraints change
/// DOF, driving dimensions move geometry, and every finish op authors a node.
@MainActor
final class SketchSessionTests: XCTestCase {
    private var url: URL!

    override func setUp() async throws {
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": ["1": ["id": 1, "name": "Block", "op": ["type": "Cube", "size": ["x": 40, "y": 40, "z": 10]]]],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 1, "material": "aluminum"]],
        ]
        url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
        try JSONSerialization.data(withJSONObject: doc).write(to: url)
    }
    override func tearDown() async throws { try? FileManager.default.removeItem(at: url) }

    private func open() throws -> EditorModel {
        let m = EditorModel()
        m.openDocument(url)
        XCTAssertTrue(m.canEditDocument, m.engineError ?? "no engine")
        return m
    }
    private func opTypes(_ m: EditorModel) -> [String] {
        let nodes = m.documentJSON?["nodes"] as? [String: Any] ?? [:]
        return nodes.values.compactMap { (($0 as? [String: Any])?["op"] as? [String: Any])?["type"] as? String }
    }

    private func volume(_ m: EditorModel) throws -> Double {
        let id = try XCTUnwrap(m.engineState?.parts.last?.id)
        let text = try XCTUnwrap(m.agentEnsureEngine()).agentRead("inspect", ["part_id": id])
        let range = try XCTUnwrap(text.range(of: #"volume [0-9.]+"#, options: .regularExpression))
        return try XCTUnwrap(Double(text[range].dropFirst(7)))
    }

    /// The top face of the block, picked the way a click picks it.
    private func pickTop(_ m: EditorModel) throws {
        let topo = try XCTUnwrap(m.partTopology(0))
        let top = try XCTUnwrap(topo.faces.max { $0.center[2] < $1.center[2] })
        m.picking.filter = .face
        m.picking.select(.face(top.id, part: 0), additive: false)
    }

    func testThroughHoleRemovesItsCylinder() throws {
        let m = try open()
        let before = try volume(m)
        try pickTop(m)
        XCTAssertTrue(m.applyHole(diameter: 6, depth: nil, offset: .zero, counterbore: nil), m.engineError ?? "")
        let height = 10.0
        XCTAssertEqual(before - (try volume(m)), .pi * 9 * height, accuracy: .pi * 9 * height * 0.02)
    }

    func testCounterboreIsOneUndoStep() throws {
        let m = try open()
        let before = try volume(m)
        try pickTop(m)
        XCTAssertTrue(m.applyHole(diameter: 3.2, depth: nil, offset: SIMD2(5, 2), counterbore: (6.5, 3.3)), m.engineError ?? "")
        let removed = Double.pi * (1.6 * 1.6 * (10 - 3.3) + 3.25 * 3.25 * 3.3)
        XCTAssertEqual(before - (try volume(m)), removed, accuracy: removed * 0.03)
        m.undo()
        XCTAssertEqual(try volume(m), before, accuracy: 1e-6)
    }

    func testBoltCircleCutsEveryHoleInOneUndoStep() throws {
        let m = try open()
        let before = try volume(m)
        try pickTop(m)
        XCTAssertTrue(m.applyHole(diameter: 3, depth: nil, offset: .zero, counterbore: nil, boltCircle: (4, 6)), m.engineError ?? "")
        let removed = 4 * Double.pi * 2.25 * 10
        XCTAssertEqual(before - (try volume(m)), removed, accuracy: removed * 0.03)
        m.undo()
        XCTAssertEqual(try volume(m), before, accuracy: 1e-6)
    }

    func testConvexHullReplacesTheSelectedParts() throws {
        let m = try open()
        XCTAssertTrue(m.addOp(.cube(sizeX: 10, sizeY: 10, sizeZ: 10), name: "Other"))
        m.multiSelectedParts = [0, 1]
        XCTAssertTrue(m.hullSelected(), m.engineError ?? "")
        XCTAssertEqual((m.documentJSON?["roots"] as? [[String: Any]])?.count, 1)
        XCTAssertEqual(try volume(m), 40 * 40 * 10, accuracy: 200)
    }

    func testDeletingALineKeepsTheRestAndItsConstraints() throws {
        let s = try XCTUnwrap(SketchSession(frame: .principal(.xy)))
        s.tool = .rectangle
        s.tap([0, 0]); s.tap([20, 10])
        let lines = s.entities.filter { $0.kind == .line }
        XCTAssertTrue(s.constrain(.length, lines[0].handle, value: 20))
        s.selection = [lines[2].handle]
        s.deleteSelection()
        XCTAssertEqual(s.entities.filter { $0.kind == .line }.count, 3)
        XCTAssertEqual(s.entities.filter { $0.kind == .point }.count, 4)
        XCTAssertEqual(s.constraints.filter { $0.kind == .length }.count, 1)
        XCTAssertEqual(s.constraints.filter { $0.kind == .horizontal }.count, 1)
        XCTAssertTrue(s.selection.isEmpty)

        // The rebuilt solver still drives geometry.
        let row = try XCTUnwrap(s.constraints.first { $0.kind == .length })
        s.setDimension(row, value: 35)
        let line = try XCTUnwrap(s.entities.first { $0.handle == row.entities[0] })
        let a = try XCTUnwrap(s.point(line.points[0])), b = try XCTUnwrap(s.point(line.points[1]))
        XCTAssertEqual(simd_distance(a, b), 35, accuracy: 1e-6)
    }

    func testDeletingACornerRemovesItsLinesAndOrphanPoints() throws {
        let s = try XCTUnwrap(SketchSession(frame: .principal(.xy)))
        s.tool = .line
        s.tap([0, 0]); s.tap([20, 0])
        s.commitPending()
        let line = try XCTUnwrap(s.entities.first { $0.kind == .line })
        s.selection = [line.points[0]]
        s.deleteSelection()
        XCTAssertTrue(s.isEmpty)
        XCTAssertTrue(s.entities.isEmpty)
    }

    func testRectangleIsClosedAndDimensionsDriveIt() throws {
        let s = try XCTUnwrap(SketchSession(frame: .principal(.xy)))
        s.tool = .rectangle
        s.tap([0, 0]); s.tap([20, 10])
        XCTAssertEqual(s.entities.filter { $0.kind == .line }.count, 4)
        XCTAssertEqual(s.constraints.filter { $0.kind == .horizontal }.count, 2)
        XCTAssertTrue(s.canFinish, s.lastError ?? "")
        let dofBefore = s.dof
        XCTAssertGreaterThan(dofBefore, 0)

        // Dimension the bottom edge, then drive it to 30.
        s.tool = .dimension
        s.tap([10, 0])
        let dim = try XCTUnwrap(s.constraints.last)
        XCTAssertEqual(dim.kind, .length)
        XCTAssertEqual(dim.value, 20, accuracy: 1e-6)
        XCTAssertLessThan(s.dof, dofBefore)
        s.setDimension(dim, value: 30)
        let bottom = try XCTUnwrap(s.entities.first { $0.kind == .line })
        let a = try XCTUnwrap(s.point(bottom.points[0])), b = try XCTUnwrap(s.point(bottom.points[1]))
        XCTAssertEqual(simd_distance(a, b), 30, accuracy: 1e-4)
    }

    func testCircleArcAndSplineTools() throws {
        let s = try XCTUnwrap(SketchSession(frame: .principal(.xy)))
        s.tool = .circle
        s.tap([0, 0]); s.tap([5, 0])
        XCTAssertEqual(s.entities.filter { $0.kind == .circle }.count, 1)
        XCTAssertTrue(s.canFinish, "a lone circle is a profile")
        s.tool = .arc
        s.tap([30, 0]); s.tap([35, 0]); s.tap([30, 5])
        XCTAssertEqual(s.entities.filter { $0.kind == .arc }.count, 1)
        s.tool = .spline
        s.tap([0, 20]); s.tap([10, 25]); s.tap([20, 20])
        s.commitPending()
        XCTAssertGreaterThan(s.entities.filter { $0.kind == .line }.count, 8)
    }

    // MARK: rings (an outer loop with holes)

    private func ring(_ m: EditorModel, center c: SIMD2<Float>, outer: SIMD2<Float>, inner: SIMD2<Float>) {
        m.setSketchTool(.rectangle)
        m.sketchTap(c - outer / 2); m.sketchTap(c + outer / 2)
        m.sketchTap(c - inner / 2); m.sketchTap(c + inner / 2)
    }

    /// The lip sketch from the mouse report: 58×38 around 56×36, same center.
    /// Finish was disabled with no word why; now the inner loop is a hole and
    /// a 2 mm extrude is exactly (58·38 − 56·36)·2 = 376 mm³.
    func testRingSketchExtrudesWithItsHole() throws {
        let m = try open()
        m.enterSketch()
        ring(m, center: [100, 100], outer: [58, 38], inner: [56, 36])
        XCTAssertEqual(m.sketch?.entities.filter { $0.kind == .line }.count, 8)
        m.sketchFinish = .extrude; m.sketchExtrudeDepth = 2
        XCTAssertNil(m.sketchFinishBlocker)
        XCTAssertTrue(m.canFinishSketch)
        let regions = try XCTUnwrap(try? m.sketch?.loopRegions().get())
        XCTAssertEqual(regions.count, 1)
        XCTAssertEqual((regions[0]["holes"] as? [Any])?.count, 1)
        XCTAssertTrue(m.finishSketch(), m.engineError ?? "")
        XCTAssertEqual(try volume(m), 376, accuracy: 1e-6)
    }

    /// The same ring as a Cut Extrude into the block's top face removes only
    /// the ring: (30·30 − 26·26)·2 = 448 mm³ out of 40·40·10.
    func testRingSketchCutsOnlyTheRing() throws {
        let m = try open()
        try pickTop(m)
        m.enterSketch()
        XCTAssertNotNil(m.sketchFace)
        ring(m, center: [0, 0], outer: [30, 30], inner: [26, 26])
        m.sketchFinish = .cut; m.sketchExtrudeDepth = 2
        XCTAssertTrue(m.canFinishSketch, m.sketchFinishBlocker ?? "")
        XCTAssertTrue(m.finishSketch(), m.engineError ?? "")
        XCTAssertEqual(try volume(m), 16000 - 448, accuracy: 1e-6)
    }

    /// An island inside the hole is a region of its own: two extrudes.
    func testIslandInsideARingIsItsOwnRegion() throws {
        let m = try open()
        m.enterSketch()
        ring(m, center: [100, 100], outer: [40, 40], inner: [30, 30])
        m.sketchTap([95, 95]); m.sketchTap([105, 105])
        let regions = try XCTUnwrap(try? m.sketch?.loopRegions().get())
        XCTAssertEqual(regions.count, 2)
        XCTAssertEqual(regions.filter { $0["holes"] != nil }.count, 1)
    }

    /// When Finish must stay disabled it says why: crossing loops, an open
    /// chain, and a ring under an op that can't take holes.
    func testDisabledFinishReportsItsReason() throws {
        let m = try open()
        m.enterSketch(); m.sketchFinish = .extrude
        m.setSketchTool(.rectangle)
        m.sketchTap([100, 100]); m.sketchTap([120, 120])
        m.sketchTap([110, 110]); m.sketchTap([130, 130])
        XCTAssertFalse(m.canFinishSketch)
        XCTAssertTrue(m.sketchFinishBlocker?.contains("cross") == true, m.sketchFinishBlocker ?? "nil")
        XCTAssertFalse(m.finishSketch())
        XCTAssertTrue(m.engineError?.contains("cross") == true, m.engineError ?? "nil")
        m.exitSketch()

        m.enterSketch()
        m.setSketchTool(.line)
        m.sketchTap([0, 0]); m.sketchTap([20, 0]); m.sketchTap([20, 20])
        m.sketch?.commitPending()
        XCTAssertFalse(m.canFinishSketch)
        XCTAssertTrue(m.sketchFinishBlocker?.contains("open") == true, m.sketchFinishBlocker ?? "nil")
        m.exitSketch()

        m.enterSketch()
        ring(m, center: [100, 100], outer: [58, 38], inner: [56, 36])
        m.sketchFinish = .revolve
        XCTAssertFalse(m.canFinishSketch)
        XCTAssertTrue(m.sketchFinishBlocker?.contains("holes") == true, m.sketchFinishBlocker ?? "nil")
    }

    func testOffsetAddsAParallelLoop() throws {
        let s = try XCTUnwrap(SketchSession(frame: .principal(.xy)))
        s.tool = .rectangle
        s.tap([0, 0]); s.tap([20, 10])
        s.offsetProfile(by: 2)
        XCTAssertEqual(s.entities.filter { $0.kind == .line }.count, 8)
        let pts = s.entities.filter { $0.kind == .point }.compactMap { s.point($0.handle) }
        XCTAssertTrue(pts.contains { abs($0.x + 2) < 1e-6 && abs($0.y + 2) < 1e-6 }, "outer corner at (-2,-2): \(pts)")
    }

    func testFinishOpsAuthorNodes() throws {
        let m = try open()
        func rect(_ x0: Float, _ y0: Float, _ x1: Float, _ y1: Float) {
            m.enterSketch(); m.setSketchTool(.rectangle)
            m.sketchTap([x0, y0]); m.sketchTap([x1, y1])
        }
        rect(50, 0, 60, 10); m.sketchFinish = .extrude
        XCTAssertTrue(m.finishSketch(), m.engineError ?? ""); XCTAssertTrue(opTypes(m).contains("Extrude"))
        rect(5, 5, 10, 20); m.sketchFinish = .revolve; m.sketchRevolveAngle = 180
        XCTAssertTrue(m.finishSketch(), m.engineError ?? ""); XCTAssertTrue(opTypes(m).contains("Revolve"))
        m.selectedFeatureID = m.featureNodes.first?.id
        rect(10, 10, 20, 20); m.sketchFinish = .cut
        XCTAssertTrue(m.finishSketch(), m.engineError ?? ""); XCTAssertTrue(opTypes(m).contains("Difference"))
        rect(0, 0, 5, 5); m.sketchFinish = .sweep; m.sweepTurns = 0
        XCTAssertTrue(m.finishSketch(), m.engineError ?? ""); XCTAssertTrue(opTypes(m).contains("Sweep"))
        // Loft: first Finish banks a section and starts the next sketch.
        rect(-10, -10, 0, 0); m.sketchFinish = .loft
        XCTAssertTrue(m.finishSketch()); XCTAssertTrue(m.sketching)
        m.setSketchTool(.rectangle); m.sketchTap([-8, -8]); m.sketchTap([-2, -2])
        XCTAssertTrue(m.finishSketch(), m.engineError ?? ""); XCTAssertTrue(opTypes(m).contains("Loft"))
        XCTAssertFalse(m.sketching)
    }

    // MARK: helix sweep

    /// A 3 mm circle drawn far from the sketch origin still rides the helix
    /// (its centroid is the anchor): radius 20, 1 turn over 50 mm gives
    /// π·3²·√((2π·20)² + 50²) ≈ 3,823.6 mm³ (the kernel's 16-gon profile
    /// tessellation sits ~2.5% under the true circle).
    func testHelixSweepAnchorsProfileCentroidOnPath() throws {
        let m = try open()
        m.enterSketch(); m.setSketchTool(.circle)
        m.sketchTap([60, 60]); m.sketchTap([63, 60])
        m.sketchFinish = .sweep
        m.sweepTurns = 1; m.sweepHelixRadius = 20; m.sweepLength = 50
        XCTAssertTrue(m.finishSketch(), m.engineError ?? "")
        let nodes = m.documentJSON?["nodes"] as? [String: Any] ?? [:]
        let ops = nodes.values.compactMap { ($0 as? [String: Any])?["op"] as? [String: Any] }
        let path = try XCTUnwrap(ops.first { $0["type"] as? String == "Sweep" }?["path"] as? [String: Any])
        XCTAssertEqual(path["radius"] as? Double, 20)
        XCTAssertEqual(path["height"] as? Double, 50)
        XCTAssertEqual(path["pitch"] as? Double, 50)
        let expected = Double.pi * 9 * ((2 * Double.pi * 20) * (2 * Double.pi * 20) + 2500).squareRoot()
        let v = try volume(m)
        XCTAssertEqual(v, expected * 0.9745, accuracy: expected * 0.01, "volume \(v) vs \(expected)")
    }

    /// A square profile has no tessellation loss: volume = area × helix length within 1%.
    func testHelixSweepSquareVolume() throws {
        let m = try open()
        m.enterSketch(); m.setSketchTool(.rectangle)
        m.sketchTap([70, 70]); m.sketchTap([74, 74])
        m.sketchFinish = .sweep
        m.sweepTurns = 1; m.sweepHelixRadius = 20; m.sweepLength = 50
        XCTAssertTrue(m.finishSketch(), m.engineError ?? "")
        let expected = 16 * ((2 * Double.pi * 20) * (2 * Double.pi * 20) + 2500).squareRoot()
        let v = try volume(m)
        XCTAssertEqual(v, expected, accuracy: expected * 0.01, "volume \(v) vs \(expected)")
    }

    /// The reported 40×30 rectangle at radius 20 crosses the axis: refused, plainly.
    func testHelixSweepRefusesProfileWiderThanRadius() throws {
        let m = try open()
        m.enterSketch(); m.setSketchTool(.rectangle)
        m.sketchTap([0, 0]); m.sketchTap([40, 30])
        m.sketchFinish = .sweep
        m.sweepTurns = 2; m.sweepHelixRadius = 20; m.sweepLength = 100
        XCTAssertFalse(m.finishSketch())
        XCTAssertTrue(m.engineError?.contains("helix radius") == true, m.engineError ?? "")
        XCTAssertTrue(m.sketching, "the sketch stays open to fix")
        XCTAssertFalse(opTypes(m).contains("Sweep"))
    }

    func testSweepPlanChecks() {
        func square(_ x0: Double, _ y0: Double, _ s: Double) -> [String: Any] {
            let pts = [(x0, y0), (x0 + s, y0), (x0 + s, y0 + s), (x0, y0 + s)]
            let segs: [[String: Any]] = (0..<4).map { i in
                let a = pts[i], b = pts[(i + 1) % 4]
                return ["type": "Line", "start": ["x": a.0, "y": a.1], "end": ["x": b.0, "y": b.1]]
            }
            return ["origin": ["x": 0, "y": 0, "z": 0], "segments": segs]
        }
        let ok = SweepPlan(profile: square(10, 20, 4), turns: 3, radius: 20, length: 30)
        XCTAssertNil(ok.problem)
        XCTAssertEqual(ok.centroid.x, 12, accuracy: 1e-9); XCTAssertEqual(ok.centroid.y, 22, accuracy: 1e-9)
        XCTAssertEqual(ok.pitch, 10, accuracy: 1e-9)
        let first = (ok.profile["segments"] as? [[String: Any]])?.first?["start"] as? [String: Double]
        XCTAssertEqual(first?["x"] ?? .nan, -2, accuracy: 1e-9, "re-centred on the centroid")
        // Turns crowd each other: 4 mm profile, 3 mm pitch.
        let tight = SweepPlan(profile: square(0, 0, 4), turns: 10, radius: 20, length: 30)
        XCTAssertTrue(tight.problem?.contains("apart") == true, tight.problem ?? "")
        // Straight sweep: left as drawn, no helix checks.
        let line = SweepPlan(profile: square(0, 0, 40), turns: 0, radius: 1, length: 30)
        XCTAssertNil(line.problem)
        let lineFirst = (line.profile["segments"] as? [[String: Any]])?.first?["start"] as? [String: Double]
        XCTAssertEqual(lineFirst?["x"], 0)
    }

    func testSketchOnPickedFace() throws {
        let m = try open()
        m.picking.filter = .face
        let hit = try XCTUnwrap(m.pickTopology(part: 0, origin: [20, 20, 50], dir: [0, 0, -1]))
        m.picking.select(hit.ref, additive: false)
        m.enterSketch()
        let frame = try XCTUnwrap(m.sketch?.frame)
        XCTAssertEqual(frame.origin.z, 10, accuracy: 1e-6, "sketch sits on the top face")
        XCTAssertEqual(frame.normal.z, 1, accuracy: 1e-6)
    }

    // MARK: sketch friction

    /// Cmd-Z inside a sketch walks the sketch's history and never removes a
    /// document feature, even once the sketch has nothing left to undo.
    func testUndoInSketchIsScopedToTheSketch() throws {
        let m = try open()
        try pickTop(m)
        XCTAssertTrue(m.applyHole(diameter: 6, depth: nil, offset: .zero, counterbore: nil), m.engineError ?? "")
        let features = opTypes(m).sorted()
        let withHole = try volume(m)
        m.picking.clear()
        m.enterSketch(); m.setSketchTool(.rectangle)
        m.sketchTap([50, 0]); m.sketchTap([60, 10])
        let sketch = try XCTUnwrap(m.sketch)
        XCTAssertEqual(sketch.entities.filter { $0.kind == .line }.count, 4)
        XCTAssertTrue(m.canUndo)
        m.undo()
        XCTAssertTrue(sketch.isEmpty, "undo removed the rectangle")
        XCTAssertEqual(opTypes(m).sorted(), features)
        // Nothing left in the sketch: undo again is a no-op, not a document undo.
        XCTAssertFalse(m.canUndo)
        m.undo(); m.undo()
        XCTAssertEqual(opTypes(m).sorted(), features, "document kept every feature")
        XCTAssertEqual(try volume(m), withHole, accuracy: 1e-6)
        // Redo brings the rectangle back.
        XCTAssertTrue(m.canRedo)
        m.redo()
        XCTAssertEqual(sketch.entities.filter { $0.kind == .line }.count, 4)
        XCTAssertEqual(sketch.constraints.count, 4)
        // An in-progress click is the first thing undo drops.
        m.sketchTap([70, 0])
        XCTAssertTrue(sketch.hasInProgress)
        m.undo()
        XCTAssertFalse(sketch.hasInProgress)
        XCTAssertEqual(sketch.entities.filter { $0.kind == .line }.count, 4)
        // Out of the sketch, undo reaches the document again.
        m.exitSketch()
        m.undo()
        XCTAssertGreaterThan(try volume(m), withHole, "document undo removed the hole")
    }

    func testSketchUndoRestoresDeletedGeometry() throws {
        let s = try XCTUnwrap(SketchSession(frame: .principal(.xy)))
        s.tool = .rectangle
        s.tap([0, 0]); s.tap([10, 5])
        let line = try XCTUnwrap(s.entities.first { $0.kind == .line })
        s.selection = [line.handle]
        s.deleteSelection()
        XCTAssertEqual(s.entities.filter { $0.kind == .line }.count, 3)
        XCTAssertTrue(s.undo())
        XCTAssertEqual(s.entities.filter { $0.kind == .line }.count, 4)
        XCTAssertEqual(s.constraints.count, 4)
        XCTAssertTrue(s.canFinish, "restored rectangle is a closed profile")
    }

    /// Two corners on the same x (or y) are not a rectangle: no degenerate
    /// line, no constraint on a refused entity, no raw solver text.
    func testZeroWidthRectangleKeepsWaitingForAValidCorner() throws {
        let s = try XCTUnwrap(SketchSession(frame: .principal(.xy)))
        s.tool = .rectangle
        for second in [SIMD2<Double>(0, 10), SIMD2<Double>(10, 0)] {
            s.tap([0, 0]); s.tap(second)
            XCTAssertTrue(s.hasInProgress, "still waiting for the opposite corner")
            XCTAssertFalse(s.entities.contains { $0.kind == .line })
            XCTAssertTrue(s.constraints.isEmpty)
            let err = try XCTUnwrap(s.lastError)
            XCTAssertFalse(err.contains("handle") || err.contains("add_constraint"), err)
            s.cancelPending()
        }
        s.tap([0, 0]); s.tap([10, 10])
        XCTAssertEqual(s.entities.filter { $0.kind == .line }.count, 4)
        XCTAssertEqual(s.constraints.count, 4)
        XCTAssertNil(s.lastError)
    }

    func testConstraintOnMissingEntityIsRefusedPlainly() throws {
        let s = try XCTUnwrap(SketchSession(frame: .principal(.xy)))
        XCTAssertFalse(s.constrain(.horizontal, 0))
        XCTAssertFalse(s.constrain(.horizontal, 999))
        let err = try XCTUnwrap(s.lastError)
        XCTAssertFalse(err.contains("handle"), err)
        XCTAssertTrue(s.constraints.isEmpty)
    }

    /// With a drawing tool armed, a click places geometry: it never moves the
    /// sketch onto a face. Only the Select tool may pick a face for an untouched sketch.
    func testArmedToolNeverRetargetsTheSketchPlane() throws {
        let m = try open()
        m.enterSketch()
        for t in SketchToolKind.allCases where t != .select {
            m.setSketchTool(t)
            XCTAssertFalse(m.sketchAwaitsPlane, "\(t) click would move the sketch")
        }
        m.setSketchTool(.select)
        XCTAssertTrue(m.sketchAwaitsPlane)
        // Adopting with Select still works and lands on the face.
        try pickTop(m)
        XCTAssertTrue(m.adoptPickedFaceAsSketchPlane())
        XCTAssertNotNil(m.sketchFace)
        XCTAssertEqual(try XCTUnwrap(m.sketch?.frame).origin.z, 10, accuracy: 1e-6)
    }

    // MARK: sketch bar (the dock while sketching)

    /// Every tool is reachable — five on the bar, three behind the chevron,
    /// Dimension beside Constrain — and each tooltip names its shortcut.
    func testSketchBarCarriesEveryTool() {
        let bar = SketchBar.primaryTools + SketchBar.overflowTools + [.dimension] + SketchBar.editTools
        XCTAssertEqual(SketchBar.primaryTools, [.select, .line, .rectangle, .circle, .spline])
        XCTAssertEqual(SketchBar.overflowTools, [.polygon, .slot, .arc])
        XCTAssertEqual(Set(bar), Set(SketchToolKind.allCases))
        XCTAssertEqual(bar.count, SketchToolKind.allCases.count, "no tool twice")
        let keys = SketchToolKind.allCases.compactMap(\.shortcut)
        XCTAssertEqual(keys.count, SketchToolKind.allCases.count)
        XCTAssertEqual(Set(keys).count, keys.count, "shortcuts are unique")
        XCTAssertEqual(SketchToolKind.line.tooltip, "Line  L")
    }

    func testSketchKeysPickTools() throws {
        let m = try open()
        m.enterSketch()
        XCTAssertTrue(GlassUI.shared.handleKey("r", model: m))
        XCTAssertEqual(m.sketch?.tool, .rectangle)
        XCTAssertTrue(GlassUI.shared.handleKey("C", model: m))
        XCTAssertEqual(m.sketch?.tool, .circle)
        XCTAssertFalse(GlassUI.shared.handleKey("z", model: m), "unbound keys still beep")
        m.exitSketch()
    }

    func testFeatureButtonTitle() throws {
        let m = try open()
        m.enterSketch()
        m.sketchFinish = .extrude; m.sketchExtrudeDepth = 10
        XCTAssertEqual(SketchBar.featureTitle(m), "Extrude 10 mm")
        m.sketchFinish = .cut; m.sketchExtrudeDepth = 2.5
        XCTAssertEqual(SketchBar.featureTitle(m), "Cut Extrude 2.5 mm")
        m.sketchFinish = .revolve; m.sketchRevolveAngle = 180
        XCTAssertEqual(SketchBar.featureTitle(m), "Revolve 180°")
        m.sketchFinish = .sweep
        XCTAssertEqual(SketchBar.featureTitle(m), "Sweep")
        m.sketchFinish = .loft
        XCTAssertEqual(SketchBar.doneTitle(m), "Add Section")
        m.sketchFinish = .extrude
        XCTAssertEqual(SketchBar.doneTitle(m), "Done")
    }

    /// The hint pill is gone: the tool prompt and the DOF are the status
    /// field's second line while sketching, and nothing outside a sketch.
    func testSketchStatusLine() throws {
        let m = try open()
        XCTAssertNil(m.sketchStatusLine())
        m.enterSketch()
        m.setSketchTool(.line)
        let line = try XCTUnwrap(m.sketchStatusLine())
        XCTAssertTrue(line.hasPrefix(SketchToolKind.line.hint), line)
        XCTAssertTrue(line.hasSuffix(try XCTUnwrap(m.sketch?.stateLabel)), line)
        m.exitSketch()
        XCTAssertNil(m.sketchStatusLine())
    }

    /// Drawing shows the snapped cursor first, then the short prompt and DOF.
    func testSketchStatusLineLeadsWithSnappedCursor() throws {
        let m = try open()
        m.enterSketch()
        m.setSketchTool(.line)
        m.sketch?.gridStep = 1
        m.sketchCursor = [12.3, -3.8]
        let dof = try XCTUnwrap(m.sketch?.stateLabel)
        XCTAssertEqual(m.sketchStatusLine(), "x 12.0, y -4.0 mm · Click points · \(dof)")
        m.sketchCursor = [0.2, -0.2]
        XCTAssertEqual(m.sketchStatusLine(), "x 0.0, y 0.0 mm · Click points · \(dof)")
        m.setSketchTool(.select)
        XCTAssertFalse(m.sketchStatusLine()?.contains("mm ·") ?? true, "no coords while selecting")
        for t in SketchToolKind.allCases {
            XCTAssertLessThanOrEqual(t.shortHint.count, 22, t.label)
        }
        m.exitSketch()
    }

    func testShortcutSheetListsSketchKeys() {
        XCTAssertEqual(ShortcutSheet.sketch.map(\.key), ["S", "L", "R", "C", "N", "P", "O", "A", "D"])
    }

    /// A blocked Done is clickable: it authors nothing, raises no error, and
    /// puts the reason in the status field — on click (for a while) and on hover.
    func testDoneWithBlockerSaysWhyAndAuthorsNothing() throws {
        let m = try open()
        m.enterSketch(); m.sketchFinish = .extrude
        m.setSketchTool(.rectangle)
        m.sketchTap([100, 100]); m.sketchTap([120, 120])
        m.sketchTap([110, 110]); m.sketchTap([130, 130])
        let why = try XCTUnwrap(m.sketchFinishBlocker)
        let before = opTypes(m).count
        let t0 = Date()
        XCTAssertFalse(m.sketchStatusLine(at: t0)?.contains(why) ?? true, "not until asked")

        XCTAssertFalse(m.attemptFinishSketch(now: t0))
        XCTAssertEqual(opTypes(m).count, before, "no feature created")
        XCTAssertTrue(m.sketching, "still sketching")
        XCTAssertNil(m.engineError)
        XCTAssertEqual(m.sketchStatusLine(at: t0.addingTimeInterval(1)), "Can't finish — \(why)")
        XCTAssertFalse(m.sketchStatusLine(at: t0.addingTimeInterval(10))?.contains(why) ?? true, "fades")

        m.sketchDoneHovered = true
        XCTAssertEqual(m.sketchStatusLine(at: t0.addingTimeInterval(10)), "Can't finish — \(why)")
        m.sketchDoneHovered = false
        m.exitSketch()

        // Unblocked, the same button finishes.
        m.enterSketch(); m.setSketchTool(.rectangle)
        m.sketchTap([50, 0]); m.sketchTap([60, 10])
        XCTAssertTrue(m.attemptFinishSketch(), m.engineError ?? "")
        XCTAssertGreaterThan(opTypes(m).count, before)
    }
}

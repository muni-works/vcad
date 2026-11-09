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
}

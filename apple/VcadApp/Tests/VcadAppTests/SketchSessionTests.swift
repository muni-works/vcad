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

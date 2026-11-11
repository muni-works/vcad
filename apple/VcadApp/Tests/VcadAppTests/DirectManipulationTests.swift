import XCTest
@testable import VcadApp

/// Press-pull, feature handles and snapping drive real document features,
/// each drag landing as exactly one named undo step.
@MainActor
final class DirectManipulationTests: XCTestCase {
    private var url: URL!

    override func setUp() async throws {
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": [
                "1": ["id": 1, "name": "Block", "op": ["type": "Cube", "size": ["x": 20, "y": 10, "z": 5]]],
                "2": ["id": 2, "name": "Pin", "op": ["type": "Cylinder", "radius": 3, "height": 12, "segments": 32]],
                "3": ["id": 3, "op": ["type": "Translate", "child": 2, "offset": ["x": 40, "y": 0, "z": 0]]],
            ],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 1, "material": "aluminum"], ["root": 3, "material": "steel"]],
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

    private func cube(_ m: EditorModel) -> [String: Any]? {
        let nodes = m.documentJSON?["nodes"] as? [String: Any] ?? [:]
        return nodes.values.compactMap { ($0 as? [String: Any])?["op"] as? [String: Any] }
            .first { $0["type"] as? String == "Cube" }
    }

    private func topFace(_ m: EditorModel, part: Int) throws -> TopoFace {
        let topo = try XCTUnwrap(m.partTopology(part))
        return try XCTUnwrap(topo.faces.filter { $0.isPlanar && $0.normalF.z > 0.99 }.max { $0.centerF.z < $1.centerF.z })
    }

    /// A ray from far along +X, at height `z`, looking back at the axis.
    private func ray(through p: SIMD3<Float>, z: Float) -> (o: SIMD3<Float>, d: SIMD3<Float>) {
        (SIMD3(p.x + 200, p.y, z), SIMD3(-1, 0, 0))
    }

    func testPressPullCubeFaceEditsSizeAsOneUndoStep() throws {
        let m = try open()
        m.selectParts([0])
        let f = try topFace(m, part: 0)
        m.picking.select(.face(f.id, part: 0), additive: false)
        XCTAssertNotNil(m.pullFace)
        XCTAssertEqual(m.gizmoHandle(for: "pull").map { _ in true }, true)
        let undoDepth = m.undoStack.count
        m.beginGizmoDrag(handle: "pull", ray: ray(through: f.centerF, z: f.centerF.z))
        for dz: Float in [2, 6, 10] { m.gizmoDragTo(ray: ray(through: f.centerF, z: f.centerF.z + dz)) }
        m.endGizmoDrag()
        let size = cube(m)?["size"] as? [String: Any]
        XCTAssertEqual((size?["z"] as? NSNumber)?.doubleValue ?? 0, 15, accuracy: 1e-6)
        XCTAssertEqual(m.undoStack.count, undoDepth + 1, "one drag, one undo step")
        XCTAssertEqual(m.undoStack.last?.name, "Press Pull")
        XCTAssertEqual(m.manipLabel?.text, "15 mm")
        m.undo()
        let back = cube(m)?["size"] as? [String: Any]
        XCTAssertEqual((back?["z"] as? NSNumber)?.doubleValue ?? 0, 5, accuracy: 1e-6)
    }

    func testTypedValueRewritesTheSameStep() throws {
        let m = try open()
        m.selectParts([0])
        let f = try topFace(m, part: 0)
        m.picking.select(.face(f.id, part: 0), additive: false)
        let undoDepth = m.undoStack.count
        m.beginGizmoDrag(handle: "pull", ray: ray(through: f.centerF, z: f.centerF.z))
        m.gizmoDragTo(ray: ray(through: f.centerF, z: f.centerF.z + 10))
        m.endGizmoDrag()
        m.commitManipValue(12.5)
        let size = cube(m)?["size"] as? [String: Any]
        XCTAssertEqual((size?["z"] as? NSNumber)?.doubleValue ?? 0, 12.5, accuracy: 1e-6)
        XCTAssertEqual(m.undoStack.count, undoDepth + 1)
    }

    func testPressPullOnCylinderCapAddsABossFeature() throws {
        let m = try open()
        m.selectParts([1])
        let f = try topFace(m, part: 1)
        m.picking.select(.face(f.id, part: 1), additive: false)
        m.beginGizmoDrag(handle: "pull", ray: ray(through: f.centerF, z: f.centerF.z))
        m.gizmoDragTo(ray: ray(through: f.centerF, z: f.centerF.z + 10))
        m.endGizmoDrag()
        XCTAssertNil(m.engineError)
        let roots = m.documentJSON?["roots"] as? [[String: Any]] ?? []
        let root = try XCTUnwrap((roots[1]["root"] as? NSNumber)?.intValue)
        let op = try XCTUnwrap(DocEdit.op(m.documentJSON ?? [:], nodeId: EditorModel.unwrapTransforms(m.documentJSON ?? [:], root)))
        XCTAssertEqual(op["type"] as? String, "Union")
        // The boss is editable afterwards: a depth handle on its extrude.
        m.selectParts([1])
        XCTAssertEqual(m.featureHandles().first?.name, "fh:extrude")
        XCTAssertEqual(m.undoStack.last?.name, "Press Pull")
    }

    func testChainLoopsJoinsUnorderedSegments() {
        let sq: [(SIMD2<Double>, SIMD2<Double>)] = [
            ([1, 0], [1, 1]), ([0, 0], [1, 0]), ([0, 1], [0, 0]), ([1, 1], [0, 1]),
        ]
        let loops = EditorModel.chainLoops(sq)
        XCTAssertEqual(loops.count, 1)
        XCTAssertEqual(abs(EditorModel.area(loops[0])), 1, accuracy: 1e-9)
    }

    func testMoveAlignsToNeighbourBoundingBox() {
        // Pin spans x 37…43, block 0…20: dragging the pin by -22.6 puts its
        // max at 20.4, beside the block's max — it snaps flush (-23).
        let d = EditorModel.alignDelta(-22.6, axis: 0, mine: (SIMD3(37, -3, 0), SIMD3(43, 3, 12)),
                                       others: [(SIMD3(0, 0, 0), SIMD3(20, 10, 5))], tol: 1)
        XCTAssertEqual(-22.6 + (d ?? 0), -23, accuracy: 1e-4)
        XCTAssertNil(EditorModel.alignDelta(-10, axis: 0, mine: (SIMD3(37, -3, 0), SIMD3(43, 3, 12)),
                                            others: [(SIMD3(0, 0, 0), SIMD3(20, 10, 5))], tol: 1))
    }
}

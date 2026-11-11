import XCTest
@testable import VcadApp

/// History editing in the tree: real suppress, the rollback marker, drag
/// reorder with legality checks, and broken-reference repair.
@MainActor
final class HistoryTreeTests: XCTestCase {
    private var urls: [URL] = []
    override func tearDown() async throws { urls.forEach { try? FileManager.default.removeItem(at: $0) } }

    private func open(_ doc: [String: Any]) throws -> EditorModel {
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
        try JSONSerialization.data(withJSONObject: doc).write(to: url)
        urls.append(url)
        let m = EditorModel()
        m.openDocument(url)
        XCTAssertTrue(m.canEditDocument, m.engineError ?? "no engine")
        return m
    }

    private static func v(_ x: Double, _ y: Double, _ z: Double) -> [String: Any] { ["x": x, "y": y, "z": z] }

    /// Plate (40×40×10) with a cylinder cut out of it, plus a free box.
    private func cutDoc() -> [String: Any] {
        [
            "version": "0.1",
            "nodes": [
                "1": ["id": 1, "name": "Plate", "op": ["type": "Cube", "size": Self.v(40, 40, 10)]],
                "2": ["id": 2, "name": "Hole", "op": ["type": "Cylinder", "radius": 5, "height": 30, "segments": 32]],
                "3": ["id": 3, "name": "Cut", "op": ["type": "Difference", "left": 1, "right": 2]],
                "4": ["id": 4, "name": "Spare", "op": ["type": "Cube", "size": Self.v(5, 5, 5)]],
            ],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 3, "material": "default"], ["root": 4, "material": "default"]],
        ]
    }

    private func row(_ m: EditorModel, _ name: String) throws -> HistoryRow {
        try XCTUnwrap(m.history.first { $0.displayName == name }, "\(m.history.map(\.displayName))")
    }

    func testHistoryListsEveryFeatureInOrder() throws {
        let m = try open(cutDoc())
        XCTAssertEqual(m.history.map(\.displayName), ["Plate", "Hole", "Cut", "Spare"])
        XCTAssertTrue(m.history.allSatisfy(\.isBuilt))
        XCTAssertEqual(try row(m, "Cut").inputs.count, 2)
        // Tree rows: the two scene roots, no ghosts, no marker.
        XCTAssertEqual(m.historyTreeItems.count, 2)
    }

    func testSuppressingTheToolSkipsTheCutAndBringsThePlateBack() throws {
        let m = try open(cutDoc())
        let hole = try row(m, "Hole").id
        m.setFeatureSuppressed(hole, true)
        XCTAssertNil(m.engineError)
        XCTAssertEqual(try row(m, "Hole").state, .suppressed)
        XCTAssertEqual(try row(m, "Cut").state, .skipped)
        XCTAssertEqual(try row(m, "Cut").skippedBy, hole)
        XCTAssertTrue(try row(m, "Plate").root, "the uncut plate is a part again")
        // Not hidden — not built: the document's roots are the plate and the spare.
        let roots = m.documentJSON?["roots"] as? [[String: Any]] ?? []
        XCTAssertEqual(roots.count, 2)
        let ghosts = m.historyTreeItems.compactMap { item -> String? in
            if case .ghost(let h) = item { return h.displayName } else { return nil }
        }
        XCTAssertEqual(ghosts, ["Hole", "Cut"])

        // Survives an IR reload (every IR edit reloads the engine).
        XCTAssertTrue(m.editIR("Touch") { _ in })
        XCTAssertEqual(try row(m, "Hole").state, .suppressed)
        XCTAssertEqual(try row(m, "Cut").state, .skipped)
        XCTAssertEqual(m.history.map(\.displayName), ["Plate", "Hole", "Cut", "Spare"], "order survives")

        m.undo(); m.undo()
        XCTAssertTrue(m.history.allSatisfy(\.isBuilt), "\(m.history.map(\.state))")
    }

    func testRollbackMarkerBuildsAPrefixAndRollsToEnd() throws {
        let m = try open(cutDoc())
        let plate = try row(m, "Plate").id
        m.rollBack(after: plate)
        XCTAssertNil(m.engineError)
        XCTAssertTrue(m.isRolledBack)
        XCTAssertEqual(m.history.map(\.state), [.built, .pastMarker, .pastMarker, .pastMarker])
        XCTAssertTrue(m.historyTreeItems.contains { if case .marker = $0 { return true } else { return false } })
        // Step forward one feature, then to the end.
        m.stepMarker(1)
        XCTAssertEqual(m.history.map(\.state), [.built, .built, .pastMarker, .pastMarker])
        m.rollBack(after: nil)
        XCTAssertFalse(m.isRolledBack)
        XCTAssertTrue(m.history.allSatisfy(\.isBuilt))
    }

    func testDragReorderRefusesMovingAFeatureAboveItsInput() throws {
        let m = try open(cutDoc())
        let cut = try row(m, "Cut").id, plate = try row(m, "Plate").id
        let spare = try row(m, "Spare").id
        XCTAssertNotNil(m.reorderProblem(moving: cut, before: plate))
        XCTAssertFalse(m.moveFeature(cut, before: plate))
        XCTAssertTrue(m.engineError?.contains("built on") == true, m.engineError ?? "")
        XCTAssertEqual(m.history.map(\.displayName), ["Plate", "Hole", "Cut", "Spare"], "refused move is a no-op")

        m.engineError = nil
        XCTAssertNil(m.reorderProblem(moving: spare, before: plate))
        XCTAssertTrue(m.moveFeature(spare, before: plate), m.engineError ?? "")
        XCTAssertEqual(m.history.map(\.displayName), ["Spare", "Plate", "Hole", "Cut"])
    }

    /// A pocket cut through a sketch attached to the box's top face.
    private func pocketDoc(face: String) -> [String: Any] {
        func p(_ x: Double, _ y: Double) -> [String: Any] { ["x": x, "y": y] }
        func line(_ a: [String: Any], _ b: [String: Any]) -> [String: Any] { ["type": "Line", "start": a, "end": b] }
        return [
            "version": "0.1",
            "nodes": [
                "1": ["id": 1, "name": "Box", "op": ["type": "Cube", "size": Self.v(40, 40, 10)]],
                "2": ["id": 2, "name": "Sketch", "op": ["type": "Sketch2D",
                    "origin": Self.v(15, 15, 10), "x_dir": Self.v(1, 0, 0), "y_dir": Self.v(0, 1, 0),
                    "segments": [line(p(0, 0), p(10, 0)), line(p(10, 0), p(10, 10)),
                                 line(p(10, 10), p(0, 10)), line(p(0, 10), p(0, 0))]]],
                "3": ["id": 3, "name": "On top", "op": ["type": "SketchOnFace", "sketch": 2, "target": 1,
                                                       "face": ["name": face]]],
                "4": ["id": 4, "name": "Tool", "op": ["type": "Extrude", "sketch": 3, "direction": Self.v(0, 0, -2)]],
                "5": ["id": 5, "name": "Pocket", "op": ["type": "Difference", "left": 1, "right": 4]],
            ],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 5, "material": "default"]],
        ]
    }

    func testBrokenFaceReferenceBadgesAndRepairsByRePicking() throws {
        let m = try open(pocketDoc(face: "n1:roof"))
        let pocket = try row(m, "Pocket")
        let broken = m.brokenRows(under: pocket)
        XCTAssertEqual(broken.count, 1, "the visible Pocket row badges its tool's lost face")
        let b = try XCTUnwrap(broken.first?.broken.first)
        XCTAssertTrue(b.message.contains("lost"), b.message)
        let top = try XCTUnwrap(b.candidates.first { $0.label == "top" }, "\(b.candidates.map(\.label))")

        XCTAssertTrue(m.repairFaceReference(node: b.node, with: top), m.engineError ?? "")
        XCTAssertTrue(m.brokenRows(under: try row(m, "Pocket")).isEmpty)
        XCTAssertEqual(m.newEvaluationErrors() ?? "", "", "the pocket builds again")
        // One undo step takes the repair back.
        m.undo()
        XCTAssertFalse(m.brokenRows(under: try row(m, "Pocket")).isEmpty)
    }

    func testAGoodFaceReferenceIsNotFlagged() throws {
        let m = try open(pocketDoc(face: "n1:top"))
        XCTAssertTrue(m.history.allSatisfy { $0.broken.isEmpty })
    }
}

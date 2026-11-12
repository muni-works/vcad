import XCTest
import simd
@testable import VcadApp

/// Cam's 1.0.0-beta.1 report: a whole-part fillet on a block with a boss
/// (Press Pull + Boss union) left the viewport empty with no message. The
/// kernel refused (concave inside-corner edges) and the refusal was dropped
/// on the way to the app. A simple block must fillet and render; a body the
/// kernel can't fillet must say why.
@MainActor
final class FilletRenderTests: XCTestCase {
    private func open(_ nodes: [String: Any], root: Int) throws -> EditorModel {
        let doc: [String: Any] = ["version": "0.1", "nodes": nodes, "materials": [:], "part_materials": [:],
                                  "roots": [["root": root, "material": "aluminum"]]]
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
        try JSONSerialization.data(withJSONObject: doc).write(to: url)
        addTeardownBlock { try? FileManager.default.removeItem(at: url) }
        let m = EditorModel()
        m.openDocument(url)
        XCTAssertTrue(m.canEditDocument, m.engineError ?? "no engine")
        m.selectedFeatureID = m.featureNodes.first?.id
        return m
    }

    private func fillet(_ m: EditorModel, _ r: Double) throws {
        let id = try XCTUnwrap(m.selectedPartFeatureId)
        XCTAssertTrue(m.addOp(.fillet(input: id, radius: r), name: "Fillet", materialFrom: id), m.engineError ?? "")
    }

    func testFilletedBlockRenders() throws {
        let m = try open(["1": ["id": 1, "name": "Block", "op": ["type": "Cube", "size": ["x": 30, "y": 97, "z": 55]]]],
                         root: 1)
        try fillet(m, 2)
        let meshes = try XCTUnwrap(m.reevalDocumentMeshes(), m.engineError ?? "no geometry")
        XCTAssertEqual(meshes.count, 1)
        XCTAssertGreaterThan(m.triangleCount, 1000, "fillet faces tessellated")
        XCTAssertEqual(m.sizeMM.x, 30, accuracy: 1e-3)
        XCTAssertEqual(m.sizeMM.y, 97, accuracy: 1e-3)
        XCTAssertEqual(m.sizeMM.z, 55, accuracy: 1e-3)
        let p = try XCTUnwrap(m.docPartMeshes.first)
        XCTAssertTrue(p.positions.allSatisfy { $0.x.isFinite && $0.y.isFinite && $0.z.isFinite })
        XCTAssertNil(m.engineError)
    }

    func testFilletOnBlockWithBossSaysWhy() throws {
        let nodes: [String: Any] = [
            "1": ["id": 1, "name": "Base", "op": ["type": "Cube", "size": ["x": 30, "y": 97, "z": 25]]],
            "2": ["id": 2, "name": "Boss", "op": ["type": "Cube", "size": ["x": 30, "y": 30, "z": 30]]],
            "3": ["id": 3, "name": "T", "op": ["type": "Translate", "child": 2, "offset": ["x": 0, "y": 0, "z": 25]]],
            "4": ["id": 4, "name": "Boss", "op": ["type": "Union", "left": 1, "right": 3]],
        ]
        let m = try open(nodes, root: 4)
        XCTAssertNotNil(m.reevalDocumentMeshes(), "the union itself renders")
        try fillet(m, 2)
        _ = m.reevalDocumentMeshes()
        let err = try XCTUnwrap(m.engineError, "a refused fillet must not blank the viewport silently")
        XCTAssertTrue(err.hasPrefix("Fillet failed:"), err)
        XCTAssertTrue(err.contains("inside-corner"), err)
        // Undo brings the part back and clears the message on the next solve.
        m.undo()
        XCTAssertNotNil(m.reevalDocumentMeshes())
    }

    func testReadablePartFailure() {
        XCTAssertEqual(EditorModel.readablePartFailure("fillet of node 1621737475 did not apply: nope"),
                       "Fillet failed: nope")
        XCTAssertEqual(EditorModel.readablePartFailure("edge blend of node 3 did not apply: x"),
                       "Edge blend failed: x")
        XCTAssertEqual(EditorModel.readablePartFailure("something else"), "something else")
    }
}

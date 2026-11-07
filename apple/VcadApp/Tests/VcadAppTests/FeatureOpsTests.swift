import XCTest
@testable import VcadApp

/// Every feature dialog op authors through the engine (or vcad_feature_append)
/// and undoes cleanly; topology picking returns stable face/edge ids.
@MainActor
final class FeatureOpsTests: XCTestCase {
    private var url: URL!

    override func setUp() async throws {
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": ["1": ["id": 1, "name": "Block", "op": ["type": "Cube", "size": ["x": 20, "y": 10, "z": 5]]]],
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
        m.selectedFeatureID = m.featureNodes.first?.id
        return m
    }
    private func roots(_ m: EditorModel) -> [[String: Any]] { m.documentJSON?["roots"] as? [[String: Any]] ?? [] }
    private func rootOp(_ m: EditorModel, _ i: Int = 0) -> String? {
        guard let id = m.rootNodeId(forPart: i),
              let nodes = m.documentJSON?["nodes"] as? [String: Any],
              let n = nodes[String(id)] as? [String: Any], let op = n["op"] as? [String: Any] else { return nil }
        return op["type"] as? String
    }
    /// Every node op type in the document.
    private func opTypes(_ m: EditorModel) -> Set<String> {
        let nodes = m.documentJSON?["nodes"] as? [String: Any] ?? [:]
        return Set(nodes.values.compactMap { (($0 as? [String: Any])?["op"] as? [String: Any])?["type"] as? String })
    }

    func testModifyOpsAuthorAndUndo() throws {
        let m = try open()
        let id = try XCTUnwrap(m.selectedPartFeatureId)
        let cases: [(FeatureInput, String)] = [
            (.fillet(input: id, radius: 1), "Fillet"),
            (.chamfer(input: id, distance: 1), "Chamfer"),
            (.shell(input: id, thickness: 1), "Shell"),
            (.linearPattern(input: id, direction: [1, 0, 0], count: 3, spacing: 25), "LinearPattern"),
            (.circularPattern(input: id, axisOrigin: [0, 0, 0], axisDir: [0, 0, 1], count: 4, angleDeg: 360), "CircularPattern"),
        ]
        for (feature, op) in cases {
            XCTAssertTrue(m.addOp(feature, name: op, materialFrom: id), "\(op): \(m.engineError ?? "")")
            XCTAssertTrue(opTypes(m).contains(op), "\(op) node missing: \(opTypes(m))")
            m.undo()
            XCTAssertFalse(opTypes(m).contains(op), "\(op) not undone")
        }
        // Mirror lowers to a reflecting transform: check the part changed.
        let before = m.documentJSON?["nodes"] as? [String: Any]
        XCTAssertTrue(m.addOp(.mirror(input: id, plane: "YZ"), name: "Mirror", materialFrom: id), m.engineError ?? "")
        XCTAssertGreaterThan((m.documentJSON?["nodes"] as? [String: Any])?.count ?? 0, before?.count ?? 0)
    }

    func testCreateOpsAddParts() throws {
        let m = try open()
        XCTAssertTrue(m.addOp(.cone(radiusBottom: 5, radiusTop: 0, height: 10, segments: 32), name: "Cone"))
        XCTAssertTrue(opTypes(m).contains("Cone"))
        XCTAssertTrue(m.addOp(.text(text: "Hi", height: 8, depth: 1, alignment: "left"), name: "Text"),
                      m.engineError ?? "")
        XCTAssertTrue(opTypes(m).contains("Text2D"))
        XCTAssertEqual(roots(m).count, 3)
    }

    func testScaleIsACommand() throws {
        let m = try open()
        let id = try XCTUnwrap(m.selectedPartFeatureId)
        XCTAssertTrue(m.runCommand(.setScale(id: id, factor: [2, 2, 2]), name: "Scale"), m.engineError ?? "")
        XCTAssertTrue(opTypes(m).contains("Scale"))
    }

    func testTopologyAndPickedEdgeBlend() throws {
        let m = try open()
        let topo = try XCTUnwrap(m.partTopology(0), "cube has a B-rep")
        XCTAssertEqual(topo.faces.count, 6)
        XCTAssertEqual(topo.edges.count, 12)
        // A ray straight down onto the top face, near its +X edge.
        m.picking.filter = .faceOrEdge
        let hit = try XCTUnwrap(m.pickTopology(part: 0, origin: [19.995, 5, 50], dir: [0, 0, -1]))
        XCTAssertEqual(hit.ref.kind, .edge)
        m.picking.filter = .face
        let face = try XCTUnwrap(m.pickTopology(part: 0, origin: [10, 5, 50], dir: [0, 0, -1]))
        XCTAssertEqual(face.ref.kind, .face)
        XCTAssertEqual(topo.face(face.ref.id)?.normalF.z ?? 0, 1, accuracy: 1e-4)

        // Fillet one picked edge: an EdgeBlend node, one undo step.
        m.picking.filter = .edge
        m.picking.select(hit.ref, additive: false)
        XCTAssertEqual(m.picking.selectedEdgeIDs(part: 0), [hit.ref.id])
        XCTAssertTrue(m.applyBlend(fillet: true, size: 1), m.engineError ?? "")
        XCTAssertTrue(opTypes(m).contains("EdgeBlend"), "\(opTypes(m))")
        XCTAssertTrue(m.picking.selection.isEmpty, "a B-rep edit clears the session ids")
        m.undo()
        XCTAssertFalse(opTypes(m).contains("EdgeBlend"))
    }
}

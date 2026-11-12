import XCTest
@testable import VcadApp

/// The Spur Gear tool: a parametric involute gear, editable in the inspector,
/// and a meshed second gear placed at the right centre distance and phase —
/// checked with the app's own Clearance & Interference analysis.
@MainActor
final class SpurGearTests: XCTestCase {
    private var url: URL!

    override func setUp() async throws {
        // A block well away from the gears, so the document is editable and
        // the clearance report has an unrelated part to ignore.
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": [
                "1": ["id": 1, "name": "Block", "op": ["type": "Cube", "size": ["x": 10, "y": 10, "z": 10]]],
                "2": ["id": 2, "name": "Move", "op": ["type": "Translate", "child": 1,
                                                      "offset": ["x": -300, "y": 0, "z": 0]]],
            ],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 2, "material": "default"]],
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

    static func roots(_ m: EditorModel) -> Int { (m.documentJSON?["roots"] as? [Any])?.count ?? 0 }

    private func gearNodes(_ m: EditorModel) throws -> [Int] {
        let nodes = try XCTUnwrap(m.documentJSON?["nodes"] as? [String: Any])
        return nodes.compactMap { k, v in
            ((v as? [String: Any])?["op"] as? [String: Any])?["type"] as? String == "SpurGear" ? Int(k) : nil
        }.sorted()
    }

    /// Viewport part index of the gear with this node.
    private func part(_ m: EditorModel, gear nodeId: Int) throws -> Int {
        try XCTUnwrap((0..<Self.roots(m)).first { m.placedSpurGear(part: $0)?.gearNodeId == nodeId },
                      "no part holds gear node \(nodeId)")
    }

    /// Kernel volume of the part owning gear node `nodeId`.
    private func volume(_ m: EditorModel, nodeId: Int) throws -> Double {
        let root = try XCTUnwrap(m.rootNodeId(forPart: try part(m, gear: nodeId)))
        let id = try XCTUnwrap(m.engineState?.owner(ofNode: root)?.part.id)
        let text = try XCTUnwrap(m.agentEnsureEngine()).agentRead("inspect", ["part_id": id])
        let range = try XCTUnwrap(text.range(of: #"volume [0-9.]+"#, options: .regularExpression))
        return try XCTUnwrap(Double(text[range].dropFirst(7)))
    }

    /// The gear pair in the clearance report.
    private func gearPair(_ m: EditorModel) throws -> ClearancePair {
        let data = try XCTUnwrap(AnalysisStore.documentData(m))
        let report = try AnalysisFFI.clearance(doc: data, includeMesh: false)
        return try XCTUnwrap(report.pairs.first { p in
            report.parts.indices.contains(p.a) && report.parts.indices.contains(p.b)
                && p.a_name.contains("Spur Gear") && p.b_name.contains("Spur Gear")
        }, "no gear pair in \(report.pairs.map { "\($0.a_name)/\($0.b_name)" })")
    }

    func testMathMatchesTheKernelFormulas() {
        XCTAssertEqual(SpurGearMath.centerDistance(module: 2, z1: 20, z2: 30), 50)
        // Pair along +X, gear 1 unturned: gear 2 shows a gap at 180°.
        XCTAssertEqual(SpurGearMath.meshRotation(z1: 20, rot1: 0, z2: 30, direction: 0), 180 - 6, accuracy: 1e-9)
        // Turning gear 1 by a whole tooth changes nothing.
        XCTAssertEqual(SpurGearMath.meshRotation(z1: 20, rot1: 18, z2: 30, direction: 0),
                       SpurGearMath.meshRotation(z1: 20, rot1: 0, z2: 30, direction: 0), accuracy: 1e-9)
        var p = SpurGearParams()
        XCTAssertNil(p.problem)
        p.bore = 40
        XCTAssertNotNil(p.problem)
    }

    func testSpurGearIsAPaletteToolAndAnEditableFeature() throws {
        let m = try open()
        XCTAssertTrue(m.paletteCommands.contains { $0.id == "op.spurGear" && $0.title.contains("Spur Gear") })
        XCTAssertTrue(FeatureOp.createOps.contains(.spurGear))
        XCTAssertTrue(FeatureParamEditors.editableOps.contains("SpurGear"))

        XCTAssertTrue(m.addSpurGear(SpurGearParams(module: 2, teeth: 20, pressureAngle: 20, faceWidth: 10, bore: 8)),
                      m.engineError ?? "")
        let g = try XCTUnwrap(try gearNodes(m).first)
        let v = try volume(m, nodeId: g)
        // Root disc + half the tooth band, minus the bore.
        let (rt, rr) = (22.0, 17.5)
        let est = (Double.pi * rr * rr + Double.pi * (rt * rt - rr * rr) / 2 - Double.pi * 16) * 10
        XCTAssertEqual(v, est, accuracy: est * 0.05)

        // Inspector edit: more teeth, more gear; undo puts it back.
        m.editInt(nodeId: g, key: "teeth", value: 30, snapshot: true, name: "Change Teeth")
        m.flushRuntime()
        let bigger = try volume(m, nodeId: try XCTUnwrap(try gearNodes(m).first))
        XCTAssertGreaterThan(bigger, v * 2)
        m.undo()
        XCTAssertEqual(try volume(m, nodeId: try XCTUnwrap(try gearNodes(m).first)), v, accuracy: 1e-6)
    }

    func testMeshWithSelectedGearClearsAndInterferesWhenCloser() throws {
        let m = try open()
        XCTAssertTrue(m.addSpurGear(SpurGearParams(module: 2, teeth: 20, pressureAngle: 20, faceWidth: 10, bore: 8)),
                      m.engineError ?? "")
        let mate = try XCTUnwrap((0..<Self.roots(m)).compactMap { m.placedSpurGear(part: $0) }.first, "the new gear should be recognised")
        XCTAssertEqual(mate.teeth, 20)
        // The dialog's module is ignored when meshing: the mate's wins.
        XCTAssertTrue(m.addSpurGear(SpurGearParams(module: 5, teeth: 30, pressureAngle: 25, faceWidth: 10, bore: 8),
                                    meshWith: mate), m.engineError ?? "")
        let nodes = try gearNodes(m)
        XCTAssertEqual(nodes.count, 2)
        let placed = try XCTUnwrap((0..<Self.roots(m)).compactMap { m.placedSpurGear(part: $0) }
            .first { $0.gearNodeId == nodes[1] })
        XCTAssertEqual(placed.module, 2)
        XCTAssertEqual(placed.pressureAngle, 20)
        XCTAssertEqual(placed.center.x, 50, accuracy: 1e-9)
        XCTAssertEqual(placed.rotationZ, 174, accuracy: 1e-9)

        let ok = try gearPair(m)
        XCTAssertFalse(ok.intersecting, "meshed gears interfere: \(ok.distance_mm) mm")
        XCTAssertGreaterThanOrEqual(ok.distance_mm, 0)
        XCTAssertLessThan(ok.distance_mm, 0.1, "meshed gears aren't in contact")

        // Pull the second gear 0.4 mm (0.2 module) closer: now they collide.
        let translate = try XCTUnwrap(m.rootNodeId(forPart: try part(m, gear: nodes[1])))
        m.editVec(nodeId: translate, key: "offset", axis: "x", value: -0.4, snapshot: true, name: "Move")
        m.flushRuntime()
        XCTAssertEqual(try XCTUnwrap((0..<Self.roots(m)).compactMap { m.placedSpurGear(part: $0) }
            .first { $0.gearNodeId == (try? gearNodes(m))?.last }).center.x, 49.6, accuracy: 1e-9)
        let bad = try gearPair(m)
        XCTAssertTrue(bad.intersecting || bad.distance_mm < 0, "0.2 m closer should interfere: \(bad.distance_mm)")
    }
}

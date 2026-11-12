import XCTest
@testable import VcadApp

/// The Model browser lists parts, named after themselves — never after the
/// last operation applied to them.
final class ModelBrowserTests: XCTestCase {
    private func graph(_ json: [String: Any]) throws -> DocumentGraph {
        try XCTUnwrap(DocumentGraph.parse(try JSONSerialization.data(withJSONObject: json)))
    }
    private func cube(_ id: Int, _ name: String? = nil) -> [String: Any] {
        var n: [String: Any] = ["id": id, "op": ["type": "Cube", "size": ["x": 10, "y": 10, "z": 10]]]
        if let name { n["name"] = name }
        return n
    }

    func testFilletedBoxStaysABox() throws {
        let g = try graph([
            "nodes": ["1": cube(1),
                      "2": ["id": 2, "name": "Fillet", "op": ["type": "Fillet", "child": 1, "radius": 1]]],
            "roots": [["root": 2]],
        ])
        let rows = g.featureRoots()
        XCTAssertEqual(rows.map(\.name), ["Box"])
        XCTAssertEqual(rows[0].partIndex, 0)
        XCTAssertEqual(ModelBrowser.items(rows).count, 1)
    }

    func testUserNameSurvivesOperationsStackedOnTop() throws {
        // Bracket → hole cut → fillet → identity transform wrapper.
        let g = try graph([
            "nodes": [
                "1": cube(1, "Bracket"),
                "2": ["id": 2, "name": "Hole", "op": ["type": "Cylinder", "radius": 2, "height": 20]],
                "3": ["id": 3, "op": ["type": "Difference", "left": 1, "right": 2]],
                "4": ["id": 4, "name": "Chamfer", "op": ["type": "Chamfer", "child": 3, "distance": 1]],
                "5": ["id": 5, "name": "Move", "op": ["type": "Translate", "child": 4,
                                                      "offset": ["x": 5, "y": 0, "z": 0]]],
            ],
            "roots": [["root": 5]],
        ])
        XCTAssertEqual(g.featureRoots().map(\.name), ["Bracket"])
    }

    func testRenamedRootWinsOverInnerName() throws {
        let g = try graph([
            "nodes": ["1": cube(1, "Blank"),
                      "2": ["id": 2, "name": "Lid", "op": ["type": "Fillet", "child": 1, "radius": 1]]],
            "roots": [["root": 2]],
        ])
        XCTAssertEqual(g.partName(root: 2), "Lid")
    }

    func testSketchDrivenBodyAndDuplicatesAreDisambiguated() throws {
        let g = try graph([
            "nodes": [
                "1": ["id": 1, "name": "Sketch", "op": ["type": "Sketch2D", "segments": []]],
                "2": ["id": 2, "name": "Extrude", "op": ["type": "Extrude", "sketch": 1,
                                                         "direction": ["x": 0, "y": 0, "z": 5]]],
                "3": cube(3), "4": cube(4),
            ],
            "roots": [["root": 2], ["root": 3], ["root": 4], ["root": 4, "visible": false]],
        ])
        XCTAssertEqual(g.featureRoots().map(\.name), ["Body", "Box", "Box 2"])
        XCTAssertEqual(g.sketchRows().map(\.name), ["Sketch 1"])
    }

    func testMaterialRidesOnTheRootRow() throws {
        let g = try graph([
            "nodes": ["1": cube(1)],
            "roots": [["root": 1, "material": "steel"]],
            "materials": ["steel": ["color": [0.5, 0.5, 0.5]]],
        ])
        XCTAssertEqual(g.featureRoots().first?.material, "steel")
    }

    func testRepeatedInstancesGroupUnderTheirPartDef() throws {
        let g = try graph([
            "nodes": ["1": cube(1, "Base"), "2": cube(2), "3": cube(3)],
            "roots": [["root": 1]],
            "partDefs": ["rotor": ["id": "rotor", "name": "Rotor", "root": 2],
                         "arm": ["id": "arm", "name": "Arm", "root": 3]],
            "instances": [["id": "r1", "partDefId": "rotor", "name": "Rotor:1"],
                          ["id": "a1", "partDefId": "arm", "name": "Arm"],
                          ["id": "r2", "partDefId": "rotor", "name": "Rotor:2"]],
        ])
        let items = ModelBrowser.items(g.featureRoots())
        XCTAssertEqual(items.map(\.title), ["Base", "Rotor × 2", "Arm"])
        XCTAssertEqual(items[1].nodes.map(\.id), ["instance:r1", "instance:r2"])
        XCTAssertEqual(ModelBrowser.partCount(items), 4)
    }

    func testFilterKeepsGroupsWhoseMembersMatch() throws {
        let a = FeatureNode(id: "a", nodeId: 1, name: "Bracket", opType: "Cube", symbol: "cube",
                            detail: nil, partIndex: 0, children: [])
        var r1 = FeatureNode(id: "instance:r1", nodeId: 2, name: "Front", opType: "Cube", symbol: "cube",
                             detail: nil, partIndex: nil, children: [])
        r1.groupName = "Rotor"
        var r2 = r1; r2 = FeatureNode(id: "instance:r2", nodeId: 2, name: "Back", opType: "Cube",
                                      symbol: "cube", detail: nil, partIndex: nil, children: [])
        r2.groupName = "Rotor"
        let items = ModelBrowser.items([a, r1, r2])
        XCTAssertEqual(ModelBrowser.filter(items, "rot").map(\.title), ["Rotor × 2"])
        XCTAssertEqual(ModelBrowser.filter(items, "back").map(\.title), ["Rotor × 2"])
        XCTAssertEqual(ModelBrowser.filter(items, "BRA").map(\.title), ["Bracket"])
        XCTAssertEqual(ModelBrowser.filter(items, "  ").count, 2)
    }

    func testShippedSamplesBrowseAsParts() throws {
        let samples = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("../../Samples").standardized
        let stirling = try XCTUnwrap(DocumentGraph.load(path: samples.appendingPathComponent("stirling-engine.vcad").path))
        let items = ModelBrowser.items(stirling.featureRoots())
        XCTAssertTrue(items.map(\.title).contains("stand × 2"), "\(items.map(\.title))")
        XCTAssertEqual(ModelBrowser.partCount(items), 10)
        let k1 = try XCTUnwrap(DocumentGraph.load(path: samples.appendingPathComponent("k1/k1-humanoid.vcad").path))
        let names = k1.featureRoots().map(\.name)
        XCTAssertFalse(names.contains { $0 == "Imported" || $0 == "Body" }, "\(names)")
    }
}

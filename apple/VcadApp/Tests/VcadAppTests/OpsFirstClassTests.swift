import AppKit
import XCTest
@testable import VcadApp

/// Every kernel primitive is a real feature: placed by click, edited in the
/// inspector, undone, saved and reloaded.
@MainActor
final class OpsFirstClassTests: XCTestCase {
    private var url: URL!

    override func setUp() async throws {
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": ["1": ["id": 1, "name": "Block", "op": ["type": "Cube", "size": ["x": 20, "y": 20, "z": 20]]]],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 1, "material": "default"]],
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

    /// The op node with this type, as the engine stores it.
    private func node(_ m: EditorModel, _ type: String) throws -> Int {
        let nodes = try XCTUnwrap(m.documentJSON?["nodes"] as? [String: Any])
        for (k, v) in nodes where ((v as? [String: Any])?["op"] as? [String: Any])?["type"] as? String == type {
            return try XCTUnwrap(Int(k))
        }
        XCTFail("no \(type) node"); return -1
    }

    /// Kernel volume of the part owning `nodeId`.
    private func volume(_ m: EditorModel, nodeId: Int) throws -> Double {
        let id = try XCTUnwrap(m.engineState?.owner(ofNode: nodeId)?.part.id)
        let text = try XCTUnwrap(m.agentEnsureEngine()).agentRead("inspect", ["part_id": id])
        let range = try XCTUnwrap(text.range(of: #"volume [0-9.]+"#, options: .regularExpression))
        return try XCTUnwrap(Double(text[range].dropFirst(7)))
    }

    /// The translate node's offset for the newest part.
    private func offset(_ m: EditorModel) throws -> [Double] {
        let part = try XCTUnwrap(m.engineState?.parts.last)
        return try XCTUnwrap(m.partVector(part.id, key: "offset"))
    }

    func testPrimitivesPlaceWhereClickedWithTypedFeatures() throws {
        let m = try open()
        for (shape, kind) in [(BaseShape.cone, "cone"), (.torus, "torus"), (.wedge, "wedge"), (.prism, "prism")] {
            m.addPrimitive(shape, at: SIMD3(40, 10, 0))
            let part = try XCTUnwrap(m.engineState?.parts.last)
            XCTAssertEqual(part.kind, kind)
            XCTAssertEqual(try offset(m), [40, 10, 0])
        }
    }

    func testEditingADimensionChangesVolumeAndUndoes() throws {
        let m = try open()
        m.addPrimitive(.torus, at: .zero)
        let id = try node(m, "Torus")
        func volume() throws -> Double { try self.volume(m, nodeId: try self.node(m, "Torus")) }
        let before = try volume()
        XCTAssertEqual(before, 2 * .pi * .pi * 20 * 25, accuracy: before * 0.03)
        m.editScalar(nodeId: id, key: "minor_radius", value: 10, snapshot: true, name: "Change Minor")
        m.flushRuntime()
        let after = try volume()
        XCTAssertEqual(after, 2 * .pi * .pi * 20 * 100, accuracy: after * 0.03)
        m.undo()
        XCTAssertEqual(try volume(), before, accuracy: 1e-6)
    }

    func testWedgeAndPrismEditThroughTheEngine() throws {
        let m = try open()
        m.addPrimitive(.wedge, at: .zero)
        let w = try node(m, "Wedge")
        m.editVec(nodeId: w, key: "size", axis: "z", value: 60, snapshot: true, name: "Height")
        m.flushRuntime()
        XCTAssertEqual(try volume(m, nodeId: try node(m, "Wedge")), 0.5 * 30 * 30 * 60, accuracy: 300)
        m.addPrimitive(.prism, at: .zero)
        let p = try node(m, "Prism")
        m.editInt(nodeId: p, key: "sides", value: 8, snapshot: true, name: "Sides")
        m.flushRuntime()
        let op = try XCTUnwrap(m.opDict(nodeId: p))
        XCTAssertEqual(op["sides"] as? NSNumber, 8)
    }

    func testSaveAndReloadKeepsDimensionsEditable() throws {
        let m = try open()
        m.addPrimitive(.torus, at: SIMD3(5, 0, 0))
        let id = try node(m, "Torus")
        m.editScalar(nodeId: id, key: "major_radius", value: 33, snapshot: true, name: "Major")
        m.flushRuntime()
        let out = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
        defer { try? FileManager.default.removeItem(at: out) }
        try XCTUnwrap(DocEdit.serialize(try XCTUnwrap(m.documentJSON))).write(to: out)

        let again = EditorModel()
        again.openDocument(out)
        XCTAssertTrue(again.canEditDocument, again.engineError ?? "")
        let part = try XCTUnwrap(again.engineState?.parts.last)
        XCTAssertEqual(part.kind, "torus")
        let reloaded = try node(again, "Torus")
        XCTAssertEqual(again.opDict(nodeId: reloaded)?["major_radius"] as? NSNumber, 33)
        again.editScalar(nodeId: reloaded, key: "minor_radius", value: 2, snapshot: true, name: "Minor")
        again.flushRuntime()
        XCTAssertEqual(again.opDict(nodeId: reloaded)?["minor_radius"] as? NSNumber, 2)
    }

    func testHullIsEditable() throws {
        let m = try open()
        XCTAssertTrue(m.addOp(.cube(sizeX: 10, sizeY: 10, sizeZ: 10), name: "Other", offset: [30, 0, 0]))
        m.multiSelectedParts = [0, 1]
        XCTAssertTrue(m.hullSelected(), m.engineError ?? "")
        let id = try node(m, "Hull")
        var hull = try XCTUnwrap(m.hullFeature(nodeId: id))
        XCTAssertEqual(hull.inputs.count, 2)
        // A part made after the hull can still join it.
        XCTAssertTrue(m.addOp(.cube(sizeX: 5, sizeY: 5, sizeZ: 5), name: "Late", offset: [0, 60, 0]))
        let late = try XCTUnwrap(m.engineState?.parts.last?.id)
        XCTAssertTrue(m.setHull(id: hull.id, inputs: hull.inputs + [late], points: hull.points, name: "Add"), m.engineError ?? "")
        let again = try node(m, "Hull")
        hull = try XCTUnwrap(m.hullFeature(nodeId: again))
        XCTAssertEqual(hull.inputs.count, 3)
        // Removing returns it to the scene.
        XCTAssertTrue(m.setHull(id: hull.id, inputs: Array(hull.inputs.dropLast()), points: hull.points, name: "Remove"))
        m.undo()
        XCTAssertEqual(try XCTUnwrap(m.hullFeature(nodeId: try node(m, "Hull"))).inputs.count, 3)
    }

    func testHullFromPointsMeasuresATetrahedron() throws {
        let m = try open()
        let pts = FeatureOpSheet.parsePoints("0 0 0\n30 0 0\n0 30 0\n0 0 30")
        XCTAssertEqual(pts.count, 12)
        XCTAssertTrue(m.addOp(.hull(inputs: [], points: pts), name: "Hull"), m.engineError ?? "")
        let id = try node(m, "Hull")
        XCTAssertEqual(try volume(m, nodeId: id), 4500, accuracy: 5)
    }

    func testRotationalSweepEditsAngle() throws {
        let m = try open()
        m.selectedFeatureID = m.featureNodes.first?.id
        let source = try XCTUnwrap(m.selectedPartFeatureId)
        XCTAssertTrue(m.addOp(.sweepRotate(input: source, axisOrigin: [0, 0, 0], axisDir: [0, 0, 1], angleLoDeg: 0, angleHiDeg: 45),
                              name: "Sweep", materialFrom: source), m.engineError ?? "")
        let id = try node(m, "SweepRotate")
        m.editScalar(nodeId: id, key: "angle_hi_deg", value: 90, snapshot: true, name: "To")
        m.flushRuntime()
        XCTAssertEqual(m.opDict(nodeId: id)?["angle_hi_deg"] as? NSNumber, 90)
        XCTAssertEqual(m.engineState?.parts.last?.kind, "sweep-rotate")
    }

    func testSheetMetalChainCarriesJogHemAndRelief() throws {
        let m = try open()
        let s = SheetMetalStore()
        s.flanges = [SheetFlange(edge: 0)]
        var jog = SheetFlange(edge: 1); jog.kind = .jog
        var hem = SheetFlange(edge: 2); hem.kind = .hem; hem.length = 8
        s.flanges += [jog, hem]
        s.bendRelief = true
        s.insert(m)
        XCTAssertNil(s.error)
        for op in ["SheetMetalEdgeFlange", "SheetMetalJog", "SheetMetalHem", "SheetMetalBendRelief"] {
            _ = try node(m, op)
        }
        XCTAssertEqual(s.chain.map { $0["type"] as? String }, ["BaseFlangeRect", "EdgeFlange", "Jog", "Hem", "BendRelief"])
    }

    func testPolygonBaseFlange() throws {
        let s = SheetMetalStore()
        s.usePolygon = true
        XCTAssertEqual(s.outline.count, 4)
        XCTAssertEqual(s.chain.first?["type"] as? String, "BaseFlangePolygon")
    }

    /// The dock must fit between the title and search, and every symbol must exist.
    func testDockFoldsAndEverySymbolResolves() throws {
        let m = try open()
        let tools = DockTools.tools(.design, model: m)
        XCTAssertEqual(tools.filter { $0.group == "Primitives" }.count, BaseShape.allCases.count)
        let full = DockItem.items(tools, compact: false)
        XCTAssertLessThanOrEqual(full.count, 12)
        XCTAssertLessThanOrEqual(DockItem.items(tools, compact: true).count, 6)
        XCTAssertEqual(Set(["bool.union", "bool.difference", "bool.intersection"]).isSubset(of: Set(tools.map(\.id))), true)
        for t in tools + FeatureOp.allCases.map({ Tool(id: $0.id, label: $0.label, symbol: $0.symbol, isActive: false) {} }) {
            XCTAssertNotNil(NSImage(systemSymbolName: t.symbol, accessibilityDescription: nil), "\(t.id): \(t.symbol)")
        }
        let keys = tools.compactMap(\.shortcut)
        XCTAssertEqual(keys.count, Set(keys).count, "duplicate shortcut")
    }

    /// Every CsgOp is either reachable or deliberately hidden with a reason.
    func testEveryCsgOpHasAnAppPath() throws {
        var dir = URL(fileURLWithPath: #filePath)
        while !FileManager.default.fileExists(atPath: dir.appendingPathComponent("crates/vcad-ir/src/lib.rs").path) {
            dir.deleteLastPathComponent()
        }
        let src = try String(contentsOf: dir.appendingPathComponent("crates/vcad-ir/src/lib.rs"), encoding: .utf8)
        let lines = src.components(separatedBy: "\n")
        let start = try XCTUnwrap(lines.firstIndex { $0.contains("pub enum CsgOp") })
        var hiddenNext = false
        var variants: [(String, Bool)] = []
        for l in lines[(start + 1)...] {
            if l == "}" { break }
            if l.contains("#[tool(hidden)") { hiddenNext = true }
            if let r = l.range(of: #"^    [A-Z][A-Za-z0-9]+"#, options: .regularExpression) {
                variants.append((String(l[r]).trimmingCharacters(in: .whitespaces), hiddenNext))
                hiddenNext = false
            }
        }
        XCTAssertGreaterThan(variants.count, 30)
        let m = try open()
        let palette = Set(m.paletteCommands.map(\.id))
        for (name, hidden) in variants {
            if let p = OpCoverage.paths[name] {
                XCTAssertTrue(palette.contains(p.palette), "\(name): no palette command \(p.palette)")
            } else if OpCoverage.hidden[name] == nil {
                XCTFail("\(name) has no app path\(hidden ? " and no hidden reason" : "")")
            } else {
                XCTAssertTrue(hidden, "\(name) is hidden in the app but exposed as a tool")
            }
        }
    }
}

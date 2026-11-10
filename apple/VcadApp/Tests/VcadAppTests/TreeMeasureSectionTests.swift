import XCTest
import simd
@testable import VcadApp

/// Feature-tree edits, the parameter table, measure, section clipping and
/// STEP/URDF import.
@MainActor
final class TreeMeasureSectionTests: XCTestCase {
    private var url: URL!
    private let repo = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()

    override func setUp() async throws {
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": [
                "1": ["id": 1, "name": "A", "op": ["type": "Cube", "size": ["x": 10, "y": 10, "z": 10]]],
                "2": ["id": 2, "name": "B", "op": ["type": "Cylinder", "radius": 3, "height": 12, "segments": 32]],
            ],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 1, "material": "aluminum"], ["root": 2, "material": "steel"]],
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

    func testReorderAndSuppress() throws {
        let m = try open()
        let a = try XCTUnwrap(m.partId(forPart: 0))
        m.reorderPart(0, to: 1)
        XCTAssertEqual(m.partId(forPart: 1), a, m.engineError ?? "")
        XCTAssertFalse(m.isSuppressed(part: 0))
        m.toggleSuppressed(part: 0)
        XCTAssertTrue(m.isSuppressed(part: 0), m.engineError ?? "")
        m.undo()
        XCTAssertFalse(m.isSuppressed(part: 0))
    }

    func testParameterTable() throws {
        let m = try open()
        m.setDocParameter("wall", value: 3.0)
        m.setDocParameter("twice", value: "wall * 2")
        XCTAssertEqual(m.docParameters.first { $0.name == "wall" }?.value, 3)
        XCTAssertEqual(m.docParameters.first { $0.name == "twice" }?.formula, "wall * 2")
        m.removeDocParameter("twice")
        XCTAssertNil(m.docParameters.first { $0.name == "twice" })
    }

    func testMeasureBetweenParallelFaces() throws {
        let m = try open()
        m.measuring = true
        let top = try XCTUnwrap(m.pickTopology(part: 0, origin: [5, 5, 50], dir: [0, 0, -1]))
        let bottom = try XCTUnwrap(m.pickTopology(part: 0, origin: [5, 5, -50], dir: [0, 0, 1]))
        m.picking.select(hit: top, additive: true)
        m.picking.select(hit: bottom, additive: true)
        let rows = try XCTUnwrap(m.measurement).rows
        XCTAssertEqual(rows.first { $0.label == "Face gap" }?.value, "10.000 mm")
        XCTAssertEqual(rows.first { $0.label == "Angle" }?.value, "0.00°")
    }

    func testSectionClipsAndOutlines() {
        let sec = SectionPlane(axis: .z, offset: 0.5, flip: false)
        // One triangle straddling z = 0.5 and one entirely above it.
        let p: [SIMD3<Float>] = [[0, 0, 0], [1, 0, 0], [0, 0, 1], [0, 0, 2], [1, 0, 2], [0, 1, 2]]
        let (tris, outline) = sec.clip(positions: p, indices: [0, 1, 2, 3, 4, 5])
        XCTAssertEqual(tris.count, 6, "two kept corners → a quad (two triangles); the far triangle is dropped")
        XCTAssertTrue(tris.allSatisfy { $0.z <= 0.5 + 1e-6 })
        XCTAssertEqual(outline.count, 2)
    }

    func testImportStepAndURDF() throws {
        let m = EditorModel()
        let step = repo.appendingPathComponent("examples/tuning-fork/tuning-fork-440hz.step")
        let out = try XCTUnwrap(m.importForeign(step), m.loadError ?? "")
        let doc = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: out)) as? [String: Any])
        XCTAssertFalse((doc["roots"] as? [Any] ?? []).isEmpty, "STEP import yields at least one root")

        let urdf = repo.appendingPathComponent("examples/robot-arm-2dof.urdf")
        let out2 = try XCTUnwrap(m.importForeign(urdf), m.loadError ?? "")
        let doc2 = try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: out2)) as? [String: Any])
        XCTAssertNotNil(doc2["nodes"])
    }

    func testPaletteScoring() {
        XCTAssertNotNil(CommandPaletteSheet.score("linear pattern…", "lp"))
        XCTAssertGreaterThan(CommandPaletteSheet.score("fillet…", "fil") ?? 0,
                             CommandPaletteSheet.score("shell…", "sl") ?? 0)
        XCTAssertNil(CommandPaletteSheet.score("mirror…", "xyz"))
    }
}

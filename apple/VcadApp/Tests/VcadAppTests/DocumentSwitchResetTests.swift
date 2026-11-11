import XCTest
@testable import VcadApp

/// Opening a different document must not carry the old one's transient UI
/// state (sketch mode, armed tool, selection, pending boolean, measure) over.
@MainActor
final class DocumentSwitchResetTests: XCTestCase {
    private var urls: [URL] = []

    private func writeDoc(_ size: Double) throws -> URL {
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": ["1": ["id": 1, "name": "Block", "op": ["type": "Cube", "size": ["x": size, "y": size, "z": 10]]]],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 1, "material": "aluminum"]],
        ]
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
        try JSONSerialization.data(withJSONObject: doc).write(to: url)
        urls.append(url)
        return url
    }
    override func tearDown() async throws { urls.forEach { try? FileManager.default.removeItem(at: $0) } }

    private func dirty(_ m: EditorModel) {
        m.enterSketch()
        m.setSketchTool(.rectangle)
        m.sketchTap([0, 0]); m.sketchTap([5, 5])
        XCTAssertTrue(m.sketching)
        XCTAssertNotNil(m.sketch)
        m.arm(.cylinder)
        m.selectParts([0])
        m.pendingBoolean = (.union, 0)
        m.measuring = true
    }
    private func assertClean(_ m: EditorModel, file: StaticString = #filePath, line: UInt = #line) {
        XCTAssertFalse(m.sketching, file: file, line: line)
        XCTAssertNil(m.sketch, file: file, line: line)
        XCTAssertNil(m.armedShape, file: file, line: line)
        XCTAssertNil(m.pendingBoolean, file: file, line: line)
        XCTAssertFalse(m.measuring, file: file, line: line)
        XCTAssertTrue(m.multiSelectedParts.isEmpty, file: file, line: line)
        XCTAssertTrue(m.picking.selection.isEmpty, file: file, line: line)
        XCTAssertNil(m.hoveredPartIndex, file: file, line: line)
    }

    func testOpeningVcadLeavesSketchAndClearsState() throws {
        let m = EditorModel()
        m.openDocument(try writeDoc(40))
        XCTAssertTrue(m.canEditDocument, m.engineError ?? "")
        dirty(m)
        m.openDocument(try writeDoc(20))
        assertClean(m)
        XCTAssertTrue(m.canEditDocument)
    }

    func testOpeningStepLeavesSketch() throws {
        let m = EditorModel()
        m.openDocument(try writeDoc(40))
        dirty(m)
        let repo = URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
            .deletingLastPathComponent().deletingLastPathComponent()
        m.openDocument(repo.appendingPathComponent("examples/tuning-fork/tuning-fork-440hz.step"))
        XCTAssertNil(m.loadError)
        assertClean(m)
    }

    func testNewDocumentLeavesSketch() throws {
        let m = EditorModel()
        m.openDocument(try writeDoc(40))
        dirty(m)
        m.newDocument()
        assertClean(m)
    }

    func testSaveAsKeepsTheSketch() throws {
        let m = EditorModel()
        m.openDocument(try writeDoc(40))
        m.enterSketch()
        let dest = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
        urls.append(dest)
        m.saveDocumentAs(dest)
        XCTAssertTrue(m.sketching)
        XCTAssertNotNil(m.sketch)
    }
}

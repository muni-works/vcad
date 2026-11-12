import XCTest
@testable import VcadApp

/// Scrubs go to the Rust document runtime as intents and come back as frames
/// (docs/runtime-design.md): the editor never materializes or tessellates
/// them on the main thread.
@MainActor
final class DocumentRuntimeTests: XCTestCase {
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
            "parameters": ["w": ["value": 20]],
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
        XCTAssertTrue(m.runtimeScrubEnabled)
        return m
    }

    /// Yield the main actor until every sent intent's frame has landed.
    private func settle(_ m: EditorModel) async throws {
        for _ in 0..<1000 where m.runtimeEditsInFlight {
            try await Task.sleep(nanoseconds: 5_000_000)
        }
        XCTAssertFalse(m.runtimeEditsInFlight, "frames never arrived")
    }

    private func cubeX(_ m: EditorModel) -> Double? {
        guard let cube = m.engineState?.parts.first(where: { $0.kind == "cube" }),
              let leaf = cube.leafNodeIds.first,
              let size = DocEdit.op(m.documentJSON ?? [:], nodeId: leaf)?["size"] as? [String: Any]
        else { return nil }
        return (size["x"] as? NSNumber)?.doubleValue
    }

    func testScrubIsAsyncCoalescedAndUndoesAsOneStep() async throws {
        let m = try open()
        XCTAssertEqual(m.reevalDocumentMeshes()?.count, 2)
        let pin = try XCTUnwrap(m.partId(forPart: 1))
        let leaf = try XCTUnwrap(m.engineState?.parts.first { $0.kind == "cube" }?.leafNodeIds.first)
        let rev0 = m.documentRevision

        for (i, x) in stride(from: 21.0, through: 40.0, by: 1).enumerated() {
            m.editVec(nodeId: leaf, key: "size", axis: "x", value: x, snapshot: i == 0)
        }
        XCTAssertEqual(m.documentRevision, rev0, "a scrub returns before the runtime applies it")
        try await settle(m)
        XCTAssertEqual(cubeX(m), 40)
        XCTAssertEqual(m.undoStack.count, 1, "one gesture, one undo step")
        XCTAssertGreaterThan(m.adoptedRuntimeRev, 0)
        XCTAssertTrue(m.cachedPartIds.contains(pin), "the untouched pin keeps its tessellation")
        XCTAssertEqual(m.cachedPartIds.count, 2, "the frame carried the block's new mesh")
        XCTAssertEqual(m.reevalDocumentMeshes()?.count, 2)

        // The editor's document equals the runtime's.
        let full = try XCTUnwrap(m.engine?.state())
        XCTAssertEqual(m.documentJSON as NSDictionary?, full.document as NSDictionary)

        m.undo()
        XCTAssertEqual(cubeX(m), 20)
        m.redo()
        XCTAssertEqual(cubeX(m), 40)
    }

    func testDocumentParameterScrub() async throws {
        let m = try open()
        m.editParameter("w", value: 30, snapshot: true)
        m.editParameter("w", value: 35, snapshot: false)
        try await settle(m)
        let w = (m.documentJSON?["parameters"] as? [String: Any])?["w"] as? [String: Any]
        XCTAssertEqual((w?["value"] as? NSNumber)?.doubleValue, 35)
        XCTAssertNotNil(m.pickingDocumentData, "frame bytes back the picking/analysis key")
        m.undo()
        let back = (m.documentJSON?["parameters"] as? [String: Any])?["w"] as? [String: Any]
        XCTAssertEqual((back?["value"] as? NSNumber)?.doubleValue, 20)
    }

    /// Picking takes its B-reps from the runtime's frames (open, scrub and
    /// blocking edits alike) instead of evaluating the document again.
    func testPickingUsesTheRuntimeScene() async throws {
        let m = try open()
        for _ in 0..<1000 where m.picking.readyPartCount == 0 {   // the open's publish frame
            try await Task.sleep(nanoseconds: 5_000_000)
        }
        XCTAssertEqual(m.picking.readyPartCount, 2, "topologies arrived with the frame")
        XCTAssertNotNil(m.partTopology(0))
        // The analysis overlay's face-id meshes came with the same frame.
        let analysis = AnalysisMeshCache.shared.parts(for: try XCTUnwrap(m.pickingDocumentData))
        XCTAssertEqual(analysis.count, 2, "no separate analysis evaluation")
        XCTAssertEqual(analysis[0].faceIds.count, analysis[0].indices.count / 3)

        m.setPartMaterial(1, "brass")       // a blocking edit publishes its revision too
        let engine = try XCTUnwrap(m.engine)
        for _ in 0..<1000 where m.adoptedRuntimeRev < engine.lastRev {
            try await Task.sleep(nanoseconds: 5_000_000)
        }
        XCTAssertEqual(m.adoptedRuntimeRev, engine.lastRev)
        XCTAssertEqual(m.picking.readyPartCount, 2)
        XCTAssertEqual(m.pickingDocumentData, m.documentBytesCache?.data)

        let leaf = try XCTUnwrap(m.engineState?.parts.first { $0.kind == "cube" }?.leafNodeIds.first)
        m.editVec(nodeId: leaf, key: "size", axis: "x", value: 26, snapshot: true)
        try await settle(m)
        XCTAssertEqual(m.picking.readyPartCount, 2)
        XCTAssertEqual(AnalysisMeshCache.shared.parts(for: try XCTUnwrap(m.pickingDocumentData)).count, 2)
        let topo = try XCTUnwrap(m.partTopology(0))
        let maxX = topo.faces.map { $0.centerF.x }.max() ?? 0
        XCTAssertEqual(maxX, 26, accuracy: 0.01, "picking sees the scrubbed block")
    }

    /// The Mallet solves on the runtime's document and meshes, and its
    /// cached modes survive edits that don't touch the struck part.
    func testSoundSolvesThroughTheRuntimeAndKeysByContent() async throws {
        let m = try open()
        for _ in 0..<1000 where m.picking.readyPartCount == 0 { try await Task.sleep(nanoseconds: 5_000_000) }
        _ = m.reevalDocumentMeshes()
        let leaf = try XCTUnwrap(m.engineState?.parts.first { $0.kind == "cube" }?.leafNodeIds.first)
        m.editVec(nodeId: leaf, key: "size", axis: "x", value: 24, snapshot: true)   // a runtime frame: content keys
        try await settle(m)
        let key = try XCTUnwrap(m.soundModesKey(part: 0), "a runtime-built mesh carries a content key")
        let solved = await SoundStore.shared.ensureModes(part: 0, m)
        let pm = try XCTUnwrap(solved, SoundStore.shared.error ?? "no modes")
        XCTAssertGreaterThan(pm.modes.first?.hz ?? 0, 0)

        let pinNode = try XCTUnwrap(m.engineState?.parts.first { $0.kind == "cylinder" }?.leafNodeIds.first)
        m.editScalar(nodeId: pinNode, key: "radius", value: 3.5, snapshot: true)     // the other part
        try await settle(m)
        XCTAssertEqual(m.soundModesKey(part: 0), key, "an edit elsewhere keeps the block's modes")
        m.editVec(nodeId: leaf, key: "size", axis: "x", value: 28, snapshot: true)
        try await settle(m)
        XCTAssertNotEqual(m.soundModesKey(part: 0), key, "resizing the block invalidates them")
    }

    /// The undo history lives in the runtime: the editor's stacks mirror
    /// it, and undo/redo (including a scrub gesture's one-step entry) run
    /// through it.
    func testUndoHistoryLivesInTheRuntime() async throws {
        let m = try open()
        let engine = try XCTUnwrap(m.engine)
        m.setPartMaterial(1, "brass")
        let leaf = try XCTUnwrap(m.engineState?.parts.first { $0.kind == "cube" }?.leafNodeIds.first)
        m.editVec(nodeId: leaf, key: "size", axis: "x", value: 30, snapshot: true, name: "Width")
        m.editVec(nodeId: leaf, key: "size", axis: "x", value: 31, snapshot: false, name: "Width")
        try await settle(m)
        let runtime = try engine.history(["op": "list"]).history
        XCTAssertEqual(runtime.undo, m.undoStack.map(\.name), "the stacks mirror the runtime")
        XCTAssertEqual(runtime.undo.last, "Width")
        XCTAssertEqual(m.undoStack.count, 2, "material + one gesture")

        m.undo()
        XCTAssertEqual(cubeX(m), 20)
        XCTAssertEqual(try engine.history(["op": "list"]).history.redo, ["Width"])
        m.undo()
        XCTAssertNil((m.documentJSON?["materials"] as? [String: Any])?["brass"])
        XCTAssertTrue(m.undoStack.isEmpty)
        m.redo(); m.redo()
        XCTAssertEqual(cubeX(m), 31)
        XCTAssertEqual(m.undoStack.map(\.name), try engine.history(["op": "list"]).history.undo)
        // A new edit after undoing drops the redo stack, on the runtime too.
        m.undo()
        m.setPartMaterial(0, "steel")
        XCTAssertTrue(try engine.history(["op": "list"]).history.redo.isEmpty)
        XCTAssertTrue(m.redoStack.isEmpty)
    }

    func testBlockingEditSupersedesInFlightFrames() async throws {
        let m = try open()
        let leaf = try XCTUnwrap(m.engineState?.parts.first { $0.kind == "cube" }?.leafNodeIds.first)
        m.editVec(nodeId: leaf, key: "size", axis: "x", value: 33, snapshot: true)
        m.setPartMaterial(1, "brass")       // blocking; ordered after the scrub
        XCTAssertEqual(cubeX(m), 33, "the blocking edit's state includes the queued scrub")
        let hash = m.engine?.stateHash
        try await Task.sleep(nanoseconds: 300_000_000)  // let the stale frame arrive
        XCTAssertEqual(m.engine?.stateHash, hash)
        XCTAssertEqual(cubeX(m), 33)
        XCTAssertNotNil((m.documentJSON?["materials"] as? [String: Any])?["brass"],
                        "a stale frame must not roll back the later edit")
    }
}

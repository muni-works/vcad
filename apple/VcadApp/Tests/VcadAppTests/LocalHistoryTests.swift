import Darwin
import XCTest
@testable import VcadApp

/// Safe save, autosave + crash recovery, and version history (LocalHistory.swift).
@MainActor
final class LocalHistoryTests: XCTestCase {
    private var root: URL!

    override func setUp() async throws {
        root = FileManager.default.temporaryDirectory.appendingPathComponent("vcad-lh-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: root, withIntermediateDirectories: true)
        LocalData.overrideRoot = root.appendingPathComponent("data")
    }
    override func tearDown() async throws {
        SafeWriter.crashAt = nil
        LocalData.overrideRoot = nil
        try? FileManager.default.removeItem(at: root)
    }

    private func doc(_ size: Double) -> Data {
        let d: [String: Any] = [
            "version": "0.1",
            "nodes": ["1": ["id": 1, "name": "Block", "op": ["type": "Cube", "size": ["x": size, "y": size, "z": 10]]]],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 1, "material": "aluminum"]],
        ]
        return try! JSONSerialization.data(withJSONObject: d, options: [.sortedKeys])
    }
    private func cubeX(_ data: Data?) -> Double? {
        guard let data, let d = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return nil }
        return cubeX(d)
    }
    private func cubeX(_ d: [String: Any]?) -> Double? {
        let nodes = (d?["nodes"] as? [String: Any]) ?? [:]
        for case let n as [String: Any] in nodes.values {
            if let op = n["op"] as? [String: Any], op["type"] as? String == "Cube" {
                return ((op["size"] as? [String: Any])?["x"] as? NSNumber)?.doubleValue
            }
        }
        return nil
    }
    /// Resize the cube through the editor, as the inspector would.
    private func setX(_ m: EditorModel, _ x: Double) {
        guard let id = m.partId(forPart: 0),
              let part = m.engineState?.parts.first(where: { $0.id == id }),
              let leaf = part.leafNodeIds.first else { return XCTFail("no editable part") }
        m.editVec(nodeId: leaf, key: "size", axis: "x", value: x, snapshot: true, name: "Size")
        m.flushRuntime()
        XCTAssertEqual(cubeX(m.documentJSON), x)
    }

    private func file(_ size: Double) throws -> URL {
        let url = root.appendingPathComponent("part-\(Int(size)).vcad")
        try doc(size).write(to: url)
        return url
    }

    // MARK: kill-mid-save

    /// A crash at every stage of a save leaves the original file byte-identical.
    func testKillMidSaveNeverCorruptsTheOriginal() throws {
        let url = try file(40)
        let original = try Data(contentsOf: url)
        for stage in [SafeWriter.Stage.afterPartialWrite, .afterSync, .beforeRename] {
            SafeWriter.crashAt = stage
            XCTAssertThrowsError(try SafeWriter.write(doc(99), to: url), "\(stage)")
            XCTAssertEqual(try Data(contentsOf: url), original, "original changed after crash at \(stage)")
        }
        SafeWriter.crashAt = nil
        // The crash left temp files behind (as a real one would); the next save
        // succeeds and the app's post-save cleanup removes them.
        XCTAssertEqual(SafeWriter.strayTemps(for: url).count, 3)
        try SafeWriter.write(doc(99), to: url)
        XCTAssertEqual(cubeX(try Data(contentsOf: url)), 99)
        SafeWriter.strayTemps(for: url).forEach { try? FileManager.default.removeItem(at: $0) }
        XCTAssertTrue(SafeWriter.strayTemps(for: url).isEmpty)
    }

    /// Disk full mid-write (simulated with RLIMIT_FSIZE → EFBIG, the same
    /// short-write path ENOSPC takes): the save fails, the file is untouched,
    /// and no temp file is left behind.
    func testDiskFullMidWriteLeavesOriginalIntact() throws {
        let url = try file(40)
        let original = try Data(contentsOf: url)
        let big = Data(repeating: 0x20, count: 256 * 1024)
        signal(SIGXFSZ, SIG_IGN)
        var old = rlimit()
        getrlimit(RLIMIT_FSIZE, &old)
        var tight = old; tight.rlim_cur = 64 * 1024
        setrlimit(RLIMIT_FSIZE, &tight)
        defer { setrlimit(RLIMIT_FSIZE, &old) }
        XCTAssertThrowsError(try SafeWriter.write(big, to: url)) { e in
            XCTAssertEqual(e as? SafeWriter.Failure, .write(EFBIG))
        }
        setrlimit(RLIMIT_FSIZE, &old)
        XCTAssertEqual(try Data(contentsOf: url), original)
        XCTAssertTrue(SafeWriter.strayTemps(for: url).isEmpty)
    }

    /// The editor's Save goes through SafeWriter: a crashing save reports an
    /// error, keeps the document dirty, and leaves the file alone.
    func testEditorSaveFailureKeepsDocumentDirty() throws {
        let url = try file(40)
        let original = try Data(contentsOf: url)
        let m = EditorModel()
        m.openDocument(url)
        XCTAssertTrue(m.canEditDocument, m.engineError ?? "")
        setX(m, 55)
        XCTAssertTrue(m.documentDirty)
        SafeWriter.crashAt = .beforeRename
        m.saveDocument()
        XCTAssertNotNil(m.saveError)
        XCTAssertTrue(m.documentDirty)
        XCTAssertEqual(try Data(contentsOf: url), original)
    }

    // MARK: autosave + recovery

    func testAutosaveDebouncesAndCapsLag() throws {
        let saver = Autosaver(root: LocalData.root, delay: 0.3, maxLag: 0.6)
        for i in 0..<5 { saver.schedule(name: "A", documentPath: nil) { self.doc(Double(i)) } }
        Thread.sleep(forTimeInterval: 0.5)
        saver.flush()
        XCTAssertEqual(saver.writes, 1, "a burst of edits costs one write")
        XCTAssertEqual(cubeX(try Data(contentsOf: saver.dataURL)), 4, "the newest state wins")
        // Continuous editing still flushes within maxLag.
        let start = Date()
        while Date().timeIntervalSince(start) < 1.2 {
            saver.schedule(name: "A", documentPath: nil) { self.doc(7) }
            Thread.sleep(forTimeInterval: 0.1)
        }
        XCTAssertGreaterThanOrEqual(saver.writes, 2)
    }

    /// The full recovery flow: an instance edits, autosaves, then "crashes"
    /// (its pid is dead). The next launch finds the orphan, materializes it as a
    /// "(Recovered)" copy beside the original, and opens it with the edit.
    func testRecoveryFlowRestoresUnsavedWork() throws {
        let url = try file(40)
        let m = EditorModel()
        m.openDocument(url)
        setX(m, 63)
        XCTAssertTrue(m.documentDirty)
        // Let the main-loop hop run, then force the debounce.
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        let saver = try XCTUnwrap(m.autosaver, "an edit schedules an autosave")
        saver.flush()

        // Once the owner is dead it is offered.
        let orphans = Recovery.orphans(isAlive: { _ in false })
        XCTAssertEqual(orphans.count, 1)
        let e = try XCTUnwrap(orphans.first)
        XCTAssertEqual(e.documentPath, url.standardizedFileURL.path)
        XCTAssertEqual(e.name, "part-40")

        let recovered = try Recovery.materialize(e)
        XCTAssertEqual(recovered.lastPathComponent, "part-40 (Recovered).vcad")
        XCTAssertEqual(cubeX(try Data(contentsOf: url)), 40, "the original file is untouched")
        XCTAssertTrue(Recovery.orphans(isAlive: { _ in false }).isEmpty, "a restored autosave is consumed")

        let next = EditorModel()
        next.openDocument(recovered)
        XCTAssertEqual(cubeX(next.documentJSON), 63)
    }

    func testOrphansIgnoreLiveInstancesAndSavedDocuments() throws {
        let saver = Autosaver(root: LocalData.root, delay: 0)
        saver.schedule(name: "Live", documentPath: nil) { self.doc(1) }
        saver.flush()
        // Owned by another live process → still being edited, not offered.
        XCTAssertTrue(Recovery.orphans(isAlive: { _ in true }).filter { $0.pid != getpid() }.isEmpty)
        XCTAssertEqual(Recovery.orphans(isAlive: { _ in false }).count, 1)
        // A save discards the autosave: nothing left to recover.
        saver.discard()
        XCTAssertTrue(Recovery.orphans(isAlive: { _ in false }).isEmpty)
    }

    func testSaveDiscardsAutosaveAndRecordsAVersion() throws {
        let url = try file(40)
        let m = EditorModel()
        m.openDocument(url)
        setX(m, 41)
        RunLoop.main.run(until: Date().addingTimeInterval(0.05))
        m.autosaver?.flush()
        XCTAssertEqual(Recovery.orphans(isAlive: { _ in false }).count, 1)
        m.saveDocument()
        XCTAssertNil(m.saveError)
        XCTAssertFalse(m.documentDirty)
        XCTAssertTrue(Recovery.orphans(isAlive: { _ in false }).isEmpty)
        XCTAssertEqual(m.versionHistory?.versions.count, 1)
    }

    // MARK: version history

    /// Save → edit → save, then preview v1 in the viewport, cancel back to v2,
    /// preview again and restore, save, and duplicate v1 as a new document.
    func testVersionRestoreRoundTrip() throws {
        let url = try file(40)
        let m = EditorModel()
        m.openDocument(url)
        setX(m, 50)
        m.saveDocument()
        let v1 = try XCTUnwrap(m.saveNamedVersion("Fifty"))
        setX(m, 70)
        m.saveDocument()
        let h = try XCTUnwrap(m.versionHistory)
        XCTAssertEqual(h.versions.filter { $0.kind == .auto }.count, 2)
        XCTAssertEqual(h.versions.filter { $0.kind == .named }.map(\.title), ["Fifty"])

        // Preview shows v1 in the live document without touching the file.
        m.previewVersion(v1)
        XCTAssertEqual(m.previewingVersion, v1)
        XCTAssertEqual(cubeX(m.documentJSON), 50)
        XCTAssertEqual(cubeX(try Data(contentsOf: url)), 70)
        // Cancel puts back exactly what you had.
        m.endVersionPreview(keep: false)
        XCTAssertNil(m.previewingVersion)
        XCTAssertEqual(cubeX(m.documentJSON), 70)
        XCTAssertFalse(m.documentDirty)

        // Restore keeps it as an undoable edit, with a safety copy of the file.
        m.previewVersion(v1)
        m.endVersionPreview(keep: true)
        XCTAssertEqual(cubeX(m.documentJSON), 50)
        XCTAssertTrue(m.documentDirty)
        XCTAssertTrue(h.versions.contains { $0.kind == .beforeRestore })
        m.saveDocument()
        XCTAssertEqual(cubeX(try Data(contentsOf: url)), 50)
        m.undo()
        XCTAssertEqual(cubeX(m.documentJSON), 70, "a restore is one undo step")

        // Duplicate as a new document.
        let dup = root.appendingPathComponent("dup.vcad")
        try h.duplicate(v1, to: dup)
        let other = EditorModel()
        other.openDocument(dup)
        XCTAssertEqual(cubeX(other.documentJSON), 50)
    }

    func testFileLevelRestoreAndCorruptionGuard() throws {
        let url = try file(40)
        let h = VersionHistory(documentURL: url)
        let v40 = try h.record(doc(40))
        XCTAssertEqual(try h.record(doc(40)).id, v40.id, "unchanged save adds nothing")
        try SafeWriter.write(doc(80), to: url)
        try h.record(doc(80))
        try h.restore(v40)
        XCTAssertEqual(cubeX(try Data(contentsOf: url)), 40)
        XCTAssertEqual(h.versions.filter { $0.kind == .beforeRestore }.count, 1)
        // A snapshot whose bytes no longer match its hash never restores.
        try Data("garbage".utf8).write(to: h.url(for: v40))
        XCTAssertNil(h.data(for: v40))
        XCTAssertThrowsError(try h.restore(v40))
    }

    func testAutoSnapshotsThinOutButNamedOnesStay() throws {
        let url = try file(1)
        let h = VersionHistory(documentURL: url, maxAuto: 1000)
        let now = Date()
        // 200 saves, one every 10 minutes, going back ~33 hours.
        for i in 0..<200 {
            try h.record(doc(Double(i)), at: now.addingTimeInterval(-Double(200 - i) * 600))
        }
        try h.record(doc(1000), kind: .named, name: "Keep", at: now.addingTimeInterval(-3 * 86_400))
        try h.record(doc(1001), at: now)
        let autos = h.versions.filter { $0.kind == .auto }
        XCTAssertLessThan(autos.count, 60, "older autos thinned to hourly/daily")
        XCTAssertGreaterThanOrEqual(autos.filter { now.timeIntervalSince($0.createdAt) < 3600 }.count, 6)
        XCTAssertTrue(h.versions.contains { $0.name == "Keep" })
        // Pruned snapshot files are gone from disk too.
        let files = try FileManager.default.contentsOfDirectory(atPath: h.dir.path).filter { $0.hasSuffix(".vcad") }
        XCTAssertEqual(files.count, h.versions.count)
    }

    /// The same file reached through a symlinked path shares one history.
    func testHistoryKeyIgnoresSymlinkedPaths() throws {
        let dir = "/tmp/vcad-lh-\(UUID().uuidString)"
        try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        defer { try? FileManager.default.removeItem(atPath: dir) }
        try doc(1).write(to: URL(fileURLWithPath: dir + "/part.vcad"))
        let a = VersionHistory(documentURL: URL(fileURLWithPath: dir + "/part.vcad"))
        let b = VersionHistory(documentURL: URL(fileURLWithPath: "/private" + dir + "/part.vcad"))
        XCTAssertEqual(a.dir, b.dir)
    }
}

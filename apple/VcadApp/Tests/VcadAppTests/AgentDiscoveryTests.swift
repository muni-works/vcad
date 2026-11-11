import Foundation
import XCTest
@testable import VcadApp

final class AgentDiscoveryTests: XCTestCase {
    private func tempHome() throws -> URL {
        let h = FileManager.default.temporaryDirectory.appendingPathComponent("vcad-disc-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: h, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: h) }
        return h
    }

    private func rec(_ pid: Int, _ started: String, doc: String? = nil) -> AgentInstanceRecord {
        AgentInstanceRecord(pid: pid, port: 47_000 + pid, url: "http://127.0.0.1:\(47_000 + pid)/mcp",
                            instanceId: "id-\(pid)", started: started,
                            documentName: doc.map { ($0 as NSString).lastPathComponent }, documentPath: doc)
    }

    private func pointerPid(_ home: URL) -> Int? {
        let url = AgentDiscovery.root(home: home).appendingPathComponent("app.json")
        guard let data = try? Data(contentsOf: url) else { return nil }
        return (try? JSONDecoder().decode(AgentInstanceRecord.self, from: data))?.pid
    }

    func testWriteListAndPointer() throws {
        let home = try tempHome()
        AgentDiscovery.write(rec(1, "2026-11-11T10:00:00.000Z", doc: "/tmp/a.vcad"), home: home, isAlive: { _ in true })
        AgentDiscovery.write(rec(2, "2026-11-11T11:00:00.000Z"), home: home, isAlive: { _ in true })
        XCTAssertEqual(AgentDiscovery.list(home: home).map(\.pid), [2, 1])
        XCTAssertEqual(AgentDiscovery.list(home: home).last?.documentPath, "/tmp/a.vcad")
        XCTAssertEqual(pointerPid(home), 2)
        // Snake-case keys are the wire format the JS clients read.
        let raw = try String(contentsOf: AgentDiscovery.instancesDir(home: home).appendingPathComponent("1.json"),
                             encoding: .utf8)
        XCTAssertTrue(raw.contains("\"instance_id\"") && raw.contains("\"document_path\""))
    }

    func testRemoveRepointsToNewestSurvivor() throws {
        let home = try tempHome()
        AgentDiscovery.write(rec(1, "2026-11-11T10:00:00.000Z"), home: home, isAlive: { _ in true })
        AgentDiscovery.write(rec(2, "2026-11-11T11:00:00.000Z"), home: home, isAlive: { _ in true })
        AgentDiscovery.write(rec(3, "2026-11-11T12:00:00.000Z"), home: home, isAlive: { _ in true })
        // Removing a non-pointer instance leaves the pointer alone.
        AgentDiscovery.remove(pid: 1, home: home, isAlive: { _ in true })
        XCTAssertEqual(pointerPid(home), 3)
        AgentDiscovery.remove(pid: 3, home: home, isAlive: { _ in true })
        XCTAssertEqual(pointerPid(home), 2)
        AgentDiscovery.remove(pid: 2, home: home, isAlive: { _ in true })
        XCTAssertNil(pointerPid(home))
        XCTAssertTrue(AgentDiscovery.list(home: home).isEmpty)
    }

    func testPruneDropsDeadPids() throws {
        let home = try tempHome()
        AgentDiscovery.write(rec(1, "2026-11-11T10:00:00.000Z"), home: home, isAlive: { _ in true })
        AgentDiscovery.write(rec(2, "2026-11-11T11:00:00.000Z"), home: home, isAlive: { _ in true })
        // Writing a new instance prunes the dead one.
        AgentDiscovery.write(rec(3, "2026-11-11T12:00:00.000Z"), home: home, isAlive: { $0 != 1 })
        XCTAssertEqual(AgentDiscovery.list(home: home).map(\.pid), [3, 2])
        XCTAssertEqual(AgentDiscovery.prune(home: home, isAlive: { $0 == 3 }), [2])
    }

    func testIsAliveOnSelfAndBogusPid() {
        XCTAssertTrue(AgentDiscovery.isAlive(Int(ProcessInfo.processInfo.processIdentifier)))
        XCTAssertFalse(AgentDiscovery.isAlive(0))
    }

    @MainActor
    func testStampInstanceAppendsTrailingNameAndMeta() {
        let meta: [String: Any] = ["pid": 42, "instance_id": "abc", "document": "bracket.vcad"]
        let out = AgentBridge.stampInstance(AgentTools.json(["ok": true]), meta)
        let content = out["content"] as? [[String: Any]]
        XCTAssertEqual(content?.count, 2)
        XCTAssertTrue((content?.first?["text"] as? String)?.contains("\"ok\"") == true)
        XCTAssertEqual(content?.last?["text"] as? String, "[vcad.app pid 42 · bracket.vcad]")
        let m = (out["_meta"] as? [String: Any])?["io.vcad/instance"] as? [String: Any]
        XCTAssertEqual(m?["instance_id"] as? String, "abc")
        // No bridge running → result untouched.
        XCTAssertEqual((AgentBridge.stampInstance(AgentTools.text("x"), [:])["content"] as? [Any])?.count, 1)
    }
}

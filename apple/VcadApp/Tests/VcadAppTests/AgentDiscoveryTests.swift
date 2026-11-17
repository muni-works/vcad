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

    func testBundleIdRoundTripsAsSnakeCase() throws {
        var r = rec(7, "2026-11-16T10:00:00.000Z")
        r.bundleId = "io.vcad.dev.app-sim-screens-fbe447"
        let json = try JSONSerialization.jsonObject(with: JSONEncoder().encode(r)) as? [String: Any]
        XCTAssertEqual(json?["bundle_id"] as? String, "io.vcad.dev.app-sim-screens-fbe447")
        XCTAssertEqual(try JSONDecoder().decode(AgentInstanceRecord.self, from: JSONEncoder().encode(r)), r)
        // Records from older builds carry no bundle id and still decode.
        let old = #"{"pid":1,"port":2,"url":"u","instance_id":"i","started":"s"}"#
        XCTAssertNil(try JSONDecoder().decode(AgentInstanceRecord.self, from: Data(old.utf8)).bundleId)
    }

    func testThrowawayRunsDoNotAdvertise() {
        let adv = { (env: [String: String]) in AgentDiscovery.advertises(env: env, underTest: false) }
        XCTAssertTrue(adv([:]))
        XCTAssertTrue(adv(["VCAD_AGENT_PORT": "47400"]))
        XCTAssertFalse(adv(["VCAD_AGENT_DISCOVERY": "0"]))
        XCTAssertFalse(adv(["VCAD_PERF_EXIT": "1"]))
        XCTAssertFalse(adv(["VCAD_BAKE_MESH_BUNDLE": "1"]))
        XCTAssertFalse(adv(["VCAD_SNAPSHOT_DIR": "/tmp/shots"]))
        XCTAssertTrue(adv(["VCAD_SNAPSHOT_DIR": ""]))
        // This very process is a test host: it must never advertise.
        XCTAssertTrue(AgentDiscovery.isUnderTest)
        XCTAssertFalse(AgentDiscovery.advertises(env: [:]))
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

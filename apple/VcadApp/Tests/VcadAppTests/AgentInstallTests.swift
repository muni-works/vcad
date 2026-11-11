import XCTest
@testable import VcadApp

final class AgentInstallTests: XCTestCase {
    var home: URL!
    let entry = AgentInstall.serverEntry(npx: "/opt/homebrew/bin/npx")

    override func setUpWithError() throws {
        home = FileManager.default.temporaryDirectory.appendingPathComponent("vcad-agent-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: home, withIntermediateDirectories: true)
    }

    override func tearDownWithError() throws { try? FileManager.default.removeItem(at: home) }

    func testDetectionOnlyReads() throws {
        XCTAssertEqual(AgentInstall.status(.cursor, home: home), .notDetected)
        try FileManager.default.createDirectory(at: home.appendingPathComponent(".cursor"), withIntermediateDirectories: true)
        XCTAssertEqual(AgentInstall.status(.cursor, home: home), .available)
        XCTAssertFalse(FileManager.default.fileExists(atPath: AgentKind.cursor.config(home: home).path))
    }

    func testJSONInstallMergesBacksUpAndUndoes() throws {
        let url = AgentKind.claudeCode.config(home: home)
        let original = #"{"numStartups":3,"mcpServers":{"other":{"command":"x"}}}"#
        try Data(original.utf8).write(to: url)
        XCTAssertEqual(try AgentInstall.install(.claudeCode, entry: entry, home: home), .added)
        XCTAssertEqual(try AgentInstall.install(.claudeCode, entry: entry, home: home), .alreadySet)
        var root = try AgentInstall.readJSON(url)
        XCTAssertEqual(root["numStartups"] as? Int, 3)
        let servers = root["mcpServers"] as! [String: Any]
        XCTAssertNotNil(servers["other"])
        XCTAssertEqual((servers["vcad"] as! [String: Any])["args"] as? [String], ["-y", "@vcad/mcp"])
        XCTAssertEqual(try String(contentsOf: url.appendingPathExtension("vcad-backup"), encoding: .utf8), original)

        XCTAssertTrue(try AgentInstall.uninstall(.claudeCode, home: home))
        root = try AgentInstall.readJSON(url)
        XCTAssertNil((root["mcpServers"] as! [String: Any])["vcad"])
        XCTAssertNotNil((root["mcpServers"] as! [String: Any])["other"])
        XCTAssertEqual(AgentInstall.status(.claudeCode, home: home), .available)
    }

    func testForeignEntryIsNeverTouched() throws {
        let url = AgentKind.claudeDesktop.config(home: home)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let mine = #"{"mcpServers":{"vcad":{"command":"node","args":["/src/vcad/packages/mcp/scripts/serve.mjs"]}}}"#
        try Data(mine.utf8).write(to: url)
        guard case .foreign = AgentInstall.status(.claudeDesktop, home: home) else { return XCTFail() }
        guard case .leftAlone = try AgentInstall.install(.claudeDesktop, entry: entry, home: home) else { return XCTFail() }
        XCTAssertFalse(try AgentInstall.uninstall(.claudeDesktop, home: home))
        XCTAssertEqual(try String(contentsOf: url, encoding: .utf8), mine)
    }

    func testCodexTomlBlockRoundTrips() throws {
        let url = AgentKind.codex.config(home: home)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        let original = "model = \"o4\"\n\n[mcp_servers.other]\ncommand = \"x\"\n"
        try Data(original.utf8).write(to: url)
        XCTAssertEqual(try AgentInstall.install(.codex, entry: entry, home: home), .added)
        let text = try String(contentsOf: url, encoding: .utf8)
        XCTAssertTrue(text.contains("[mcp_servers.vcad]\ncommand = \"/opt/homebrew/bin/npx\"\nargs = [\"-y\", \"@vcad/mcp\"]"))
        XCTAssertEqual(AgentInstall.status(.codex, home: home), .connected)
        XCTAssertTrue(try AgentInstall.uninstall(.codex, home: home))
        XCTAssertEqual(try String(contentsOf: url, encoding: .utf8), original)
    }

    func testCodexForeignTable() throws {
        let url = AgentKind.codex.config(home: home)
        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("[mcp_servers.vcad]\ncommand = \"vcad\"\n".utf8).write(to: url)
        guard case .foreign = AgentInstall.status(.codex, home: home) else { return XCTFail() }
    }

    func testSkillInstallsWithClaudeCodeAndRespectsUserFile() throws {
        try AgentInstall.installSkill(home: home, skill: "---\nname: vcad\n---\nbody")
        let url = AgentInstall.skillURL(home: home)
        XCTAssertTrue(try String(contentsOf: url, encoding: .utf8).contains(AgentInstall.skillMarker))
        AgentInstall.removeSkill(home: home)
        XCTAssertFalse(FileManager.default.fileExists(atPath: url.path))

        try FileManager.default.createDirectory(at: url.deletingLastPathComponent(), withIntermediateDirectories: true)
        try Data("my own".utf8).write(to: url)
        try AgentInstall.installSkill(home: home, skill: "theirs")
        AgentInstall.removeSkill(home: home)
        XCTAssertEqual(try String(contentsOf: url, encoding: .utf8), "my own")
    }

    func testBundledSkillIsThePluginCopy() {
        let s = AgentInstall.bundledSkill()
        XCTAssertNotNil(s)
        XCTAssertTrue(s!.hasPrefix("---\nname: vcad\n"))
    }
}

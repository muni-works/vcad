import XCTest
@testable import VcadApp

/// The node proxy's launcher: an `npx` only counts with a `node` beside it.
final class AgentNodeCommandTests: XCTestCase {
    func testSkipsNpxWithoutSiblingNode() {
        // Cam's machine: a stale standalone npx in /opt/homebrew/bin (no
        // node there), the working one in the node@20 keg.
        let present: Set<String> = [
            "/opt/homebrew/bin/npx",
            "/opt/homebrew/opt/node@20/bin/npx", "/opt/homebrew/opt/node@20/bin/node",
        ]
        let cmd = AgentCore.nodeCommand(env: [:], isExecutable: present.contains,
                                        kegDirs: ["/opt/homebrew/opt/node@20/bin"])
        XCTAssertEqual(cmd, "/opt/homebrew/opt/node@20/bin/npx -y @vcad/mcp")
    }

    func testEnvOverrideWins() {
        XCTAssertEqual(AgentCore.nodeCommand(env: ["VCAD_MCP_NODE_CMD": "off"],
                                             isExecutable: { _ in true }, kegDirs: []), "off")
    }

    func testNoNodeAnywhere() {
        XCTAssertNil(AgentCore.nodeCommand(env: [:], isExecutable: { $0.hasSuffix("/npx") }, kegDirs: []))
    }
}

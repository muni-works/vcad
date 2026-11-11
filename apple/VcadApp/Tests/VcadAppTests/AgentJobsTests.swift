import XCTest
@testable import VcadApp

/// Phase C: every long-running MCP tool call is a job in the header pill,
/// with stage, fraction and a ✕ that cancels it in the core.
@MainActor
final class AgentJobsTests: XCTestCase {
    func testJobEventsDriveTheProgressPill() {
        let center = ProgressCenter(interval: 0)
        let jobs = AgentJobs()
        jobs.handle(["jobId": "j1", "tool": "fab_print", "state": "running"], progress: center)
        XCTAssertEqual(center.primary?.id, AgentJobs.progressId("j1"))
        XCTAssertTrue(center.primary?.title.hasSuffix("Print dry run") ?? false)
        XCTAssertTrue(center.primary?.cancellable ?? false, "agent jobs carry the ✕")
        jobs.handle(["jobId": "j1", "tool": "fab_print", "state": "running", "stage": "Slicing", "fraction": 0.4],
                    progress: center)
        center.flush()
        XCTAssertEqual(center.primary?.stage, "Slicing")
        XCTAssertEqual(center.primary?.fraction, 0.4)
        jobs.handle(["jobId": "j1", "tool": "fab_print", "state": "cancelled"], progress: center)
        XCTAssertNil(center.primary)
        XCTAssertTrue(jobs.cancelled.contains("j1"), "an agent's tasks/cancel trips app-side work")
        XCTAssertNil(jobs.running["j1"])
    }

    /// The real path: an agent's analyze_structure through the core is a job
    /// in the pill; the pill's ✕ stops the FEA study and the agent gets a
    /// cancelled result.
    func testFeaThroughTheCoreShowsInThePillAndCancelsFromIt() async throws {
        let m = EditorModel()
        _ = await coreCall(m, "create", ["type": "cube", "name": "Beam", "params": ["size": ["x": 80, "y": 10, "z": 10]]])
        let call = Task { () -> (Bool, String) in
            let r = await coreCall(m, "analyze_structure", [
                "part_index": 0, "material": "steel", "resolution": 24, "levels": 4,
                "supports": [["region": ["min": [-1, -1, -1], "max": [1, 11, 11]]]],
                "loads": [["region": ["min": [79, -1, -1], "max": [81, 11, 11]], "force": [0, 0, -100]]],
            ])
            return ((r["isError"] as? Bool) ?? false, resultText(r))
        }
        var job: ProgressTask?
        for _ in 0..<600 {
            m.progress.flush()
            job = m.progress.tasks.first { $0.id.hasPrefix("agent-job:") }
            if job?.stage != nil { break }
            try await Task.sleep(for: .milliseconds(50))
        }
        let shown = try XCTUnwrap(job, "the agent's FEA job never reached the pill")
        XCTAssertTrue(shown.title.hasSuffix("Structural analysis"))
        XCTAssertTrue(shown.stage?.contains("level") ?? false, "stage: \(shown.stage ?? "nil")")
        m.progress.cancel(shown.id)
        let (isError, text) = await call.value
        XCTAssertTrue(isError)
        XCTAssertTrue(text.contains("cancelled"), text)
        m.progress.flush()
        XCTAssertFalse(m.progress.tasks.contains { $0.id == shown.id })
    }
}

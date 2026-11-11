import Foundation

// Agent jobs in the header pill. Every long-running MCP tool call — fab dry
// runs, optimize, FEA, topology optimization, meshing, slicing — is a job in
// the Rust core (crates/vcad-mcp/src/jobs.rs) whether or not the agent asked
// for an MCP Task. The core forwards each job event here (`op: "job"`), and
// the job lands in the window's ONE progress system (ProgressCenter) with its
// stage, fraction and a ✕.
//
// Cancel goes both ways: the ✕ sends `vcad/jobs/cancel` to the core (which
// also cancels the MCP Task the agent is polling); an agent's `tasks/cancel`
// comes back as a `cancelled` job event that ends the pill entry and trips
// any app-side work running under that job (the in-app FEA solve).

@MainActor
final class AgentJobs {
    static let shared = AgentJobs()

    /// One running job as the core describes it.
    struct Job: Equatable {
        let id: String
        let tool: String
        var stage: String?
        var fraction: Double?
        var taskId: String?
    }

    private(set) var running: [String: Job] = [:]
    /// Jobs whose app-side work must stop (read from worker threads).
    nonisolated let cancelled = CancelSet()

    /// Progress-center id for a job.
    static func progressId(_ job: String) -> String { "agent-job:\(job)" }

    /// "Claude · Structural analysis".
    static func title(_ tool: String) -> String {
        let names: [String: String] = [
            "analyze_structure": "Structural analysis", "fab_print": "Print dry run",
            "fab_mill": "Mill dry run", "optimize_parameters": "Optimizing parameters",
            "topology_optimize": "Topology optimization", "simulate_flow": "Flow simulation",
            "solve_thermal": "Thermal solve", "route_nets": "Routing",
        ]
        let what = names[tool] ?? tool.replacingOccurrences(of: "_", with: " ").capitalized
        return "\(AgentCore.agentPrefix) · \(what)"
    }

    /// A job event from the core.
    func handle(_ event: [String: Any], progress: ProgressCenter?) {
        guard let id = event["jobId"] as? String else { return }
        let state = event["state"] as? String ?? "running"
        let pid = Self.progressId(id)
        guard state == "running" else {
            running[id] = nil
            if state == "cancelled" { cancelled.insert(id) }
            progress?.end(pid)
            return
        }
        let job = Job(id: id, tool: event["tool"] as? String ?? "job",
                      stage: event["stage"] as? String, fraction: event["fraction"] as? Double,
                      taskId: event["taskId"] as? String)
        running[id] = job
        progress?.report(pid, title: Self.title(job.tool), stage: job.stage, fraction: job.fraction,
                         cancel: { [weak self] in self?.cancel(id) })
    }

    /// The pill's ✕: cancel in the core (the MCP Task too).
    func cancel(_ id: String) {
        cancelled.insert(id)
        Task { _ = await AgentCore.shared.rpc("vcad/jobs/cancel", ["jobId": id]) }
    }

    /// Progress from app-side work running under a job (reported back to the
    /// core so the agent's Task status moves too; the pill hears the echo).
    nonisolated static func report(_ id: String, fraction: Double?, stage: String?) {
        Task { @MainActor in
            var p: [String: Any] = ["jobId": id]
            if let fraction { p["fraction"] = fraction }
            if let stage { p["stage"] = stage }
            _ = await AgentCore.shared.rpc("vcad/jobs/progress", p)
        }
    }
}

/// Thread-safe set of cancelled job ids.
final class CancelSet: @unchecked Sendable {
    private let lock = NSLock()
    private var ids: Set<String> = []
    func insert(_ id: String) { lock.lock(); ids.insert(id); lock.unlock() }
    func contains(_ id: String) -> Bool { lock.lock(); defer { lock.unlock() }; return ids.contains(id) }
}

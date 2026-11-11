import Foundation

// One agent turn = one named undo step ("Agent: thickened walls").
//
// MCP has no notion of a turn, so the agent opens one: `agent_turn
// {label}` before its first edit. Every edit until `agent_turn {end:true}`
// (or 30 s of silence) folds into the same undo entry, which ⌘Z rewinds as a
// whole. Without an open turn each call stays its own step, as before.
//
// A user edit in the middle splits the turn: the agent's later edits start a
// new entry, so ⌘Z never takes the user's own work along with the agent's.

@MainActor
final class AgentTurn {
    static let shared = AgentTurn()

    static let idleTimeout: TimeInterval = 30

    private(set) var label: String?
    private var lastActivity: Date?
    /// The engine state right after this turn's last edit; a mismatch at the
    /// next edit means someone else edited in between.
    private var afterHash: String?
    /// The undo entry name this turn owns once it has pushed one.
    private var entryName: String?

    var now: () -> Date = Date.init

    func begin(_ label: String) {
        let trimmed = label.trimmingCharacters(in: .whitespacesAndNewlines)
        self.label = trimmed.isEmpty ? "Edit" : trimmed
        lastActivity = now()
        afterHash = nil
        entryName = nil
    }

    func end() {
        label = nil; lastActivity = nil; afterHash = nil; entryName = nil
    }

    var isOpen: Bool {
        guard label != nil, let t = lastActivity else { return false }
        return now().timeIntervalSince(t) < Self.idleTimeout
    }

    /// The undo name for an edit that would otherwise be `fallback`.
    func undoName(fallback: String) -> String {
        guard isOpen, let label else { return fallback }
        return "Agent: \(label)"
    }

    /// Run `body` (which may push undo entries of its own, or none) and land
    /// everything it changed as this turn's single undo entry.
    @discardableResult
    func perform<T>(_ m: EditorModel, name fallback: String, _ body: () throws -> T) throws -> T {
        guard let engine = m.agentEnsureEngine() else {
            throw AgentTools.Failure(message: "The open document is view-only (\(m.documentName)).")
        }
        if !isOpen, label != nil { end() }   // timed out
        let before = engine.save()
        let hashBefore = engine.stateHash
        let depth = m.undoStack.count
        let result = try body()
        // Every edit lands, even one the state hash can't see (materials):
        // the hash only decides whether the turn's entry can absorb it.
        let hashAfter = engine.stateHash
        let name = undoName(fallback: fallback)
        let continuing = isOpen && entryName == name && afterHash == hashBefore
            && m.undoActionName == name
        m.agentCommitUndo(before: before, above: depth, coalesce: continuing, name: name)
        if isOpen {
            entryName = name
            afterHash = hashAfter
            lastActivity = now()
        }
        return result
    }
}

// MARK: - tool

extension AgentTurn {

    func call(_ a: [String: Any]) -> [String: Any] {
        if a["end"] as? Bool == true {
            let was = label
            end()
            return AgentTools.text(was.map { "Turn \"\($0)\" closed." } ?? "No turn was open.")
        }
        begin(a["label"] as? String ?? "Edit")
        return AgentTools.text("Turn open: your edits land as one undo step \"Agent: \(label ?? "")\".")
    }
}

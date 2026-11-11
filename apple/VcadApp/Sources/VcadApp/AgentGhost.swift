import Foundation
import Observation
import RealityKit
import SwiftUI

// Ghost preview: a destructive or large agent edit waits for the user.
//
// The edit lands (so the viewport already shows the AFTER), the parts it
// removed stay on screen as translucent red ghosts (the BEFORE), and a pill
// asks Apply / Reject. The agent's tool call blocks until the user answers:
// Apply keeps the edit, Reject undoes exactly that step and tells the agent
// nothing changed. No answer in `timeout` counts as Reject (fail closed).
//
// What needs review: removing or consuming a part the agent did not create
// this session (the user's work), and a whole-document replace that drops
// parts or rewrites most of the document. Booleans on the agent's own fresh
// parts never stop it — a bracket with ten holes is not ten prompts.
//
// Off with `VCAD_AGENT_GHOST=off` or the `agentGhostPreview` default.

@MainActor
@Observable
final class AgentGhost {
    static let shared = AgentGhost()

    struct Pending: Equatable {
        var label: String
        var summary: String
    }

    private(set) var pending: Pending?
    @ObservationIgnored private var continuation: CheckedContinuation<Bool, Never>?
    /// Translucent clones of the parts the edit removed (re-parented into
    /// every geometry rebuild while the review is open).
    @ObservationIgnored private(set) var ghostEntity: Entity?

    /// Part ids the agent created this session: removing those needs no review.
    @ObservationIgnored var agentCreated: Set<String> = []

    @ObservationIgnored var timeout: Duration = .seconds(180)
    /// Test seam: answer every review immediately (true = Apply).
    @ObservationIgnored var autoDecision: Bool?
    /// Test seam: reviews asked, in order.
    @ObservationIgnored var asked: [Pending] = []

    var enabled: Bool {
        if ProcessInfo.processInfo.environment["VCAD_AGENT_GHOST"] == "off" { return false }
        return UserDefaults.standard.object(forKey: "agentGhostPreview") as? Bool ?? true
    }

    /// Tools that can remove a part (so their undo entry must stand alone).
    static let mayRemoveParts: Set<String> = ["delete", "union", "difference", "intersection"]

    // MARK: deciding

    /// Could this call remove a part the agent did not create? Only those
    /// edits get their own undo entry and a review.
    func touchesUserParts(_ tool: String, _ a: [String: Any]) -> Bool {
        guard Self.mayRemoveParts.contains(tool) else { return false }
        let inputs = ["part_id", "left", "right"].compactMap { a[$0] as? String }
        return inputs.contains { !agentCreated.contains($0) }
    }

    /// Why an edit needs the user's eyes, or nil. `before`/`after` are the
    /// window's part ids around the edit.
    func reason(before: [String], after: [String], wholeDocument: Bool = false) -> String? {
        let gone = before.filter { !after.contains($0) && !agentCreated.contains($0) }
        if !gone.isEmpty {
            let names = gone.prefix(3).joined(separator: ", ") + (gone.count > 3 ? " +\(gone.count - 3)" : "")
            return "removes \(gone.count == 1 ? "part" : "\(gone.count) parts") \(names)"
        }
        if wholeDocument, before.count >= 3 {
            let kept = before.filter(after.contains).count
            if kept * 2 < before.count { return "rewrites \(before.count - kept) of \(before.count) parts" }
        }
        return nil
    }

    /// Show the ghost + pill and wait for the user. True = Apply.
    func review(_ p: Pending, ghost: Entity?) async -> Bool {
        asked.append(p)
        if let d = autoDecision { return d }
        pending = p
        ghostEntity = ghost
        attachGhost()
        AgentViewer.focusWindow()
        let t = timeout
        let timer = Task { @MainActor [weak self] in
            try? await Task.sleep(for: t)
            if !Task.isCancelled { self?.decide(false) }
        }
        let ok = await withCheckedContinuation { continuation = $0 }
        timer.cancel()
        return ok
    }

    func decide(_ apply: Bool) {
        guard let c = continuation else { return }
        continuation = nil
        pending = nil
        ghostEntity?.removeFromParent()
        ReleaseWindowController.shared.centeringEntity?.findEntity(named: "agentGhost")?.removeFromParent()
        ghostEntity = nil
        c.resume(returning: apply)
    }

    // MARK: the ghost geometry

    /// Clone the part entities about to be removed, before the edit runs.
    static func capture(parts: [Int]) -> Entity? {
        guard let centering = ReleaseWindowController.shared.centeringEntity, !parts.isEmpty else { return nil }
        let root = Entity()
        root.name = "agentGhost"
        for i in parts {
            guard let e = centering.findEntity(named: "part\(i)") else { continue }
            let c = e.clone(recursive: true)
            c.name = "ghost\(i)"
            ghostify(c)
            root.addChild(c)
        }
        return root.children.isEmpty ? nil : root
    }

    private static func ghostify(_ e: Entity) {
        if let m = e as? ModelEntity, var model = m.model {
            var mat = UnlitMaterial(color: .init(red: 1, green: 0.32, blue: 0.28, alpha: 1))
            mat.blending = .transparent(opacity: .init(floatLiteral: 0.32))
            model.materials = model.materials.map { _ in mat }
            m.model = model
            m.components.remove(CollisionComponent.self)   // never pickable
        }
        for child in e.children { ghostify(child) }
    }

    /// Put the ghost under the current centering entity (called after every
    /// geometry rebuild, which replaces it).
    func attachGhost() {
        guard let g = ghostEntity, let centering = ReleaseWindowController.shared.centeringEntity else { return }
        centering.findEntity(named: "agentGhost")?.removeFromParent()
        centering.addChild(g.clone(recursive: true))
    }
}

// MARK: - pill

/// "Claude wants to delete Bracket — removes part bracket. [Reject] [Apply]"
struct AgentGhostBar: View {
    var body: some View {
        let ghost = AgentGhost.shared
        if let p = ghost.pending {
            HStack(spacing: 10) {
                Image(systemName: "exclamationmark.triangle.fill")
                    .foregroundStyle(AgentCursorGlyph.tint)
                VStack(alignment: .leading, spacing: 1) {
                    Text(p.label).font(.callout.weight(.semibold))
                    Text(p.summary + " — red is what goes away").font(.caption).foregroundStyle(.secondary)
                }
                Button("Reject") { ghost.decide(false) }
                    .keyboardShortcut(.escape, modifiers: [])
                Button("Apply") { ghost.decide(true) }
                    .keyboardShortcut(.return, modifiers: [])
                    .buttonStyle(.borderedProminent)
                    .tint(AgentCursorGlyph.tint)
            }
            .padding(.horizontal, 14).padding(.vertical, 9)
            .pillSurface()
            .padding(.bottom, 72)
            .accessibilityElement(children: .contain)
            .accessibilityLabel("Agent edit waiting for review: \(p.label)")
        }
    }
}

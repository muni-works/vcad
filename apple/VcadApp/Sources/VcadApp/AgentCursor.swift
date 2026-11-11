import Foundation
import Observation
import RealityKit
import simd
import SwiftUI

// The agent cursor: a second, labeled pointer in the viewport that shows
// where the agent is working.
//
// v1 (this file): after each agent edit the cursor glides to the geometry the
// tool touched — the edges the user had picked, else the part it created or
// rebuilt — and says what it did ("Agent · Fillet"). It is a reconstruction,
// not the agent's real focus, but it reads as a collaborator in the room.
//
// v2 seam: `Mode.live`. When the agent drives camera and selection for real
// (the bridge's set_camera / select tools streaming intent ahead of the
// edit), the bridge sets `target` directly before acting instead of
// `pointAt(touched:)` reconstructing it after. The overlay already only
// reads `target`, so v2 is a change to who writes it, not to the view.

/// Where the agent cursor points.
enum AgentCursorTarget: Equatable {
    /// A point in kernel space (mm, Z-up), e.g. an edge midpoint.
    case point(SIMD3<Float>)
    /// The visual centre of a viewport part.
    case part(Int)
}

@MainActor
@Observable
final class AgentCursor {
    static let shared = AgentCursor()

    enum Mode { case reconstructed, live }
    /// v1 always reconstructs; `live` is the v2 seam (see header).
    var mode: Mode = .reconstructed

    private(set) var target: AgentCursorTarget?
    private(set) var label: String = "Agent"
    /// Bumped on every move so the overlay restarts its glide + pulse.
    private(set) var seq = 0
    private(set) var movedAt: Date = .distantPast

    /// Seconds the cursor stays fully visible after its last move.
    static let linger: TimeInterval = 6

    /// Agent display name; MCP clients identify themselves at `initialize`.
    var agentName = "Agent"

    func move(to t: AgentCursorTarget, label: String) {
        target = t
        self.label = label
        seq &+= 1
        movedAt = Date()
    }

    func hide() { target = nil }

    /// How long the cursor travels ahead of an edit before it lands.
    static var leadInDelay: Duration = .milliseconds(280)

    /// Before an edit: glide to where it is about to act, labelled with what
    /// it is doing ("Claude Code · Delete Bracket…"), and give the glide a
    /// beat on screen. The edit's own `pointAt` lands it afterwards.
    func leadIn(to t: AgentCursorTarget?, verb: String) async {
        guard let t, mode == .reconstructed else { return }
        move(to: t, label: Self.caption(verb, agent: agentName) + "…")
        #if canImport(AppKit)
        guard ReleaseWindowController.shared.hostWindow?.isVisible == true else { return }
        try? await Task.sleep(for: Self.leadInDelay)
        #endif
    }

    // MARK: v1: reconstruct from what the edit touched

    /// What the user was pointing at before an edit (the edit's re-evaluation
    /// clears topology picks, so read it first).
    struct Focus { var part: Int?; var point: SIMD3<Float>? }

    static func focus(_ m: EditorModel) -> Focus {
        let edges = m.picking.selectedEdges
        let faces = m.picking.selectedFaces
        if let part = m.picking.selectionPart, let topo = m.partTopology(part) {
            let pts: [SIMD3<Float>] = edges.compactMap { topo.edge($0.id)?.midpoint }
                + faces.compactMap { topo.face($0.id)?.centerF }
            if !pts.isEmpty { return Focus(part: part, point: pts.reduce(.zero, +) / Float(pts.count)) }
        }
        return Focus(part: m.selectedPartIndex, point: nil)
    }

    /// After an agent edit: glide to what it touched.
    func pointAt(touched r: DocumentEngine.Result, verb: String, focus: Focus, model m: EditorModel) {
        guard mode == .reconstructed else { return }
        let ids = Set([r.createdFeatureId].compactMap { $0 }).union(r.changes?.rebuild ?? [])
        let touchedPart = (0..<m.partCount).first { i in m.partId(forPart: i).map(ids.contains) ?? false }
        let target: AgentCursorTarget?
        if let p = focus.point, focus.part == touchedPart || touchedPart == nil {
            target = .point(p)
        } else if let i = touchedPart ?? focus.part {
            target = .part(i)
        } else {
            target = nil
        }
        guard let target else { return }
        move(to: target, label: Self.caption(verb, agent: agentName))
    }

    /// "Claude: Fillet Edges" / "Agent: thickened walls" → "Claude · Fillet Edges".
    static func caption(_ undoName: String, agent: String) -> String {
        var action = undoName
        for prefix in ["Agent: ", "Claude: "] where action.hasPrefix(prefix) {
            action = String(action.dropFirst(prefix.count))
        }
        return "\(agent) · \(action)"
    }
}

// MARK: - overlay

/// Draws the agent cursor over the viewport. Re-projects every frame so it
/// stays on its geometry through orbit, pan and zoom, and eases toward a new
/// target instead of jumping.
struct AgentCursorOverlay: View {
    let model: EditorModel
    @State private var glide = Glide()

    var body: some View {
        let cursor = AgentCursor.shared
        if cursor.target == nil || Date().timeIntervalSince(cursor.movedAt) > AgentCursor.linger + 1.5 {
            // Observed reads above re-render this when the cursor moves.
            Color.clear.frame(width: 0, height: 0).allowsHitTesting(false)
        } else {
            TimelineView(.animation) { tl in
                let age = tl.date.timeIntervalSince(cursor.movedAt)
                let opacity = age < AgentCursor.linger ? 1 : max(0, 1 - (age - AgentCursor.linger) / 1.5)
                if let goal = cursor.target.flatMap({ Self.screenPoint($0, model: model) }) {
                    let p = glide.step(toward: goal, seq: cursor.seq, at: tl.date)
                    AgentCursorGlyph(label: cursor.label, pulse: age)
                        .position(x: p.x + 70, y: p.y + 16)   // glyph's tip sits at p
                        .opacity(opacity)
                }
            }
            .allowsHitTesting(false)
            .accessibilityLabel(cursor.label)
        }
    }

    /// Screen point (overlay coords, top-left origin) of a cursor target.
    static func screenPoint(_ t: AgentCursorTarget, model: EditorModel) -> CGPoint? {
        let rw = ReleaseWindowController.shared
        guard let centering = rw.centeringEntity else { return nil }
        let world: SIMD3<Float>
        switch t {
        case .point(let p):
            // Topology overlays live in the centering entity's frame, in kernel mm.
            world = centering.convert(position: p, to: nil)
        case .part(let i):
            guard let e = centering.findEntity(named: "part\(i)") else { return nil }
            world = e.visualBounds(relativeTo: nil).center
        }
        return rw.viewPoint(forWorld: world, from: model.cameraPosition, lookAt: model.panOffset)
    }

    /// Critically-damped ease from wherever the cursor was to the goal.
    final class Glide {
        private var pos: CGPoint?
        private var seq = -1
        private var last: Date?

        func step(toward goal: CGPoint, seq s: Int, at now: Date) -> CGPoint {
            defer { last = now }
            guard var p = pos else { pos = goal; seq = s; return goal }
            if s != seq { seq = s }
            let dt = min(0.05, now.timeIntervalSince(last ?? now))
            let k = 1 - exp(-dt * 7)   // ~0.45 s to settle
            p.x += (goal.x - p.x) * k
            p.y += (goal.y - p.y) * k
            pos = p
            return p
        }
    }
}

/// The pointer arrow and its name tag.
struct AgentCursorGlyph: View {
    let label: String
    var pulse: TimeInterval = 10

    static let tint = Color(red: 0.55, green: 0.42, blue: 0.98)

    var body: some View {
        HStack(alignment: .top, spacing: 2) {
            ZStack {
                // A landing ring for the first moment after each move.
                if pulse < 0.8 {
                    Circle().stroke(Self.tint.opacity(1 - pulse / 0.8), lineWidth: 2)
                        .frame(width: 8 + pulse * 40, height: 8 + pulse * 40)
                        .offset(x: -7, y: -9)
                }
                Image(systemName: "cursorarrow")
                    .font(.system(size: 22, weight: .semibold))
                    .foregroundStyle(Self.tint)
                    .shadow(color: .black.opacity(0.35), radius: 2, y: 1)
            }
            Text(label)
                .font(.callout.weight(.semibold))
                .foregroundStyle(.white)
                .padding(.horizontal, 9).padding(.vertical, 4)
                .background(Self.tint, in: Capsule())
                .fixedSize()
                .offset(y: 16)
        }
        .frame(width: 140, height: 32, alignment: .topLeading)
    }
}

import CVcadFFI
import Foundation
import RealityKit

// The editor's side of the document runtime (docs/runtime-design.md, slice 1).
//
// Parameter and node-field scrubs no longer touch the engine on the main
// thread: they go to the Rust runtime as fire-and-forget intents, which
// coalesces them (a scrub keeps only its latest value), materializes,
// serializes and evaluates on its own thread, and publishes a frame. The
// frame is decoded and its meshes gathered on the engine's frame queue; the
// main thread only swaps in the result and builds MeshResources for parts
// whose content key changed.
//
// SHIM(runtime): every other edit still makes a blocking `DocumentEngine`
// call (now executed on the runtime thread, in order with the intents).
// Instanced (pattern) documents still re-tessellate on main after a frame.

extension EditorModel {
    /// Route frames from a (new) engine to this editor.
    func installRuntimeSink() {
        adoptedRuntimeRev = 0
        runtimeGesture = nil
        collideAfterSeq = 0
        guard let engine else { return }
        engine.onFrame = { [weak self] frame in
            // Off main: pull the meshes out of the scene before hopping.
            let parts: PartsBox? = frame.takeScene().map { scene in
                defer { vcad_scene_free(scene) }
                return PartsBox(EditorModel.gatherParts(scene, count: vcad_scene_part_count(scene)))
            }
            DispatchQueue.main.async {
                MainActor.assumeIsolated { self?.adoptRuntimeFrame(frame, parts: parts?.items) }
            }
        }
    }

    /// Send one scrub event to the runtime. False when there is no engine,
    /// so the caller falls back to its blocking path.
    func runtimeScrub(_ name: String, snapshot: Bool,
                      _ make: (_ gesture: UInt64, _ start: Bool) -> DocumentEngine.Intent) -> Bool {
        guard let engine, runtimeScrubEnabled else { return false }
        let start = snapshot || runtimeGesture == nil
        if start {
            if let open = runtimeGesture { engine.send(.gestureEnd(open)) }
            lastGestureId &+= 1
            runtimeGesture = lastGestureId
        }
        guard let gid = runtimeGesture else { return false }
        let seq = engine.send(make(gid, start))
        guard seq > 0 else { return false }
        if start {
            pushUndo(.gesture(gid), name: name)
            collideAfterSeq = seq
        }
        engineError = nil
        return true
    }

    /// `VCAD_RUNTIME_SCRUB=0` restores the old blocking scrub path (A/B).
    nonisolated static let runtimeScrubDisabled = ProcessInfo.processInfo.environment["VCAD_RUNTIME_SCRUB"] == "0"

    /// Gesture snapshots whose undo entries are gone.
    func forgetGestures(_ entries: [UndoEntry]) {
        guard let engine else { return }
        for e in entries { if case .gesture(let id) = e.step { engine.forgetGesture(id) } }
    }

    /// Whether scrub edits are still in flight (frames not yet adopted).
    var runtimeEditsInFlight: Bool { engine?.intentsInFlight ?? false }

    /// Adopt a frame: the document, its bytes, the feature tree and the part
    /// meshes, all built off main. Frames older than a blocking edit (or an
    /// already-adopted frame) are dropped.
    func adoptRuntimeFrame(_ frame: DocumentEngine.Frame,
                           parts: [(mesh: KernelMesh, edges: [SIMD3<Float>], rootKey: String?)?]?) {
        guard let engine, frame.rev > adoptedRuntimeRev, frame.rev > engine.lastRev else { return }
        adoptedRuntimeRev = frame.rev
        engine.adopt(frame)
        engineError = frame.errors.isEmpty ? nil : frame.errors.joined(separator: "\n")
        guard let r = frame.result else { return }
        engineState = r
        adoptingDelta = true                    // meshes below decide what's stale
        documentJSON = r.document
        adoptingDelta = false
        documentBytesCache = (documentRevision, frame.documentBytes)
        documentDirty = frame.stateHash != savedHash
        if let g = frame.graph {
            documentGraph = g
            adoptingRuntimeFrame = frame.history != nil
            featureNodes = g.featureRoots()
            adoptingRuntimeFrame = false
            if let rows = frame.history, usesDocumentTree, rows != history { history = rows }
        }
        if let parts, let g = documentGraph, parts.count == g.visibleRoots.count, documentInstancingPlan().isEmpty {
            for (i, root) in g.visibleRoots.enumerated() {
                guard let id = r.partId(forRootNode: root.nodeId) else { continue }
                guard let p = parts[i] else { partMeshCache.removeValue(forKey: id); continue }
                if let old = partMeshCache[id], let k = old.rootKey, k == p.rootKey { continue }
                partMeshCache[id] = TessellatedPart(mesh: p.mesh, edges: p.edges,
                                                    resource: p.mesh.resource(name: "part" + String(i)),
                                                    rootKey: p.rootKey)
            }
        } else {
            // SHIM(runtime): instanced roots (or a failed evaluation) take the
            // old path — `reevalDocumentMeshes` re-tessellates on main.
            partMeshCache.removeAll()
            if let e = frame.evalError { loadError = e }
        }
        let collide = collideAfterSeq > 0 && frame.ackedSeq >= collideAfterSeq
        if collide { collideAfterSeq = 0 }
        docParamDirty = true
        ReleaseWindowController.shared.refreshGeometry(model: self, collisions: collide, fromRuntime: true)
    }
}

/// Gathered meshes crossing from the frame queue to main.
private final class PartsBox: @unchecked Sendable {
    let items: [(mesh: KernelMesh, edges: [SIMD3<Float>], rootKey: String?)?]
    init(_ items: [(mesh: KernelMesh, edges: [SIMD3<Float>], rootKey: String?)?]) { self.items = items }
}

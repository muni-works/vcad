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
        picking.runtimeScenes = engine.map { e in { e.currentScene() } }
        AnalysisMeshCache.shared.sceneProvider = engine.map { e in { @Sendable in e.currentScene() } }
        guard let engine else { return }
        let topologies = TopologyCache()       // frame-queue confined
        engine.onFrame = { [weak self] frame in
            // Off main: pull the meshes out of the scene and build each part's
            // B-rep topology (reused across frames by part fingerprint), so
            // neither the viewport nor picking evaluates or decodes on main.
            let extras = frame.takeScene().map { scene in
                let count = vcad_scene_part_count(scene)
                let (topos, analysis) = topologies.update(scene, count: count, keys: frame.partKeys)
                return FrameExtras(scene: scene,
                                   parts: EditorModel.gatherParts(scene, count: count),
                                   topologies: topos, analysis: analysis)
            }
            DispatchQueue.main.async {
                MainActor.assumeIsolated {
                    guard let self else { extras?.release(); return }
                    self.adoptRuntimeFrame(frame, extras: extras)
                }
            }
        }
        // Whatever the engine already holds (an opened document) gets a frame,
        // so picking has the runtime's scene from the start.
        engine.send(.publish)
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

    /// What a part's vibration modes depend on — its geometry's content key
    /// and material — or nil when unknown (no runtime-built mesh yet).
    func soundModesKey(part: Int) -> String? {
        guard engine != nil, let id = partId(forPart: part), let k = partMeshCache[id]?.contentKey else { return nil }
        return k + "|" + (engineState?.material(ofPart: id) ?? "")
    }

    /// Whether scrub edits are still in flight (frames not yet adopted).
    var runtimeEditsInFlight: Bool { engine?.intentsInFlight ?? false }

    /// Adopt a frame: the document, its bytes, the feature tree and the part
    /// meshes, all built off main. Frames older than a blocking edit (or an
    /// already-adopted frame) are dropped.
    func adoptRuntimeFrame(_ frame: DocumentEngine.Frame, extras: FrameExtras?) {
        guard let engine, frame.rev > adoptedRuntimeRev || (frame.rev == adoptedRuntimeRev && frame.ackedSeq > engine.adoptedSeq)
        else { extras?.release(); return }
        engine.adoptedSeq = max(engine.adoptedSeq, frame.ackedSeq)
        if frame.rev < engine.lastRev { extras?.release(); return }
        if frame.rev == engine.lastRev {
            // A blocking edit (or a publish) already put this revision in the
            // editor; the frame brings its scene for picking.
            adoptedRuntimeRev = frame.rev
            adoptPickingScene(frame, extras, sameDocument: true)
            return
        }
        adoptedRuntimeRev = frame.rev
        engine.adopt(frame)
        let parts = extras?.parts
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
                let key = i < frame.partKeys.count && !frame.partKeys[i].isEmpty ? frame.partKeys[i] : nil
                if let old = partMeshCache[id], let k = old.contentKey, k == key { continue }
                partMeshCache[id] = TessellatedPart(mesh: p.mesh, edges: p.edges,
                                                    resource: p.mesh.resource(name: "part" + String(i)),
                                                    rootKey: p.rootKey, contentKey: key)
            }
        } else {
            // SHIM(runtime): instanced roots (or a failed evaluation) take the
            // old path — `reevalDocumentMeshes` re-tessellates on main.
            partMeshCache.removeAll()
            if let e = frame.evalError { loadError = e }
        }
        adoptPickingScene(frame, extras)
        let collide = collideAfterSeq > 0 && frame.ackedSeq >= collideAfterSeq
        if collide { collideAfterSeq = 0 }
        docParamDirty = true
        ReleaseWindowController.shared.refreshGeometry(model: self, collisions: collide, fromRuntime: true)
    }
}

extension EditorModel {
    /// Hand the frame's scene and topologies to picking, keyed by the
    /// frame's document bytes (which then also serve `pickingDocumentData`).
    fileprivate func adoptPickingScene(_ frame: DocumentEngine.Frame, _ extras: FrameExtras?, sameDocument: Bool = false) {
        guard let extras, let scene = extras.take() else { return }
        // Keep whatever bytes this revision already has (keys elsewhere —
        // the viewport's prepared scene — depend on them); otherwise the
        // frame's, serialized off main.
        let key: Data
        if let c = documentBytesCache, c.revision == documentRevision { key = c.data }
        else { key = frame.documentBytes; documentBytesCache = (documentRevision, key) }
        picking.adopt(scene: scene, topologies: extras.topologies, data: key, sameDocument: sameDocument)
        AnalysisMeshCache.shared.adopt(key: key, parts: extras.analysis)
    }
}

/// What the frame queue built from a frame's scene, crossing to main. Owns
/// the scene until `take()` (or frees it).
final class FrameExtras: @unchecked Sendable {
    private var scene: OpaquePointer?
    let parts: [(mesh: KernelMesh, edges: [SIMD3<Float>], rootKey: String?)?]
    let topologies: [Int: PartTopology]
    let analysis: [AnalysisMeshCache.Part]

    init(scene: OpaquePointer, parts: [(mesh: KernelMesh, edges: [SIMD3<Float>], rootKey: String?)?],
         topologies: [Int: PartTopology], analysis: [AnalysisMeshCache.Part]) {
        self.scene = scene; self.parts = parts; self.topologies = topologies; self.analysis = analysis
    }
    func take() -> OpaquePointer? { defer { scene = nil }; return scene }
    func release() { if let s = take() { vcad_scene_free(s) } }
    deinit { release() }
}

/// Per-part B-rep topology (picking) and face-id meshes (analysis overlay)
/// by the frame's part fingerprint, so a frame only builds the parts whose
/// geometry changed. Touched only on the engine's frame queue.
private final class TopologyCache: @unchecked Sendable {
    private var byKey: [String: (topo: PartTopology?, analysis: AnalysisMeshCache.Part)] = [:]

    func update(_ scene: OpaquePointer, count: Int, keys: [String]) -> ([Int: PartTopology], [AnalysisMeshCache.Part]) {
        var topos: [Int: PartTopology] = [:]
        var analysis: [AnalysisMeshCache.Part] = []
        var next: [String: (topo: PartTopology?, analysis: AnalysisMeshCache.Part)] = [:]
        for i in 0..<count {
            let key = i < keys.count && !keys[i].isEmpty ? keys[i] : nil
            let entry = key.flatMap { byKey[$0] } ?? (
                topo: vcad_scene_part_solid(scene, i).flatMap { PartTopology(solid: $0) },
                analysis: AnalysisMeshCache.part(scene, i))
            topos[i] = entry.topo
            analysis.append(entry.analysis)
            if let key { next[key] = entry }
        }
        byKey = next
        return (topos, analysis)
    }
}

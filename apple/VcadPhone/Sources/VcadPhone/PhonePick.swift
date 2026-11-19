import SwiftUI
import RealityKit
import UIKit

/// What a tap in the viewport does — the Mac's click semantics
/// (`ReleasedOverlayView.tapSelect` / `sketchTap` in ReleasedDesktop.swift),
/// ported for touch so every dock tool completes on the phone:
///
/// - sketching: the tap places sketch geometry (and the first tap picks the
///   sketch plane from a face under the finger)
/// - an armed primitive: the tap places it
/// - mallet: the tap strikes the part
/// - face/edge pick filters, Measure: the tap picks topology
/// - a pending Combine: the tap picks the tool part
/// - otherwise: select the part / instance (same spot again walks into the
///   stack), empty space deselects
///
/// Kept a port rather than shared code: the Mac's version lives in its
/// NSView overlay and reads click counts and modifier keys a touch never has.
@MainActor
enum PhonePick {
    private static var cycleAnchor: CGPoint?
    private static var cycleDepth = 0

    static func tap(at p: CGPoint, model: EditorModel, additive: Bool = false) {
        defer {
            if ProcessInfo.processInfo.environment["VCAD_PHONE_LOG"] != nil {
                NSLog("[phone] tap (%.0f,%.0f) filter=%@ feature=%@ picks=%d", p.x, p.y, model.picking.filter.rawValue,
                      String(describing: model.selectedFeatureID), model.picking.selection.count)
            }
        }
        if model.sketching { sketchTap(at: p, model: model); return }
        if SoundStore.shared.malletArmed { malletStrike(at: p, model: model); return }
        let ctl = ReleaseWindowController.shared

        if model.picking.filter.picksTopology { ensurePickableDocument(at: p, model: model) }
        guard model.usesDocumentTree || model.armedShape != nil else {
            selectBody(at: p, model: model, additive: additive); return
        }

        // Face/edge picking (and Measure): the tap chooses topology — unless a
        // Combine waits for its tool part or a primitive is armed.
        if model.picking.filter.picksTopology, model.armedShape == nil, model.pendingBoolean == nil {
            let hit = topologyHit(at: p, model: model)
            if hit != nil || bodyName(at: p) == nil || model.measuring {
                if hit == nil, !additive, !model.measuring {
                    model.deselectAll()
                } else {
                    if !additive, !model.measuring,
                       model.selectedFeatureID != nil || model.selectedInstanceIndex != nil
                        || !model.multiSelectedParts.isEmpty {
                        model.selectedFeatureID = nil
                        model.selectedInstanceIndex = nil
                        model.multiSelectedParts = []
                        model.selectionDirty = false
                    }
                    model.picking.select(hit: hit, additive: additive || model.measuring)
                }
                refreshTopologyOverlay(model: model)
                ctl.rebuildGizmo(model: model)
                return
            }
            model.picking.clear()
            refreshTopologyOverlay(model: model)
        }

        // Press-pull drill-down: a tap on the already-selected part picks the
        // planar face under the finger; tapping it again goes back to the body.
        if model.picking.filter == .body, model.armedShape == nil, model.pendingBoolean == nil, !additive {
            if let pi = model.selectedPartIndex, partIndex(at: p) == pi,
               let hit = topologyHit(at: p, model: model), hit.ref.kind == .face, hit.ref.part == pi,
               model.partTopology(pi)?.face(hit.ref.id)?.isPlanar == true,
               model.picking.selection != [hit.ref] {
                model.picking.select(hit: hit, additive: false)
                refreshTopologyOverlay(model: model)
                ctl.rebuildGizmo(model: model)
                return
            }
            if !model.picking.selection.isEmpty {
                model.picking.clear()
                refreshTopologyOverlay(model: model)
                ctl.rebuildGizmo(model: model)
            }
        }

        // An armed tool consumes the tap: a placement, not a selection.
        if let shape = model.armedShape {
            if let point = placementPoint(at: p, model: model) {
                model.addPrimitive(shape, at: point)
            } else {
                model.addPrimitive(shape)
            }
            model.disarm()
            return
        }
        selectBody(at: p, model: model, additive: additive)
    }

    /// Double tap — the Mac's double-click: reach the whole body (dropping a
    /// face/edge pick) and frame it; on empty space, frame everything.
    static func doubleTap(at p: CGPoint, model: EditorModel) {
        if ProcessInfo.processInfo.environment["VCAD_PHONE_LOG"] != nil {
            NSLog("[phone] tap double (%.0f,%.0f) body=%@", p.x, p.y, bodyName(at: p) ?? "-")
        }
        guard !model.sketching else { return }
        guard let name = bodyName(at: p) else { model.resetCamera(animated: true); return }
        if model.usesDocumentTree, model.armedShape == nil, model.pendingBoolean == nil {
            model.picking.clear()
            refreshTopologyOverlay(model: model)
            if name.hasPrefix("inst"), let i = Int(name.dropFirst(4)) {
                model.selectInstance(i)
            } else if let pi = Int(name.dropFirst(4)),
                      let fid = model.featureNodes.first(where: { $0.partIndex == pi })?.id {
                model.selectFeature(fid)
            }
            model.selectionDirty = false
        }
        ReleaseWindowController.shared.frame(entityNamed: name)
    }

    /// Instances first (assemblies), then the part stack under the finger.
    private static func selectBody(at p: CGPoint, model: EditorModel, additive: Bool) {
        let ctl = ReleaseWindowController.shared
        if model.assemblyInstanceCount > 0 {
            if let i = ctl.hits(atViewPoint: p).lazy.compactMap({ instanceIndex(of: $0.entity) }).first {
                model.selectInstance(i)
            } else if !additive {
                model.deselectAll()
            }
            return
        }
        var stack: [Int] = []
        for hit in ctl.hits(atViewPoint: p) {
            if let i = partIndex(of: hit.entity), !stack.contains(i) { stack.append(i) }
        }
        var depth = 0
        if let a = cycleAnchor, hypot(a.x - p.x, a.y - p.y) < 12, !stack.isEmpty {
            depth = (cycleDepth + 1) % stack.count
        }
        cycleAnchor = p
        cycleDepth = depth
        if depth < stack.count,
           let fid = model.featureNodes.first(where: { $0.partIndex == stack[depth] })?.id {
            if model.completePendingBoolean(withPart: stack[depth]) {
                // the tap picked the boolean's tool part
            } else if additive {
                model.toggleMultiSelect(part: stack[depth], featureID: fid)
            } else {
                model.selectFeature(fid)
            }
        } else if !additive {
            if model.hasSelection { model.deselectAll() }
        }
        model.selectionDirty = false
    }

    // MARK: sketch

    static func sketchTap(at p: CGPoint, model: EditorModel) {
        if model.sketchAwaitsPlane {
            let filter = model.picking.filter
            model.picking.filter = .face
            model.picking.select(hit: topologyHit(at: p, model: model), additive: false)
            let adopted = model.adoptPickedFaceAsSketchPlane()
            model.picking.filter = filter
            if adopted { refreshSketchInk(model: model); return }
        }
        guard let ray = kernelRay(at: p) else { return }
        model.sketchTap(ray: ray, neighbour: kernelRay(at: CGPoint(x: p.x + 1, y: p.y)))
        refreshSketchInk(model: model)
    }

    static func refreshSketchInk(model: EditorModel, force: Bool = false) {
        guard force || model.sketchDirty || model.sketching else { return }
        guard let centering = ReleaseWindowController.shared.centeringEntity else { return }
        centering.findEntity(named: "sketchRoot")?.removeFromParent()
        if model.sketching { centering.addChild(buildSketchRoot(model: model)) }
        syncSketchGrid(model, in: centering)
        model.sketchDirty = false
    }

    // MARK: overlays

    static func refreshTopologyOverlay(model: EditorModel) {
        guard let centering = ReleaseWindowController.shared.centeringEntity else { return }
        centering.findEntity(named: "topoOverlay")?.removeFromParent()
        if let root = topologyOverlayEntity(model: model) { centering.addChild(root) }
    }

    // MARK: rays + hits

    /// Screen point → kernel-space ray via the ARView's own ray and the
    /// centering entity. UIKit's ARView is top-left origin: no flip.
    static func kernelRay(at p: CGPoint) -> (o: SIMD3<Float>, d: SIMD3<Float>)? {
        guard let ar = ReleaseWindowController.shared.arView,
              let centering = ReleaseWindowController.shared.centeringEntity,
              let ray = ar.ray(through: p) else { return nil }
        let o = centering.convert(position: ray.origin, from: nil)
        let d = normalize(centering.convert(direction: ray.direction, from: nil))
        return (o, d)
    }

    static func topologyHit(at p: CGPoint, model: EditorModel) -> TopoHit? {
        guard let ray = kernelRay(at: p) else { return nil }
        let ctl = ReleaseWindowController.shared
        var parts: [Int] = []
        for hit in ctl.hits(atViewPoint: p) {
            if let i = partIndex(of: hit.entity), !parts.contains(i) { parts.append(i) }
        }
        var dist: Float?
        if let c = ctl.centeringEntity { dist = simd_distance(ray.o, c.convert(position: model.panOffset, from: nil)) }
        // A fingertip is wider than a pointer: scale the edge tolerance up.
        let h = ctl.arView.map { Float($0.bounds.height) / 2.5 }
        for part in parts {
            if let hit = model.pickTopology(part: part, origin: ray.o, dir: ray.d,
                                            cameraDistance: dist, viewportHeight: h) { return hit }
        }
        return nil
    }

    static func partIndex(at p: CGPoint) -> Int? {
        ReleaseWindowController.shared.hits(atViewPoint: p).lazy.compactMap { partIndex(of: $0.entity) }.first
    }

    private static func bodyName(at p: CGPoint) -> String? {
        for hit in ReleaseWindowController.shared.hits(atViewPoint: p) {
            if let i = instanceIndex(of: hit.entity) { return "inst\(i)" }
            if let i = partIndex(of: hit.entity) { return "part\(i)" }
        }
        return nil
    }

    /// "part3", "part3_inst7" → 3 (pattern children count as their part).
    static func partIndex(of entity: Entity) -> Int? {
        var e: Entity? = entity
        while let cur = e {
            if cur.name.hasPrefix("part"), let i = Int(cur.name.dropFirst(4).prefix(while: \.isNumber)) { return i }
            e = cur.parent
        }
        return nil
    }

    static func instanceIndex(of entity: Entity) -> Int? {
        var e: Entity? = entity
        while let cur = e {
            if cur.name.hasPrefix("inst"), let i = Int(cur.name.dropFirst(4)) { return i }
            e = cur.parent
        }
        return nil
    }

    private static func ensurePickableDocument(at p: CGPoint, model: EditorModel) {
        guard model.source.isSandbox, model.documentJSON == nil, partIndex(at: p) != nil else { return }
        model.promoteSandboxToDocument(silent: true)
    }

    // MARK: placement + mallet (verbatim from the Mac overlay)

    private static func placementPoint(at p: CGPoint, model: EditorModel) -> SIMD3<Float>? {
        let ctl = ReleaseWindowController.shared
        guard let centering = ctl.centeringEntity else { return nil }
        if let hit = ctl.hits(atViewPoint: p).first(where: { partIndex(of: $0.entity) != nil }) {
            return centering.convert(position: hit.position, from: nil)
        }
        var ground: SIMD3<Float>?
        if let w = PlacementMath.worldRay(viewPoint: p, viewSize: ctl.overlaySize,
                                          camera: model.cameraPosition, lookAt: model.panOffset) {
            let o = centering.convert(position: w.o, from: nil)
            let d = centering.convert(position: w.o + w.d, from: nil) - o
            ground = PlacementMath.groundHit(origin: o, direction: normalize(d))
        }
        let bodies = centering.children.filter {
            $0.name.hasPrefix("part") || $0.name.hasPrefix("inst") || $0.name.hasPrefix("free")
        }
        var lo = SIMD3<Float>(repeating: .infinity), hi = -lo
        for e in bodies {
            let b = e.visualBounds(relativeTo: centering)
            lo = simd_min(lo, b.min); hi = simd_max(hi, b.max)
        }
        let has = !bodies.isEmpty && lo.x <= hi.x
        var fallback = SIMD3<Float>(0, 0, 0)
        if let pi = model.selectedPartIndex, let e = centering.findEntity(named: "part\(pi)") {
            let b = e.visualBounds(relativeTo: centering)
            fallback = SIMD3(b.max.x + 10 + 15, b.center.y, 0)
        }
        return PlacementMath.resolve(hit: ground,
                                     sceneCenter: has ? (lo + hi) / 2 : .zero,
                                     sceneExtent: has ? simd_reduce_max(hi - lo) : 0,
                                     fallback: fallback)
    }

    private static func malletStrike(at p: CGPoint, model: EditorModel) {
        ensurePickableDocument(at: p, model: model)
        guard let ray = kernelRay(at: p),
              let part = nearestPartHit(model, origin: ray.o, dir: simd_normalize(ray.d)) ?? partIndex(at: p) else { return }
        let store = SoundStore.shared
        Task { @MainActor in
            guard let pm = await store.ensureModes(part: part, model) else { return }
            if let c = ReleaseWindowController.shared.centeringEntity { syncSoundOverlay(store, in: c) }
            if let hit = soundHit(pm, origin: ray.o, dir: ray.d) {
                store.strike(vertex: hit.vertex, dir: hit.dir)
            } else {
                store.playMode(0)
            }
        }
    }
}

import SwiftUI
import RealityKit
import UIKit

/// The iPhone viewport: a non-AR RealityKit ARView showing the document
/// scene (parts, pattern instancing, assembly instances, materials, feature
/// edges, selection, gizmo, sketch ink) through the SAME shared builders the
/// Mac and Vision Pro use (`EditorModel.buildScene`, `buildGizmo`,
/// `buildSketchRoot`), driven by touch:
///
/// - one finger drag: orbit (flick to coast — the model's inertial orbit)
/// - two finger drag: pan
/// - pinch: zoom
/// - tap: the Mac's click (PhonePick: face/edge pick, sketch, place, combine…)
/// - double tap: the Mac's double-click — select the whole body and frame it
/// - long press: select + context actions (`onLongPress`)
struct PhoneViewport: View {
    static let fovDegrees: Float = 60

    @Bindable var model: EditorModel
    var onLongPress: (Int) -> Void = { _ in }

    var body: some View {
        // Read here so Observation re-runs updateUIView when they change.
        PhoneARView(model: model,
                    cameraPosition: model.cameraPosition,
                    lookAt: model.panOffset,
                    geometryKey: geometryKey,
                    geometryDirty: model.geometryDirty,
                    paramDirty: model.docParamDirty,
                    playbackDirty: model.playbackDirty,
                    selection: model.highlightedParts,
                    selectedInstances: model.selectedInstances,
                    hidden: model.hiddenParts,
                    isolated: model.isolatedPart,
                    sketchDirty: model.sketchDirty,
                    onLongPress: onLongPress)
            .ignoresSafeArea()
    }

    /// Everything that requires rebuilding the entity tree. Selection is NOT
    /// here (it repaints materials in place), nor are parameter scrubs (they
    /// swap meshes in place).
    private var geometryKey: String {
        "\(model.source.id)|\(model.triangleCount)|\(model.sketching)|\(model.showsGizmo)|\(model.zebraMode)|\(model.playbackTime)|\(model.baseShape)|\(model.modifier)|\(model.modifierValue)"
    }
}

private struct PhoneARView: UIViewRepresentable {
    let model: EditorModel
    let cameraPosition: SIMD3<Float>
    let lookAt: SIMD3<Float>
    let geometryKey: String
    let geometryDirty: Bool
    let paramDirty: Bool
    let playbackDirty: Bool
    let selection: Set<Int>
    let selectedInstances: Set<Int>
    let hidden: Set<Int>
    let isolated: Int?
    let sketchDirty: Bool
    let onLongPress: (Int) -> Void

    final class Coordinator: NSObject, UIGestureRecognizerDelegate {
        var model: EditorModel?
        var camera: PerspectiveCamera?
        var anchor: AnchorEntity?
        var builtKey = ""
        var builtSourceID: String?
        var builtSelection: Set<Int> = []
        var builtInstances: Set<Int> = []
        var builtHidden: Set<Int> = []
        var builtIsolated: Int?
        var instanceEntities: [ModelEntity] = []
        var onLongPress: (Int) -> Void = { _ in }
        private var lastPan: CGPoint = .zero
        private var lastTwo: CGPoint = .zero

        // MARK: gestures

        @objc func orbit(_ g: UIPanGestureRecognizer) {
            guard let model else { return }
            let t = g.translation(in: g.view)
            switch g.state {
            case .began:
                lastPan = .zero
                model.beginOrbit()
            case .changed:
                model.orbitDrag(dx: Float(t.x - lastPan.x), dy: Float(t.y - lastPan.y))
                lastPan = t
            case .ended, .cancelled:
                model.endOrbit()
            default: break
            }
        }

        @objc func pan(_ g: UIPanGestureRecognizer) {
            guard let model else { return }
            let t = g.translation(in: g.view)
            switch g.state {
            case .began:
                lastTwo = .zero
                model.stopSpin()
            case .changed:
                // Content follows the fingers.
                model.panBy(dx: Float(t.x - lastTwo.x), dy: Float(t.y - lastTwo.y))
                lastTwo = t
            default: break
            }
        }

        @objc func pinch(_ g: UIPinchGestureRecognizer) {
            guard let model else { return }
            switch g.state {
            case .began:
                model.stopSpin()
                model.cameraUserMoved = true
                model.pinchBaseline = model.distance
            case .changed:
                model.distance = max(0.45, min(8.0, model.pinchBaseline / Float(max(g.scale, 0.01))))
            case .ended, .cancelled:
                model.pinchBaseline = model.distance
            default: break
            }
        }

        @objc func tap(_ g: UITapGestureRecognizer) {
            guard let model, let ar = g.view as? ARView else { return }
            model.stopSpin()
            // The Mac's click semantics: sketch, place, strike, pick, combine, select.
            PhonePick.tap(at: g.location(in: ar), model: model)
        }

        @objc func doubleTap(_ g: UITapGestureRecognizer) {
            guard let model, let ar = g.view as? ARView else { return }
            PhonePick.doubleTap(at: g.location(in: ar), model: model)
        }

        @objc func longPress(_ g: UILongPressGestureRecognizer) {
            guard g.state == .began, let model, let ar = g.view as? ARView else { return }
            if case .part(let pi) = pickTarget(ar, at: g.location(in: ar)) {
                if let fid = model.featureNodes.first(where: { $0.partIndex == pi })?.id { model.selectFeature(fid) }
                UIImpactFeedbackGenerator(style: .medium).impactOccurred()
                onLongPress(pi)
            }
        }

        enum Target { case part(Int), instance(Int), none }

        /// Nearest part/instance under a touch: "part3", "part3_inst7" → part
        /// 3; "inst2" → instance 2. Edge ribbons and the grid aren't pickable.
        private func pickTarget(_ ar: ARView, at p: CGPoint) -> Target {
            for hit in ar.hitTest(p) {
                var e: Entity? = hit.entity
                while let cur = e {
                    if cur.name.hasPrefix("inst"), let i = Int(cur.name.dropFirst(4)) { return .instance(i) }
                    if cur.name.hasPrefix("part") {
                        let digits = cur.name.dropFirst(4).prefix(while: \.isNumber)
                        if let i = Int(digits) { return .part(i) }
                    }
                    e = cur.parent
                }
            }
            return .none
        }

        func gestureRecognizer(_ g: UIGestureRecognizer,
                               shouldRecognizeSimultaneouslyWith other: UIGestureRecognizer) -> Bool {
            // Pan + pinch together (the two-finger family); orbit stays alone.
            let twoFinger: (UIGestureRecognizer) -> Bool = {
                $0 is UIPinchGestureRecognizer || ($0 as? UIPanGestureRecognizer)?.minimumNumberOfTouches == 2
            }
            return twoFinger(g) && twoFinger(other)
        }
    }

    func makeCoordinator() -> Coordinator { Coordinator() }

    func makeUIView(context: Context) -> ARView {
        let ar = ARView(frame: .zero, cameraMode: .nonAR, automaticallyConfigureSession: false)
        ar.environment.background = .color(UIColor { $0.userInterfaceStyle == .dark
            ? UIColor(white: 0.11, alpha: 1) : UIColor(white: 0.93, alpha: 1) })
        ar.renderOptions.insert(.disableMotionBlur)
        let c = context.coordinator
        c.model = model
        c.onLongPress = onLongPress
        ReleaseWindowController.shared.arView = ar
        ReleaseWindowController.shared.model = model

        let anchor = AnchorEntity(world: .zero)
        let camera = PerspectiveCamera()
        camera.camera.fieldOfViewInDegrees = PhoneViewport.fovDegrees
        anchor.addChild(camera)
        addLightRig(to: anchor)
        ar.scene.addAnchor(anchor)
        c.anchor = anchor
        c.camera = camera
        ar.environment.lighting.resource = SceneAssets.studioEnvironment
        ar.environment.lighting.intensityExponent = 0.6

        let orbit = UIPanGestureRecognizer(target: c, action: #selector(Coordinator.orbit(_:)))
        orbit.maximumNumberOfTouches = 1
        let pan = UIPanGestureRecognizer(target: c, action: #selector(Coordinator.pan(_:)))
        pan.minimumNumberOfTouches = 2
        pan.maximumNumberOfTouches = 2
        let pinch = UIPinchGestureRecognizer(target: c, action: #selector(Coordinator.pinch(_:)))
        let doubleTap = UITapGestureRecognizer(target: c, action: #selector(Coordinator.doubleTap(_:)))
        doubleTap.numberOfTapsRequired = 2
        let tap = UITapGestureRecognizer(target: c, action: #selector(Coordinator.tap(_:)))
        tap.require(toFail: doubleTap)
        let long = UILongPressGestureRecognizer(target: c, action: #selector(Coordinator.longPress(_:)))
        long.minimumPressDuration = 0.45
        for g in [orbit, pan, pinch, doubleTap, tap, long] as [UIGestureRecognizer] {
            g.delegate = c
            ar.addGestureRecognizer(g)
        }

        rebuild(ar, c)
        syncCamera(c)
        return ar
    }

    func updateUIView(_ ar: ARView, context: Context) {
        let c = context.coordinator
        c.model = model
        c.onLongPress = onLongPress
        let size = ar.bounds.size
        if size.height > 0 { model.viewportAspect = Float(size.width / size.height) }

        if ProcessInfo.processInfo.environment["VCAD_PHONE_LOG"] != nil {
            NSLog("[phone] update dirty=%d key=%@ built=%@", geometryDirty ? 1 : 0, geometryKey, c.builtKey)
        }
        if geometryDirty || c.builtKey != geometryKey {
            rebuild(ar, c)
        } else if paramDirty {
            // Parameter scrub: swap meshes in place (no rebuild pop).
            ReleaseWindowController.shared.refreshGeometry(model: model, collisions: false)
        }
        if c.builtSelection != selection || c.builtInstances != selectedInstances {
            c.builtSelection = selection
            c.builtInstances = selectedInstances
            ReleaseWindowController.shared.repaintAll()
            rebuildGizmo()
        }
        if c.builtHidden != hidden || c.builtIsolated != isolated {
            c.builtHidden = hidden
            c.builtIsolated = isolated
            applyVisibility()
        }
        if playbackDirty, !c.instanceEntities.isEmpty {
            for (i, m) in model.instanceTransforms.enumerated() where i < c.instanceEntities.count {
                c.instanceEntities[i].transform = Transform(matrix: m)
            }
            model.playbackDirty = false
        }
        if sketchDirty { PhonePick.refreshSketchInk(model: model) }
        syncCamera(c)
    }

    // MARK: scene

    private func addLightRig(to anchor: AnchorEntity) {
        let key = DirectionalLight()
        key.light.intensity = 1300
        key.look(at: .zero, from: [0.7, 1.1, 0.85], relativeTo: nil)
        anchor.addChild(key)
        let rim = DirectionalLight()
        rim.light.intensity = 750
        rim.look(at: .zero, from: [-0.9, 0.35, -1.0], relativeTo: nil)
        anchor.addChild(rim)
        let fill = DirectionalLight()
        fill.light.intensity = 800
        fill.look(at: .zero, from: [-0.2, 0.5, 1.2], relativeTo: nil)
        anchor.addChild(fill)
    }

    private func syncCamera(_ c: Coordinator) {
        guard let camera = c.camera else { return }
        camera.look(at: lookAt, from: cameraPosition, relativeTo: nil)
        if let gizmo = ReleaseWindowController.shared.centeringEntity?.findEntity(named: "gizmoRoot") {
            gizmo.scale = SIMD3<Float>(repeating: gizmoScreenScale(model: model))
        }
    }

    private func rebuildGizmo() {
        guard let centering = ReleaseWindowController.shared.centeringEntity else { return }
        centering.findEntity(named: "gizmoRoot")?.removeFromParent()
        if model.showsGizmo { centering.addChild(buildGizmo(model: model)) }
    }

    private func applyVisibility() {
        guard let centering = ReleaseWindowController.shared.centeringEntity else { return }
        for i in 0..<model.partCount {
            let on = model.isPartVisible(i)
            centering.findEntity(named: "part\(i)")?.isEnabled = on
            centering.findEntity(named: "edges\(i)")?.isEnabled = on
        }
    }

    /// Kernel mm (Z-up) → scene units (Y-up), normalized to a constant display
    /// size like the Mac viewport (`0.6 / size`), so one camera fits any part.
    private func rebuild(_ ar: ARView, _ c: Coordinator) {
        guard let anchor = c.anchor else { return }
        c.builtKey = geometryKey
        c.builtSelection = model.highlightedParts
        c.builtInstances = model.selectedInstances
        c.builtHidden = model.hiddenParts
        c.builtIsolated = model.isolatedPart
        model.geometryDirty = false
        model.docParamDirty = false

        let scene = model.buildScene()
        if ProcessInfo.processInfo.environment["VCAD_PHONE_LOG"] != nil {
            NSLog("[phone] rebuild solving=%d meshes=%d instances=%d size=%.2f src=%@ load=%@ engine=%@ msolving=%d", scene.solving ? 1 : 0,
                  scene.meshes.count, scene.instances.count, scene.size, model.source.id,
                  model.loadError ?? "-", model.engineError ?? "-", model.solving ? 1 : 0)
        }
        // Solving with nothing landed: keep this source's previous geometry,
        // but never show the last document's under a freshly opened one.
        if scene.solving && scene.meshes.isEmpty && scene.instances.isEmpty {
            if c.builtSourceID != model.source.id {
                anchor.children.filter { $0.name == "geomRoot" }.forEach { $0.removeFromParent() }
                c.instanceEntities.removeAll()
                c.builtSourceID = nil
            }
            c.builtKey = ""
            return
        }
        c.builtSourceID = model.source.id
        anchor.children.filter { $0.name == "geomRoot" }.forEach { $0.removeFromParent() }
        c.instanceEntities.removeAll()
        ReleaseWindowController.shared.baseEmission.removeAll()

        let centering = Entity()
        centering.name = "centering"
        centering.position = -scene.center
        ReleaseWindowController.shared.centeringEntity = centering
        model.centeringEntity = centering
        model.orbitPivotProvider = { [weak model, weak centering] in
            guard let model, let centering else { return nil }
            let parts = model.highlightedParts.sorted().compactMap { i -> SIMD3<Float>? in
                centering.findEntity(named: "part\(i)").map { $0.visualBounds(relativeTo: nil).center }
            }
            return SelectionMath.orbitPivot(topology: [], parts: parts)
        }

        let edgeWidth = max(scene.size * 0.0016, 0.02)
        for (i, inst) in scene.instances.enumerated() {
            var m = PhysicallyBasedMaterial()
            m.baseColor = .init(tint: inst.material.color)
            m.roughness = .init(floatLiteral: inst.material.roughness)
            m.metallic = .init(floatLiteral: inst.material.metallic)
            SceneAssets.studioSoften(&m)
            let e = ModelEntity(mesh: inst.mesh, materials: [m])
            let isFreeRoot = inst.index >= model.assemblyInstanceCount
            e.name = isFreeRoot ? "free\(i)" : "inst\(i)"
            e.transform = Transform(matrix: !isFreeRoot && i < model.instanceTransforms.count
                                    ? model.instanceTransforms[i] : inst.transform)
            e.generateCollisionShapes(recursive: false)
            if let r = inst.edges {
                e.addChild(ModelEntity(mesh: r, materials: [SceneAssets.edgeMaterial]))
            }
            centering.addChild(e)
            if !isFreeRoot { c.instanceEntities.append(e) }
        }

        if scene.instances.isEmpty {
            for (i, item) in scene.meshes.enumerated() {
                var m = PhysicallyBasedMaterial()
                if model.usesDocumentTree {
                    let r = model.resolvedMaterial(forPart: i)
                    m.baseColor = .init(tint: r.color)
                    m.roughness = .init(floatLiteral: r.roughness)
                    m.metallic = .init(floatLiteral: r.metallic)
                } else {
                    m.baseColor = .init(tint: item.color)
                    m.roughness = 0.34
                    m.metallic = 0.55
                }
                SceneAssets.studioSoften(&m)
                if model.highlightedParts.contains(i) {
                    m.emissiveColor = .init(color: SelectionStyle.selected)
                    m.emissiveIntensity = SelectionStyle.partSelectedIntensity
                }
                let e = ModelEntity()
                e.name = "part\(i)"
                if let pat = i < scene.instancing.count ? scene.instancing[i] : nil {
                    for (j, t) in pat.transforms.enumerated() {
                        let child = ModelEntity(mesh: item.mesh, materials: [m])
                        child.name = "part\(i)_inst\(j)"
                        child.transform = Transform(matrix: t)
                        child.generateCollisionShapes(recursive: false)
                        e.addChild(child)
                    }
                } else {
                    e.model = ModelComponent(mesh: item.mesh, materials: [m])
                    e.generateCollisionShapes(recursive: false)
                }
                e.isEnabled = model.isPartVisible(i)
                centering.addChild(e)

                if i < scene.edges.count,
                   let ribbon = EdgeOverlay.ribbonResource(segments: scene.edges[i], width: edgeWidth, name: "edges\(i)") {
                    let edges = ModelEntity(mesh: ribbon, materials: [SceneAssets.edgeMaterial])
                    edges.name = "edges\(i)"
                    edges.isEnabled = model.isPartVisible(i)
                    centering.addChild(edges)
                }
            }
        }

        if model.sketching { centering.addChild(buildSketchRoot(model: model)) }
        syncSketchGrid(model, in: centering, force: true)
        // Face/edge picks and the section cut survive a rebuild, as on the Mac.
        if let topo = topologyOverlayEntity(model: model) { centering.addChild(topo) }
        if model.showsGizmo {
            let gizmo = buildGizmo(model: model)
            gizmo.scale = SIMD3<Float>(repeating: gizmoScreenScale(model: model))
            centering.addChild(gizmo)
        }

        let zUp = Entity()
        zUp.orientation = simd_quatf(angle: -.pi / 2, axis: [1, 0, 0])
        zUp.addChild(centering)
        let geom = Entity()
        geom.name = "geomRoot"
        geom.scale = SIMD3<Float>(repeating: 0.6 / max(scene.size, 0.0001))
        geom.addChild(zUp)
        anchor.addChild(geom)
        applySectionView(model: model)

        if !scene.solving, !model.cameraUserMoved, c.builtKey != "" {
            // First full scene of a document: frame it for the phone's aspect.
            Task { @MainActor in model.resetCamera() }
        }
    }
}

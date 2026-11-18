#if os(macOS)
import AppKit
import RealityKit

// Realtime photoreal: the live RealityKit viewport, lit like a product shot.
// The key light casts real dynamic shadows onto an invisible shadow catcher
// at the floor, the image-based light is brought up, and metals pick up a
// clearcoat so they read as polished. It stays interactive — drag, flick and
// hold space all keep working — unlike the progressive path tracer.
extension ReleaseWindowController {
    func applyRealtimePhotoreal() {
        guard let model, let ar = arView, let centering = centeringEntity else { return }
        let on = model.realtimePhotoreal
        // Shadow-casting key light.
        let key = ar.scene.anchors.flatMap { $0.children }.first { $0.name == "keyLight" } as? DirectionalLight
        if let key {
            key.light.intensity = on ? 2600 : 1300
            key.shadow = on ? DirectionalLightComponent.Shadow(maximumDistance: 4, depthBias: 2) : nil
        }
        ar.environment.lighting.intensityExponent = on ? 1.1 : (model.zebraMode ? 0 : 0.6)
        // Shadow catcher at the floor (world space, under the model).
        let catcherName = "photorealCatcher"
        ar.scene.anchors.forEach { a in a.children.filter { $0.name == catcherName }.forEach { $0.removeFromParent() } }
        if on, let anchor = ar.scene.anchors.first {
            let b = centering.visualBounds(relativeTo: nil)
            let span = max(b.extents.x, b.extents.z) * 6 + 1
            let plane = ModelEntity(mesh: .generatePlane(width: span, depth: span),
                                    materials: [OcclusionMaterial(receivesDynamicLighting: true)])
            plane.name = catcherName
            plane.position = [b.center.x, b.min.y - 0.0005, b.center.z]
            anchor.addChild(plane)
        }
        // Polished metals.
        for case let e as ModelEntity in centering.children where e.name.hasPrefix("inst") || e.name.hasPrefix("part") {
            guard var m = e.model?.materials.first as? PhysicallyBasedMaterial else { continue }
            if m.metallic.scale > 0.5 {
                m.clearcoat = .init(floatLiteral: on ? 0.6 : 0)
                m.clearcoatRoughness = .init(floatLiteral: 0.15)
            }
            e.model?.materials = [m]
        }
        RenderGate.wake()
    }
}
#endif

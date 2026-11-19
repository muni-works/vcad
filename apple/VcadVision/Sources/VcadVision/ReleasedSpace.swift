import SwiftUI
import RealityKit

// Release-to-desktop, Vision Pro edition. On the Mac the parts lift out of the
// window and composite over the desktop; here the desktop is the room. The
// document's bodies are released into an ImmersiveSpace (mixed: passthrough
// stays on) at REAL scale — a 40 mm bracket is 40 mm in front of you — and
// each body can be looked at, pinched and carried anywhere in the room. The
// Component Palette and the Object Inspector float as their own windows
// beside them (ReleasedPanels.swift), and "Return to Window" puts everything
// back in the volume.
//
// Bodies are dressed by the same code as the Mac's released ARView
// (`ReleaseWindowController.dressPart` / `dressInstance` in the shared
// ReleasedDesktop.swift): the studio-softened PBR material, the document's own
// emission, the brand-orange selection lift, and the same entity naming
// ("part3", "inst7"), so the shared repaint (`repaintAll`) lights the
// selection here without rebuilding anything.

/// Ids of the scenes the release path opens and closes.
enum ReleaseScene {
    static let space = "released"
    static let volume = "part"
    static let palette = "palette"
    static let inspector = "inspector"
}

/// Where the released bodies sit and how they've been rearranged. Lives
/// outside the view so a rebuild (a parameter edit) keeps every part where the
/// person left it.
@MainActor @Observable
final class ReleasedRoom {
    static let shared = ReleasedRoom()
    /// Per-body offsets (entity name → kernel-mm offset), from pinch-drags.
    var offsets: [String: SIMD3<Float>] = [:]
    /// Uniform scale on top of real scale (two-hand pinch). 1 = 1:1.
    var scale: Float = 1
    /// Bumped to request a full rebuild (reset, document swap).
    var revision = 0
    /// The immersive space is up. The panel windows check it: visionOS can
    /// restore them at launch without the space (scene restoration), and a
    /// palette with no parts to drive should hand back to the volume.
    var isReleased = false
    /// Dev hook (`VCAD_RELEASE_RETURN`): return to the window this many
    /// seconds after the palette opens.
    var scriptedReturn: Double?

    func reset() {
        offsets = [:]
        scale = 1
        revision += 1
    }
}

struct ReleasedSpace: View {
    @Bindable var model: EditorModel
    @Bindable var room = ReleasedRoom.shared
    /// Where each dragged body started, by name, for the gesture in flight.
    @State private var dragOrigin: [String: SIMD3<Float>] = [:]
    @State private var scaleOrigin: Float?

    /// Real scale: kernel millimetres → RealityKit metres.
    private static let mm: Float = 0.001
    /// Where the release lands: just below eye level, about where the volume
    /// stood, between the two panels.
    private static let anchor = SIMD3<Float>(0, 1.45, -0.6)

    var body: some View {
        RealityView { content in
            let root = Entity()
            root.name = "releasedRoot"
            root.position = Self.anchor
            content.add(root)
            rebuild(root)
        } update: { content in
            guard let root = content.entities.first(where: { $0.name == "releasedRoot" }) else { return }
            if root.components[GeometryKey.self]?.key != geometryKey {
                rebuild(root)
            } else if model.docParamDirty {
                // Parameter scrub from the inspector: swap meshes in place
                // (shared with the Mac), so the parts don't pop.
                // Colliders follow the new meshes (an inspector translate
                // doesn't change the triangle count, so nothing re-keys), but
                // the Mac's "rebuild on commit" flag is not for us: the room
                // is real scale, there's no fit to re-normalise.
                ReleaseWindowController.shared.refreshGeometry(model: model)
                model.geometryDirty = false
            } else {
                ReleaseWindowController.shared.repaintAll()
            }
            root.findEntity(named: "releasedScale")?.scale = SIMD3(repeating: room.scale * Self.mm)
        }
        .onAppear { room.isReleased = true }
        .onDisappear { room.isReleased = false }
        .gesture(tapGesture)
        .gesture(addGesture)
        .gesture(dragGesture)
        .simultaneousGesture(magnifyGesture)
    }

    /// Everything that requires rebuilding the entity tree. Selection and
    /// hover are NOT in here — they repaint in place, as on the Mac.
    private var geometryKey: String {
        [String(describing: model.source), String(describing: model.baseShape),
         String(describing: model.modifier), String(model.modifierValue),
         String(model.triangleCount), "\(model.hiddenParts.sorted())",
         String(describing: model.isolatedPart), String(model.zebraMode),
         String(room.revision)].joined(separator: "|")
    }

    // MARK: gestures

    /// Look at a body and tap: select it (the Object Inspector follows).
    private var tapGesture: some Gesture {
        SpatialTapGesture()
            .targetedToAnyEntity()
            .onEnded { value in select(value.entity, additive: false) }
    }

    /// Long-press toggles a body in the multi-selection — the room's ⌘-click.
    private var addGesture: some Gesture {
        LongPressGesture(minimumDuration: 0.45)
            .targetedToAnyEntity()
            .onEnded { value in select(value.entity, additive: true) }
    }

    /// Pinch and carry: the grabbed body follows the hand through the room.
    private var dragGesture: some Gesture {
        DragGesture(minimumDistance: 0.01)
            .targetedToAnyEntity()
            .onChanged { value in
                guard let body = Self.body(of: value.entity), let parent = body.parent else { return }
                if dragOrigin[body.name] == nil {
                    dragOrigin[body.name] = body.position
                    select(body, additive: false)
                }
                let start = value.convert(value.startLocation3D, from: .local, to: parent)
                let now = value.convert(value.location3D, from: .local, to: parent)
                body.position = (dragOrigin[body.name] ?? body.position) + (now - start)
            }
            .onEnded { value in
                guard let body = Self.body(of: value.entity) else { return }
                if let origin = dragOrigin.removeValue(forKey: body.name) {
                    let base = room.offsets[body.name] ?? .zero
                    room.offsets[body.name] = base + (body.position - origin)
                }
            }
    }

    /// Two-hand pinch scales the whole release about its anchor.
    private var magnifyGesture: some Gesture {
        MagnifyGesture()
            .targetedToAnyEntity()
            .onChanged { value in
                let base = scaleOrigin ?? room.scale
                if scaleOrigin == nil { scaleOrigin = base }
                room.scale = min(max(base * Float(value.magnification), 0.05), 20)
            }
            .onEnded { _ in scaleOrigin = nil }
    }

    /// The named body ("part3", "part3_inst1", "inst7") a hit landed on.
    private static func body(of entity: Entity) -> Entity? {
        var e: Entity? = entity
        while let cur = e {
            if cur.name.hasPrefix("part") || cur.name.hasPrefix("inst") {
                // A pattern copy moves with its pattern root.
                if cur.name.contains("_inst"), let p = cur.parent, p.name.hasPrefix("part") { return p }
                return cur
            }
            e = cur.parent
        }
        return nil
    }

    private func select(_ entity: Entity, additive: Bool) {
        guard model.usesDocumentTree, let body = Self.body(of: entity) else { return }
        if body.name.hasPrefix("inst"), let i = Int(body.name.dropFirst(4)) {
            model.selectInstance(i)
        } else if let pi = Int(body.name.dropFirst(4).prefix(while: \.isNumber)),
                  let fid = model.featureNodes.first(where: { $0.partIndex == pi })?.id {
            if additive { model.toggleMultiSelect(part: pi, featureID: fid) } else { model.selectFeature(fid) }
        }
        model.selectionDirty = false
        ReleaseWindowController.shared.repaintAll()
    }

    // MARK: build

    /// Kernel mm (Z-up) → room metres (Y-up), at 1:1. The model is centred
    /// on the anchor; each body carries its room offset and a collision shape
    /// so it can be targeted, plus the system gaze highlight and a grounding
    /// shadow so it reads as an object in the room rather than a decal.
    private func rebuild(_ root: Entity) {
        root.children.removeAll()
        root.components.set(GeometryKey(key: geometryKey))
        model.docParamDirty = false
        model.geometryDirty = false

        let ctl = ReleaseWindowController.shared
        ctl.model = model
        ctl.baseEmission.removeAll()
        let scene = model.buildScene()

        let centering = Entity()
        centering.name = "centering"
        centering.position = -scene.center
        ctl.centeringEntity = centering
        model.centeringEntity = centering

        for (i, inst) in scene.instances.enumerated() {
            let e = ModelEntity(mesh: inst.mesh,
                                materials: [ctl.dressInstance(model: model, inst.material, index: i)])
            let isFreeRoot = inst.index >= model.assemblyInstanceCount
            e.name = isFreeRoot ? "free\(i)" : "inst\(i)"
            e.transform = Transform(matrix: !isFreeRoot && i < model.instanceTransforms.count
                                    ? model.instanceTransforms[i] : inst.transform)
            dressForRoom(e)
            centering.addChild(e)
        }
        for (i, item) in scene.meshes.enumerated() {
            let m = ctl.dressPart(model: model, index: i, fallback: item.color)
            let e = ModelEntity()
            e.name = "part\(i)"
            if let pat = i < scene.instancing.count ? scene.instancing[i] : nil {
                for (j, t) in pat.transforms.enumerated() {
                    let child = ModelEntity(mesh: item.mesh, materials: [m])
                    child.name = "part\(i)_inst\(j)"
                    child.transform = Transform(matrix: t)
                    dressForRoom(child)
                    e.addChild(child)
                }
            } else {
                e.model = ModelComponent(mesh: item.mesh, materials: [m])
                dressForRoom(e)
            }
            e.isEnabled = model.isPartVisible(i)
            // Feature edges: ribbons (visionOS has no screen-space line
            // shader), a fixed ~0.35 mm so they read at arm's length.
            if !model.zebraMode, i < scene.edges.count, !scene.edges[i].isEmpty,
               let r = EdgeOverlay.ribbonResource(segments: scene.edges[i],
                                                  width: max(scene.size * 0.0012, 0.35),
                                                  name: "edges\(i)") {
                e.addChild(ModelEntity(mesh: r, materials: [SceneAssets.edgeMaterial]))
            }
            centering.addChild(e)
        }
        // Room placement survives rebuilds.
        for (name, offset) in room.offsets {
            if let e = centering.findEntity(named: name) { e.position += offset }
        }

        let zUp = Entity()
        zUp.orientation = simd_quatf(angle: -.pi / 2, axis: [1, 0, 0])
        zUp.addChild(centering)
        let scaled = Entity()
        scaled.name = "releasedScale"
        scaled.scale = SIMD3(repeating: room.scale * Self.mm)
        scaled.addChild(zUp)
        root.addChild(scaled)
        addGlowLights(centering, in: root)
        ctl.repaintAll()
    }

    private func dressForRoom(_ e: ModelEntity) {
        e.components.set(InputTargetComponent())
        e.components.set(HoverEffectComponent())
        e.components.set(GroundingShadowComponent(castsShadow: true))
        e.generateCollisionShapes(recursive: false)
    }

    /// The Mac draws emitters with a bloom halo (an ARView post-process that
    /// visionOS doesn't have). Here a self-lit body instead puts real light
    /// into the scene: a point light in its emission colour at its centre, so
    /// a glowing medium lights the parts around it.
    private func addGlowLights(_ centering: Entity, in root: Entity) {
        // Parented to the unscaled root (metres), not under the 0.001 kernel
        // scale, so the attenuation radius means what it says.
        let ctl = ReleaseWindowController.shared
        for (name, emission) in ctl.baseEmission {
            guard let e = centering.findEntity(named: name) else { continue }
            let light = Entity()
            light.name = "\(name)_glow"
            var p = PointLightComponent(color: emission.color,
                                        intensity: 400 * emission.intensity,
                                        attenuationRadius: 0.6)
            p.attenuationFalloffExponent = 2
            light.components.set(p)
            light.position = e.visualBounds(relativeTo: root).center
            root.addChild(light)
        }
    }
}

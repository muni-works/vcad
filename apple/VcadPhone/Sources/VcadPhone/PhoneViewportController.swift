import SwiftUI
import RealityKit
import UIKit

// The iPhone's stand-in for the Mac `ReleaseWindowController`
// (ReleasedDesktop.swift, excluded on iOS: it is the Mac's borderless window,
// mouse monitors and NSView viewport). Shared code reaches the live viewport
// through `ReleaseWindowController.shared` — the ARView, the kernel-frame
// centering entity, in-place mesh swaps, selection repaint, framing — so the
// phone keeps that exact API and backs it with PhoneViewport's ARView.

@MainActor
final class ReleaseWindowController {
    static let shared = ReleaseWindowController()

    /// The phone viewport's ARView (set by PhoneViewport).
    weak var arView: ARView?
    /// Kernel-space frame entity (scale + Z-up + centering), set on rebuild.
    weak var centeringEntity: Entity?
    weak var model: EditorModel?
    /// Each self-lit body's own emission, so a selection lift can hand it back.
    var baseEmission: [String: ViewportGlow.Emission] = [:]
    /// Mac pass-through hit-test bookkeeping; unused on a touch screen.
    var chromeRects: [String: CGRect] = [:]
    var sketchModal = false

    /// No floating editor window on iPhone: sheets present from SwiftUI.
    var window: UIWindow? { arView?.window }
    var hostWindow: UIWindow? { window }
    var editorModel: EditorModel? { model }
    var overlaySize: CGSize { arView?.bounds.size ?? .zero }

    func show(model: EditorModel, intent: IntentEngine) {
        self.model = model
        // Same as the Mac window: solves run off main and land as runtime
        // frames (progressive open, in-place scrubs) — never beachball.
        model.evaluatesOffMainThread = true
    }
    func setWindowed(_ windowed: Bool) {}
    func updateDocumentWindow() {}
    func updateCNCWindowMinimum() {}
    func requestClose() {}
    func confirmClose() -> Bool { true }
    func followCNCTool() {}
    func applyRealtimePhotoreal() {}
    func repaintHover() {}
    func syncHoverPaint() {}

    // MARK: picking + projection (same math as the Mac controller)

    /// Every collision hit under a SwiftUI/UIKit point (top-left origin, which
    /// is also what UIKit's ARView.hitTest wants — no flip on iOS).
    func hits(atViewPoint p: CGPoint) -> [CollisionCastHit] {
        guard let ar = arView else { return [] }
        return ar.hitTest(p)
    }

    func viewPoint(forPart i: Int, from camera: SIMD3<Float>, lookAt: SIMD3<Float>) -> CGPoint? {
        guard let centering = centeringEntity,
              let e = centering.findEntity(named: "part\(i)") else { return nil }
        return viewPoint(forWorld: e.visualBounds(relativeTo: nil).center, from: camera, lookAt: lookAt)
    }

    func projector(from entity: Entity, camera: SIMD3<Float>, lookAt: SIMD3<Float>) -> ((SIMD3<Float>) -> CGPoint?)? {
        guard let ar = arView else { return nil }
        let size = ar.bounds.size
        guard size.width > 0, size.height > 0 else { return nil }
        let m = entity.transformMatrix(relativeTo: nil)
        let basis = Self.basis(camera: camera, lookAt: lookAt)
        return { k in
            let p4 = m * SIMD4<Float>(k, 1)
            return Self.project(SIMD3(p4.x, p4.y, p4.z), camera: camera, basis: basis, size: size)
        }
    }

    func viewPoint(forWorld w: SIMD3<Float>, from camera: SIMD3<Float>, lookAt: SIMD3<Float>) -> CGPoint? {
        guard let ar = arView, ar.bounds.width > 0, ar.bounds.height > 0 else { return nil }
        return Self.project(w, camera: camera, basis: Self.basis(camera: camera, lookAt: lookAt), size: ar.bounds.size)
    }

    private static func basis(camera: SIMD3<Float>, lookAt: SIMD3<Float>) -> (SIMD3<Float>, SIMD3<Float>, SIMD3<Float>) {
        let fwd = normalize(lookAt - camera)
        let right = normalize(cross(fwd, SIMD3<Float>(0, 1, 0)))
        return (fwd, right, cross(right, fwd))
    }

    private static func project(_ w: SIMD3<Float>, camera: SIMD3<Float>,
                                basis: (SIMD3<Float>, SIMD3<Float>, SIMD3<Float>), size: CGSize) -> CGPoint? {
        let (fwd, right, up) = basis
        let v = w - camera
        let z = dot(v, fwd)
        guard z > 0.0001 else { return nil }
        let tanHalf = tan(PhoneViewport.fovDegrees / 2 * .pi / 180)
        let aspect = Float(size.width / size.height)
        let ndcX = (dot(v, right) / z) / (tanHalf * aspect)
        let ndcY = (dot(v, up) / z) / tanHalf
        return CGPoint(x: CGFloat(ndcX * 0.5 + 0.5) * size.width, y: CGFloat(0.5 - ndcY * 0.5) * size.height)
    }

    static func instanceIndex(_ e: Entity) -> Int? {
        guard e.name.hasPrefix("inst"), let i = Int(e.name.dropFirst(4)) else { return nil }
        return i
    }

    // MARK: framing

    func frameSelection() {
        guard let model else { return }
        if let ii = model.selectedInstanceIndex { frame(entityNamed: "inst\(ii)") }
        else if let pi = model.selectedPartIndex { frame(entityNamed: "part\(pi)") }
        else { model.resetCamera(animated: true) }
    }

    func frame(entityNamed name: String) {
        guard let model, let e = centeringEntity?.findEntity(named: name) else { return }
        let b = e.visualBounds(relativeTo: nil)
        model.stopSpin()
        model.cameraUserMoved = true
        withAnimation(.smooth(duration: 0.35)) {
            model.panOffset = b.center
            let extent = max(b.extents.x, max(b.extents.y, b.extents.z))
            model.distance = max(0.45, min(8.0, extent * 2.2))
        }
        model.pinchBaseline = model.distance
    }

    // MARK: selection paint (material-only, no rebuild)

    enum Lift { case none, selected, hover }

    func repaintAll() {
        guard let model, let centering = centeringEntity else { return }
        for i in 0..<model.partCount where centering.findEntity(named: "part\(i)") != nil {
            paint(entity: "part\(i)", lift: model.highlightedParts.contains(i) ? .selected : .none)
        }
        for i in 0..<model.assemblyInstanceCount where centering.findEntity(named: "inst\(i)") != nil {
            paint(entity: "inst\(i)", lift: model.selectedInstances.contains(i) ? .selected : .none)
        }
    }

    func glow(parts: Set<Int>, level: Float) {
        for i in parts {
            guard let e = centeringEntity?.findEntity(named: "part\(i)") as? ModelEntity,
                  var m = e.model?.materials.first as? PhysicallyBasedMaterial else { continue }
            m.emissiveColor = .init(color: UIColor(red: 0.62, green: 0.42, blue: 1.0, alpha: 1))
            m.emissiveIntensity = level
            e.model?.materials = [m]
        }
    }

    private func paint(entity name: String, lift: Lift) {
        guard let root = centeringEntity?.findEntity(named: name) else { return }
        // A pattern part is a container of instance children; paint them all.
        let targets = (root as? ModelEntity)?.model != nil
            ? [root] : root.children.filter { $0.name.hasPrefix("\(name)_inst") }
        for t in targets {
            guard let e = t as? ModelEntity,
                  var m = e.model?.materials.first as? PhysicallyBasedMaterial else { continue }
            switch lift {
            case .selected, .hover:
                m.emissiveColor = .init(color: SelectionStyle.selected)
                m.emissiveIntensity = SelectionStyle.partSelectedIntensity
            case .none:
                if let own = baseEmission[name] {
                    m.emissiveColor = .init(color: own.color)
                    m.emissiveIntensity = own.intensity
                } else {
                    m.emissiveIntensity = 0
                }
            }
            e.model?.materials = [m]
        }
    }

    /// In-place re-eval → mesh swap (parameter scrubs), the Mac controller's
    /// twin: parts update without a rebuild pop; a commit rebuilds to re-fit.
    func refreshGeometry(model: EditorModel, collisions: Bool = true, fromRuntime: Bool = false) {
        if !fromRuntime, model.runtimeEditsInFlight { return }
        guard let centering = centeringEntity,
              let meshes = model.reevalDocumentMeshes() else { return }
        for (i, m) in meshes.enumerated() {
            guard let pe = centering.findEntity(named: "part\(i)") as? ModelEntity else { continue }
            if pe.model != nil {
                pe.model?.mesh = m
                if collisions { pe.generateCollisionShapes(recursive: false) }
            } else {
                for child in pe.children.compactMap({ $0 as? ModelEntity }) where child.name.hasPrefix("part\(i)_inst") {
                    child.model?.mesh = m
                    if collisions { child.generateCollisionShapes(recursive: false) }
                }
            }
        }
        model.docParamDirty = false
        if collisions { model.geometryDirty = true }
    }
}

/// Mac: reports a chrome element's frame for the pass-through hit test. A
/// touch screen has no pass-through, so this is a no-op modifier on iPhone.
struct ChromeRegion: ViewModifier {
    let key: String
    func body(content: Content) -> some View { content }
}

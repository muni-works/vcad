#if canImport(AppKit)
import AppKit
#else
import UIKit
#endif
import CoreImage
import Metal
import RealityKit

/// Live glow for emissive parts in the viewport.
///
/// Two halves. `dress` makes a self-lit body *look* self-lit: the document's
/// emission becomes the material's emissive tint, and a glowing medium
/// (`volumeEmission`) is drawn see-through with a dark base so it reads as
/// light rather than as a coloured solid. `install` adds the camera's glare:
/// a cheap bloom on `ARView.renderCallbacks.postProcess` — bright pixels
/// (above `threshold`, colour-preserving) blurred at two scales and added
/// back.
///
/// The bloom is installed **only while something emits**. With no emitter
/// the callback is `nil`, RealityKit renders straight to the drawable, and
/// the viewport costs exactly what it did before — the perf-sensitive paths
/// (`K1PerfTests`, the launch orbit) never see it. It is a look, not
/// physics: the photoreal renderer's bloom (`vcad_render::glow`) is the
/// energy-conserving one.
@MainActor
enum ViewportGlow {
    /// A body's own emission, as the viewport draws it.
    struct Emission {
        let color: NSColor
        let intensity: Float
    }

    /// Pixels brighter than this (per channel, after RealityKit's tone map)
    /// feed the halo. Emissive surfaces saturate past it; lit plastic and
    /// metal highlights mostly do not.
    nonisolated static let threshold: CGFloat = 0.82
    /// Halo gain on the thresholded excess.
    nonisolated static let gain: CGFloat = 5.0
    /// The two blur scales, in drawable pixels (retina): a tight glow and a
    /// wide halo.
    nonisolated static let sigmas: [Double] = [6, 24]

    /// Apply a resolved material's emission to a RealityKit material, and
    /// remember it under `entity` so hover/selection lifts can restore it.
    static func dress(_ m: inout PhysicallyBasedMaterial, _ r: ResolvedMaterial, entity: String) {
        let e = r.emission
        let peak = max(e.x, max(e.y, e.z))
        guard peak > 0 else { return }
        // Hue from the normalised emission; brightness from its peak,
        // clamped to what an 8-bit drawable can show before it all clips.
        let n = e / peak
        let color = NSColor(red: CGFloat(n.x), green: CGFloat(n.y), blue: CGFloat(n.z), alpha: 1)
        let intensity = min(max(peak, 0.6), 4.0)
        m.emissiveColor = .init(color: color)
        m.emissiveIntensity = intensity
        if r.isMedium {
            // A medium has no surface to shade: no albedo, no specular, and
            // see-through so what is behind it still shows.
            m.baseColor = .init(tint: .black)
            m.specular = .init(floatLiteral: 0)
            m.blending = .transparent(opacity: .init(floatLiteral: 0.55))
        }
        ReleaseWindowController.shared.baseEmission[entity] = Emission(color: color, intensity: intensity)
    }

    // visionOS has no ARView and no post-process hook: emissive bodies glow
    // through `dress` alone there, without the bloom halo.
    #if !os(visionOS)
    /// Turn the bloom pass on or off for `ar`. Idempotent.
    static func install(on ar: ARView, enabled: Bool) {
        if !enabled {
            if installed.contains(ObjectIdentifier(ar)) {
                ar.renderCallbacks.postProcess = nil
                installed.remove(ObjectIdentifier(ar))
            }
            return
        }
        guard !installed.contains(ObjectIdentifier(ar)) else { return }
        installed.insert(ObjectIdentifier(ar))
        ar.renderCallbacks.postProcess = callback()
    }

    /// The callback, formed outside the main actor: RealityKit invokes it on
    /// its render thread, and a closure made in a `@MainActor` context would
    /// trip Swift 6's isolation check there.
    private nonisolated static func callback() -> (ARView.PostProcessContext) -> Void {
        { context in bloom(context) }
    }

    private static var installed: Set<ObjectIdentifier> = []

    /// One CIContext per device, colour management off: the halo is added in
    /// the drawable's own encoding, like the photoreal bloom works on film.
    nonisolated(unsafe) private static var ciContexts: [ObjectIdentifier: CIContext] = [:]
    nonisolated(unsafe) private static let lock = NSLock()

    private nonisolated static func ciContext(_ device: MTLDevice) -> CIContext {
        lock.lock()
        defer { lock.unlock() }
        let key = ObjectIdentifier(device)
        if let c = ciContexts[key] { return c }
        let c = CIContext(mtlDevice: device, options: [
            .workingColorSpace: NSNull(),
            .outputColorSpace: NSNull(),
            .cacheIntermediates: false,
        ])
        ciContexts[key] = c
        return c
    }

    /// The post-process itself. It must always write the target — RealityKit
    /// presents whatever is there — so any failure falls back to a plain
    /// copy of the source.
    private nonisolated static func bloom(_ context: ARView.PostProcessContext) {
        let src = context.sourceColorTexture
        let dst = context.targetColorTexture
        guard let image = CIImage(mtlTexture: src, options: [.colorSpace: NSNull()]) else {
            blit(context)
            return
        }
        let extent = image.extent
        let t = -threshold
        let bright = image
            .applyingFilter("CIColorMatrix", parameters: [
                "inputBiasVector": CIVector(x: t, y: t, z: t, w: 0),
            ])
            .applyingFilter("CIColorClamp", parameters: [
                "inputMinComponents": CIVector(x: 0, y: 0, z: 0, w: 0),
                "inputMaxComponents": CIVector(x: 8, y: 8, z: 8, w: 1),
            ])
            .clampedToExtent()
        var halo: CIImage?
        for s in sigmas {
            // Sigmas are stated for a 2400 px wide drawable and scale with it.
            let g = bright.applyingGaussianBlur(sigma: s * Double(src.width) / 2400.0)
            halo = halo.map { $0.applyingFilter("CIAdditionCompositing", parameters: [kCIInputBackgroundImageKey: g]) } ?? g
        }
        guard let halo else {
            blit(context)
            return
        }
        let k = gain / CGFloat(sigmas.count)
        let scaled = halo.cropped(to: extent).applyingFilter("CIColorMatrix", parameters: [
            "inputRVector": CIVector(x: k, y: 0, z: 0, w: 0),
            "inputGVector": CIVector(x: 0, y: k, z: 0, w: 0),
            "inputBVector": CIVector(x: 0, y: 0, z: k, w: 0),
            // The halo adds light, not coverage: the source keeps its alpha.
            "inputAVector": CIVector(x: 0, y: 0, z: 0, w: 0),
        ])
        let out = scaled.applyingFilter("CIAdditionCompositing", parameters: [
            kCIInputBackgroundImageKey: image,
        ])
        let ci = ciContext(context.device)
        ci.render(out, to: dst, commandBuffer: context.commandBuffer, bounds: extent,
                  colorSpace: CGColorSpaceCreateDeviceRGB())
    }

    /// Copy source to target unchanged.
    private nonisolated static func blit(_ context: ARView.PostProcessContext) {
        guard let enc = context.commandBuffer.makeBlitCommandEncoder() else { return }
        enc.copy(from: context.sourceColorTexture, to: context.targetColorTexture)
        enc.endEncoding()
    }
    #endif
}

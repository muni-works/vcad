import RealityKit
#if canImport(AppKit)
import AppKit
#else
import UIKit
#endif

// How a released body is dressed: shared by the Mac's released ARView
// (ReleasedDesktop.swift), the visionOS room (VcadVision/ReleasedSpace.swift)
// and any other viewport that wants the same look. Free of any particular
// viewport controller; the only state it touches is the `baseEmission` table
// that `ViewportGlow.dress` records into.

@MainActor
enum ReleasedLook {
    /// A document part's material as the released scene draws it: the
    /// resolved PBR material softened for the studio IBL, its own emission
    /// (recorded in `baseEmission` so hover/selection lifts restore it), and
    /// the brand-orange selection lift. Zebra mode swaps in the analysis
    /// chrome; the sandbox (no document tree) falls back to `fallback`.
    static func dressPart(model: EditorModel, index i: Int, fallback: NSColor) -> PhysicallyBasedMaterial {
        var m = PhysicallyBasedMaterial()
        if model.zebraMode {
            m = SceneAssets.zebraChrome
        } else if model.usesDocumentTree {
            let r = model.resolvedMaterial(forPart: i)
            m.baseColor = .init(tint: r.color)
            m.roughness = .init(floatLiteral: r.roughness)
            m.metallic = .init(floatLiteral: r.metallic)
            SceneAssets.studioSoften(&m)
            ViewportGlow.dress(&m, r, entity: "part\(i)")
        } else {
            m.baseColor = .init(tint: fallback)
            m.roughness = 0.34
            m.metallic = 0.55
            SceneAssets.studioSoften(&m)
        }
        if model.usesDocumentTree, !model.zebraMode, model.highlightedParts.contains(i) {
            // Brand orange = action: a subtle emissive lift marks selection
            // without repainting the material (mirrors the studio rule).
            m.emissiveColor = .init(color: SelectionStyle.selected)
            m.emissiveIntensity = SelectionStyle.partSelectedIntensity
        }
        return m
    }

    /// An assembly instance's material — `dressPart`'s twin for instances.
    static func dressInstance(model: EditorModel, _ material: ResolvedMaterial, index i: Int) -> PhysicallyBasedMaterial {
        if model.zebraMode { return SceneAssets.zebraChrome }
        var m = PhysicallyBasedMaterial()
        m.baseColor = .init(tint: material.color)
        m.roughness = .init(floatLiteral: material.roughness)
        m.metallic = .init(floatLiteral: material.metallic)
        SceneAssets.studioSoften(&m)
        ViewportGlow.dress(&m, material, entity: "inst\(i)")
        return m
    }
}

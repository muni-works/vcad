import AppKit
import RealityKit

/// What the ground grid is built from (captured at scene build time).
struct GroundSpec {
    var partSizeMM: SIMD3<Float>
    var sceneSize: Float
    var sceneScale: Float
    var dark: Bool
    var center: SIMD3<Float>
}

// MARK: - Sketch-plane grid

/// The key the live sketch grid was last built for. A cursor move refreshes
/// the sketch ink on every event; the grid only rebuilds when this changes.
@MainActor private var sketchGridBuiltKey = ""

/// Show the grid on the active sketch plane (at the snap step) while
/// sketching with the Sketch Grid toggle on; remove it otherwise.
@MainActor
func syncSketchGrid(_ model: EditorModel, in centering: Entity, force: Bool = false) {
    let existing = centering.findEntity(named: "sketchGrid")
    guard model.sketching, Prefs.showsSketchGrid, let sketch = model.sketch else {
        existing?.removeFromParent()
        sketchGridBuiltKey = ""
        return
    }
    let dark = NSApp.effectiveAppearance.bestMatch(from: [.darkAqua, .aqua]) == .darkAqua
    let f = sketch.frame
    let extent = max(model.displayedSceneSize, 40)
    let key = "\(f.origin)|\(f.xDir)|\(f.yDir)|\(sketch.gridStep)|\(extent)|\(dark)"
    if !force, existing != nil, key == sketchGridBuiltKey { return }
    existing?.removeFromParent()
    sketchGridBuiltKey = key
    if let g = SceneAssets.sketchGridEntity(frame: f, snapMM: sketch.gridStep, extentMM: extent, dark: dark) {
        centering.addChild(g)
    }
}

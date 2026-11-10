import CoreGraphics
import simd

/// Screen → kernel math for placing an armed primitive with a click.
///
/// The viewport draws the kernel scene scaled down (`sceneScale`, so the
/// whole scene spans ~0.6 world units) under a centering entity. A placement
/// ray must therefore be built from the camera the renderer actually uses and
/// then carried into kernel space through that same scale; `ARView.ray(through:)`
/// answers for the AR session's camera, not the scene's `PerspectiveCamera`
/// (the same reason `viewPoint(forPart:)` projects by hand), and its rays put
/// clicks hundreds of millimetres from where the user pointed.
enum PlacementMath {
    /// `PerspectiveCamera`'s default vertical field of view.
    static let fovDegrees: Float = 60

    /// World-space ray through a top-left-origin view point, for a camera at
    /// `camera` looking at `lookAt` with world +Y up. The exact inverse of
    /// `ReleaseWindowController.viewPoint(forPart:from:lookAt:)`.
    static func worldRay(viewPoint p: CGPoint, viewSize: CGSize,
                         camera: SIMD3<Float>, lookAt: SIMD3<Float>,
                         fovDegrees: Float = fovDegrees) -> (o: SIMD3<Float>, d: SIMD3<Float>)? {
        guard viewSize.width > 0, viewSize.height > 0, simd_length(lookAt - camera) > 1e-9 else { return nil }
        let fwd = normalize(lookAt - camera)
        let side = cross(fwd, SIMD3<Float>(0, 1, 0))
        guard simd_length(side) > 1e-6 else { return nil }   // looking straight up/down the world Y
        let right = normalize(side)
        let up = cross(right, fwd)
        let tanHalf = tan(fovDegrees / 2 * .pi / 180)
        let aspect = Float(viewSize.width / viewSize.height)
        let ndcX = Float(p.x / viewSize.width) * 2 - 1
        let ndcY = 1 - Float(p.y / viewSize.height) * 2
        let d = fwd + right * (ndcX * tanHalf * aspect) + up * (ndcY * tanHalf)
        return (camera, normalize(d))
    }

    /// Where a kernel-space ray crosses the ground plane (kernel z = 0), in
    /// front of the camera.
    static func groundHit(origin o: SIMD3<Float>, direction d: SIMD3<Float>) -> SIMD3<Float>? {
        guard abs(d.z) > 1e-5 else { return nil }
        let t = -o.z / d.z
        guard t > 0 else { return nil }
        return o + d * t
    }

    /// Keep a placement near what the user is looking at. A ground hit within
    /// reach of the existing geometry is taken as is; a miss, or a grazing ray
    /// that lands far beyond it (a click near the horizon), falls back to
    /// `fallback` — the selection, else the origin.
    static func resolve(hit: SIMD3<Float>?, sceneCenter: SIMD3<Float>, sceneExtent: Float,
                        fallback: SIMD3<Float>) -> SIMD3<Float> {
        guard let hit else { return fallback }
        let reach = max(3 * sceneExtent, 150)
        let off = SIMD2(hit.x - sceneCenter.x, hit.y - sceneCenter.y)
        return simd_length(off) <= reach ? hit : fallback
    }
}

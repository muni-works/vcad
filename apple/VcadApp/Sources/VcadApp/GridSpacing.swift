import Foundation
import simd

/// Pure layout math for the viewport grids (ground + sketch plane): metric
/// decade spacing with a major line every `majorEvery` minor cells, and how
/// large the single textured quad that draws the grid should be.
struct GridSpacing {
    /// Minor cells per major cell (1/10 mm, 10/100 mm, …).
    static let majorEvery = 10
    /// Bounds on the minor-cell count across one grid quad.
    static let minCells = 40, maxCells = 400

    struct Layout: Equatable {
        /// Minor line spacing, mm.
        var minor: Float
        /// Major line spacing, mm.
        var major: Float { minor * Float(GridSpacing.majorEvery) }
        /// Minor cells across the quad (a multiple of 2 × majorEvery, so the
        /// centre falls on a major line).
        var cells: Int
        /// Side of the quad, mm.
        var sideMM: Float { minor * Float(cells) }
    }

    /// Round `n` up to a multiple of 2 × majorEvery and clamp it.
    static func cells(_ n: Float) -> Int {
        let unit = 2 * majorEvery
        let c = Int((n / Float(unit)).rounded(.up)) * unit
        return min(maxCells, max(minCells, c))
    }

    /// Cell counts the ground quad may use. A short ladder keeps the texture
    /// cache small and means zooming only rebuilds the grid at a step.
    static let groundCellSteps = [40, 80, 160, 320]

    /// Ground grid for a model of `extentMM` (largest bbox side) seen across
    /// `viewSpanMM` (visible width at the target; defaults to 3× the part).
    /// The minor step is the metric decade putting ≈30 minor cells across
    /// the view (so ~10–95), major every 10; the quad spans ≥ 2.5 views and
    /// ≥ 1.5 parts so its fade lands well past both. Sizing the quad to the
    /// view, not just the part, keeps texels near screen pixels: lines stay
    /// hairlines when you zoom in instead of fattening.
    static func ground(extentMM: Float, viewSpanMM: Float? = nil) -> Layout {
        guard extentMM.isFinite, extentMM > 0 else { return Layout(minor: 1, cells: 160) }
        let view = viewSpanMM.flatMap { $0.isFinite && $0 > 0 ? $0 : nil } ?? 3 * extentMM
        var minor = pow(10, (log10(view / 30)).rounded())
        let need = max(2.5 * view, 1.5 * extentMM)
        while true {
            if let c = groundCellSteps.first(where: { Float($0) * minor >= need }) {
                return Layout(minor: minor, cells: c)
            }
            minor *= 10
        }
    }

    /// Sketch-plane grid: minor lines at the snap step (1 mm when snapping is
    /// off), coarsened by decades until the quad fits in `maxCells`, so a fine
    /// snap over a big view never turns into grey mush.
    static func sketch(snapMM: Float, extentMM: Float) -> Layout {
        var minor = snapMM.isFinite && snapMM > 0 ? snapMM : 1
        let side = max(extentMM.isFinite ? extentMM : 0, 100) * 2.5
        while side / minor > Float(maxCells) { minor *= 10 }
        return Layout(minor: minor, cells: cells(side / minor))
    }

    /// Where the ground quad goes: on the world origin (axes drawn) when the
    /// part sits comfortably inside the crisp middle of a quad centred there,
    /// else under the part snapped to a major line (no axes, which would be
    /// in the wrong place).
    static func placement(partCenterMM c: SIMD2<Float>, partExtentMM e: Float,
                          layout: Layout) -> (centerMM: SIMD2<Float>, axes: Bool) {
        let reach = max(abs(c.x), abs(c.y)) + e / 2
        if reach <= layout.sideMM * 0.3 { return (.zero, true) }
        let m = layout.major
        return ((c / m).rounded(.toNearestOrAwayFromZero) * m, false)
    }
}

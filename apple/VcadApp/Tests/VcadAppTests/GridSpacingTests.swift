import XCTest
@testable import VcadApp

final class GridSpacingTests: XCTestCase {
    func testGroundSpacingIsMetricDecadesWithMajorEveryTen() {
        // Default view = 3× the part.
        let cases: [(Float, Float)] = [(5, 1), (20, 1), (50, 10), (100, 10), (300, 10), (1000, 100), (4000, 1000)]
        for (extent, minor) in cases {
            let l = GridSpacing.ground(extentMM: extent)
            XCTAssertEqual(l.minor, minor, accuracy: minor * 1e-4, "extent \(extent)")
            XCTAssertEqual(l.major, minor * 10, accuracy: minor * 1e-3)
        }
    }

    func testGroundSpacingFollowsCameraZoom() {
        // Same 60 mm part: zooming in refines the decade, pulling out coarsens it.
        XCTAssertEqual(GridSpacing.ground(extentMM: 60, viewSpanMM: 30).minor, 1)
        XCTAssertEqual(GridSpacing.ground(extentMM: 60, viewSpanMM: 300).minor, 10)
        XCTAssertEqual(GridSpacing.ground(extentMM: 60, viewSpanMM: 3000).minor, 100)
        // A small zoom inside one step doesn't change the layout (no rebuild).
        XCTAssertEqual(GridSpacing.ground(extentMM: 60, viewSpanMM: 300),
                       GridSpacing.ground(extentMM: 60, viewSpanMM: 310))
    }

    func testGroundGridOutreachesPartAndViewAndCentresOnAMajorLine() {
        for extent: Float in [3, 17, 80, 250, 2000] {
            for view in [extent * 0.5, extent * 3, extent * 20] {
                let l = GridSpacing.ground(extentMM: extent, viewSpanMM: view)
                XCTAssertGreaterThanOrEqual(l.sideMM, max(extent * 1.5, view * 2.5) * 0.999, "\(extent)/\(view)")
                XCTAssertTrue(GridSpacing.groundCellSteps.contains(l.cells))
                XCTAssertEqual(l.cells % (2 * GridSpacing.majorEvery), 0)
                let across = view / l.minor
                XCTAssertTrue((9...96).contains(across), "\(across) minor cells across a \(view) mm view")
            }
        }
    }

    func testDegenerateExtentFallsBackToMillimetres() {
        XCTAssertEqual(GridSpacing.ground(extentMM: 0).minor, 1)
        XCTAssertEqual(GridSpacing.ground(extentMM: .nan).minor, 1)
    }

    func testSketchGridUsesSnapStepUntilTooDense() {
        XCTAssertEqual(GridSpacing.sketch(snapMM: 1, extentMM: 50).minor, 1)
        XCTAssertEqual(GridSpacing.sketch(snapMM: 0.5, extentMM: 50).minor, 5)   // 250 mm / 0.5 = 500 > 400
        XCTAssertEqual(GridSpacing.sketch(snapMM: 0, extentMM: 50).minor, 1)    // snapping off
        XCTAssertEqual(GridSpacing.sketch(snapMM: 1, extentMM: 1000).minor, 10)
        XCTAssertLessThanOrEqual(GridSpacing.sketch(snapMM: 0.1, extentMM: 5000).cells, GridSpacing.maxCells)
    }

    func testPlacementUsesOriginNearbyElseSnapsUnderThePart() {
        let l = GridSpacing.ground(extentMM: 50)
        let near = GridSpacing.placement(partCenterMM: [5, -3], partExtentMM: 50, layout: l)
        XCTAssertEqual(near.centerMM, .zero); XCTAssertTrue(near.axes)
        let far = GridSpacing.placement(partCenterMM: [1234, -567], partExtentMM: 50, layout: l)
        XCTAssertFalse(far.axes)
        XCTAssertEqual(far.centerMM, [1200, -600])
    }

    func testGridTogglesDefaultOnAndAreIndependent() {
        let d = UserDefaults.standard
        let saved = (d.object(forKey: Prefs.gridKey), d.object(forKey: Prefs.sketchGridKey))
        defer {
            d.set(saved.0, forKey: Prefs.gridKey)
            d.set(saved.1, forKey: Prefs.sketchGridKey)
        }
        d.removeObject(forKey: Prefs.gridKey); d.removeObject(forKey: Prefs.sketchGridKey)
        XCTAssertTrue(Prefs.showsGrid); XCTAssertTrue(Prefs.showsSketchGrid)
        d.set(false, forKey: Prefs.gridKey)
        XCTAssertFalse(Prefs.showsGrid); XCTAssertTrue(Prefs.showsSketchGrid)
        d.set(true, forKey: Prefs.gridKey); d.set(false, forKey: Prefs.sketchGridKey)
        XCTAssertTrue(Prefs.showsGrid); XCTAssertFalse(Prefs.showsSketchGrid)
    }
}

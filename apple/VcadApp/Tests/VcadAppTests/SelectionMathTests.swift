import XCTest
import simd
@testable import VcadApp

final class SelectionMathTests: XCTestCase {
    // MARK: tolerance

    func testPixelToleranceMatchesFrustum() {
        // At distance d the 60° frustum is 2·d·tan30° tall; 1 px of an H-px
        // viewport is that over H.
        let d: Float = 500, h: Float = 1000
        let px1 = SelectionMath.pixelTolerance(px: 1, distance: d, viewportHeight: h)
        XCTAssertEqual(px1, 2 * d * tan(Float.pi / 6) / h, accuracy: 1e-5)
        XCTAssertEqual(SelectionMath.pixelTolerance(px: 6, distance: d, viewportHeight: h), 6 * px1, accuracy: 1e-4)
    }

    func testToleranceScalesWithZoom() {
        let near = SelectionMath.edgePickTolerance(distance: 100, viewportHeight: 800)
        let far = SelectionMath.edgePickTolerance(distance: 1000, viewportHeight: 800)
        XCTAssertEqual(far / near, 10, accuracy: 1e-3)
        XCTAssertGreaterThan(SelectionMath.edgePickTolerance(distance: 0, viewportHeight: 800), 0)
    }

    // MARK: marquee

    func testMarqueeDirection() {
        XCTAssertEqual(SelectionMath.MarqueeMode(start: .init(x: 10, y: 50), end: .init(x: 90, y: 0)), .window)
        XCTAssertEqual(SelectionMath.MarqueeMode(start: .init(x: 90, y: 0), end: .init(x: 10, y: 50)), .crossing)
    }

    func testWindowNeedsAllPointsCrossingAny() {
        let r = CGRect(x: 0, y: 0, width: 100, height: 100)
        let inside = [CGPoint(x: 10, y: 10), CGPoint(x: 90, y: 90)]
        let straddle = [CGPoint(x: 10, y: 10), CGPoint(x: 150, y: 90)]
        let outside = [CGPoint(x: 110, y: 10), CGPoint(x: 150, y: 90)]
        XCTAssertTrue(SelectionMath.marqueeCatches(inside, rect: r, mode: .window))
        XCTAssertFalse(SelectionMath.marqueeCatches(straddle, rect: r, mode: .window))
        XCTAssertTrue(SelectionMath.marqueeCatches(straddle, rect: r, mode: .crossing))
        XCTAssertFalse(SelectionMath.marqueeCatches(outside, rect: r, mode: .crossing))
        XCTAssertFalse(SelectionMath.marqueeCatches([], rect: r, mode: .crossing))
    }

    func testCrossingCatchesSegmentPassingThrough() {
        let r = CGRect(x: 40, y: 40, width: 20, height: 20)
        let through = [CGPoint(x: 0, y: 50), CGPoint(x: 100, y: 50)]
        let miss = [CGPoint(x: 0, y: 0), CGPoint(x: 100, y: 10)]
        XCTAssertFalse(SelectionMath.marqueeCatches(through, rect: r, mode: .crossing))
        XCTAssertTrue(SelectionMath.marqueeCatches(through, rect: r, mode: .crossing, segments: true))
        XCTAssertFalse(SelectionMath.marqueeCatches(miss, rect: r, mode: .crossing, segments: true))
    }

    // MARK: readout

    func testReadoutFormatting() {
        XCTAssertNil(SelectionMath.readout(faceAreas: [], edgeLengths: []))
        XCTAssertEqual(SelectionMath.readout(faceAreas: [], edgeLengths: [42]), "1 edge · 42.0 mm")
        XCTAssertEqual(SelectionMath.readout(faceAreas: [1000, 240], edgeLengths: []), "2 faces · 1\u{2009}240 mm²")
        XCTAssertEqual(SelectionMath.readout(faceAreas: [12.34], edgeLengths: [1, 2.5]),
                       "1 face · 12.3 mm²   2 edges · 3.5 mm")
    }

    // MARK: orbit pivot

    func testPivotPrefersTopologyThenParts() {
        let t = [SIMD3<Float>(0, 0, 0), SIMD3<Float>(2, 0, 0)]
        let p = [SIMD3<Float>(10, 10, 10)]
        XCTAssertEqual(SelectionMath.orbitPivot(topology: t, parts: p), SIMD3(1, 0, 0))
        XCTAssertEqual(SelectionMath.orbitPivot(topology: [], parts: p), SIMD3(10, 10, 10))
        XCTAssertNil(SelectionMath.orbitPivot(topology: [], parts: []))
    }

    func testOrbitAboutTargetLeavesPanAlone() {
        let pan = SIMD3<Float>(1, 2, 3)
        let o0 = SIMD3<Float>(0, 0, 5), o1 = 5 * simd_normalize(SIMD3<Float>(3, 2, 3.5))
        let out = SelectionMath.panAfterOrbit(pan: pan, from: o0, to: o1, pivot: pan)
        XCTAssertEqual(simd_distance(out, pan), 0, accuracy: 1e-4)
    }

    func testOrbitHoldsPivotFixedInCameraFrame() {
        func orbit(_ az: Float, _ el: Float) -> SIMD3<Float> {
            5 * SIMD3(cos(el) * sin(az), sin(el), cos(el) * cos(az))
        }
        func cameraCoords(_ p: SIMD3<Float>, pan: SIMD3<Float>, o: SIMD3<Float>) -> SIMD3<Float> {
            let cam = pan + o, fwd = simd_normalize(-o)
            let right = simd_normalize(simd_cross(fwd, SIMD3(0, 1, 0))), up = simd_cross(right, fwd)
            let v = p - cam
            return SIMD3(simd_dot(v, right), simd_dot(v, up), simd_dot(v, fwd))
        }
        let pan = SIMD3<Float>(0, 0, 0), pivot = SIMD3<Float>(1.5, -0.5, 0.8)
        let o0 = orbit(0.3, 0.2), o1 = orbit(1.1, 0.7)
        let pan1 = SelectionMath.panAfterOrbit(pan: pan, from: o0, to: o1, pivot: pivot)
        let before = cameraCoords(pivot, pan: pan, o: o0), after = cameraCoords(pivot, pan: pan1, o: o1)
        XCTAssertEqual(simd_distance(before, after), 0, accuracy: 1e-4)
    }
}

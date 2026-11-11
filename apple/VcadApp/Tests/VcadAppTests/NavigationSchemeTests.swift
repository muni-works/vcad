import XCTest
import simd
@testable import VcadApp

final class NavigationSchemeTests: XCTestCase {
    typealias S = NavigationScheme

    func testFusionIsTheDefault() {
        XCTAssertEqual(S.default, .fusion360)
        let key = S.prefsKey
        let saved = UserDefaults.standard.object(forKey: key)
        defer { UserDefaults.standard.set(saved, forKey: key) }
        UserDefaults.standard.removeObject(forKey: key)
        XCTAssertEqual(S.current, .fusion360)
        UserDefaults.standard.set("classic", forKey: key)
        XCTAssertEqual(S.current, .classic)
        UserDefaults.standard.set("bogus", forKey: key)
        XCTAssertEqual(S.current, .fusion360)
    }

    func testFusionMouseBindings() {
        let f = S.fusion360
        XCTAssertEqual(f.action(for: .drag(.middle, shift: false, command: false)), .pan)
        XCTAssertEqual(f.action(for: .drag(.middle, shift: true, command: false)), .orbit)
        XCTAssertEqual(f.action(for: .scroll(precise: false, shift: false)), .zoomAtCursor)
        XCTAssertEqual(f.action(for: .doubleClick(.middle)), .fitAll)
        // Left-drag selects instead of orbiting — from empty space or a part.
        XCTAssertEqual(f.action(for: .drag(.left, shift: false, command: false)), .marquee)
        XCTAssertEqual(f.action(for: .drag(.left, shift: false, command: false, onEmpty: false)), .marquee)
        XCTAssertEqual(f.action(for: .drag(.left, shift: true, command: false)), .marquee)
    }

    func testFusionTrackpadBindings() {
        let f = S.fusion360
        XCTAssertEqual(f.action(for: .scroll(precise: true, shift: false)), .pan)
        XCTAssertEqual(f.action(for: .scroll(precise: true, shift: true)), .orbit)
        XCTAssertEqual(f.action(for: .pinch), .zoom)
    }

    func testClassicKeepsTheOriginalBindings() {
        let c = S.classic
        XCTAssertEqual(c.action(for: .drag(.left, shift: false, command: false)), .orbit)
        XCTAssertEqual(c.action(for: .drag(.left, shift: true, command: false)), .pan)
        XCTAssertEqual(c.action(for: .drag(.left, shift: false, command: true, onEmpty: true)), .marquee)
        // ⌘-drag that starts on a part is still an orbit.
        XCTAssertEqual(c.action(for: .drag(.left, shift: false, command: true, onEmpty: false)), .orbit)
        XCTAssertEqual(c.action(for: .scroll(precise: true, shift: false)), .zoom)
        XCTAssertEqual(c.action(for: .scroll(precise: false, shift: false)), .zoom)
        XCTAssertEqual(c.action(for: .pinch), .zoom)
    }

    func testEverySchemeSharesTheNonCameraInputs() {
        for s in S.allCases {
            XCTAssertEqual(s.action(for: .drag(.left, shift: false, command: false, onHandle: true)), .dragHandle, "\(s)")
            // Right-click is the context menu, never the camera.
            XCTAssertEqual(s.action(for: .drag(.right, shift: false, command: false)), .none, "\(s)")
            XCTAssertEqual(s.action(for: .doubleClick(.right)), .none, "\(s)")
            XCTAssertEqual(s.action(for: .doubleClick(.left)), .framePart, "\(s)")
        }
    }

    func testSketchModeNeverRubberBandsParts() {
        XCTAssertEqual(S.fusion360.action(for: .drag(.left, shift: false, command: false), sketching: true), .none)
        XCTAssertEqual(S.classic.action(for: .drag(.left, shift: false, command: true), sketching: true), .orbit)
        XCTAssertEqual(S.fusion360.action(for: .doubleClick(.left), sketching: true), .none)
        // The camera still moves while sketching.
        XCTAssertEqual(S.fusion360.action(for: .drag(.middle, shift: true, command: false), sketching: true), .orbit)
    }

    func testZoomAtCursorKeepsThePivotFixed() {
        let target = SIMD3<Float>(0, 0, 0), camera = SIMD3<Float>(0, 0, 2)
        // A ray off-axis, toward the upper right.
        let dir = simd_normalize(SIMD3<Float>(0.2, 0.1, -1))
        let pivot = camera + dir * (2 / simd_dot(dir, SIMD3(0, 0, -1)))
        let newTarget = S.zoomTarget(panOffset: target, camera: camera, rayDirection: dir,
                                     distance: 2, newDistance: 1)
        // Halving the distance halves the target's offset from the pivot.
        XCTAssertEqual(simd_distance(newTarget, pivot), simd_distance(target, pivot) / 2, accuracy: 1e-5)
        // The new camera, pivot, and old ray stay collinear: pivot is still under the cursor.
        let newCamera = newTarget + (camera - target) * 0.5
        let toPivot = simd_normalize(pivot - newCamera)
        XCTAssertEqual(simd_dot(toPivot, dir), 1, accuracy: 1e-5)
    }

    func testZoomThroughTheCentreDoesNotPan() {
        let t = S.zoomTarget(panOffset: [1, 2, 3], camera: [1, 2, 5], rayDirection: [0, 0, -1],
                             distance: 2, newDistance: 0.5)
        XCTAssertEqual(simd_distance(t, [1, 2, 3]), 0, accuracy: 1e-6)
    }

    /// Orbiting with a selection swings the camera about the selection's
    /// centre (Fusion); with none, the target stays put.
    @MainActor
    func testOrbitPivotsAboutSelection() {
        let m = EditorModel()
        m.panOffset = .zero
        m.orbitPivotProvider = { nil }
        m.beginOrbit(); m.orbitDrag(dx: 40, dy: 20); m.stopSpin()
        XCTAssertEqual(simd_length(m.panOffset), 0, accuracy: 1e-6, "no selection: keep the current pivot")

        let pivot = SIMD3<Float>(0.3, 0.1, -0.2)
        m.orbitPivotProvider = { pivot }
        let d0 = simd_distance(m.cameraPosition, pivot)
        m.beginOrbit(); m.orbitDrag(dx: -60, dy: 15); m.stopSpin()
        XCTAssertGreaterThan(simd_length(m.panOffset), 1e-3, "target moved: orbit was about the selection")
        XCTAssertEqual(simd_distance(m.cameraPosition, pivot), d0, accuracy: 1e-4)
    }
}

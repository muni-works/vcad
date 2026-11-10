import XCTest
import simd
@testable import VcadApp

/// Defects from building a six-part assembly by mouse: parts listed as
/// "Part N", click-placed primitives landing hundreds of mm out, and a dock
/// interference button that never enabled.
@MainActor
final class AssemblyBenchTests: XCTestCase {
    private var temps: [URL] = []
    override func tearDown() async throws {
        for u in temps { try? FileManager.default.removeItem(at: u) }
    }

    /// A seed Box, then four click-placed primitives — each root an unnamed
    /// Translate over the named feature, which is what tripped the lists.
    private func bench() throws -> EditorModel {
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": ["1": ["id": 1, "name": "Box", "op": ["type": "Cube", "size": ["x": 30, "y": 30, "z": 30]]]],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 1, "material": "default"]],
        ]
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
        temps.append(url)
        try JSONSerialization.data(withJSONObject: doc).write(to: url)
        let m = EditorModel()
        m.openDocument(url)
        XCTAssertTrue(m.canEditDocument, m.engineError ?? "no engine")
        m.addPrimitive(.cylinder, at: SIMD3(60, 0, 0))
        m.addPrimitive(.sphere, at: SIMD3(120, 0, 0))
        m.addPrimitive(.cone, at: SIMD3(180, 0, 0))
        m.addPrimitive(.wedge, at: SIMD3(240, 0, 0))
        return m
    }

    // MARK: 1. part names

    func testPartsAreListedByTheirModelTreeNames() throws {
        let m = try bench()
        XCTAssertEqual((0..<5).map(m.partDisplayName), ["Box", "Cylinder", "Sphere", "Cone", "Wedge"])
        // The placed root really is unnamed — the name lives under it.
        let roots = try XCTUnwrap(m.documentGraph?.visibleRoots)
        XCTAssertNil(m.documentGraph?.nodes[roots[4].nodeId]?.name)
    }

    func testClearanceNamesPartsAndPrefersInstanceNames() async throws {
        let m = try bench()
        await AnalysisStore.shared.runClearance(m)
        let plain = try XCTUnwrap(AnalysisStore.shared.clearance, AnalysisStore.shared.clearanceError ?? "no report")
        XCTAssertEqual(plain.parts, ["Box", "Cylinder", "Sphere", "Cone", "Wedge"])
        XCTAssertFalse(plain.pairs.contains { $0.a_name.hasPrefix("Part ") || $0.b_name.hasPrefix("Part ") })

        // Assembly: instances replace their def roots; the rest stay free.
        XCTAssertNotNil(m.insertInstance(ofPart: 0))
        XCTAssertNotNil(m.insertInstance(ofPart: 0))
        await AnalysisStore.shared.runClearance(m)
        let asm = try XCTUnwrap(AnalysisStore.shared.clearance, AnalysisStore.shared.clearanceError ?? "no report")
        XCTAssertEqual(asm.parts, ["Box 1", "Box 2", "Cylinder", "Sphere", "Cone", "Wedge"])
    }

    // MARK: 2. click placement

    func testPlacementRayIsTheInverseOfTheViewportProjection() throws {
        let size = CGSize(width: 1200, height: 800)
        let camera = SIMD3<Float>(0.4, 0.5, 0.9), look = SIMD3<Float>(0, 0, 0)
        // Centre of the view looks straight at the target.
        let c = try XCTUnwrap(PlacementMath.worldRay(viewPoint: CGPoint(x: 600, y: 400), viewSize: size,
                                                     camera: camera, lookAt: look))
        XCTAssertLessThan(simd_distance(c.d, normalize(look - camera)), 1e-5)
        // A point projected the way viewPoint(forPart:) does lies on its ray.
        let target = SIMD3<Float>(0.1, -0.05, 0.07)
        let fwd = normalize(look - camera), right = normalize(cross(fwd, SIMD3(0, 1, 0))), up = cross(right, fwd)
        let v = target - camera, z = dot(v, fwd), t = tan(Float(30) * .pi / 180)
        let px = CGFloat(((dot(v, right) / z) / (t * 1.5)) * 0.5 + 0.5) * size.width
        let py = CGFloat(0.5 - ((dot(v, up) / z) / t) * 0.5) * size.height
        let r = try XCTUnwrap(PlacementMath.worldRay(viewPoint: CGPoint(x: px, y: py), viewSize: size,
                                                     camera: camera, lookAt: look))
        let along = dot(target - r.o, r.d)
        XCTAssertLessThan(simd_distance(r.o + r.d * along, target), 1e-4)
    }

    func testGroundHitAndFarClicksFallBackNearTheGeometry() {
        // Looking down at 45°, from 100 mm up: lands 100 mm ahead.
        let hit = PlacementMath.groundHit(origin: SIMD3(0, -100, 100), direction: normalize(SIMD3(0, 1, -1)))
        XCTAssertEqual(hit.map { simd_distance($0, .zero) } ?? 99, 0, accuracy: 1e-3)
        XCTAssertNil(PlacementMath.groundHit(origin: SIMD3(0, 0, 10), direction: SIMD3(1, 0, 0)), "parallel")
        XCTAssertNil(PlacementMath.groundHit(origin: SIMD3(0, 0, 10), direction: SIMD3(0, 0, 1)), "behind")

        let near = SIMD3<Float>(40, -20, 0)
        XCTAssertEqual(PlacementMath.resolve(hit: near, sceneCenter: .zero, sceneExtent: 30, fallback: .zero), near)
        // The bench's wedge at (354.7, -303.3) next to a 30 mm box: rejected.
        let far = SIMD3<Float>(354.7, -303.3, 0), beside = SIMD3<Float>(55, 0, 0)
        XCTAssertEqual(PlacementMath.resolve(hit: far, sceneCenter: SIMD3(15, 15, 15), sceneExtent: 30,
                                             fallback: beside), beside)
        XCTAssertEqual(PlacementMath.resolve(hit: nil, sceneCenter: .zero, sceneExtent: 30, fallback: beside), beside)
        // Reach grows with the scene: the same point is fine beside a big part.
        XCTAssertEqual(PlacementMath.resolve(hit: far, sceneCenter: .zero, sceneExtent: 400, fallback: .zero), far)
    }

    // MARK: 3. dock interference button

    func testDockInterferenceEnablesWithTwoBodiesAndOpensClearance() throws {
        let m = try bench()
        func check() throws -> Tool { try XCTUnwrap(DockTools.tools(.assembly, model: m).first { $0.id == "asm.check" }) }
        XCTAssertEqual(m.clearanceBodyCount, 5)
        XCTAssertTrue(try check().enabled, "five parts, no mates — still checkable")
        for _ in 0..<6 { XCTAssertNotNil(m.insertInstance(ofPart: 0)) }
        XCTAssertEqual(m.clearanceBodyCount, 6 + 4)
        XCTAssertTrue(try check().enabled)

        let s = AnalysisStore.shared
        s.shown = false; s.tab = .dfm
        try check().action()
        XCTAssertTrue(s.shown)
        XCTAssertEqual(s.tab, .clearance)

        let one = EditorModel()
        XCTAssertEqual(one.clearanceBodyCount, 0)
        XCTAssertFalse(try XCTUnwrap(DockTools.tools(.assembly, model: one).first { $0.id == "asm.check" }).enabled)
    }
}

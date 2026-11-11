import XCTest
import simd
@testable import VcadApp

final class CommandRankerTests: XCTestCase {
    private struct C { let id: String; let title: String }
    private let items = [C(id: "stop", title: "Stop Measuring"), C(id: "top", title: "View: Top"),
                         C(id: "ext", title: "Export STL…"), C(id: "fil", title: "Fillet…"),
                         C(id: "flip", title: "Sketch → Fillet")]

    private func rank(_ q: String, recents: [String] = []) -> [String] {
        CommandRanker.rank(items, query: q, recents: recents, id: \.id, title: \.title).map(\.id)
    }

    func testPrefixAndWordStartBeatMidWord() {
        XCTAssertEqual(rank("fil").first, "fil")               // prefix
        XCTAssertEqual(rank("top").first, "top")               // word start beats "s-top"
    }

    func testSubsequenceMatchesAndMissesDrop() {
        XCTAssertEqual(rank("xst"), ["ext"])                   // e-X-port S-T-l
        XCTAssertTrue(rank("zzz").isEmpty)
        XCTAssertNil(CommandRanker.score("Fillet", "q"))
    }

    func testEmptyQueryPutsRecentsFirstInOrder() {
        XCTAssertEqual(rank("", recents: ["ext", "top"]).prefix(2), ["ext", "top"])
        XCTAssertEqual(rank("").count, items.count)
    }

    func testRecencyBreaksTies() {
        // Both "Fillet…" and "Sketch → Fillet" match; the recent one rises.
        let plain = rank("fillet")
        XCTAssertEqual(plain.first, "fil")
        let boosted = CommandRanker.rank([C(id: "a", title: "Fillet A"), C(id: "b", title: "Fillet B")],
                                         query: "fillet", recents: ["b"], id: \.id, title: \.title)
        XCTAssertEqual(boosted.map(\.id), ["b", "a"])
    }

    func testRecentsAreMRUAndCapped() {
        var r = RecentCommands()
        for i in 0..<12 { r.record("c\(i)") }
        XCTAssertEqual(r.ids.count, RecentCommands.limit)
        XCTAssertEqual(r.ids.first, "c11")
        r.record("c5")
        XCTAssertEqual(r.ids.first, "c5")
        XCTAssertEqual(r.ids.filter { $0 == "c5" }.count, 1)
    }

    func testRecentsPersist() {
        let d = UserDefaults(suiteName: "nav-recents-\(UUID())")!
        var r = RecentCommands(); r.record("x"); r.record("y")
        r.save(d)
        XCTAssertEqual(RecentCommands.load(d).ids, ["y", "x"])
    }
}

final class ViewTargetTests: XCTestCase {
    func testTwentySixTargets() {
        XCTAssertEqual(ViewTarget.all.count, 26)
        XCTAssertEqual(ViewTarget.all.filter { $0.kind == .face }.count, 6)
        XCTAssertEqual(ViewTarget.all.filter { $0.kind == .edge }.count, 12)
        XCTAssertEqual(ViewTarget.all.filter { $0.kind == .corner }.count, 8)
    }

    func testFacesMatchCameraPresets() {
        XCTAssertEqual(ViewTarget.front.azimuth, CameraPreset.front.azimuth, accuracy: 1e-6)
        XCTAssertEqual(ViewTarget.right.azimuth, CameraPreset.right.azimuth, accuracy: 1e-6)
        XCTAssertEqual(ViewTarget.top.elevation, CameraPreset.top.elevation, accuracy: 1e-6)
        XCTAssertEqual(ViewTarget.front.elevation, 0, accuracy: 1e-6)
    }

    func testCameraDirectionRoundTrips() {
        for t in ViewTarget.all where abs(t.elevation) < ViewTarget.maxElevation - 1e-3 {
            let c = ViewTarget.cameraDirection(azimuth: t.azimuth, elevation: t.elevation)
            XCTAssertEqual(simd_dot(c, t.unit), 1, accuracy: 1e-5, t.label)
        }
    }

    func testFacingAndOpposite() {
        XCTAssertEqual(ViewTarget.facing(azimuth: 0.1, elevation: 0.1), .front)
        XCTAssertEqual(ViewTarget.facing(azimuth: .pi / 2, elevation: 0), .right)
        XCTAssertEqual(ViewTarget.facing(azimuth: 0, elevation: 1.4), .top)
        XCTAssertEqual(ViewTarget.front.opposite, .back)
    }

    func testLabels() {
        XCTAssertEqual(ViewTarget(dir: SIMD3(1, -1, 1)).label, "Top Front Right")
        XCTAssertEqual(ViewTarget(dir: SIMD3(-1, 0, -1)).label, "Bottom Left")
    }
}

final class CompassProjectionTests: XCTestCase {
    func testHeadOnFrontShowsOneFaceAndHitsItsRegions() {
        let p = CompassProjection(azimuth: 0, elevation: 0, scale: 10)
        XCTAssertEqual(p.visibleFaces.map(\.target), [.front])
        XCTAssertEqual(p.target(at: SIMD2(0, 0)), .front)
        // Screen up is +Z, screen right is +X when facing the front.
        XCTAssertEqual(p.target(at: SIMD2(0, -9)), ViewTarget(dir: SIMD3(0, -1, 1)))   // top edge
        XCTAssertEqual(p.target(at: SIMD2(9, 9)), ViewTarget(dir: SIMD3(1, -1, -1)))   // bottom-right corner
        XCTAssertNil(p.target(at: SIMD2(20, 0)))
    }

    func testIsoViewShowsThreeFacesNearestFirstForHits() {
        let p = CompassProjection(azimuth: .pi / 4, elevation: 0.6, scale: 10)
        XCTAssertEqual(Set(p.visibleFaces.map(\.target)), [.front, .right, .top])
        // The cube's projected centre is the shared corner of all three faces.
        XCTAssertEqual(p.target(at: p.project(SIMD3(1, -1, 1)) * 0.98), ViewTarget(dir: SIMD3(1, -1, 1)))
        // Every target hit lands on the face it claims to be on.
        for f in p.visibleFaces {
            let c = f.corners.reduce(SIMD2<Float>(0, 0), +) / 4
            XCTAssertEqual(p.target(at: c), f.target)
        }
    }

    func testHighlightCoversTargetOnEveryFaceItTouches() {
        let p = CompassProjection(azimuth: .pi / 4, elevation: 0.6, scale: 10)
        XCTAssertEqual(p.highlight(ViewTarget(dir: SIMD3(1, -1, 1))).count, 3)   // corner
        XCTAssertEqual(p.highlight(ViewTarget(dir: SIMD3(1, -1, 0))).count, 2)   // vertical edge
        XCTAssertEqual(p.highlight(.top).count, 1)
        XCTAssertEqual(p.highlight(.bottom).count, 0)                            // hidden
    }
}

final class CameraTweenTests: XCTestCase {
    func testShortestArcAndEndpoints() {
        let a = CameraPose(azimuth: 3.0, elevation: 0, distance: 1, pan: .zero)
        let b = CameraPose(azimuth: -3.0, elevation: 0.5, distance: 2, pan: SIMD3(1, 0, 0))
        let t = CameraTween(from: a, to: b, duration: 1)
        XCTAssertEqual(t.to.azimuth, -3.0 + 2 * .pi, accuracy: 1e-5)      // goes the short way
        XCTAssertEqual(t.pose(at: 0), a)
        let end = t.pose(at: 1)
        XCTAssertEqual(end.elevation, 0.5, accuracy: 1e-6)
        XCTAssertEqual(end.distance, 2, accuracy: 1e-6)
        XCTAssertTrue(t.finished(at: 1))
        XCTAssertFalse(t.finished(at: 0.5))
    }

    func testEaseIsMonotoneAndSymmetric() {
        var last = -1.0
        for i in 0...20 { let v = CameraTween.ease(Double(i) / 20); XCTAssertGreaterThanOrEqual(v, last); last = v }
        XCTAssertEqual(CameraTween.ease(0.5), 0.5, accuracy: 1e-9)
        XCTAssertEqual(CameraTween.ease(0.25) + CameraTween.ease(0.75), 1, accuracy: 1e-9)
    }

    func testZeroDurationJumps() {
        let a = CameraPose(azimuth: 0, elevation: 0, distance: 1, pan: .zero)
        let b = CameraPose(azimuth: 1, elevation: 1, distance: 1, pan: .zero)
        XCTAssertEqual(CameraTween(from: a, to: b, duration: 0).pose(at: 0), b)
    }
}

final class SavedViewTests: XCTestCase {
    private let pose = CameraPose(azimuth: 1, elevation: 0.2, distance: 1.2, pan: SIMD3(0.1, 0, 0))

    func testSaveReplacesByNameAndDefaultsNames() {
        var l = SavedViewList()
        l.save("Detail", pose: pose)
        var p2 = pose; p2.azimuth = 2
        l.save("detail", pose: p2)                         // case-insensitive replace
        XCTAssertEqual(l.views.count, 1)
        XCTAssertEqual(l.views[0].pose.azimuth, 2)
        XCTAssertEqual(l.save("  ", pose: pose).name, "View 2")
        l.remove("Detail")
        XCTAssertEqual(l.views.map(\.name), ["View 2"])
    }

    func testPersistsPerDocument() {
        let d = UserDefaults(suiteName: "nav-views-\(UUID())")!
        var l = SavedViewList(); l.save("Front Detail", pose: pose)
        l.save(for: "/tmp/a.vcad", d)
        XCTAssertEqual(SavedViewList.load(for: "/tmp/a.vcad", d), l)
        XCTAssertTrue(SavedViewList.load(for: "/tmp/b.vcad", d).views.isEmpty)
    }
}

final class SmartSectionTests: XCTestCase {
    func testCutsOnAxisFacingCameraAndRemovesNearSide() {
        // Looking from the front (camera at −Y): cut on Y through the centre,
        // removing the −Y half (the half toward the camera).
        let s = SectionPlane.smart(toCamera: SIMD3(0.2, -1, 0.3), center: SIMD3(5, 7, 9))
        XCTAssertEqual(s.axis, .y)
        XCTAssertEqual(s.offset, 7)
        XCTAssertGreaterThan(s.distance(SIMD3(5, 0, 9)), 0)       // camera side is cut away
        XCTAssertLessThan(s.distance(SIMD3(5, 20, 9)), 0)
    }

    func testPlanarAxisFaceWinsAndSitsOnIt() {
        let s = SectionPlane.smart(toCamera: SIMD3(0, -1, 0), center: .zero,
                                   faceNormal: SIMD3(0, 0, 1), facePoint: SIMD3(0, 0, 12))
        XCTAssertEqual(s.axis, .z)
        XCTAssertEqual(s.offset, 12)
        XCTAssertGreaterThan(s.distance(SIMD3(0, 0, 13)), 0)      // above the face is removed
    }

    func testObliqueFaceFallsBackToView() {
        let s = SectionPlane.smart(toCamera: SIMD3(1, 0, 0), center: SIMD3(3, 0, 0),
                                   faceNormal: SIMD3(1, 1, 0), facePoint: SIMD3(9, 9, 9))
        XCTAssertEqual(s.axis, .x)
        XCTAssertEqual(s.offset, 3)
    }
}

final class RadialLayoutTests: XCTestCase {
    func testSlotsByDirection() {
        XCTAssertNil(RadialLayout.slot(for: CGSize(width: 3, height: 3), count: 8))       // dead zone
        XCTAssertEqual(RadialLayout.slot(for: CGSize(width: 0, height: -50), count: 8), 0) // north
        XCTAssertEqual(RadialLayout.slot(for: CGSize(width: 50, height: 0), count: 8), 2)  // east
        XCTAssertEqual(RadialLayout.slot(for: CGSize(width: 0, height: 50), count: 8), 4)  // south
        XCTAssertEqual(RadialLayout.slot(for: CGSize(width: -40, height: -40), count: 8), 7)
        XCTAssertEqual(RadialLayout.slot(for: CGSize(width: -5, height: -50), count: 8), 0) // wraps near north
    }

    func testPositionsMatchSlots() {
        for i in 0..<8 {
            let p = RadialLayout.position(slot: i, of: 8, radius: 60)
            XCTAssertEqual(RadialLayout.slot(for: CGSize(width: p.x, height: p.y), count: 8), i)
        }
    }

    func testHaloKeepsFindNorthAndFrameSouth() {
        for ctx in [HaloContext.nothing, .part, .face, .edge, .sketch] {
            XCTAssertEqual(ctx.slots.count, 8)
            XCTAssertEqual(ctx.slots[0], "find")
            XCTAssertEqual(ctx.slots[4], "frame")
            XCTAssertEqual(Set(ctx.slots).count, 8, "\(ctx) has a duplicate slot")
        }
        XCTAssertTrue(HaloContext.face.slots.contains("sectionHere"))
        XCTAssertTrue(HaloContext.edge.slots.contains("op.fillet"))
    }
}

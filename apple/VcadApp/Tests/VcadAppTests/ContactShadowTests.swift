import XCTest
import simd
@testable import VcadApp

final class ContactShadowTests: XCTestCase {
    /// Alpha bytes of a baked map (row 0 = top = kernel max Y).
    private func bytes(_ tris: [SIMD3<Float>], _ lo: SIMD3<Float>, _ hi: SIMD3<Float>) -> [UInt8] {
        guard let img = ContactShadow.bake(triangles: tris, lo: lo, hi: hi),
              let data = img.dataProvider?.data as Data? else { XCTFail("bake failed"); return [] }
        XCTAssertEqual(img.width, ContactShadow.pixels)
        return [UInt8](data)
    }

    func testPlaneIsCentredOnTheFootprintWhereverThePartSits() {
        // A 20 mm cube moved to (60, 40, 0).
        let p = ContactShadow.plane(lo: [50, 30, 0], hi: [70, 50, 20])
        XCTAssertEqual(p.center.x, 60, accuracy: 1e-5)
        XCTAssertEqual(p.center.y, 40, accuracy: 1e-5)
        XCTAssertEqual(p.floorZ, 0)
        XCTAssertEqual(p.size.x, 20 + 2 * p.margin, accuracy: 1e-4)
        XCTAssertGreaterThan(p.margin, 2)
    }

    func testTallPartsThrowAWiderPool() {
        let short = ContactShadow.plane(lo: [-8, -8, 0], hi: [8, 8, 10])
        let tall = ContactShadow.plane(lo: [-8, -8, 0], hi: [8, 8, 80])
        XCTAssertGreaterThan(tall.margin, short.margin)
    }

    func testFloorIsThePartsLowestPoint() {
        let p = ContactShadow.plane(lo: [0, 0, -12.5], hi: [10, 10, 30])
        XCTAssertEqual(p.floorZ, -12.5)
    }

    func testContactLayerKeepsOnlyNearFloorTriangles() {
        let floorTri: [SIMD3<Float>] = [[0, 0, 0], [10, 0, 0], [0, 10, 0]]
        let roofTri: [SIMD3<Float>] = [[0, 0, 50], [10, 0, 50], [0, 10, 50]]
        let wallTri: [SIMD3<Float>] = [[0, 0, 0], [10, 0, 0], [10, 0, 50]]   // edge-on from above
        let fp = ContactShadow.footprint(triangles: floorTri + roofTri + wallTri, floorZ: 0, band: 1)
        XCTAssertEqual(fp.all.count, 6)
        XCTAssertEqual(fp.contact.count, 3)
    }

    func testShadowIsDarkestUnderTheFootprintAndClearAtTheEdge() {
        let lo = SIMD3<Float>(-10, -10, 0), hi = SIMD3<Float>(10, 10, 20)
        let b = bytes(ContactShadow.boxTriangles(lo: lo, hi: hi), lo, hi)
        let n = ContactShadow.pixels
        let centre = b[(n / 2) * n + n / 2]
        let corner = b[1 * n + 1]
        XCTAssertGreaterThan(centre, 120, "solid contact under the part")
        XCTAssertLessThan(corner, 8, "penumbra fades out before the quad edge")
        // Contact darkening: just inside the footprint edge stays dark —
        // a tight falloff at the base edge, not a broad blob.
        let edgePx = Int(ContactShadow.pixel([9, 0], plane: ContactShadow.plane(lo: lo, hi: hi)).x)
        XCTAssertGreaterThan(b[(n / 2) * n + edgePx], 90)
    }

    func testMapOrientationFollowsKernelY() {
        // Footprint only in the +Y half → shadow in the top rows of the image.
        let lo = SIMD3<Float>(-10, -10, 0), hi = SIMD3<Float>(10, 10, 5)
        let tris = ContactShadow.boxTriangles(lo: [-10, 4, 0], hi: [10, 10, 0])
        let b = bytes(tris, lo, hi)
        let n = ContactShadow.pixels
        let plane = ContactShadow.plane(lo: lo, hi: hi)
        let yPlus = Int(ContactShadow.pixel([0, 7], plane: plane).y)    // CG y (bottom-up)
        let yMinus = Int(ContactShadow.pixel([0, -7], plane: plane).y)
        let rowPlus = n - 1 - yPlus, rowMinus = n - 1 - yMinus
        XCTAssertGreaterThan(b[rowPlus * n + n / 2], 100)
        XCTAssertLessThan(b[rowMinus * n + n / 2], 20)
    }
}

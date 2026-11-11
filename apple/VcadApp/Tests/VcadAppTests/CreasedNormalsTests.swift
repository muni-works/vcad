import XCTest
import simd
@testable import VcadApp

final class CreasedNormalsTests: XCTestCase {
    /// A welded cube (8 shared corners) must shade as 6 flat faces: every
    /// corner splits into 3 copies, one per face, each with an axis normal.
    func testWeldedCubeSplitsIntoFlatFaces() {
        let p: [SIMD3<Float>] = [[0,0,0],[1,0,0],[1,1,0],[0,1,0],[0,0,1],[1,0,1],[1,1,1],[0,1,1]]
        let i: [UInt32] = [0,2,1, 0,3,2,  4,5,6, 4,6,7,  0,1,5, 0,5,4,
                           2,3,7, 2,7,6,  1,2,6, 1,6,5,  3,0,4, 3,4,7]
        let (pos, n, idx) = KernelMesh.creasedNormals(positions: p, indices: i)
        XCTAssertEqual(pos.count, 24)
        XCTAssertEqual(idx.count, i.count)
        for v in n {
            let a = simd_abs(v)
            XCTAssertEqual(a.max(), 1, accuracy: 1e-5, "normal \(v) is not axis-aligned")
        }
        // Every triangle's corners share its face normal.
        for t in 0..<(idx.count / 3) {
            let (a, b, c) = (Int(idx[3*t]), Int(idx[3*t+1]), Int(idx[3*t+2]))
            let fn = normalize(cross(pos[b] - pos[a], pos[c] - pos[a]))
            for v in [a, b, c] { XCTAssertGreaterThan(dot(n[v], fn), 0.9999) }
        }
    }

    /// A gentle 10° fold is below the crease angle: the shared edge stays
    /// welded and its normals blend smooth.
    func testShallowFoldStaysSmooth() {
        let s = sin(Float(10) * .pi / 180)
        let p: [SIMD3<Float>] = [[0,0,0],[0,1,0],[1,0,0],[1,1,0],[-1,0,s],[-1,1,s]]
        let i: [UInt32] = [0,2,3, 0,3,1,  0,1,5, 0,5,4]
        let (pos, n, _) = KernelMesh.creasedNormals(positions: p, indices: i)
        XCTAssertEqual(pos.count, 6)
        let shared = n[0]
        XCTAssertGreaterThan(shared.z, 0.99)
        XCTAssertGreaterThan(shared.x, 0.01, "shared edge normal should lean toward the fold")
    }
}

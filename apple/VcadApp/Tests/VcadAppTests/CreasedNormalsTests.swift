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

    /// A gentle 10° fold between two long thin strips (a coarse curved
    /// tessellation) is below the crease angle: the shared edge stays
    /// welded and its normals blend smooth.
    func testShallowFoldStaysSmooth() {
        let s = 0.1 * sin(Float(10) * .pi / 180), c = 0.1 * cos(Float(10) * .pi / 180)
        let p: [SIMD3<Float>] = [[0,0,0],[0,1,0],[0.1,0,0],[0.1,1,0],[-c,0,s],[-c,1,s]]
        let i: [UInt32] = [0,2,3, 0,3,1,  0,1,5, 0,5,4]
        let (pos, n, _) = KernelMesh.creasedNormals(positions: p, indices: i)
        XCTAssertEqual(pos.count, 6)
        let shared = n[0]
        XCTAssertGreaterThan(shared.z, 0.99)
        XCTAssertGreaterThan(shared.x, 0.01, "shared edge normal should lean toward the fold")
    }
}

final class PlanarRegionNormalsTests: XCTestCase {
    /// A flat face (two triangles) meeting a gentle 10° facet must keep its
    /// exact plane normal at the shared edge — no tilt smeared across it.
    func testFlatFaceStaysExactlyFlatNextToGentleFacet() {
        let s = sin(Float(10) * .pi / 180), c = cos(Float(10) * .pi / 180)
        let p: [SIMD3<Float>] = [[0,0,0],[0,1,0],[4,0,0],[4,1,0],[4 + c,0,-s],[4 + c,1,-s],[2,0.5,0]]
        // Flat face as a 4-triangle fan around a centre vertex + one 10°
        // facet (a 2-triangle quad, so it still blends) off its right edge.
        let i: [UInt32] = [6,0,2, 6,2,3, 6,3,1, 6,1,0,  2,4,5, 2,5,3]
        let (pos, n, idx) = KernelMesh.creasedNormals(positions: p, indices: i)
        for t in 0..<4 {
            for k in 0..<3 {
                let v = n[Int(idx[3*t+k])]
                XCTAssertEqual(v.z, 1, accuracy: 1e-6, "flat face corner tilted: \(v) at \(pos[Int(idx[3*t+k])])")
            }
        }
    }
}

final class NormalsTrustTests: XCTestCase {
    private let cubeP: [SIMD3<Float>] = [[0,0,0],[1,0,0],[1,1,0],[0,1,0],[0,0,1],[1,0,1],[1,1,1],[0,1,1]]
    private let cubeI: [UInt32] = [0,2,1, 0,3,2,  4,5,6, 4,6,7,  0,1,5, 0,5,4,
                                   2,3,7, 2,7,6,  1,2,6, 1,6,5,  3,0,4, 3,4,7]

    /// STL-loader style: each shared corner keeps one face's normal, so the
    /// other faces at that corner see a normal 90° off — not trustworthy.
    func testLastFaceWinsNormalsAreRejected() {
        let n = [SIMD3<Float>](repeating: [0, 0, 1], count: 8)
        XCTAssertFalse(KernelMesh.normalsTrustworthy(n, positions: cubeP, indices: cubeI))
    }

    /// Per-face (BRep-style) vertices with their own face normal are kept.
    func testPerFaceNormalsAreKept() {
        let (p, n, i) = KernelMesh.creasedNormals(positions: cubeP, indices: cubeI)
        XCTAssertTrue(KernelMesh.normalsTrustworthy(n, positions: p, indices: i))
    }
}

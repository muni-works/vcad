import XCTest
import RealityKit
@testable import VcadApp

/// The live plasmoid's app wiring (PlasmoidSim.swift): binding parts by name,
/// the bounds → dish-frame math, and the frame loop driving the kernel
/// (`vcad_plasmoid_*`) through `PlasmoidStore.step` — the same call
/// `frame()` makes from the live model each display frame.
@MainActor
final class PlasmoidSimTests: XCTestCase {
    /// plasmoid-dish.vcad's dish: 87 mm outer radius, rim at z = 0, bowl 14 mm deep.
    let dish = PlasmoidBox(lo: SIMD3(-87, -87, -14), hi: SIMD3(87, 87, 0))
    let spike = PlasmoidBox(lo: SIMD3(-7, -7, -14), hi: SIMD3(7, 7, 10))

    /// The magnet wand (5 mm disc + 60 mm holder) with its disc bottom `gap` above the rim.
    func wand(x: Double, y: Double, gap: Double = 10) -> PlasmoidBox {
        PlasmoidBox(lo: SIMD3(x - 5, y - 5, gap), hi: SIMD3(x + 5, y + 5, gap + 65))
    }

    func testBindsByPartName() {
        let b = PlasmoidBinding.find(names: ["Dish", "spike", "coil", "magnet"])
        XCTAssertEqual(b, PlasmoidBinding(dishPart: 0, magnetPart: 3, spikePart: 1))
        XCTAssertNil(PlasmoidBinding.find(names: ["Box", "magnet"]))
    }

    func testFrameMath() {
        XCTAssertEqual(PlasmoidBinding.dishRadius(dish), 87)
        XCTAssertEqual(PlasmoidBinding.dishOrigin(dish), SIMD3(0, 0, 0))
        XCTAssertEqual(PlasmoidBinding.spikeTip(spike, dish: dish), SIMD3(0, 0, 10))
        // Disc centre = bottom of the part + 2.5 mm.
        let w = wand(x: -27.5, y: 47.6)
        XCTAssertEqual(PlasmoidBinding.magnetCenter(w, liveCenter: nil), SIMD3(-27.5, 47.6, 12.5))
        // Mid-drag: the live centre (bbox centre + offset) wins over stale bounds.
        let live = SIMD3(10.0, 20.0, w.center.z)
        XCTAssertEqual(PlasmoidBinding.magnetCenter(w, liveCenter: live), SIMD3(10, 20, 12.5))
        // The kernel pose is dish-relative, north pole down.
        let shifted = PlasmoidBinding.kernelPose(magnetCenter: SIMD3(10, 20, 12.5), dishOrigin: SIMD3(5, 5, 2))
        XCTAssertEqual(shifted, [5, 15, 10.5, 0, 0, -1])
    }

    /// Racing with no magnet; held magnet → the racer locks within ~1 s; a
    /// flick breaks it; 1 atm kills the lock.
    func testFrameLoopLocksBreaksAndVanishesAtAtmosphere() {
        let store = PlasmoidStore()
        store.active = true
        let fdt = 1.0 / 60.0
        let r0 = 87.0 * 0.055 / 0.08
        for _ in 0..<30 { store.step(dt: fdt, dish: dish, spike: spike, magnetCenter: nil) }
        XCTAssertFalse(store.readout.locked)
        XCTAssertEqual(store.readout.beta, 0)
        XCTAssertEqual(store.readout.substeps, 40, "1/60 s at 2.4 kHz")
        XCTAssertEqual(store.readout.root, SIMD3(0, 0, 10))

        var ang = Double.pi / 2
        func magnet() -> SIMD3<Double> {
            PlasmoidBinding.magnetCenter(wand(x: r0 * cos(ang), y: r0 * sin(ang)), liveCenter: nil)
        }
        var lockedAt: Int?
        for i in 0..<120 {
            store.step(dt: fdt, dish: dish, spike: spike, magnetCenter: magnet())
            if store.readout.locked, lockedAt == nil { lockedAt = i }
        }
        XCTAssertNotNil(lockedAt, "held magnet never captured the racer")
        XCTAssertTrue(store.readout.locked)
        XCTAssertGreaterThan(store.readout.beta, 1.5)
        // The foot is under the magnet (within the 8 mm lock radius).
        let m = magnet()
        XCTAssertLessThan(simd_length(SIMD2(store.readout.foot.x - m.x, store.readout.foot.y - m.y)), 8)

        // Slow drag (1.5 rad/s, against the race): follows.
        var followed = 0
        for _ in 0..<60 {
            ang -= 1.5 * fdt
            store.step(dt: fdt, dish: dish, spike: spike, magnetCenter: magnet())
            followed += store.readout.locked ? 1 : 0
        }
        XCTAssertGreaterThanOrEqual(followed, 57, "lost the lock on a slow drag")
        XCTAssertTrue(store.readout.locked)
        // Flick (25 rad/s): breaks away.
        for _ in 0..<6 {
            ang -= 25 * fdt
            store.step(dt: fdt, dish: dish, spike: spike, magnetCenter: magnet())
        }
        XCTAssertFalse(store.readout.locked, "a flick should beat the pin")

        // 1 atm: β ≈ 0.004, never locks however long it is held.
        store.setPressure(760)
        for _ in 0..<120 {
            store.step(dt: fdt, dish: dish, spike: spike, magnetCenter: magnet())
            XCTAssertFalse(store.readout.locked)
        }
        XCTAssertLessThan(store.readout.beta, 0.01)

        // Cost of one display frame (40 RK4 substeps), for the doc's budget.
        store.setPressure(1)
        let t0 = CFAbsoluteTimeGetCurrent()
        for _ in 0..<60 { store.step(dt: fdt, dish: dish, spike: spike, magnetCenter: magnet()) }
        let msPerFrame = (CFAbsoluteTimeGetCurrent() - t0) * 1000 / 60
        print(String(format: "[plasmoid] %.2f ms per 1/60 s frame (40 substeps)", msPerFrame))
        XCTAssertLessThan(msPerFrame, 16, "a frame of sim must fit in a frame")
        store.stop()
    }

    func testSurfaceZFollowsTheBowl() {
        // One triangle sloping from z = -10 at the origin to 0 at x = 10.
        let mesh = EditorModel.PickMesh(positions: [SIMD3(0, -10, -10), SIMD3(10, -10, 0), SIMD3(0, 10, -10)],
                                        indices: [0, 1, 2], lo: SIMD3(0, -10, -10), hi: SIMD3(10, 10, 0))
        XCTAssertEqual(PlasmoidStore.surfaceZ(x: 5, y: -5, mesh: mesh)!, -5, accuracy: 1e-5)
        XCTAssertNil(PlasmoidStore.surfaceZ(x: 20, y: 0, mesh: mesh))
    }
}

import XCTest
import CVcadFFI
@testable import VcadApp

/// Mallet / pitch-lock: the strike synth, the plain-words pitch readout, and
/// the kernel round trip (modes → lock → document) through `vcad_sound_call`.
@MainActor
final class SoundTests: XCTestCase {
    /// A parametric 6061 bar, compiled from loon through the FFI.
    func barDoc(len: Double = 120) throws -> Data {
        let src = "[defparam len \(len) :unit \"mm\" :min 40 :max 300]\n[let bar [cube len 25 6]]\n[root bar \"aluminum\"]\n"
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("vcad-sound-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let file = dir.appendingPathComponent("bar.loon")
        try src.write(to: file, atomically: true, encoding: .utf8)
        let path = file.path
        let raw = path.withCString { p in vcad_loon_file_to_json(UnsafeRawPointer(p).assumingMemoryBound(to: UInt8.self), path.utf8.count) }
        let ptr = try XCTUnwrap(raw, AnalysisFFI.lastError() ?? "loon failed")
        defer { vcad_string_free(ptr) }
        return Data(bytes: ptr, count: strlen(ptr))
    }

    func testPitchWordsNeverShowCents() {
        XCTAssertEqual(Pitch.words(1.2, target: "G6").0, "In tune with G6")
        XCTAssertEqual(Pitch.words(-6, target: "G6").0, "A hair flat of G6, barely audible")
        XCTAssertEqual(Pitch.words(25, target: "G6").0, "Noticeably sharp of G6")
        XCTAssertEqual(Pitch.hz("A4")!, 440, accuracy: 1e-9)
        XCTAssertEqual(Pitch.hz("C#5")!, 554.365, accuracy: 0.01)
        XCTAssertTrue(Pitch.pickable.contains("G6"))
    }

    func testSynthRingsAtTheModeFrequency() throws {
        // The rendered strike's dominant peak is the loudest partial.
        let s = ToneSynth.render([(hz: 1568, gain: 1, decay: 2), (hz: 6272, gain: 0.3, decay: 0.8)], seconds: 0.4)
        let e = try XCTUnwrap(PitchDetector.fundamental(Array(s.prefix(8192)), sampleRate: 48_000))
        XCTAssertEqual(PitchDetector.cents(e.hz, 1568), 0, accuracy: 0.5)
        XCTAssertLessThanOrEqual(s.map(abs).max()!, 0.61)
    }

    func testMalletSpectrumMatchesKernel() {
        XCTAssertEqual(Pitch.mallet(0, contactMs: 1), 1, accuracy: 1e-12)
        XCTAssertEqual(Pitch.mallet(500, contactMs: 1), .pi / 4, accuracy: 1e-9) // removable singularity
        XCTAssertLessThan(Pitch.mallet(4000, contactMs: 1), 0.2)
    }

    func testModesThenLockThroughTheKernel() throws {
        let doc = try barDoc()
        let pm = try SoundFFI.modes(doc: doc, part: 0)
        XCTAssertEqual(pm.name, "bar")
        XCTAssertTrue(pm.material_known)
        XCTAssertGreaterThan(pm.vertices, 100, "refined animation mesh")
        XCTAssertEqual(pm.modes[0].shape.count, 3 * pm.vertices)
        // 120 mm plain bar rings well above G6 (G6 needs ~140.7 mm).
        XCTAssertGreaterThan(pm.modes[0].hz, 1568)

        // Strike gains: the bar's end rings the fundamental; striking a node doesn't.
        let store = SoundStore.shared
        func vertexNear(x: Float) -> Int {
            (0..<pm.vertices).min { a, b in
                abs(pm.positions[3 * a] - x) + abs(pm.positions[3 * a + 2] - 6) < abs(pm.positions[3 * b] - x) + abs(pm.positions[3 * b + 2] - 6)
            }!
        }
        let up = SIMD3<Float>(0, 0, 1)
        let end = pm.modes[0].shape[3 * vertexNear(x: 0) + 2]
        let node = pm.modes[0].shape[3 * vertexNear(x: 120 * 0.2242) + 2]
        XCTAssertGreaterThan(abs(end), 0.7)
        XCTAssertLessThan(abs(node), 0.2)
        _ = store; _ = up

        let lock = PitchLockDTO(part: "bar", mode: 1, target_hz: Pitch.hz("G6")!, note: "G6", drives: "len", tolerance_cents: 1)
        let r = try SoundFFI.lock(doc: doc, lock)
        XCTAssertTrue(r.converged, "cents \(r.cents)")
        let locks = try XCTUnwrap(r.document["pitch_locks"] as? [[String: Any]])
        XCTAssertEqual(locks.first?["note"] as? String, "G6")
        let solvedLen = ((r.document["parameters"] as? [String: Any])?["len"] as? [String: Any])?["value"]
        XCTAssertNotNil(solvedLen)
        let check = try SoundFFI.check(doc: JSONSerialization.data(withJSONObject: r.document))
        XCTAssertEqual(check.first?.holds, true)
    }

    func testHitTestFindsTheTopFace() throws {
        let pm = try SoundFFI.modes(doc: try barDoc(), part: 0)
        // Ray straight down onto the bar's top at x = 10.
        let hit = try XCTUnwrap(soundHit(pm, origin: SIMD3(10, 12, 50), dir: SIMD3(0, 0, -1)))
        let z = pm.positions[3 * hit.vertex + 2]
        XCTAssertEqual(z, 6, accuracy: 0.01)
        XCTAssertNil(soundHit(pm, origin: SIMD3(10, 80, 50), dir: SIMD3(0, 0, -1)), "miss beside the bar")
    }
}

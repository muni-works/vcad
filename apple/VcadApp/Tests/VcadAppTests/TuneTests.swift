import XCTest
@testable import VcadApp

/// Pitch detection on synthetic strikes + the receipt flip through the kernel.
final class TuneTests: XCTestCase {
    let fs = 48_000.0

    /// A struck free-free bar: decaying partials at f, ratio·f, 2nd overtone,
    /// plus white noise at `noise` RMS.
    func bar(_ f: Double, ratio: Double = 4.0, overtoneGain: Double = 0.6,
             seconds: Double = 0.17, noise: Float = 0, seed: UInt64 = 1) -> [Float] {
        var rng = seed
        func uniform() -> Float {
            rng = rng &* 6364136223846793005 &+ 1442695040888963407
            return Float(rng >> 40) / Float(1 << 24) * 2 - 1
        }
        let n = Int(seconds * fs)
        return (0..<n).map { i in
            let t = Double(i) / fs
            let s = exp(-t * 3) * sin(2 * .pi * f * t)
                + overtoneGain * exp(-t * 9) * sin(2 * .pi * f * ratio * t + 0.3)
                + 0.2 * exp(-t * 15) * sin(2 * .pi * f * 9.9 * t)
            return Float(s * 0.5) + noise * uniform() * 1.732
        }
    }

    func testPureTonesWithinATenthOfACent() throws {
        for f in [523.25, 1046.5, 1318.51, 1567.98, 2093.0, 3520.0] {
            let x = (0..<8192).map { Float(sin(2 * .pi * f * Double($0) / fs)) }
            let e = try XCTUnwrap(PitchDetector.fundamental(x, sampleRate: fs))
            XCTAssertEqual(PitchDetector.cents(e.hz, f), 0, accuracy: 0.1, "\(f) Hz → \(e.hz)")
        }
    }

    func testStruckBarPicksFundamentalNotLouderOvertone() throws {
        // Overtone two octaves up rings LOUDER than the fundamental here.
        for f in [1046.5, 1174.66, 1975.53] {
            let e = try XCTUnwrap(PitchDetector.fundamental(bar(f, overtoneGain: 1.6, noise: 0.01), sampleRate: fs))
            XCTAssertEqual(PitchDetector.cents(e.hz, f), 0, accuracy: 0.5, "\(f) Hz → \(e.hz)")
        }
        // Plain (untuned) bar: overtone at 2.756·f.
        let e = try XCTUnwrap(PitchDetector.fundamental(bar(880, ratio: 2.756, noise: 0.01), sampleRate: fs))
        XCTAssertEqual(PitchDetector.cents(e.hz, 880), 0, accuracy: 0.5)
    }

    func testNoiseAloneIsRejected() {
        var rng: UInt64 = 7
        let x: [Float] = (0..<8192).map { _ in
            rng = rng &* 6364136223846793005 &+ 1442695040888963407
            return Float(rng >> 40) / Float(1 << 24) - 0.5
        }
        XCTAssertNil(PitchDetector.fundamental(x, sampleRate: fs))
    }

    func testNearestNote() {
        let (n, c) = PitchDetector.nearestNote(1046.5)
        XCTAssertEqual(n, "C6")
        XCTAssertEqual(c, 0, accuracy: 0.05)
        XCTAssertEqual(PitchDetector.nearestNote(1479.98).name, "F#6")
    }

    func testStrikeDetectorFindsEachStrikeOnce() {
        let det = StrikeDetector(sampleRate: fs)
        var stream: [Float] = []
        let silence = { (s: Double) in (0..<Int(s * self.fs)).map { _ in Float(0.0005) * Float.random(in: -1...1) } }
        stream += silence(0.3)
        stream += bar(1318.51, seconds: 0.8, noise: 0.0005)
        stream += silence(0.2)
        stream += bar(2093.0, seconds: 0.8, noise: 0.0005, seed: 3)
        stream += silence(0.2)
        var hits: [PitchEstimate] = []
        // Feed in audio-tap sized chunks.
        var i = 0
        while i < stream.count {
            let j = min(i + 2048, stream.count)
            stream[i..<j].withUnsafeBufferPointer { hits += det.process($0) }
            i = j
        }
        XCTAssertEqual(hits.count, 2, "\(hits.map(\.hz))")
        guard hits.count == 2 else { return }
        XCTAssertEqual(PitchDetector.cents(hits[0].hz, 1318.51), 0, accuracy: 1)
        XCTAssertEqual(PitchDetector.cents(hits[1].hz, 2093.0), 0, accuracy: 1)
    }

    /// A document with instrument metadata only (no geometry needed for the
    /// receipt ops).
    func instrumentDoc() throws -> Data {
        func bar(_ note: String, _ hz: Double) -> [String: Any] {
            ["note": note, "part": "bar_\(note.lowercased())", "target_hz": hz, "predicted_hz": hz * 1.00001,
             "tolerance_cents": 5.0, "length_mm": 130.0, "width_mm": 25.0, "thickness_mm": 6.0,
             "undercut_depth_mm": 2.7, "undercut_span": 0.2, "hole_dia_mm": 4.0, "nodes_mm": [29.0, 101.0]]
        }
        let doc: [String: Any] = [
            "version": "0.1", "nodes": [:], "materials": [:], "part_materials": [:], "roots": [],
            "instrument": ["kind": "glockenspiel", "tuning": "12-TET, A4 = 440 Hz", "material": "aluminium_6061",
                           "bars": [bar("C6", 1046.5), bar("D6", 1174.66)]],
        ]
        return try JSONSerialization.data(withJSONObject: doc)
    }

    func testReceiptFlipsPredictedToMeasured() throws {
        let doc = try instrumentDoc()
        let r0 = try InstrumentFFI.receipt(doc: doc)
        XCTAssertEqual(r0.verdict, "provisional")
        XCTAssertEqual(r0.bars.map(\.state), ["predicted", "predicted"])

        // 1049 Hz → nearest bar C6, +4.3 cents → pass.
        let r1 = try InstrumentFFI.record(doc: doc, hz: 1049.1)
        XCTAssertEqual(r1.note, "C6")
        XCTAssertEqual(r1.report.bars[0].state, "measured_pass")
        XCTAssertEqual(r1.report.bars[0].centsError ?? 99, 4.3, accuracy: 0.1)
        XCTAssertEqual(r1.report.bars[1].state, "predicted")
        XCTAssertEqual(r1.report.verdict, "provisional")

        // The patched document carries the measurement; D6 20 cents flat fails.
        let doc2 = try JSONSerialization.data(withJSONObject: r1.document)
        let r2 = try InstrumentFFI.record(doc: doc2, hz: 1174.66 * pow(2, -20.0 / 1200))
        XCTAssertEqual(r2.note, "D6")
        XCTAssertEqual(r2.report.bars[1].state, "measured_fail")
        XCTAssertEqual(r2.report.verdict, "fail")
    }

    func testStrikeFarFromEveryBarIsRefused() throws {
        XCTAssertThrowsError(try InstrumentFFI.record(doc: try instrumentDoc(), hz: 1500))
    }
}

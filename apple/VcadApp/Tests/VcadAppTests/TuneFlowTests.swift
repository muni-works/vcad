import SwiftUI
import XCTest
@testable import VcadApp

/// The hero demo's measurement loop, end to end in-process: the designed
/// glockenspiel (docs/launch/hero-demo/glockenspiel.vcad, written by the
/// design_instrument MCP tool) is opened, synthetic strike audio for every bar
/// goes through the live StrikeDetector → TuneStore.heard → kernel record
/// path, and every receipt flips from predicted to measured.
///
/// `VCAD_TUNE_SNAPSHOT=<dir>` also renders the panel before/after to PNG.
@MainActor
final class TuneFlowTests: XCTestCase {
    static let docURL = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .deletingLastPathComponent().deletingLastPathComponent()
        .appendingPathComponent("docs/launch/hero-demo/glockenspiel.vcad")

    func strike(_ f: Double, fs: Double) -> [Float] {
        let quiet = (0..<Int(0.25 * fs)).map { _ in Float.random(in: -0.0005...0.0005) }
        let ring = (0..<Int(0.6 * fs)).map { i -> Float in
            let t = Double(i) / fs
            return Float(0.4 * exp(-t * 3) * sin(2 * .pi * f * t) + 0.3 * exp(-t * 9) * sin(2 * .pi * 4 * f * t))
                + Float.random(in: -0.0005...0.0005)
        }
        return quiet + ring
    }

    func snapshot(_ m: EditorModel, _ name: String) throws {
        guard let dir = ProcessInfo.processInfo.environment["VCAD_TUNE_SNAPSHOT"] else { return }
        let view = TunePanel(model: m).padding(24).background(Color(white: 0.93)).environment(\.colorScheme, .light)
        let r = ImageRenderer(content: view)
        r.scale = 2
        guard let img = r.nsImage, let tiff = img.tiffRepresentation,
              let png = NSBitmapImageRep(data: tiff)?.representation(using: .png, properties: [:]) else {
            XCTFail("render failed"); return
        }
        try FileManager.default.createDirectory(atPath: dir, withIntermediateDirectories: true)
        try png.write(to: URL(fileURLWithPath: dir).appendingPathComponent("\(name).png"))
    }

    func testStrikesFlipEveryBarToMeasured() throws {
        let data = try Data(contentsOf: Self.docURL)
        let doc = try XCTUnwrap(try JSONSerialization.jsonObject(with: data) as? [String: Any])
        let m = EditorModel()
        _ = try AgentEdits.edit(m, name: "Open glockenspiel") { try $0.loadIR(doc) }
        XCTAssertTrue(TuneStore.hasInstrument(m), "instrument metadata survives the engine round trip")

        let store = TuneStore.shared
        store.refresh(m)
        let before = try XCTUnwrap(store.report)
        XCTAssertEqual(before.bars.count, 8)
        XCTAssertEqual(before.verdict, "provisional")
        XCTAssertTrue(before.bars.allSatisfy { $0.state == "predicted" })
        try snapshot(m, "tune-predicted")

        // Strike each bar slightly off target, as a real bar would ring.
        let fs = 48_000.0
        let offsets: [Double] = [1.8, -2.4, 0.6, 3.1, -1.2, 2.2, -0.4, 4.0]
        for (bar, cents) in zip(before.bars, offsets) {
            let det = StrikeDetector(sampleRate: fs)
            let audio = strike(bar.targetHz * pow(2, cents / 1200), fs: fs)
            var hits: [PitchEstimate] = []
            audio.withUnsafeBufferPointer { hits = det.process($0) }
            XCTAssertEqual(hits.count, 1, "\(bar.note)")
            hits.forEach { store.heard($0) }
            XCTAssertEqual(store.last?.bar, bar.note)
        }
        let after = try XCTUnwrap(store.report)
        XCTAssertEqual(after.verdict, "pass", "\(after.bars.map { ($0.note, $0.state, $0.centsError ?? .nan) })")
        for (b, c) in zip(after.bars, offsets) {
            XCTAssertEqual(b.state, "measured_pass")
            XCTAssertEqual(b.centsError ?? .nan, c, accuracy: 0.5, b.note)
        }
        XCTAssertEqual(m.undoStack.last?.name.hasPrefix("Tune"), true, "\(m.undoStack.map(\.name))")
        try snapshot(m, "tune-measured")

        // One bar filed too far: strike C7 22 cents flat → Fail.
        let c7 = after.bars[7]
        store.heard(PitchEstimate(hz: c7.targetHz * pow(2, -22.0 / 1200), db: 0, snrDb: 40))
        XCTAssertEqual(store.report?.verdict, "fail")
        try snapshot(m, "tune-one-flat")
    }
}

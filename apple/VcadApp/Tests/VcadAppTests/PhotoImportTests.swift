import XCTest
@testable import VcadApp

/// Import from Photos (experimental): the FFI pre-flight on fixture photos,
/// the failure messages, the result decoding and the document it opens.
///
/// Fixtures (`Fixtures/recon/`, written by vcad-ffi's ignored
/// `recon::tests::write_fixture_photos`): a white page with tag36h11 id 0
/// drawn square-on, with and without an EXIF focal length, and a page with
/// no tag. The full reconstruction takes minutes and runs on a build box
/// (`recon::tests::reconstruct_photos_end_to_end`), not here.
@MainActor
final class PhotoImportTests: XCTestCase {
    private func fixture(_ name: String) -> URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent()
            .appendingPathComponent("Fixtures/recon/\(name)")
    }

    func testCheckFindsTheTag() throws {
        let cals = try ReconFFI.check([fixture("tag-exif.jpg")])
        XCTAssertEqual(cals.count, 1)
        XCTAssertEqual((cals[0]["tag_id"] as? NSNumber)?.intValue, 0)
        XCTAssertGreaterThan((cals[0]["focal_exif_px"] as? NSNumber)?.doubleValue ?? 0, 100)
    }

    func testNoTagSaysSo() {
        XCTAssertThrowsError(try ReconFFI.check([fixture("no-tag-exif.jpg")])) { e in
            let m = (e as? ReconFFI.Failure)?.message ?? ""
            XCTAssertTrue(m.contains("No AprilTag found"), m)
            XCTAssertTrue(m.contains("tag36h11"), m)
        }
    }

    func testNoExifFocalLengthSaysSo() {
        XCTAssertThrowsError(try ReconFFI.check([fixture("tag-no-exif.jpg")])) { e in
            let m = (e as? ReconFFI.Failure)?.message ?? ""
            XCTAssertTrue(m.contains("no EXIF focal length"), m)
        }
    }

    func testOnePhotoMissingTheTagFailsTheSet() {
        XCTAssertThrowsError(try ReconFFI.check([fixture("tag-exif.jpg"), fixture("no-tag-exif.jpg")]))
    }

    func testUnreadableFile() {
        XCTAssertThrowsError(try ReconFFI.check([URL(fileURLWithPath: "/nonexistent/part.jpg")])) { e in
            XCTAssertTrue(((e as? ReconFFI.Failure)?.message ?? "").contains("Couldn't read"))
        }
    }

    /// A document the real pipeline produced (vcad_recon_photos on irys's
    /// synthetic render 3001, one view; truth `base 70.4 59.3 17.1`, `hole
    /// 4.8 -10.8 3.4`) opens editable with its dimensions as parameters.
    func testPipelineOutputOpensEditable() throws {
        let src = fixture("synth-3001-recon.vcad")
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
        try FileManager.default.copyItem(at: src, to: url)
        defer { try? FileManager.default.removeItem(at: url) }
        let m = EditorModel()
        m.openDocument(url)
        XCTAssertTrue(m.canEditDocument, m.engineError ?? "no engine")
        XCTAssertEqual(Set(m.docParameters.map(\.name)), ["base_w", "base_d", "base_h", "f0_x", "f0_y", "f0_r"])
        let w = try XCTUnwrap(m.docParameters.first { $0.name == "base_w" }?.value)
        XCTAssertEqual(w, 70.4, accuracy: 1.0)
        m.editParameter("base_w", value: 80, snapshot: true)
        XCTAssertEqual(m.docParameters.first { $0.name == "base_w" }?.value, 80)
        XCTAssertNil(m.engineError)
    }

    func testOnlyPhotosRouteToTheImporter() {
        XCTAssertTrue(PhotoImportFiles.isPhoto(URL(fileURLWithPath: "/a/IMG_0579.HEIC")))
        XCTAssertTrue(PhotoImportFiles.isPhoto(URL(fileURLWithPath: "/a/part.jpeg")))
        XCTAssertFalse(PhotoImportFiles.isPhoto(URL(fileURLWithPath: "/a/part.vcad")))
        XCTAssertFalse(PhotoImportFiles.isPhoto(URL(fileURLWithPath: "/a/part.step")))
    }

    func testControllerRejectsNonPhotos() {
        let c = PhotoImportController()
        c.start([URL(fileURLWithPath: "/a/part.step")])
        guard case .failed(let m) = c.phase else { return XCTFail("\(c.phase)") }
        XCTAssertTrue(m.contains("aren't photos"))
    }

    /// Fewer than three photos is refused up front, before any decoding.
    func testControllerRefusesFewerThanThreePhotos() {
        let c = PhotoImportController()
        c.start([fixture("no-tag-exif.jpg")])
        guard case .failed(let m) = c.phase else { return XCTFail("\(c.phase)") }
        XCTAssertTrue(m.contains("at least 3 photos"), m)
    }

    /// The controller's pre-flight runs off the main thread and lands a
    /// specific failure on the main actor.
    func testControllerReportsAMissingTag() async throws {
        let c = PhotoImportController()
        c.start(Array(repeating: fixture("no-tag-exif.jpg"), count: 3))
        XCTAssertEqual(c.phase, .checking)
        for _ in 0..<200 where c.phase == .checking { try await Task.sleep(nanoseconds: 50_000_000) }
        guard case .failed(let m) = c.phase else { return XCTFail("\(c.phase)") }
        XCTAssertTrue(m.contains("No AprilTag found"), m)
    }

    /// Cancel discards whatever the background call returns later.
    func testCancelDiscardsTheLateResult() async throws {
        let c = PhotoImportController()
        c.start(Array(repeating: fixture("no-tag-exif.jpg"), count: 3))
        c.cancel()
        try await Task.sleep(nanoseconds: 1_500_000_000)
        XCTAssertEqual(c.phase, .idle)
    }

    /// A reconstruction result (shape of `vcad_recon_photos`) decodes, and its
    /// document opens as an editable document whose parameters are the
    /// fitted dimensions.
    func testResultOpensAsAParametricDocument() throws {
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": [
                "1": ["id": 1, "name": "base block", "op": ["type": "Cube", "size": ["x": 60, "y": 40, "z": 8]]],
                "2": ["id": 2, "name": "Part from photos",
                      "op": ["type": "Translate", "child": 1, "offset": ["x": -30, "y": -20, "z": 0]]],
            ],
            "materials": ["pla": ["name": "PLA (from photos)", "color": [0.8, 0.2, 0.2], "metallic": 0, "roughness": 0.4]],
            "part_materials": [:],
            "roots": [["root": 2, "material": "pla"]],
            "parameters": [
                "base_w": ["value": 60, "unit": "mm", "description": "From photos (experimental): ±0.12 mm fit σ, likely optimistic"],
                "base_d": ["value": 40, "unit": "mm"],
                "base_h": ["value": 8, "unit": "mm"],
            ],
            "bindings": ["1:size.x": "base_w", "1:size.y": "base_d", "1:size.z": "base_h",
                         "2:offset.x": "-base_w / 2", "2:offset.y": "-base_d / 2"],
        ]
        let json: [String: Any] = [
            "document": doc, "program": "base 60 40 8",
            "dimensions": [
                ["name": "base.w", "parameter": "base_w", "value": 60.0, "sigma": 0.12, "unit": "mm"],
                ["name": "base.d", "parameter": "base_d", "value": 40.0, "sigma": 0.8, "unit": "mm"],
                ["name": "base.h", "parameter": "base_h", "value": 8.0, "sigma": NSNull(), "unit": "mm"],
            ],
            "weak": [], "low_confidence": true,
            "warnings": ["base_d is uncertain: ±0.80 mm"], "photos": [], "seconds": 1.5,
        ]
        let r = try ReconResult(json: JSONSerialization.data(withJSONObject: json))
        XCTAssertEqual(r.dimensions.map(\.parameter), ["base_w", "base_d", "base_h"])
        XCTAssertEqual(r.dimensions[0].sigmaText, "±0.12 mm")
        XCTAssertFalse(r.dimensions[0].uncertain)
        XCTAssertTrue(r.dimensions[1].uncertain)
        XCTAssertEqual(r.dimensions[2].sigmaText, "—")
        XCTAssertTrue(r.lowConfidence)

        let url = try XCTUnwrap(PhotoImportController.writeDocument(r, named: "photo-import-test-\(UUID().uuidString)"))
        defer { try? FileManager.default.removeItem(at: url) }
        let model = EditorModel()
        model.openDocument(url)
        XCTAssertTrue(model.canEditDocument, model.engineError ?? "no engine")
        XCTAssertEqual(Set(model.docParameters.map(\.name)), ["base_w", "base_d", "base_h"])
        XCTAssertEqual(model.docParameters.first { $0.name == "base_w" }?.description?.contains("±0.12"), true)
        // A dimension is editable like any parameter.
        model.editParameter("base_w", value: 70, snapshot: true)
        XCTAssertEqual(model.docParameters.first { $0.name == "base_w" }?.value, 70)
        XCTAssertNil(model.engineError)
    }
}

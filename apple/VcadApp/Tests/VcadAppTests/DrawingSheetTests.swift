import XCTest
@testable import VcadApp

/// The Drawing workspace's sheet model: hit testing, snapping, persisted
/// annotation edits (no kernel needed — the result JSON is hand-built in the
/// shape `vcad_drawing_sheet_json` returns).
final class DrawingSheetTests: XCTestCase {
    private func result() throws -> DrawingSheetResult {
        let json = """
        {"width":297,"height":210,
         "sheet":{"lines":[
            {"class":"visible","start":{"x":60,"y":60},"end":{"x":100,"y":60}},
            {"class":"center","start":{"x":75,"y":70},"end":{"x":85,"y":70}}],
           "arcs":[],"polygons":[],"texts":[]},
         "main":{"name":"front","sheet_center":[80,70],"view_center":[20,5],"scale":1,"scale_note":"1:1",
                 "bounds":{"min_x":0,"min_y":0,"max_x":40,"max_y":10}},
         "views":[
           {"name":"front","sheet_center":[80,70],"view_center":[20,5],"scale":1,"scale_note":"1:1",
            "bounds":{"min_x":0,"min_y":0,"max_x":40,"max_y":10}},
           {"name":"top","sheet_center":[80,120],"view_center":[20,10],"scale":1,"scale_note":"1:1",
            "bounds":{"min_x":0,"min_y":0,"max_x":40,"max_y":20}}],
         "dimensions":[
           {"id":"auto:front:width","view":"front","auto":true,"min":[60,48],"max":[100,56],"offset":-10,"kind":"horizontal"},
           {"id":"u1","view":"front","auto":false,"min":[70,50],"max":[80,54],"offset":-8,"kind":"horizontal"}],
         "visible_edges":12,"hidden_edges":4,"dxf_entities":16,"sections":0,"circles":1,"sheet_size":"A4"}
        """
        return try JSONDecoder().decode(DrawingSheetResult.self, from: Data(json.utf8))
    }

    func testViewHitTestAndMapping() throws {
        let r = try result()
        XCTAssertEqual(r.placements.count, 2)
        XCTAssertEqual(r.view(at: DrawingPoint(x: 80, y: 121))?.name, "top")
        XCTAssertEqual(r.view(at: DrawingPoint(x: 80, y: 68))?.name, "front")
        XCTAssertNil(r.view(at: DrawingPoint(x: 280, y: 20)))
        let v = r.main.toView(DrawingPoint(x: 60, y: 65))
        XCTAssertEqual(v.x, 0, accuracy: 1e-9)
        XCTAssertEqual(v.y, 0, accuracy: 1e-9)
    }

    func testHandleHitPrefersSmallestBox() throws {
        let r = try result()
        XCTAssertEqual(r.handle(at: DrawingPoint(x: 75, y: 52))?.id, "u1")
        XCTAssertEqual(r.handle(at: DrawingPoint(x: 95, y: 52))?.id, "auto:front:width")
        XCTAssertNil(r.handle(at: DrawingPoint(x: 200, y: 200)))
    }

    func testSnapToCornersAndHoleCenters() throws {
        let r = try result()
        let c = r.snap(DrawingPoint(x: 61, y: 61))
        XCTAssertEqual(c, DrawingPoint(x: 60, y: 60))
        let h = r.snap(DrawingPoint(x: 80.5, y: 71))
        XCTAssertEqual(h, DrawingPoint(x: 80, y: 70), "center-mark midpoint = hole center")
        let free = DrawingPoint(x: 150, y: 150)
        XCTAssertEqual(r.snap(free), free)
    }

    @MainActor
    func testDimensionEditsPersistInSettings() throws {
        let store = DrawingStore()
        store.addLinear(on: "front", from: DrawingPoint(x: 0, y: 0), to: DrawingPoint(x: 40, y: 0.2), scale: 2, centerLine: false)
        store.addLinear(on: "front", from: DrawingPoint(x: 0, y: 0), to: DrawingPoint(x: 0.1, y: 10), scale: 2, centerLine: false)
        store.addLinear(on: "front", from: DrawingPoint(x: 0, y: 0), to: DrawingPoint(x: 10, y: 10), scale: 2, centerLine: false)
        store.addLinear(on: "top", from: DrawingPoint(x: 20, y: -5), to: DrawingPoint(x: 20, y: 25), scale: 2, centerLine: true)
        let dims = store.documentSettings.dimensions ?? []
        XCTAssertEqual(dims.map(\.kind), ["horizontal", "vertical", "aligned", "centerline"])
        XCTAssertEqual(dims[0].offset, -4, accuracy: 1e-9, "8 sheet mm below at 2:1")
        XCTAssertEqual(store.selection, dims[3].id)

        // Deleting an automatic dimension hides it; a user one is removed.
        let r = try result()
        store.delete(r.dimensions![0])
        XCTAssertEqual(store.documentSettings.hiddenDimensions, ["auto:front:width"])
        let user = DrawingSheetResult.Handle(id: dims[0].id, view: "front", auto: false, min: [0, 0], max: [1, 1],
                                             offset: dims[0].offset, kind: "horizontal")
        store.delete(user)
        XCTAssertEqual(store.documentSettings.dimensions?.count, 3)
        store.restoreAutomaticDimensions()
        XCTAssertNil(store.documentSettings.hiddenDimensions)
    }

    @MainActor
    func testDetailsAndGdtPersist() {
        let store = DrawingStore()
        store.addGdt(.flatness, at: DrawingPoint(x: 1, y: 2), on: "top")
        XCTAssertEqual(store.documentSettings.gdt?.first?.view, "top")
        XCTAssertEqual(store.gdt.count, 1)
        store.gdt = []
        XCTAssertEqual(store.documentSettings.gdt?.count, 0)
    }

    func testAnnotationsEncodeWithIRKeys() throws {
        var s = DrawingSettingsIR()
        s.sheetSize = "a3"
        s.hiddenDimensions = ["auto:front:height"]
        s.dimensionOffsets = ["auto:front:width": -12]
        s.gdt = [DrawingGdt(id: "g1", view: "front", kind: .datum, tolerance: 0, datum: "A", at: [0, 0])]
        s.centerMarks = false
        let json = try JSONSerialization.jsonObject(with: JSONEncoder().encode(s.annotations)) as! [String: Any]
        XCTAssertEqual(json["sheetSize"] as? String, "a3")
        XCTAssertEqual(json["hiddenDimensions"] as? [String], ["auto:front:height"])
        XCTAssertNotNil(json["dimensionOffsets"])
        XCTAssertEqual(json["centerMarks"] as? Bool, false)
        let g = (json["gdt"] as? [[String: Any]])?.first
        XCTAssertEqual(g?["id"] as? String, "g1")
        XCTAssertEqual(g?["kind"] as? String, "datum")
    }

    func testStandardSheetIsTheDefaultView() {
        XCTAssertEqual(DrawingView.allCases.first, .standard)
        XCTAssertEqual(DrawingView.standard.label, "Standard Sheet")
        XCTAssertEqual(DrawingExportFormat.dxf.ffiCode, 2)
    }
}

import AppKit
import XCTest
import CVcadFFI
import simd
@testable import VcadApp

/// Mouse report (benchmark part 4 retry): with Dimension armed, a click near a
/// rectangle corner fixed the point, vertical edges were hard to pick, and a
/// dimension field kept the keyboard after Return.
@MainActor
final class SketchDimensionPickTests: XCTestCase {
    private func rectangle(scale: Double?) throws -> SketchSession {
        let s = try XCTUnwrap(SketchSession(frame: .principal(.xy)))
        s.tool = .rectangle
        s.tap([0, 0]); s.tap([20, 10])
        s.pickScale = scale
        s.tool = .dimension
        return s
    }

    private func lengths(_ s: SketchSession) -> [Double] {
        s.constraints.filter { $0.kind == .length }.map(\.value).sorted()
    }

    /// Every edge, horizontal and vertical, picked off-centre (nearer a corner
    /// than the middle) gets a Length — at the fixed fallback radius and at
    /// zoomed-in and zoomed-out screen scales.
    func testDimensioningEachRectangleEdgeAddsLengths() throws {
        for scale in [nil, 0.02, 0.1, 0.25] as [Double?] {
            let s = try rectangle(scale: scale)
            // bottom, right, top, left — each 2.5 mm from a corner, 0.1 mm off the ink
            for p: SIMD2<Double> in [[17.5, 0.1], [20.1, 2.5], [2.5, 9.9], [-0.1, 7.5]] { s.tap(p) }
            XCTAssertEqual(lengths(s), [10, 10, 20, 20], "scale \(String(describing: scale))")
            XCTAssertFalse(s.constraints.contains { $0.kind == .fixed })
        }
    }

    /// Zoomed out, a click a few screen points off a vertical edge still lands it.
    func testVerticalEdgePickedWithinAFewScreenPoints() throws {
        let s = try rectangle(scale: 0.25)       // 8 pt reach = 2 mm
        s.tap([21.5, 5])                         // 6 pt right of the right edge
        XCTAssertEqual(lengths(s), [10])
        s.tap([-1.5, 3])
        XCTAssertEqual(lengths(s), [10, 10])
    }

    /// Clicking a point never fixes it: two points make a distance.
    func testDimensionOnAPointNeverFixes() throws {
        let s = try rectangle(scale: 0.05)
        let before = s.constraints.count
        s.tap([0.05, 0.05])                       // squarely on a corner
        XCTAssertEqual(s.constraints.count, before, "the first point only arms the distance")
        XCTAssertNotNil(s.dimensionAnchor)
        s.tap([20, 10])
        let d = try XCTUnwrap(s.constraints.last)
        XCTAssertEqual(d.kind, .distance)
        XCTAssertEqual(d.value, (20.0 * 20 + 10 * 10).squareRoot(), accuracy: 1e-6)
        // A point then a line: the line gets its length, nothing is fixed.
        s.tap([20, 0]); s.tap([10, 0])
        XCTAssertEqual(s.constraints.last?.kind, .length)
        XCTAssertFalse(s.constraints.contains { $0.kind == .fixed })
        XCTAssertNil(s.dimensionAnchor)
    }

    /// A second dimension on the same edge is refused rather than over-constraining.
    func testSecondDimensionOnAnEdgeIsRefused() throws {
        let s = try rectangle(scale: nil)
        s.tap([10, 0]); s.tap([10, 0])
        XCTAssertEqual(lengths(s), [20])
        XCTAssertNotNil(s.lastError)
        XCTAssertFalse(s.isOverConstrained)
    }

    /// Over-constraint is recoverable: the culprits are named, and deleting
    /// a row clears it.
    func testOverConstraintNamesCulpritsAndDeletingARowRecovers() throws {
        let s = try rectangle(scale: nil)
        s.tap([10, 0]); s.tap([0, 5])             // width + height
        s.tool = .select
        s.tap([0, 0]); XCTAssertTrue(s.constrainSelection(.fixed))
        XCTAssertTrue(s.isFullyConstrained, s.stateLabel)
        s.tap([20, 10]); XCTAssertTrue(s.constrainSelection(.fixed))
        XCTAssertTrue(s.isOverConstrained, s.stateLabel)
        let last = try XCTUnwrap(s.constraints.last)
        XCTAssertTrue(s.conflicting.contains(last.index))
        XCTAssertLessThan(s.conflicting.count, s.constraints.count, "not every row is blamed")
        XCTAssertFalse(s.conflicting.contains { i in s.constraints.first { $0.index == i }?.kind == .horizontal })
        s.deleteConstraint(last)
        XCTAssertFalse(s.isOverConstrained)
        XCTAssertTrue(s.isFullyConstrained, s.stateLabel)
        XCTAssertTrue(s.conflicting.isEmpty)
        XCTAssertTrue(s.undo())
        XCTAssertTrue(s.isOverConstrained, "the delete is one undo step")
    }

    /// Editor level: a viewport click's ray pair sets the pick scale and a
    /// Dimension click on each edge of a rectangle adds a Length.
    func testEditorRayTapDimensionsEdges() throws {
        let doc: [String: Any] = [
            "version": "0.1",
            "nodes": ["1": ["id": 1, "name": "Block", "op": ["type": "Cube", "size": ["x": 40, "y": 40, "z": 10]]]],
            "materials": [:], "part_materials": [:],
            "roots": [["root": 1, "material": "aluminum"]],
        ]
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
        try JSONSerialization.data(withJSONObject: doc).write(to: url)
        defer { try? FileManager.default.removeItem(at: url) }
        let m = EditorModel()
        m.openDocument(url)
        m.enterSketch()
        let sketch = try XCTUnwrap(m.sketch)
        let f = sketch.frame
        func ray(_ p: SIMD2<Double>) -> (o: SIMD3<Float>, d: SIMD3<Float>) {
            (SIMD3<Float>(f.world(p) + f.normal * 100), SIMD3<Float>(-f.normal))
        }
        func click(_ p: SIMD2<Double>) { m.sketchTap(ray: ray(p), neighbour: ray(p + [0.1, 0])) }
        m.setSketchTool(.rectangle)
        click([0, 0]); click([20, 10])
        XCTAssertEqual(sketch.entities.filter { $0.kind == .line }.count, 4)
        m.setSketchTool(.dimension)
        for p: SIMD2<Double> in [[15, 0.3], [20.3, 2], [5, 10.3], [-0.3, 8]] { click(p) }
        XCTAssertEqual(sketch.pickScale ?? 0, 0.1, accuracy: 1e-4)
        XCTAssertEqual(lengths(sketch), [10, 10, 20, 20])
        XCTAssertFalse(sketch.constraints.contains { $0.kind == .fixed })
    }

    /// Return in a constraint-panel field hands the keyboard back to the canvas.
    func testReturnInPanelFieldResignsFocus() throws {
        let window = KeyableWindow(contentRect: NSRect(x: 0, y: 0, width: 300, height: 100),
                                   styleMask: [.titled], backing: .buffered, defer: false)
        let field = NSTextField(frame: NSRect(x: 10, y: 10, width: 100, height: 22))
        field.stringValue = "20"
        window.contentView?.addSubview(field)
        XCTAssertTrue(window.makeFirstResponder(field))
        XCTAssertNotNil(field.currentEditor())
        XCTAssertTrue(SketchConstraintPanel.returnFocusToCanvas(window))
        XCTAssertNil(field.currentEditor(), "the field let go of the field editor")
        XCTAssertFalse(window.firstResponder is NSText)
    }
}

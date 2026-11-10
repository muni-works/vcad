import XCTest
@testable import VcadApp

/// Section/detail letters and datum letters share one alphabet without
/// I, O, Q (ASME Y14.3 / Y14.5) — a section must not take "A" while datum A
/// is on the sheet, and vice versa.
final class DrawingLettersTests: XCTestCase {
    private func section(_ l: String) -> DrawingSectionLine {
        DrawingSectionLine(id: "s\(l)", view: "front", label: l, points: [[0, 0], [0, 1]])
    }

    private func datum(_ l: String?, _ kind: GdtKind = .datum) -> DrawingGdt {
        DrawingGdt(kind: kind, tolerance: 0.05, datum: l, at: [0, 0])
    }

    func testAlphabetSkipsIOQ() {
        XCTAssertFalse(DrawingLetters.alphabet.contains("I"))
        XCTAssertFalse(DrawingLetters.alphabet.contains("O"))
        XCTAssertFalse(DrawingLetters.alphabet.contains("Q"))
        XCTAssertEqual(DrawingLetters.alphabet.count, 23)
        XCTAssertEqual(DrawingLetters.next(excluding: ["A", "B", "C", "D", "E", "F", "G", "H"]), "J")
        XCTAssertEqual(DrawingLetters.next(excluding: Set("ABCDEFGHJKLMN".map(String.init))), "P")
    }

    func testSectionSkipsDatumLetters() {
        let used = DrawingLetters.used(sections: [], details: [], gdt: [datum("A")])
        XCTAssertEqual(DrawingLetters.next(excluding: used), "B")
        // A datum referenced only by a feature control frame counts too.
        let ref = DrawingLetters.used(sections: [], details: [], gdt: [datum("A", .perpendicularity)])
        XCTAssertEqual(DrawingLetters.next(excluding: ref), "B")
    }

    func testDatumSkipsViewLetters() {
        let used = DrawingLetters.used(sections: [section("A")], details: [], gdt: [])
        XCTAssertEqual(DrawingLetters.resolve("A", excluding: used), "B")
        XCTAssertEqual(DrawingLetters.resolve("C", excluding: used), "C")
        XCTAssertEqual(DrawingLetters.resolve("I", excluding: used), "B", "I is never a datum letter")
        XCTAssertEqual(DrawingLetters.resolve(nil, excluding: used), "B")
    }

    func testDoublesAfterAlphabet() {
        let all = Set(DrawingLetters.alphabet)
        XCTAssertEqual(DrawingLetters.next(excluding: all), "AA")
    }

    @MainActor
    func testStoreAddsSectionAndDatumWithoutCollision() {
        let store = DrawingStore()
        store.gdt = [datum("A")]
        XCTAssertTrue(store.usedLetters.contains("A"))
        XCTAssertEqual(DrawingLetters.next(excluding: store.usedLetters), "B")
        // Placing a second datum "A" takes the next free letter instead.
        store.gdtDatum = "A"
        store.addGdt(.datum, at: DrawingPoint(x: 1, y: 1))
        XCTAssertEqual(store.gdt.last?.datum, "B")
        XCTAssertEqual(store.gdtDatum, "B")
        // A feature control frame keeps the datum it references.
        store.gdtDatum = "A"
        store.addGdt(.perpendicularity, at: DrawingPoint(x: 2, y: 2))
        XCTAssertEqual(store.gdt.last?.datum, "A")
    }

    func testWorkspacePickerOpensUpward() {
        // The dock is at the bottom of the window; a drop-down would open
        // off-screen.
        XCTAssertEqual(GlassDock.workspacePickerEdge, .top)
        XCTAssertEqual(DockSpace.allCases.count, 5)
        XCTAssertEqual(DockSpace.allCases.map(\.shortcutHint), ["⌃⌘1", "⌃⌘2", "⌃⌘3", "⌃⌘4", "⌃⌘5"])
    }
}

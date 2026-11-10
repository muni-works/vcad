import AppKit
import XCTest
@testable import VcadApp

/// Clicking a toolbar number field must take the window's field editor away
/// from a dimension field that had it — keystrokes went to the old field.
@MainActor
final class ScrubFieldFocusTests: XCTestCase {
    func testFocusMovesFieldEditorFromThePreviousField() throws {
        let window = KeyableWindow(contentRect: NSRect(x: 0, y: 0, width: 300, height: 100),
                                   styleMask: [.titled], backing: .buffered, defer: false)
        let dimension = NSTextField(frame: NSRect(x: 10, y: 10, width: 100, height: 22))
        let depth = NSTextField(frame: NSRect(x: 150, y: 10, width: 100, height: 22))
        dimension.stringValue = "30"; depth.stringValue = "2"
        window.contentView?.addSubview(dimension)
        window.contentView?.addSubview(depth)
        XCTAssertTrue(window.makeFirstResponder(dimension))
        XCTAssertNotNil(dimension.currentEditor())

        XCTAssertNotNil(ScrubField.focus(depth))
        XCTAssertNil(dimension.currentEditor(), "the dimension field gave up the field editor")
        let editor = try XCTUnwrap(depth.currentEditor() as? NSTextView)
        XCTAssertTrue(window.firstResponder === editor)
        XCTAssertEqual(editor.selectedRange(), NSRange(location: 0, length: 1), "text selected, ready to overtype")
        editor.insertText("5", replacementRange: editor.selectedRange())
        XCTAssertEqual(editor.string, "5")
        XCTAssertEqual(dimension.stringValue, "30", "typing never reaches the old field")
    }
}

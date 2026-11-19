import SwiftUI
import UIKit
import UniformTypeIdentifiers

// The shared sources (the whole ../VcadApp source dir, symlinked as App/)
// speak AppKit's dialect.
// iOS is UIKit-flavored; these shims keep the shared code word-for-word.

typealias NSColor = UIColor

/// Stand-in for NSColorSpace at the one shared call site
/// (`part.color.usingColorSpace(.sRGB)`): UIColor components are already fine.
struct PortedColorSpace {
    static let sRGB = PortedColorSpace()
}

extension UIColor {
    /// AppKit spelling; UIKit's device-RGB init is close enough on visionOS.
    convenience init(srgbRed r: CGFloat, green g: CGFloat, blue b: CGFloat, alpha a: CGFloat) {
        self.init(red: r, green: g, blue: b, alpha: a)
    }

    /// AppKit color-space accessors the shared code touches. UIColor's
    /// getRed(_:green:blue:alpha:) already handles conversion.
    var redComponent: CGFloat { rgba.0 }
    var greenComponent: CGFloat { rgba.1 }
    var blueComponent: CGFloat { rgba.2 }
    var alphaComponent: CGFloat { rgba.3 }
    func usingColorSpace(_ space: PortedColorSpace) -> UIColor? { self }

    private var rgba: (CGFloat, CGFloat, CGFloat, CGFloat) {
        var r: CGFloat = 0, g: CGFloat = 0, b: CGFloat = 0, a: CGFloat = 0
        getRed(&r, green: &g, blue: &b, alpha: &a)
        return (r, g, b, a)
    }
}

// Geometry + image type names the shared code spells the AppKit way.
typealias NSRect = CGRect
typealias NSSize = CGSize
typealias NSPoint = CGPoint
typealias NSImage = UIImage

extension UIColor {
    /// AppKit semantic colors the shared chrome reads.
    static var windowBackgroundColor: UIColor { .systemBackground }
    static var controlBackgroundColor: UIColor { .secondarySystemBackground }
    static var textBackgroundColor: UIColor { .systemBackground }
    static var underPageBackgroundColor: UIColor { .systemGroupedBackground }
    static var labelColor: UIColor { .label }
    static var secondaryLabelColor: UIColor { .secondaryLabel }
    static var tertiaryLabelColor: UIColor { .tertiaryLabel }
    static var separatorColor: UIColor { .separator }
    static var controlAccentColor: UIColor { .tintColor }
    convenience init(calibratedWhite w: CGFloat, alpha a: CGFloat) { self.init(white: w, alpha: a) }
    convenience init(calibratedRed r: CGFloat, green g: CGFloat, blue b: CGFloat, alpha a: CGFloat) {
        self.init(red: r, green: g, blue: b, alpha: a)
    }
    convenience init(deviceRed r: CGFloat, green g: CGFloat, blue b: CGFloat, alpha a: CGFloat) {
        self.init(red: r, green: g, blue: b, alpha: a)
    }
}

extension Color {
    init(nsColor: UIColor) { self.init(uiColor: nsColor) }
}

extension Image {
    init(nsImage: UIImage) { self.init(uiImage: nsImage) }
}

/// No pointer cursor on a touch screen: cursor changes are no-ops.
@MainActor
final class NSCursor {
    static let arrow = NSCursor(), crosshair = NSCursor(), pointingHand = NSCursor()
    static let openHand = NSCursor(), closedHand = NSCursor(), iBeam = NSCursor()
    static let resizeLeftRight = NSCursor(), resizeUpDown = NSCursor(), operationNotAllowed = NSCursor()
    static let dragCopy = NSCursor(), contextualMenu = NSCursor(), disappearingItem = NSCursor()
    static func hide() {}
    static func unhide() {}
    static func pop() {}
    func set() {}
    func push() {}
}

/// The static half of NSEvent the shared views poll (held modifier keys). A
/// phone has no held modifiers; instance events never exist on iOS.
enum NSEvent {
    struct ModifierFlags: OptionSet, Sendable {
        let rawValue: UInt
        static let shift = ModifierFlags(rawValue: 1 << 17)
        static let control = ModifierFlags(rawValue: 1 << 18)
        static let option = ModifierFlags(rawValue: 1 << 19)
        static let command = ModifierFlags(rawValue: 1 << 20)
    }
    static var modifierFlags: ModifierFlags { [] }
    static var pressedMouseButtons: Int { 0 }
    static var doubleClickInterval: TimeInterval { 0.3 }
}

/// AppKit haptics → UIKit feedback generators.
@MainActor
enum NSHapticFeedbackManager {
    enum Pattern { case alignment, levelChange, generic }
    enum PerformanceTime { case now, `default`, drawCompleted }
    struct Performer {
        func perform(_ p: Pattern, performanceTime: PerformanceTime) {
            UIImpactFeedbackGenerator(style: p == .levelChange ? .medium : .light).impactOccurred()
        }
    }
    static var defaultPerformer: Performer { Performer() }
}

/// The NSWorkspace calls shared code makes, mapped to UIApplication.
@MainActor
final class NSWorkspace {
    static let shared = NSWorkspace()
    var accessibilityDisplayShouldReduceMotion: Bool { UIAccessibility.isReduceMotionEnabled }
    @discardableResult func open(_ url: URL) -> Bool {
        UIApplication.shared.open(url)
        return true
    }
    func activateFileViewerSelecting(_ urls: [URL]) {}
}

import RealityKit
extension ARView {
    /// UIKit views are top-left origin — the AppKit "flipped" case.
    var isFlipped: Bool { true }
}

/// The NSBitmapImageRep subset shared code uses to turn a CGImage (or image
/// data) into PNG/JPEG bytes.
final class NSBitmapImageRep {
    enum FileType { case png, jpeg, tiff }
    struct PropertyKey: Hashable, RawRepresentable {
        let rawValue: String
        static let compressionFactor = PropertyKey(rawValue: "compressionFactor")
    }
    let cgImage: CGImage?
    init(cgImage: CGImage) { self.cgImage = cgImage }
    init?(data: Data) {
        guard let cg = UIImage(data: data)?.cgImage else { return nil }
        cgImage = cg
    }
    var pixelsWide: Int { cgImage?.width ?? 0 }
    var pixelsHigh: Int { cgImage?.height ?? 0 }
    func representation(using type: FileType, properties: [PropertyKey: Any]) -> Data? {
        guard let cgImage else { return nil }
        let img = UIImage(cgImage: cgImage)
        if type == .jpeg {
            return img.jpegData(compressionQuality: properties[.compressionFactor] as? CGFloat ?? 0.85)
        }
        return img.pngData()
    }
}

extension UIImage {
    /// Only ever fed back into NSBitmapImageRep(data:) by shared code.
    var tiffRepresentation: Data? { pngData() }
}

extension View {
    /// Esc has no key on a phone; the Mac's exit-command hook is a no-op.
    func onExitCommand(perform action: (() -> Void)?) -> some View { self }
}

extension UIWindow {
    var backingScaleFactor: CGFloat { screen.scale }
}

extension FileManager {
    /// iOS has no user home outside the sandbox; the app container stands in.
    var homeDirectoryForCurrentUser: URL { URL(fileURLWithPath: NSHomeDirectory()) }
}

extension ToggleStyle where Self == SwitchToggleStyle {
    /// No checkboxes on iOS: the Mac's checkbox toggles become switches.
    static var checkbox: SwitchToggleStyle { .switch }
}

extension UIImage {
    /// AppKit's NSImage → CGImage accessor.
    func cgImage(forProposedRect rect: UnsafeMutablePointer<CGRect>?, context: Any?,
                 hints: [AnyHashable: Any]?) -> CGImage? { cgImage }
}

/// NSPasteboard's string API over UIPasteboard.
@MainActor
final class NSPasteboard {
    struct PasteboardType: Hashable, Sendable { let rawValue: String
        static let string = PasteboardType(rawValue: "public.utf8-plain-text") }
    static let general = NSPasteboard()
    @discardableResult func clearContents() -> Int { UIPasteboard.general.items = []; return 0 }
    @discardableResult func setString(_ s: String, forType t: PasteboardType) -> Bool {
        UIPasteboard.general.string = s; return true
    }
    func string(forType t: PasteboardType) -> String? { UIPasteboard.general.string }
}

// MARK: - modal file panels

/// AppKit's modal open/save panels have no iOS equivalent that can block.
/// Shared code that reaches for one gets a panel that reports Cancel, so
/// desktop-only flows (CNC files, Gerber folders, video export) quietly do
/// nothing; the phone's own document flows use fileImporter/fileExporter.
@MainActor
class NSSavePanel {
    enum ModalResponse { case OK, cancel }
    var allowedContentTypes: [UTType] = []
    var nameFieldStringValue = ""
    var message = "", prompt = "", title = ""
    var directoryURL: URL?
    var canCreateDirectories = true
    var isExtensionHidden = false
    var url: URL? { nil }
    func runModal() -> ModalResponse { .cancel }
    func begin(completionHandler: @escaping (ModalResponse) -> Void) { completionHandler(.cancel) }
}

@MainActor
final class NSOpenPanel: NSSavePanel {
    var canChooseFiles = true, canChooseDirectories = false, allowsMultipleSelection = false
    var urls: [URL] { [] }
}

/// iOS can't spawn processes. Shared code that probes for `node`, `npx` or
/// the vcad CLI (agent connect/install — desktop features) gets a Process
/// whose run() fails, which every caller already treats as "not installed".
final class Process: @unchecked Sendable {
    struct LaunchError: Error {}
    var executableURL: URL?
    var arguments: [String]?
    var environment: [String: String]?
    var currentDirectoryURL: URL?
    var standardInput: Any?
    var standardOutput: Any?
    var standardError: Any?
    var terminationHandler: ((Process) -> Void)?
    var terminationStatus: Int32 { -1 }
    var isRunning: Bool { false }
    var processIdentifier: Int32 { 0 }
    func run() throws { throw LaunchError() }
    func waitUntilExit() {}
    func terminate() {}
}

extension PrimitiveButtonStyle where Self == BorderlessButtonStyle {
    /// AppKit's link-styled button → a borderless (tinted text) button.
    static var link: BorderlessButtonStyle { .borderless }
}

/// The NSApplication calls shared code makes (key window focus, quitting).
@MainActor
final class PhoneApplicationShim {
    struct CurrentEvent { var modifierFlags: NSEvent.ModifierFlags { [] } }
    var keyWindow: UIWindow? {
        UIApplication.shared.connectedScenes.compactMap { ($0 as? UIWindowScene)?.keyWindow }.first
    }
    var mainWindow: UIWindow? { keyWindow }
    var currentEvent: CurrentEvent? { nil }
    func terminate(_ sender: Any?) { exit(0) }
    func activate(ignoringOtherApps: Bool = true) {}
}
@MainActor let NSApp = PhoneApplicationShim()

extension UIWindow {
    /// AppKit: clear first responder. UIKit: end editing.
    @discardableResult func makeFirstResponder(_ r: UIResponder?) -> Bool { endEditing(true) }
}

typealias NSWindow = UIWindow

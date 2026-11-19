import SwiftUI

// The phone's versions of the Mac's File-menu panels (Shell.swift, Mac-only).
// Shared code calls `openPanel(model)` / `saveAsPanel(model)` /
// `exportUSDZPanel(model)`; on iPhone those raise a request the phone shell
// presents as a document picker / exporter / AR Quick Look.

@MainActor
@Observable
final class PhoneDocumentFlow {
    static let shared = PhoneDocumentFlow()
    var importing = false
    var quickLookURL: URL?
    /// Phone-only cards above the dock, opened from the ⋯ menu (the Mac's
    /// equivalents are its menu bar and floating windows).
    var materialsShown = false
    var simulationShown = false
    /// The Object Inspector card: opens when something is selected, closes
    /// on deselect or with its ×.
    var inspectorShown = false
}

@MainActor func openPanel(_ model: EditorModel) { PhoneDocumentFlow.shared.importing = true }
/// Save As on a phone: a new document in vcad's Files folder (named after
/// the current one), then keep editing it there — Export a Copy… is the
/// route to anywhere else.
@MainActor func saveAsPanel(_ model: EditorModel) {
    let name = model.documentName.isEmpty || model.source.isSandbox ? "Untitled" : model.documentName
    let url = PhoneDocuments.uniqueDestination(for: "\(name).vcad")
    model.saveDocumentAs(url)
    if model.saveError == nil { GlassUI.shared.flash("Saved to Files as \(url.lastPathComponent)", symbol: "checkmark.circle") }
}
@MainActor func exportUSDZPanel(_ model: EditorModel) { PhoneDocumentFlow.shared.presentQuickLook(model) }

extension PhoneDocumentFlow {
    /// Write the part as USDZ (UsdzExport.swift, shared) and show it in AR
    /// Quick Look — place it on the desk at physical size.
    func presentQuickLook(_ model: EditorModel) {
        let name = model.documentName.isEmpty ? "part" : model.documentName
        let url = FileManager.default.temporaryDirectory.appendingPathComponent("\(name).usdz")
        try? FileManager.default.removeItem(at: url)
        if model.exportUSDZ(to: url) { quickLookURL = url }
        else { GlassUI.shared.flash("Nothing to show in AR yet", symbol: "arkit") }
    }
}

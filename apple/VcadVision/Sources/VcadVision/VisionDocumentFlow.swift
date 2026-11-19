import SwiftUI
import UniformTypeIdentifiers

// Vision Pro's versions of the Mac's File-menu panels (Shell.swift, Mac-only).
// Shared code calls `openPanel(model)` / `saveAsPanel(model)` /
// `exportUSDZPanel(model)`; here those raise a request the volume presents
// as a document picker / exporter (the iPhone port's PhoneDocumentFlow pattern).

@MainActor
@Observable
final class VisionDocumentFlow {
    static let shared = VisionDocumentFlow()
    var importing = false
    /// A document waiting for the exporter (Save As / Export USDZ).
    var export: VisionExport?
}

struct VisionExport: Identifiable {
    let id = UUID()
    let document: VisionFileDocument
    let type: UTType
    let name: String
    /// Save As adopts the chosen location as the open document.
    let adopt: Bool
}

/// Raw bytes as a FileDocument, for `.fileExporter`.
struct VisionFileDocument: FileDocument {
    static var readableContentTypes: [UTType] { [.data] }
    let data: Data
    init(data: Data) { self.data = data }
    init(configuration: ReadConfiguration) throws { data = configuration.file.regularFileContents ?? Data() }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper { FileWrapper(regularFileWithContents: data) }
}

@MainActor func openPanel(_ model: EditorModel) { VisionDocumentFlow.shared.importing = true }

@MainActor func saveAsPanel(_ model: EditorModel) {
    // Save As writes the document's current bytes through the exporter, then
    // keeps editing at the new location.
    let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("vcad-save.vcad")
    try? FileManager.default.removeItem(at: tmp)
    model.saveDocumentAs(tmp)
    guard model.saveError == nil, let data = try? Data(contentsOf: tmp) else { return }
    let name = model.documentName.isEmpty || model.source.isSandbox ? "Untitled" : model.documentName
    VisionDocumentFlow.shared.export = VisionExport(document: VisionFileDocument(data: data),
                                                    type: UTType(filenameExtension: "vcad") ?? .json,
                                                    name: name, adopt: true)
}

@MainActor func exportUSDZPanel(_ model: EditorModel) {
    let name = model.documentName.isEmpty ? "part" : model.documentName
    let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("\(name).usdz")
    try? FileManager.default.removeItem(at: tmp)
    guard model.exportUSDZ(to: tmp), let data = try? Data(contentsOf: tmp) else {
        GlassUI.shared.flash("Nothing to export yet", symbol: "cube.transparent")
        return
    }
    VisionDocumentFlow.shared.export = VisionExport(document: VisionFileDocument(data: data),
                                                    type: .usdz, name: name, adopt: false)
}

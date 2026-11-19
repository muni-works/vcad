import SwiftUI
import UniformTypeIdentifiers

/// Files-app document handling for the phone.
///
/// The editor saves the way the Mac does — `SafeWriter` writes a temp file
/// beside the document and renames it over (atomic, fsynced). That needs
/// write access to the document's DIRECTORY, which a document picked from
/// iCloud Drive or another app's folder doesn't grant. So a picked document
/// is copied into vcad's own Documents folder (shown in Files as
/// "On My iPhone › vcad", via UIFileSharingEnabled +
/// LSSupportsOpeningDocumentsInPlace) and edited there; documents already in
/// that folder open in place. "Save a Copy…" exports anywhere through the
/// system exporter.
@MainActor
enum PhoneDocuments {
    static var folder: URL {
        FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
    }

    nonisolated static let vcadType = UTType(filenameExtension: "vcad") ?? .json
    static let openableTypes: [UTType] = [vcadType] + ["loon", "step", "stp", "stl"]
        .compactMap { UTType(filenameExtension: $0) }

    /// Open `url` (from the picker, Files, or a share) into the editor.
    static func open(_ picked: URL, into model: EditorModel) {
        // Samples in the repo are symlinks; copy what they point at.
        let url = picked.resolvingSymlinksInPath()
        let scoped = url.startAccessingSecurityScopedResource()
        defer { if scoped { url.stopAccessingSecurityScopedResource() } }
        let local: URL
        if url.standardizedFileURL.path.hasPrefix(folder.standardizedFileURL.path) {
            local = url
        } else {
            local = uniqueDestination(for: picked.lastPathComponent)
            do {
                // Coordinated read: iCloud files may need downloading first.
                var coordError: NSError?
                var data: Data?
                var readError: Error?
                NSFileCoordinator().coordinate(readingItemAt: url, options: [], error: &coordError) { src in
                    do { data = try Data(contentsOf: src) } catch { readError = error }
                }
                if let e = coordError ?? readError { throw e }
                try (data ?? Data()).write(to: local, options: .withoutOverwriting)
            } catch {
                GlassUI.shared.flash("Couldn’t open \(url.lastPathComponent): \(error.localizedDescription)",
                                     symbol: "exclamationmark.triangle")
                return
            }
        }
        model.openDocument(local)
    }

    /// `name` in Documents, numbered if taken ("part 2.vcad").
    static func uniqueDestination(for name: String) -> URL {
        let base = (name as NSString).deletingPathExtension, ext = (name as NSString).pathExtension
        var url = folder.appendingPathComponent(name)
        var n = 2
        // A dangling symlink still takes the name (fileExists follows links).
        func taken(_ u: URL) -> Bool {
            FileManager.default.fileExists(atPath: u.path)
                || (try? FileManager.default.destinationOfSymbolicLink(atPath: u.path)) != nil
        }
        while taken(url) {
            url = folder.appendingPathComponent("\(base) \(n).\(ext)")
            n += 1
        }
        return url
    }

    /// Bundled sample documents (the Mac app's Samples/), for the open menu.
    static var samples: [URL] {
        guard let dir = Bundle.main.resourceURL?.appendingPathComponent("samples"),
              let items = try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)
        else { return [] }
        return items.filter { $0.pathExtension == "vcad" }.sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    /// Documents in vcad's Files folder, newest first.
    static var recents: [URL] {
        let keys: [URLResourceKey] = [.contentModificationDateKey]
        let items = (try? FileManager.default.contentsOfDirectory(at: folder, includingPropertiesForKeys: keys)) ?? []
        return items.filter { ["vcad", "loon"].contains($0.pathExtension) }
            .sorted {
                let a = (try? $0.resourceValues(forKeys: Set(keys)).contentModificationDate) ?? .distantPast
                let b = (try? $1.resourceValues(forKeys: Set(keys)).contentModificationDate) ?? .distantPast
                return a > b
            }
    }

    /// First launch: nothing to install — samples open by copy-on-open, so
    /// the Files folder holds only what the user made. Kept as the hook for
    /// a future "Getting started" document.
    static func installSamplesIfNeeded() {}
}

/// The current document as a FileDocument, for "Save a Copy…" (fileExporter).
struct VcadExportDocument: FileDocument {
    /// .vcad (Export a Copy) and the kernel formats (Export ▸ STL/STEP/GLB).
    static var readableContentTypes: [UTType] {
        [PhoneDocuments.vcadType] + DocumentExportFormat.allCases.map(\.contentType)
    }
    var data: Data
    var contentType: UTType = PhoneDocuments.vcadType
    var filename = ""
    init(data: Data) { self.data = data }
    init(data: Data, contentType: UTType, filename: String) {
        self.data = data; self.contentType = contentType; self.filename = filename
    }
    init(configuration: ReadConfiguration) throws { data = configuration.file.regularFileContents ?? Data() }
    func fileWrapper(configuration: WriteConfiguration) throws -> FileWrapper { FileWrapper(regularFileWithContents: data) }
}

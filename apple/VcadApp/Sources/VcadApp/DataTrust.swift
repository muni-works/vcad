#if canImport(AppKit)
import AppKit
#endif
import Foundation
import SwiftUI

// The editor side of LocalHistory.swift: autosave scheduling, version
// history for the open document, and the launch-time recovery prompt.

extension EditorModel {
    /// Autosave is off inside unit tests unless a test points LocalData at a
    /// temp root — hundreds of throwaway models must not litter the user's
    /// Application Support with "crashed" documents.
    static var autosaveEnabled: Bool {
        LocalData.overrideRoot != nil || NSClassFromString("XCTestCase") == nil
    }

    /// Called on every document change. Hops once to the next main-loop turn
    /// (so `documentDirty` has settled), then hands an immutable copy of the
    /// document to the autosaver, which serializes and writes it off-main.
    func noteEditForAutosave() {
        guard Self.autosaveEnabled, !autosavePending else { return }
        autosavePending = true
        DispatchQueue.main.async { [weak self] in
            guard let self else { return }
            self.autosavePending = false
            guard self.documentDirty, self.usesDocumentTree, let json = self.documentJSON else { return }
            let saver = self.autosaver ?? Autosaver()
            self.autosaver = saver
            nonisolated(unsafe) let copy = json         // [String: Any] is a value; never mutated after this
            saver.schedule(name: self.documentName, documentPath: self.documentURL?.path) {
                DocEdit.serializePretty(copy)
            }
        }
    }

    /// A save landed on disk: snapshot it into the history and drop the
    /// autosave (there is nothing unsaved left to recover).
    func didSaveDocument(_ data: Data, at url: URL) {
        saveError = nil
        autosaver?.discard()
        guard Self.autosaveEnabled else { return }
        try? VersionHistory(documentURL: url).record(data)
        for tmp in SafeWriter.strayTemps(for: url) { try? FileManager.default.removeItem(at: tmp) }
    }

    /// "Don't Save" on close: the user chose to throw the edits away.
    func discardAutosave() { autosaver?.discard() }

    var versionHistory: VersionHistory? { documentURL.map { VersionHistory(documentURL: $0) } }

    /// Record the CURRENT state (saved or not) as a named version.
    @discardableResult
    func saveNamedVersion(_ name: String) -> DocumentVersion? {
        guard let h = versionHistory, let json = documentJSON, let data = DocEdit.serializePretty(json) else { return nil }
        return try? h.record(data, kind: .named, name: name.isEmpty ? nil : name)
    }

    /// Show `v` in the viewport as an undoable edit. `endVersionPreview(keep:)`
    /// either keeps it (Restore) or undoes it (back to what you had).
    func previewVersion(_ v: DocumentVersion) {
        guard let h = versionHistory, let data = h.data(for: v),
              let dict = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return }
        if previewingVersion != nil { undo() }
        if replaceDocument(with: dict, name: "Preview \(v.title)") { previewingVersion = v }
    }

    func endVersionPreview(keep: Bool) {
        guard let v = previewingVersion else { return }
        previewingVersion = nil
        if !keep { undo(); return }
        // Keep a safety copy of what the file held before the restore.
        if let h = versionHistory, let url = documentURL, let current = try? Data(contentsOf: url) {
            try? h.record(current, kind: .beforeRestore)
        }
        _ = v
    }
}

// MARK: - Recovery prompt

#if os(macOS)
@MainActor
enum RecoveryPrompt {
    /// At launch: if a previous instance died with unsaved work, offer it back.
    static func runIfNeeded() {
        guard EditorModel.autosaveEnabled, !Recovery.orphans().isEmpty else { return }
        // Let the editor window come up first. A modal run from inside
        // applicationDidFinishLaunching returned instantly with the LAST
        // button's code (seen live: response 1002, no click), which once
        // silently discarded recovered work.
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.8) { run() }
    }

    static func run() {
        let orphans = Recovery.orphans()
        guard !orphans.isEmpty else { return }
        NSApp.activate(ignoringOtherApps: true)
        let response = makeAlert(orphans).runModal()
        switch response {
        case .alertFirstButtonReturn: restore(orphans)
        case .alertThirdButtonReturn where confirmDiscard(orphans.count):
            orphans.forEach { Recovery.discard($0) }
        default: break     // Not Now (or anything unexpected): keep them for next launch
        }
    }

    /// Discarding recovered work can't be undone, so it takes a second, explicit yes.
    static func confirmDiscard(_ count: Int) -> Bool {
        let a = NSAlert()
        a.messageText = count == 1 ? "Discard the unsaved changes?" : "Discard unsaved changes to \(count) documents?"
        a.informativeText = "This can’t be undone."
        a.alertStyle = .critical
        a.addButton(withTitle: "Keep")
        a.addButton(withTitle: "Discard").hasDestructiveAction = true
        return a.runModal() == .alertSecondButtonReturn
    }

    static func makeAlert(_ orphans: [AutosaveEntry]) -> NSAlert {
        let alert = NSAlert()
        alert.messageText = orphans.count == 1
            ? "Restore unsaved changes to “\(orphans[0].name)”?"
            : "Restore \(orphans.count) documents with unsaved changes?"
        let f = RelativeDateTimeFormatter()
        let lines = orphans.prefix(6).map { "• \($0.name) — autosaved \(f.localizedString(for: $0.savedAt, relativeTo: Date()))" }
        alert.informativeText = "vcad quit before these were saved.\n\n" + lines.joined(separator: "\n")
            + "\n\nRestored work opens as a “(Recovered)” copy; your original files are untouched."
        alert.alertStyle = .warning
        alert.addButton(withTitle: "Restore")
        alert.addButton(withTitle: "Not Now")
        alert.addButton(withTitle: "Discard")
        return alert
    }

    static func restore(_ orphans: [AutosaveEntry]) {
        let urls = orphans.compactMap { try? Recovery.materialize($0) }
        guard let first = urls.first else { return }
        if let model = AppInstance.currentModel { model.openDocument(first) }
        else { AppInstance.pendingOpen.append(first) }
        for url in urls.dropFirst() { AppInstance.open(document: url) }
    }
}

// MARK: - Version browser

@MainActor
final class VersionBrowserWindow {
    static var window: NSWindow?

    static func show(_ model: EditorModel) {
        if let w = window { w.makeKeyAndOrderFront(nil); return }
        let w = NSPanel(contentRect: NSRect(x: 0, y: 0, width: 460, height: 480),
                        styleMask: [.titled, .closable, .resizable, .utilityWindow],
                        backing: .buffered, defer: false)
        w.title = "Versions — \(model.documentName)"
        w.isReleasedWhenClosed = false
        w.hidesOnDeactivate = false
        w.contentViewController = NSHostingController(rootView: VersionBrowser(model: model))
        w.center()
        w.makeKeyAndOrderFront(nil)
        NotificationCenter.default.addObserver(forName: NSWindow.willCloseNotification, object: w, queue: .main) { _ in
            MainActor.assumeIsolated {
                if model.previewingVersion != nil { model.endVersionPreview(keep: false) }
                window = nil
            }
        }
        window = w
    }

    /// File ▸ Save Version… — ask for a name, record the current state.
    static func promptForNamedVersion(_ model: EditorModel) {
        let alert = NSAlert()
        alert.messageText = "Save a named version"
        alert.informativeText = "Named versions are kept forever and show up in Browse Versions."
        let field = NSTextField(frame: NSRect(x: 0, y: 0, width: 260, height: 24))
        field.placeholderString = "e.g. Before rib redesign"
        alert.accessoryView = field
        alert.addButton(withTitle: "Save Version")
        alert.addButton(withTitle: "Cancel")
        alert.window.initialFirstResponder = field
        guard alert.runModal() == .alertFirstButtonReturn else { return }
        model.saveNamedVersion(field.stringValue)
    }
}

struct VersionBrowser: View {
    let model: EditorModel
    @State private var versions: [DocumentVersion] = []
    @State private var selection: DocumentVersion.ID?
    @State private var newName = ""

    var body: some View {
        VStack(spacing: 0) {
            if model.documentURL == nil {
                ContentUnavailableView("Save this document to start its history",
                                       systemImage: "clock.arrow.circlepath",
                                       description: Text("Every save is kept as a version you can preview and restore."))
            } else {
                HStack {
                    TextField("Name this version…", text: $newName).textFieldStyle(.roundedBorder)
                        .onSubmit(saveNamed)
                    Button("Save Version", action: saveNamed)
                }
                .padding(10)
                List(selection: $selection) {
                    ForEach(versions) { v in row(v).tag(v.id) }
                }
                .onChange(of: selection) { _, id in
                    if let v = versions.first(where: { $0.id == id }) { model.previewVersion(v) }
                }
                if let v = model.previewingVersion { previewBar(v) }
            }
        }
        .frame(minWidth: 440, minHeight: 360)
        .onAppear(perform: reload)
    }

    private func row(_ v: DocumentVersion) -> some View {
        HStack(spacing: 8) {
            Image(systemName: v.kind == .named ? "bookmark.fill" : v.kind == .beforeRestore ? "arrow.uturn.backward" : "clock")
                .foregroundStyle(v.kind == .named ? Color.accentColor : .secondary)
                .frame(width: 18)
            VStack(alignment: .leading, spacing: 1) {
                Text(v.title).fontWeight(v.kind == .named ? .semibold : .regular)
                Text(v.createdAt.formatted(date: .abbreviated, time: .shortened)
                     + " · " + ByteCountFormatter.string(fromByteCount: Int64(v.byteCount), countStyle: .file))
                    .font(.caption).foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 2)
        .contextMenu {
            Button("Duplicate as New Document…") { duplicate(v) }
            Button("Delete Version", role: .destructive) {
                try? model.versionHistory?.delete(v); reload()
            }
        }
    }

    private func previewBar(_ v: DocumentVersion) -> some View {
        HStack {
            Image(systemName: "eye")
            Text("Previewing “\(v.title)”").lineLimit(1)
            Spacer()
            Button("Cancel") { model.endVersionPreview(keep: false); selection = nil }
            Button("Duplicate…") { duplicate(v) }
            Button("Restore") { model.endVersionPreview(keep: true); selection = nil; reload() }
                .buttonStyle(.borderedProminent)
        }
        .padding(10)
        .background(.bar)
    }

    private func reload() { versions = model.versionHistory?.versions ?? [] }

    private func saveNamed() {
        model.saveNamedVersion(newName.trimmingCharacters(in: .whitespaces))
        newName = ""; reload()
    }

    private func duplicate(_ v: DocumentVersion) {
        guard let h = model.versionHistory else { return }
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.init(filenameExtension: "vcad") ?? .json]
        panel.nameFieldStringValue = "\(model.documentName) — \(v.title).vcad"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        do { try h.duplicate(v, to: url); AppInstance.open(document: url) }
        catch { model.saveError = "Couldn’t duplicate the version: \(error)" }
    }
}
#endif

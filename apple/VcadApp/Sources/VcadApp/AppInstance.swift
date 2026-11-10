#if canImport(AppKit)
import AppKit
#endif
import Foundation

/// Opening a second document opens a second COPY OF THE APP.
///
/// The ask was "each tab is another icon in the Dock, and ⌘-Tab shows them
/// separately". macOS does not offer that within one process: the Dock tile and
/// the ⌘-Tab entry belong to the *application*, not to its windows, so a
/// document-per-window app (Pages, Xcode) shows exactly one of each no matter
/// how many documents are open. `NSApp.dockTile` customises that single tile; it
/// does not mint new ones. There is no supported API for a second tile short of
/// a second app bundle — which would mean shipping a decoy bundle pretending to
/// be vcad, and would still be one tile per bundle rather than per document.
///
/// So a document is an app INSTANCE. That is a real fit rather than a
/// workaround here: release-to-desktop has no window of its own — the app *is*
/// the parts floating over the desktop — so "another window" was already a
/// fiction, while another instance gives each document its own Dock tile (which
/// already renders that document's viewport), its own ⌘-Tab entry, its own menu
/// bar, and its own crash domain.
///
/// The cost is that instances share nothing but the disk: no cross-document
/// undo, no shared selection, and anything written to `UserDefaults` is
/// last-writer-wins (see `EditorModel.rememberRecent`).
@MainActor
enum AppInstance {
    /// This instance's document, for the AppDelegate paths that have no view
    /// to read it from (Finder open/drop). Weak: the model outlives nothing.
    weak static var currentModel: EditorModel?
    /// Files macOS handed us before the editor view existed (a launch by
    /// double-click). Drained by the view's first task instead of spawning a
    /// second instance and leaving an empty scratch behind.
    static var pendingOpen: [URL] = []

    /// Launch another copy of this app, optionally opening a document.
    ///
    /// `createsNewApplicationInstance` is the whole trick: without it the
    /// launch is a no-op that just activates the running copy.
    static func open(document url: URL? = nil) {
        let config = NSWorkspace.OpenConfiguration()
        config.createsNewApplicationInstance = true
        config.activates = true
        // Pass the document as an argument rather than an environment variable:
        // env is inherited by anything the instance spawns, and a stale
        // VCAD_OPEN two launches later would silently reopen the wrong file.
        if let url { config.arguments = [Self.openFlag, url.path] }

        let me = Bundle.main.bundleURL
        NSWorkspace.shared.openApplication(at: me, configuration: config) { _, error in
            guard let error else { return }
            // Not fatal: the current instance is still usable, and saying so is
            // better than a menu item that silently does nothing.
            FileHandle.standardError.write(
                Data("[vcad] could not open a new instance: \(error.localizedDescription)\n".utf8))
        }
    }

    /// The flag this app passes to itself. Not a user-facing CLI.
    static let openFlag = "--vcad-open"

    /// The document this instance was launched to open, if any.
    ///
    /// Accepts the launch argument, the `VCAD_OPEN` dev hook, and a bare path
    /// argument (so `open -a vcad --args file.vcad` and a drop onto the icon
    /// both work).
    static func launchDocument() -> URL? {
        let args = CommandLine.arguments
        if let i = args.firstIndex(of: openFlag), i + 1 < args.count {
            return URL(fileURLWithPath: args[i + 1])
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_OPEN"], !path.isEmpty {
            return URL(fileURLWithPath: path)
        }
        if let bare = args.dropFirst().first(where: { $0.hasSuffix(".vcad") || $0.hasSuffix(".loon") }) {
            return URL(fileURLWithPath: bare)
        }
        return nil
    }

    /// Where a document should open: in this instance, or a new one.
    ///
    /// An untouched sandbox is a scratch instance — reuse it, the way a Mac app
    /// reuses its empty Untitled window. Anything else already has a document
    /// worth keeping on screen, so the new one gets its own instance.
    static func opening(_ url: URL, from model: EditorModel) {
        // An outline always joins the document on screen.
        if url.pathExtension.lowercased() == "dxf" { model.openDocument(url); return }
        if model.isDisposableScratch {
            model.openDocument(url)
        } else {
            open(document: url)
        }
    }

    // MARK: - vcad:// deep links

    /// Turn a `vcad://` URL into a file this app can open, or nil.
    ///
    /// Format (docs/launch/vcad-url-scheme.md), all forms `vcad://open?…`:
    /// - `path=<absolute path>` — a local `.vcad` / `.loon` / `.dxf` file.
    /// - `session=<id>[&dir=<session dir>]` — an MCP session persisted by a
    ///   local server to `~/.vcad/mcp-sessions/<id>.json` (or `dir`).
    /// - `doc=<gzip+base64url .vcad JSON>` — the document inline.
    /// - `name=<label>` — optional display name for session/inline documents.
    ///
    /// Session and inline documents are copied to a hand-off file first: the
    /// app then owns a private copy, so a later Save never writes into the MCP
    /// server's session store.
    static func resolveDeepLink(_ url: URL) -> URL? {
        guard let link = DeepLink(url) else {
            FileHandle.standardError.write(Data("[vcad] ignoring unrecognised link: \(url.absoluteString.prefix(120))\n".utf8))
            return nil
        }
        do {
            return try link.materialize()
        } catch {
            FileHandle.standardError.write(Data("[vcad] could not open link: \(error)\n".utf8))
            return nil
        }
    }
}

/// A parsed `vcad://open?…` link. Pure parsing — no file access — so the
/// format can be unit-tested on its own.
enum DeepLink: Equatable {
    case file(URL)
    case session(id: String, dir: URL, name: String?)
    case inline(Data, name: String?)

    static let openableExtensions: Set<String> = ["vcad", "loon", "dxf"]

    init?(_ url: URL) {
        guard url.scheme?.lowercased() == "vcad",
              let comps = URLComponents(url: url, resolvingAgainstBaseURL: false),
              (comps.host ?? "").lowercased() == "open"
        else { return nil }
        var q: [String: String] = [:]
        for item in comps.queryItems ?? [] { if let v = item.value, q[item.name] == nil { q[item.name] = v } }
        let name = q["name"].flatMap { $0.isEmpty ? nil : $0 }

        if let path = q["path"] {
            // Only absolute paths to document types: a link from a web page
            // must not be able to make the app open arbitrary files.
            guard path.hasPrefix("/") else { return nil }
            let file = URL(fileURLWithPath: path).standardizedFileURL
            guard Self.openableExtensions.contains(file.pathExtension.lowercased()) else { return nil }
            self = .file(file)
        } else if let id = q["session"] {
            guard id.range(of: "^[A-Za-z0-9_-]{1,128}$", options: .regularExpression) != nil else { return nil }
            let dir: URL
            if let d = q["dir"] {
                guard d.hasPrefix("/") else { return nil }
                dir = URL(fileURLWithPath: d, isDirectory: true)
            } else {
                dir = Self.defaultSessionDir
            }
            self = .session(id: id, dir: dir, name: name)
        } else if let doc = q["doc"], let data = Self.decodeInline(doc) {
            self = .inline(data, name: name)
        } else {
            return nil
        }
    }

    /// Mirrors the MCP server's default: `$VCAD_MCP_SESSION_DIR`, else
    /// `~/.vcad/mcp-sessions`.
    static var defaultSessionDir: URL {
        if let d = ProcessInfo.processInfo.environment["VCAD_MCP_SESSION_DIR"], !d.isEmpty {
            return URL(fileURLWithPath: d, isDirectory: true)
        }
        return FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent(".vcad/mcp-sessions", isDirectory: true)
    }

    enum Failure: Error, CustomStringConvertible {
        case missing(String)
        case notADocument
        var description: String {
            switch self {
            case .missing(let p): return "no such file: \(p)"
            case .notADocument: return "link payload is not a .vcad JSON document"
            }
        }
    }

    /// Resolve to a file on disk the editor can open.
    func materialize() throws -> URL {
        switch self {
        case .file(let url):
            guard FileManager.default.fileExists(atPath: url.path) else { throw Failure.missing(url.path) }
            return url
        case .session(let id, let dir, let name):
            let src = dir.appendingPathComponent("\(id).json")
            guard let data = FileManager.default.contents(atPath: src.path) else { throw Failure.missing(src.path) }
            return try Self.handOff(data, name: name ?? id)
        case .inline(let data, let name):
            return try Self.handOff(data, name: name ?? "Shared")
        }
    }

    /// Write `data` (must be a JSON object) to a private hand-off file.
    static func handOff(_ data: Data, name: String) throws -> URL {
        guard (try? JSONSerialization.jsonObject(with: data)) is [String: Any] else { throw Failure.notADocument }
        let dir = FileManager.default.temporaryDirectory
            .appendingPathComponent("vcad-handoff/\(UUID().uuidString)", isDirectory: true)
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let safe = name.replacingOccurrences(of: "[^A-Za-z0-9 _.-]", with: "_", options: .regularExpression)
        let file = dir.appendingPathComponent("\(safe.isEmpty ? "Shared" : safe).vcad")
        try data.write(to: file, options: .atomic)
        return file
    }

    /// base64url → gunzip. Same encoding the MCP server and the old web
    /// `#/new?doc=` links use.
    static func decodeInline(_ s: String) -> Data? {
        var b64 = s.replacingOccurrences(of: "-", with: "+").replacingOccurrences(of: "_", with: "/")
        while b64.count % 4 != 0 { b64 += "=" }
        guard let gz = Data(base64Encoded: b64) else { return nil }
        return gunzip(gz)
    }

    /// Minimal RFC 1952 reader: skip the header, inflate the raw DEFLATE body.
    static func gunzip(_ gz: Data) -> Data? {
        let b = [UInt8](gz)
        guard b.count > 18, b[0] == 0x1f, b[1] == 0x8b, b[2] == 8 else { return nil }
        let flags = b[3]
        var i = 10
        if flags & 0x04 != 0 { guard i + 2 <= b.count else { return nil }; i += 2 + Int(b[i]) | Int(b[i + 1]) << 8 }
        if flags & 0x08 != 0 { while i < b.count && b[i] != 0 { i += 1 }; i += 1 }
        if flags & 0x10 != 0 { while i < b.count && b[i] != 0 { i += 1 }; i += 1 }
        if flags & 0x02 != 0 { i += 2 }
        guard i < b.count - 8 else { return nil }
        let body = Data(b[i..<(b.count - 8)])
        guard let out = try? (body as NSData).decompressed(using: .zlib) else { return nil }
        return out as Data
    }
}

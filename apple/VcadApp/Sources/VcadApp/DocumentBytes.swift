import CVcadFFI
import Foundation
import os

// A document path → `.vcad` JSON bytes (with the `.loon` compile cache).
// Moved out of PoseStudio.swift unchanged so the model layer — and the
// visionOS app, which shares it but not the pose studio's UI — can load
// documents without the studio.

extension EditorModel {
    /// The document at `path` as `.vcad` JSON bytes. A `.loon` file is compiled
    /// by the kernel, with its modules and relative mesh imports resolved
    /// against its own directory; anything else is read as-is.
    nonisolated static func documentBytes(path: String) -> Data? {
        let bytes = Array(path.utf8)
        guard path.lowercased().hasSuffix(".loon") else {
            guard let data = try? Data(contentsOf: URL(fileURLWithPath: path)) else { return nil }
            // JSON passes straight through; VCode (`# vcad 0.2`) and loon-source
            // `.vcad` files go through the kernel's format-aware loader.
            let first = data.first { !($0 == 0x20 || $0 == 0x0A || $0 == 0x0D || $0 == 0x09) }
            if first == UInt8(ascii: "{") || first == nil { return data }
            guard let c = bytes.withUnsafeBufferPointer({ vcad_document_file_to_json($0.baseAddress, $0.count) })
            else { return nil }
            defer { vcad_cam_free(c) }
            return Data(bytes: c, count: strlen(c))
        }
        // A .loon compile is a full evaluation (an ECAD board resolves every
        // KiCad symbol and footprint), and this is called from every scene
        // rebuild, export and graph parse. Uncached, a 111-part board was
        // recompiled on the main thread on each SwiftUI update and never
        // settled. Memoize on (path, mtime, size): the file watch's reload
        // changes the mtime, so hot reload still sees the new source.
        let key = loonCacheKey(path)
        if let key, let hit = loonCache.withLock({ $0[path] }), hit.key == key { return hit.data }
        guard let c = bytes.withUnsafeBufferPointer({ vcad_loon_file_to_json($0.baseAddress, $0.count) })
        else { return nil }
        defer { vcad_cam_free(c) }
        let data = Data(bytes: c, count: strlen(c))
        if let key { loonCache.withLock { $0[path] = (key, data) } }
        return data
    }

    private nonisolated static let loonCache = OSAllocatedUnfairLock(initialState: [String: (key: String, data: Data)]())

    /// Identity of a .loon file's current contents for the compile cache.
    /// `[use]` modules beside it are not tracked: editing only a module needs
    /// the main file touched to recompile.
    private nonisolated static func loonCacheKey(_ path: String) -> String? {
        guard let a = try? FileManager.default.attributesOfItem(atPath: path),
              let m = a[.modificationDate] as? Date, let s = a[.size] as? NSNumber else { return nil }
        return "\(m.timeIntervalSince1970):\(s.intValue)"
    }

    /// The open document is generated loon source: read-only, watched.
    var isLoonDocument: Bool {
        if case let .document(path, _) = source { return path.lowercased().hasSuffix(".loon") }
        return false
    }
}

import CVcadFFI
import Foundation

// The loon compiler front: compile a `.loon` file (or find a folder's entry
// file) and report where it failed. Moved out of LoonLoop.swift unchanged so
// the model layer — and the visionOS app, which has no FSEvents watch — can
// compile loon without the hot-reload loop.

/// A loon compile error with its source location, when one is attributable.
struct LoonDiagnostic: Error, Equatable, Sendable, Decodable {
    var message: String
    var file: String?
    var line: Int?
    var column: Int?

    /// `main.loon:12:5`, or just the file name, or nil.
    var location: String? {
        guard let file else { return nil }
        let name = (file as NSString).lastPathComponent
        switch (line, column) {
        case let (l?, c?): return "\(name):\(l):\(c)"
        case let (l?, nil): return "\(name):\(l)"
        default: return name
        }
    }
}

enum LoonCompiler {
    /// Compile a loon file to `.vcad` JSON, or say where it failed.
    nonisolated static func compile(path: String) -> Result<Data, LoonDiagnostic> {
        let bytes = Array(path.utf8)
        var diag: UnsafeMutablePointer<CChar>?
        let json = bytes.withUnsafeBufferPointer {
            vcad_loon_file_to_json_diag($0.baseAddress, $0.count, &diag)
        }
        if let json {
            defer { vcad_cam_free(json) }
            return .success(Data(bytes: json, count: strlen(json)))
        }
        guard let diag else {
            return .failure(LoonDiagnostic(message: vcadLastError("did not compile"), file: path))
        }
        defer { vcad_cam_free(diag) }
        let data = Data(bytes: diag, count: strlen(diag))
        let decoded = (try? JSONDecoder().decode(LoonDiagnostic.self, from: data))
            ?? LoonDiagnostic(message: String(decoding: data, as: UTF8.self), file: path)
        return .failure(decoded)
    }

    /// The program a folder stands for: `main.loon`, else `<folder>.loon`,
    /// else its only top-level `.loon` file.
    static func entryFile(inFolder dir: URL) -> URL? {
        let fm = FileManager.default
        for name in ["main.loon", "\(dir.lastPathComponent).loon"] {
            let u = dir.appendingPathComponent(name)
            if fm.fileExists(atPath: u.path) { return u }
        }
        let loons = (try? fm.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil))?
            .filter { $0.pathExtension.lowercased() == "loon" } ?? []
        return loons.count == 1 ? loons[0] : nil
    }

    /// Directories whose loon files feed a program at `path`: its own
    /// directory, then each `$VCAD_LOON_PATH` entry that exists.
    static func watchedDirectories(for path: String,
                                   environment: [String: String] = ProcessInfo.processInfo.environment) -> [String] {
        var dirs = [(path as NSString).deletingLastPathComponent]
        if let lib = environment["VCAD_LOON_PATH"] {
            for d in lib.split(separator: ":").map(String.init) where !d.isEmpty {
                var isDir: ObjCBool = false
                if FileManager.default.fileExists(atPath: d, isDirectory: &isDir), isDir.boolValue,
                   !dirs.contains(d) {
                    dirs.append(d)
                }
            }
        }
        return dirs
    }
}

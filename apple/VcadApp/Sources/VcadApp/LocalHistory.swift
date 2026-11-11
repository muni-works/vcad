import CryptoKit
import Darwin
import Foundation

// Local data trust: safe writes, autosave, crash recovery, version history.
//
// vcad has no cloud, so the disk is the only place work lives. Everything in
// this file is plain Foundation (no UI, no FFI) so the guarantees are unit
// tested directly — see LocalHistoryTests.
//
// Layout under `~/Library/Application Support/vcad/` (override the root with
// `VCAD_DATA_DIR`, which the tests and screenshot runs use):
//
//   Autosave/<id>.vcad        latest unsaved state of one open document
//   Autosave/<id>.json        AutosaveEntry: whose it is (pid), what file, when
//   History/<key>/index.json  VersionIndex for one document (key = path hash)
//   History/<key>/<vid>.vcad  one snapshot's bytes

// MARK: - Safe write

/// Crash- and full-disk-safe replacement of a file's contents.
///
/// Bytes go to a temp file in the SAME directory (so the final step is a
/// same-volume `rename(2)`, which is atomic), are `fsync`ed, read back and
/// compared, and only then renamed over the target. A failure at any stage —
/// ENOSPC mid-write, a crash, a short write — leaves the original untouched
/// and removes the temp file. `Data.write(.atomic)` does the rename but not
/// the fsync or the verify, so a power cut can still leave a zero-length file
/// on APFS.
enum SafeWriter {
    enum Failure: Error, Equatable { case create(Int32), write(Int32), sync(Int32), verify, rename(Int32) }

    /// Stages a write can be interrupted at, for the kill-mid-save tests.
    enum Stage { case afterPartialWrite, afterSync, beforeRename }

    /// Test hook: simulate the process dying at a stage. The temp file is left
    /// exactly as a real crash would leave it (no cleanup runs).
    nonisolated(unsafe) static var crashAt: Stage?
    struct SimulatedCrash: Error {}

    static func write(_ data: Data, to url: URL) throws {
        let dir = url.deletingLastPathComponent()
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let tmp = dir.appendingPathComponent(".\(url.lastPathComponent).vcad-tmp-\(UUID().uuidString.prefix(8))")
        let fd = open(tmp.path, O_WRONLY | O_CREAT | O_EXCL, 0o644)
        guard fd >= 0 else { throw Failure.create(errno) }
        var closed = false
        var crashed = false
        defer {
            if !closed { close(fd) }
            if !crashed { unlink(tmp.path) }      // no-op after a successful rename
        }
        let ok: Bool = data.withUnsafeBytes { raw in
            guard var p = raw.baseAddress else { return true }
            var left = raw.count
            if crashAt == .afterPartialWrite { left = left / 2 }
            while left > 0 {
                let n = Darwin.write(fd, p, left)
                if n < 0 { if errno == EINTR { continue }; return false }
                p += n; left -= n
            }
            return true
        }
        if crashAt == .afterPartialWrite { crashed = true; throw SimulatedCrash() }
        guard ok else { throw Failure.write(errno) }
        // F_FULLFSYNC flushes the drive's cache too; fall back to fsync on
        // filesystems that don't support it.
        if fcntl(fd, F_FULLFSYNC) != 0, fsync(fd) != 0 { throw Failure.sync(errno) }
        close(fd); closed = true
        if crashAt == .afterSync { crashed = true; throw SimulatedCrash() }
        guard (try? Data(contentsOf: tmp)) == data else { throw Failure.verify }
        if crashAt == .beforeRename { crashed = true; throw SimulatedCrash() }
        guard rename(tmp.path, url.path) == 0 else { throw Failure.rename(errno) }
        // Persist the directory entry so the rename itself survives power loss.
        let dfd = open(dir.path, O_RDONLY)
        if dfd >= 0 { fsync(dfd); close(dfd) }
    }

    /// Temp files a crash left behind next to `url`. Harmless, but cleaned on
    /// the next successful save of the same file.
    static func strayTemps(for url: URL) -> [URL] {
        let dir = url.deletingLastPathComponent()
        let prefix = ".\(url.lastPathComponent).vcad-tmp-"
        let names = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        return names.filter { $0.hasPrefix(prefix) }.map { dir.appendingPathComponent($0) }
    }
}

// MARK: - Storage root

enum LocalData {
    /// Root of vcad's local data. `VCAD_DATA_DIR` overrides (tests, screenshots).
    static var root: URL {
        if let o = overrideRoot { return o }
        if let env = ProcessInfo.processInfo.environment["VCAD_DATA_DIR"], !env.isEmpty {
            return URL(fileURLWithPath: env, isDirectory: true)
        }
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("vcad", isDirectory: true)
    }
    nonisolated(unsafe) static var overrideRoot: URL?

    static let encoder: JSONEncoder = {
        let e = JSONEncoder(); e.dateEncodingStrategy = .iso8601
        e.outputFormatting = [.prettyPrinted, .sortedKeys]; return e
    }()
    static let decoder: JSONDecoder = {
        let d = JSONDecoder(); d.dateDecodingStrategy = .iso8601; return d
    }()

    static func sha(_ data: Data) -> String {
        SHA256.hash(data: data).map { String(format: "%02x", $0) }.joined()
    }
    static func sha(_ s: String) -> String { sha(Data(s.utf8)) }
}

// MARK: - Autosave + crash recovery

/// Sidecar record for one autosaved document.
struct AutosaveEntry: Codable, Equatable, Identifiable {
    var id: String
    /// The file this is unsaved work FOR; nil for an untitled document.
    var documentPath: String?
    var name: String
    var savedAt: Date
    /// The instance that owns it. A live pid means "still being edited";
    /// a dead one means it crashed (or was killed) with unsaved work.
    var pid: Int32
}

/// Autosaves one document. Writes run on a private serial queue (never the
/// main thread) and are debounced: a burst of edits costs one write, and a
/// pause of `delay` seconds — or `maxLag` of continuous editing — flushes it.
/// So at most a few seconds of work is ever at risk.
final class Autosaver: @unchecked Sendable {
    let id: String
    let dir: URL
    let delay: TimeInterval
    let maxLag: TimeInterval
    private let queue = DispatchQueue(label: "io.vcad.autosave", qos: .utility)
    private let lock = NSLock()
    private var pending: (() -> Data?, String?, String)?
    private var firstPendingAt: Date?
    private var timer: DispatchWorkItem?
    private(set) var writes = 0

    init(id: String = UUID().uuidString, root: URL = LocalData.root, delay: TimeInterval = 2, maxLag: TimeInterval = 5) {
        self.id = id
        self.dir = root.appendingPathComponent("Autosave", isDirectory: true)
        self.delay = delay; self.maxLag = maxLag
    }

    var dataURL: URL { dir.appendingPathComponent("\(id).vcad") }
    var metaURL: URL { dir.appendingPathComponent("\(id).json") }

    /// Note an edit. `snapshot` is called on the autosave queue, so the
    /// caller hands over an immutable copy of the document, not live state.
    func schedule(name: String, documentPath: String?, snapshot: @escaping () -> Data?) {
        lock.lock(); defer { lock.unlock() }
        pending = (snapshot, documentPath, name)
        let now = Date()
        if firstPendingAt == nil { firstPendingAt = now }
        timer?.cancel()
        let overdue = now.timeIntervalSince(firstPendingAt!) >= maxLag
        let item = DispatchWorkItem { [weak self] in self?.flushNow() }
        timer = item
        queue.asyncAfter(deadline: .now() + (overdue ? 0 : delay), execute: item)
    }

    /// Write whatever is pending, synchronously on the autosave queue.
    func flush() { queue.sync { flushNow() } }

    private func flushNow() {
        lock.lock()
        let job = pending; pending = nil; firstPendingAt = nil; timer = nil
        lock.unlock()
        guard let (snapshot, path, name) = job, let data = snapshot() else { return }
        do {
            try SafeWriter.write(data, to: dataURL)
            let entry = AutosaveEntry(id: id, documentPath: path, name: name, savedAt: Date(),
                                      pid: ProcessInfo.processInfo.processIdentifier)
            try SafeWriter.write(LocalData.encoder.encode(entry), to: metaURL)
            lock.lock(); writes += 1; lock.unlock()
        } catch {
            FileHandle.standardError.write(Data("[vcad] autosave failed: \(error)\n".utf8))
        }
    }

    /// The document was saved (or deliberately discarded): nothing to recover.
    func discard() {
        lock.lock(); timer?.cancel(); timer = nil; pending = nil; firstPendingAt = nil; lock.unlock()
        queue.sync {
            try? FileManager.default.removeItem(at: dataURL)
            try? FileManager.default.removeItem(at: metaURL)
        }
    }
}

enum Recovery {
    /// Autosaves whose owning instance is gone — work a crash left behind.
    /// Entries owned by a live process (another open vcad window) are not
    /// offered: that document is still being edited.
    static func orphans(root: URL = LocalData.root, isAlive: (Int32) -> Bool = Recovery.processAlive) -> [AutosaveEntry] {
        let dir = root.appendingPathComponent("Autosave", isDirectory: true)
        let names = (try? FileManager.default.contentsOfDirectory(atPath: dir.path)) ?? []
        return names.filter { $0.hasSuffix(".json") }.compactMap { name -> AutosaveEntry? in
            guard let d = try? Data(contentsOf: dir.appendingPathComponent(name)),
                  let e = try? LocalData.decoder.decode(AutosaveEntry.self, from: d),
                  FileManager.default.fileExists(atPath: dir.appendingPathComponent("\(e.id).vcad").path)
            else { return nil }
            return isAlive(e.pid) && e.pid != ProcessInfo.processInfo.processIdentifier ? nil : e
        }
        .sorted { $0.savedAt > $1.savedAt }
    }

    static func data(for e: AutosaveEntry, root: URL = LocalData.root) -> Data? {
        try? Data(contentsOf: root.appendingPathComponent("Autosave/\(e.id).vcad"))
    }

    /// Materialize a recovered autosave as a file the editor can open.
    /// Untitled work lands in `~/Documents/vcad Recovered/`; work for an
    /// existing file is written NEXT to it as "<name> (Recovered).vcad" so the
    /// original is never overwritten without the user choosing to.
    static func materialize(_ e: AutosaveEntry, root: URL = LocalData.root, recoveredDir: URL? = nil) throws -> URL {
        guard let data = data(for: e, root: root) else { throw CocoaError(.fileNoSuchFile) }
        let dir: URL
        if let p = e.documentPath { dir = URL(fileURLWithPath: p).deletingLastPathComponent() }
        else { dir = recoveredDir ?? FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("vcad Recovered", isDirectory: true) }
        var url = dir.appendingPathComponent("\(e.name) (Recovered).vcad")
        var n = 2
        while FileManager.default.fileExists(atPath: url.path) {
            url = dir.appendingPathComponent("\(e.name) (Recovered \(n)).vcad"); n += 1
        }
        try SafeWriter.write(data, to: url)
        discard(e, root: root)
        return url
    }

    static func discard(_ e: AutosaveEntry, root: URL = LocalData.root) {
        let dir = root.appendingPathComponent("Autosave", isDirectory: true)
        try? FileManager.default.removeItem(at: dir.appendingPathComponent("\(e.id).vcad"))
        try? FileManager.default.removeItem(at: dir.appendingPathComponent("\(e.id).json"))
    }

    static func processAlive(_ pid: Int32) -> Bool {
        pid > 0 && (kill(pid, 0) == 0 || errno == EPERM)
    }
}

// MARK: - Version history

struct DocumentVersion: Codable, Equatable, Identifiable {
    enum Kind: String, Codable { case auto, named, beforeRestore }
    var id: String
    var kind: Kind
    /// User-given name for `.named`; nil otherwise.
    var name: String?
    var createdAt: Date
    var byteCount: Int
    var sha256: String

    var title: String {
        switch kind {
        case .named: return name ?? "Untitled Version"
        case .beforeRestore: return "Before restore"
        case .auto: return "Autosaved version"
        }
    }
}

struct VersionIndex: Codable, Equatable {
    var documentPath: String
    var versions: [DocumentVersion] = []
}

/// Per-document version history in a sidecar store (not NSFileVersion: that
/// API only records versions for NSDocument-managed saves, can't hold a user
/// name, and loses everything when the file is copied off the volume — a
/// sidecar keyed by path is simpler and fully under test).
///
/// Snapshots are content-addressed by hash, so an unchanged save adds nothing.
/// Auto snapshots are thinned: all from the last hour, one per hour for a
/// day, one per day after that, at most `maxAuto`. Named versions and
/// pre-restore safety copies are never pruned.
final class VersionHistory {
    let documentURL: URL
    let dir: URL
    let maxAuto: Int

    init(documentURL: URL, root: URL = LocalData.root, maxAuto: Int = 100) {
        // Resolve symlinks so /tmp vs /private/tmp (or any alias path) share one history.
        self.documentURL = documentURL.standardizedFileURL.resolvingSymlinksInPath()
        self.dir = root.appendingPathComponent("History", isDirectory: true)
            .appendingPathComponent(String(LocalData.sha(self.documentURL.path).prefix(24)), isDirectory: true)
        self.maxAuto = maxAuto
    }

    private var indexURL: URL { dir.appendingPathComponent("index.json") }
    func url(for v: DocumentVersion) -> URL { dir.appendingPathComponent("\(v.id).vcad") }

    var index: VersionIndex {
        guard let d = try? Data(contentsOf: indexURL),
              let i = try? LocalData.decoder.decode(VersionIndex.self, from: d) else {
            return VersionIndex(documentPath: documentURL.path)
        }
        return i
    }

    /// Newest first.
    var versions: [DocumentVersion] { index.versions.sorted { $0.createdAt > $1.createdAt } }

    /// Record a snapshot. An auto snapshot identical to the newest version is
    /// skipped (returns that version); named versions always get an entry.
    @discardableResult
    func record(_ data: Data, kind: DocumentVersion.Kind = .auto, name: String? = nil, at date: Date = Date()) throws -> DocumentVersion {
        var idx = index
        let hash = LocalData.sha(data)
        if kind == .auto, let newest = idx.versions.max(by: { $0.createdAt < $1.createdAt }), newest.sha256 == hash {
            return newest
        }
        let v = DocumentVersion(id: "\(Int(date.timeIntervalSince1970 * 1000))-\(UUID().uuidString.prefix(6))",
                                kind: kind, name: name, createdAt: date, byteCount: data.count, sha256: hash)
        try SafeWriter.write(data, to: url(for: v))
        idx.versions.append(v)
        prune(&idx, now: date)
        try SafeWriter.write(LocalData.encoder.encode(idx), to: indexURL)
        return v
    }

    /// Bytes of a version, verified against its recorded hash (a corrupted
    /// snapshot reads as nil rather than restoring garbage).
    func data(for v: DocumentVersion) -> Data? {
        guard let d = try? Data(contentsOf: url(for: v)), LocalData.sha(d) == v.sha256 else { return nil }
        return d
    }

    func rename(_ v: DocumentVersion, to name: String) throws {
        var idx = index
        guard let i = idx.versions.firstIndex(where: { $0.id == v.id }) else { return }
        idx.versions[i].name = name; idx.versions[i].kind = .named
        try SafeWriter.write(LocalData.encoder.encode(idx), to: indexURL)
    }

    func delete(_ v: DocumentVersion) throws {
        var idx = index
        idx.versions.removeAll { $0.id == v.id }
        try? FileManager.default.removeItem(at: url(for: v))
        try SafeWriter.write(LocalData.encoder.encode(idx), to: indexURL)
    }

    /// Restore `v` over the document file: the current file is first recorded
    /// as a `.beforeRestore` version, so a restore is itself undoable.
    func restore(_ v: DocumentVersion) throws {
        guard let d = data(for: v) else { throw CocoaError(.fileReadCorruptFile) }
        if let current = try? Data(contentsOf: documentURL) { try record(current, kind: .beforeRestore) }
        try SafeWriter.write(d, to: documentURL)
        try record(d)
    }

    /// Write `v` to a new file (Duplicate as New Document).
    func duplicate(_ v: DocumentVersion, to url: URL) throws {
        guard let d = data(for: v) else { throw CocoaError(.fileReadCorruptFile) }
        try SafeWriter.write(d, to: url)
    }

    private func prune(_ idx: inout VersionIndex, now: Date) {
        let autos = idx.versions.filter { $0.kind == .auto }.sorted { $0.createdAt > $1.createdAt }
        var keep = Set<String>()
        var buckets = Set<String>()
        for v in autos {
            let age = now.timeIntervalSince(v.createdAt)
            let bucket: String
            if age < 3600 { bucket = v.id }
            else if age < 86_400 { bucket = "h\(Int(v.createdAt.timeIntervalSince1970 / 3600))" }
            else { bucket = "d\(Int(v.createdAt.timeIntervalSince1970 / 86_400))" }
            if buckets.insert(bucket).inserted, keep.count < maxAuto { keep.insert(v.id) }
        }
        let drop = autos.filter { !keep.contains($0.id) }
        for v in drop { try? FileManager.default.removeItem(at: url(for: v)) }
        let dropIDs = Set(drop.map(\.id))
        idx.versions.removeAll { dropIDs.contains($0.id) }
    }
}

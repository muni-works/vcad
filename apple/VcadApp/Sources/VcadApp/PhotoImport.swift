#if os(macOS)
import AppKit
import CVcadFFI
import Foundation
import ImageIO
import SwiftUI
import UniformTypeIdentifiers

// Import from Photos (EXPERIMENTAL): photographs of one part beside a printed
// tag36h11 AprilTag become an editable, parametric document — every fitted
// dimension a named parameter with its ±σ, not a mesh. The reconstruction is
// irys-recon, reached through vcad-ffi's `vcad_recon_check` (seconds: decode,
// EXIF focal length, tag, camera) and the cancellable job
// `vcad_recon_start` → `vcad_recon_poll` / `vcad_recon_cancel` →
// `vcad_recon_finish` (minutes). The panel polls the job for stage and
// step/of on the main actor; Cancel stops the fit within one iteration and
// the job's "cancelled" result is a quiet return to idle.

/// What the user is told about the evidence. Keep in step with the paper:
/// synthetic photos only, no real-photo test yet.
enum PhotoImportCopy {
    static let title = "Import from Photos"
    static let badge = "Experimental"
    static let requirement =
        "Take at least 3 photos of the part from different sides. Every photo needs a printed AprilTag (family tag36h11) lying flat beside the part, whole and in focus, and must be the camera's original file: the EXIF focal length sets the scale. Measure the tag's black square and enter it below: every dimension scales with it, so a wrong tag size makes the whole part the wrong size."
    static let scope =
        "For small machined parts photographed on a table: plates and blocks with sharp, filleted or chamfered corners, and holes, counterbores, slots, bosses and hole patterns on top. It can't model furniture, organic shapes or large objects; those come back as the nearest plate."
    static let evidence =
        "Tested on synthetic renders only: with three views, 15 of 20 parts came back within 1 mm and 10 of 20 within 0.5 mm. It has not yet been tested on real photos — check every dimension before you make anything."
    /// Shown on every result. Neither the fit's ± nor its residual check can
    /// tell a right answer from a wrong one: on 100 held-out parts the check
    /// caught 4 of 37 wrong answers, because most wrong shapes still explain
    /// the photos well. So no result is ever presented as confident.
    static let resultCaveat =
        "This is a best guess and can be wrong even when it matches the photos closely: a missed hole or a wrong-sized plate looks no different here. Measure every dimension against the part before you make anything."
    /// Fewer than `PhotoImportFiles.minimum` photos: refused, not attempted.
    static func tooFew(_ n: Int) -> String {
        "Choose at least \(PhotoImportFiles.minimum) photos, each with the AprilTag in frame (you chose \(n)). One photo is not reliable: in tests 6 of 20 parts came back within 1 mm from one photo, 15 of 20 from three."
    }
}

/// File types the importer accepts (HEIC is transcoded to JPEG first).
enum PhotoImportFiles {
    /// Photos needed before a reconstruction starts.
    static let minimum = 3
    static let extensions: Set<String> = ["jpg", "jpeg", "png", "heic", "heif"]
    static func isPhoto(_ url: URL) -> Bool { extensions.contains(url.pathExtension.lowercased()) }
    static var contentTypes: [UTType] {
        [.jpeg, .png, .heic] + [UTType(filenameExtension: "heif")].compactMap { $0 }
    }
}

/// One fitted dimension.
struct ReconDimension: Identifiable, Equatable, Sendable {
    let name: String        // irys-recon's (`base.w`)
    let parameter: String   // the document parameter (`base_w`)
    let value: Double
    let sigma: Double?
    let unit: String
    var id: String { parameter }

    var valueText: String { String(format: unit == "deg" ? "%.1f°" : "%.2f mm", value) }
    var sigmaText: String {
        guard let sigma else { return "—" }
        return String(format: unit == "deg" ? "±%.1f°" : "±%.2f mm", sigma)
    }
    /// σ above 0.5 mm: the photos don't pin this one.
    var uncertain: Bool { unit == "mm" && (sigma ?? .infinity) > 0.5 }
}

/// The reconstruction, decoded from `vcad_recon_photos`.
struct ReconResult: Sendable {
    let documentJSON: Data
    let program: String
    let dimensions: [ReconDimension]
    let warnings: [String]
    let lowConfidence: Bool
    let seconds: Double
    /// Photos the fit used.
    let photoCount: Int

    init(json: Data) throws {
        guard let obj = try JSONSerialization.jsonObject(with: json) as? [String: Any],
              let doc = obj["document"] else { throw ReconFFI.Failure("The reconstruction returned no document.") }
        documentJSON = try JSONSerialization.data(withJSONObject: doc, options: [.sortedKeys])
        program = obj["program"] as? String ?? ""
        dimensions = (obj["dimensions"] as? [[String: Any]] ?? []).map {
            ReconDimension(name: $0["name"] as? String ?? "", parameter: $0["parameter"] as? String ?? "",
                           value: ($0["value"] as? NSNumber)?.doubleValue ?? .nan,
                           sigma: ($0["sigma"] as? NSNumber)?.doubleValue,
                           unit: $0["unit"] as? String ?? "mm")
        }
        warnings = obj["warnings"] as? [String] ?? []
        lowConfidence = obj["low_confidence"] as? Bool ?? true
        seconds = (obj["seconds"] as? NSNumber)?.doubleValue ?? 0
        photoCount = (obj["photos"] as? [Any])?.count ?? 0
    }
}

/// The C ABI, off the main actor (both calls block).
enum ReconFFI {
    struct Failure: Error, LocalizedError, Equatable {
        let message: String
        init(_ message: String) { self.message = message }
        var errorDescription: String? { message }
    }

    static func request(_ photos: [URL], tagMM: Double) -> String {
        let obj: [String: Any] = ["photos": photos.map(\.path), "tag_mm": tagMM]
        let data = (try? JSONSerialization.data(withJSONObject: obj)) ?? Data("{}".utf8)
        return String(decoding: data, as: UTF8.self)
    }

    private static func call(_ body: String, _ fn: (UnsafePointer<CChar>) -> UnsafeMutablePointer<CChar>?) throws -> Data {
        // The last-error slot is thread-local: read it on this same thread.
        let raw = body.withCString { fn($0) }
        guard let raw else { throw Failure(SimError.pending() ?? "The reconstruction failed for an unknown reason.") }
        defer { vcad_string_free(raw) }
        return Data(String(cString: raw).utf8)
    }

    /// Decode, EXIF focal length, tag, camera — per photo. Seconds.
    static func check(_ photos: [URL], tagMM: Double = 50) throws -> [[String: Any]] {
        let data = try call(request(photos, tagMM: tagMM)) { vcad_recon_check($0) }
        return (try JSONSerialization.jsonObject(with: data) as? [[String: Any]]) ?? []
    }

    /// The reconstruction, blocking. Minutes; prefer `Job`.
    static func reconstruct(_ photos: [URL], tagMM: Double = 50) throws -> ReconResult {
        try ReconResult(json: call(request(photos, tagMM: tagMM)) { vcad_recon_photos($0) })
    }

    /// The error `vcad_recon_finish` reports after a cancel.
    static let cancelled = "cancelled"

    /// Where a running reconstruction is.
    struct Progress: Equatable, Sendable {
        var stage: Int32 = 0
        var step: UInt32 = 0
        var of: UInt32 = 0

        var label: String {
            switch stage {
            case 1: "Calibrating photo"
            case 2: "Fitting appearance"
            case 3: "Searching shapes"
            case 4: "Refining finalists"
            case 5: "Final polish"
            default: "Starting"
            }
        }
        /// 0…1 within the stage, nil while there is nothing to count.
        var fraction: Double? {
            guard of > 0 else { return nil }
            return min(1, Double(step) / Double(of))
        }
        var stepText: String { of > 0 ? "\(step) of \(of)" : "" }
    }

    /// A reconstruction running on its own Rust thread. The handle is only
    /// touched through the C ABI, whose poll/cancel are thread-safe; `finish`
    /// must not run on two threads at once (the controller calls it once).
    final class Job: @unchecked Sendable {
        private let handle: OpaquePointer

        init(_ photos: [URL], tagMM: Double) throws {
            let body = ReconFFI.request(photos, tagMM: tagMM)
            guard let h = body.withCString({ vcad_recon_start($0) }) else {
                throw Failure(SimError.pending() ?? "Couldn't start the reconstruction.")
            }
            handle = h
        }

        deinit { vcad_recon_job_free(handle) }

        /// Non-blocking; any thread.
        func poll() -> (Progress, done: Bool) {
            var p = VcadReconProgress()
            let done = vcad_recon_poll(handle, &p) == 1
            return (Progress(stage: p.stage, step: p.step, of: p.of), done)
        }

        /// Returns at once; the fit stops within one iteration.
        func cancel() { vcad_recon_cancel(handle) }

        /// Blocks until the job ends: call off the main actor. Throws
        /// `Failure(ReconFFI.cancelled)` after a cancel.
        func finish() throws -> ReconResult {
            // The last-error slot is thread-local: read it on this thread.
            guard let raw = vcad_recon_finish(handle) else {
                throw Failure(SimError.pending() ?? "The reconstruction failed for an unknown reason.")
            }
            defer { vcad_string_free(raw) }
            return try ReconResult(json: Data(String(cString: raw).utf8))
        }
    }

    /// HEIC/HEIF (an iPhone's default) → JPEG beside the import cache, EXIF
    /// kept (the focal length lives there). JPEG/PNG pass through.
    static func prepare(_ photos: [URL], in dir: URL) throws -> [URL] {
        try photos.map { url in
            let ext = url.pathExtension.lowercased()
            guard ext == "heic" || ext == "heif" else { return url }
            try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            let out = dir.appendingPathComponent(url.deletingPathExtension().lastPathComponent + ".jpg")
            guard let src = CGImageSourceCreateWithURL(url as CFURL, nil),
                  let dst = CGImageDestinationCreateWithURL(out as CFURL, UTType.jpeg.identifier as CFString, 1, nil)
            else { throw Failure("Couldn't read \(url.lastPathComponent).") }
            CGImageDestinationAddImageFromSource(dst, src, 0, [kCGImageDestinationLossyCompressionQuality: 0.95] as CFDictionary)
            guard CGImageDestinationFinalize(dst) else { throw Failure("Couldn't convert \(url.lastPathComponent) to JPEG.") }
            return out
        }
    }

    /// Where reconstructed documents and transcoded photos go.
    static var cacheDir: URL {
        FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("vcad/imports", isDirectory: true)
    }
}

/// State for File ▸ Import from Photos….
@MainActor @Observable
final class PhotoImportController {
    static let shared = PhotoImportController()

    enum Phase: Equatable {
        case idle
        case checking
        case fitting(since: Date)
        case done
        case failed(String)
    }

    private(set) var phase: Phase = .idle
    private(set) var photos: [URL] = []
    private(set) var result: ReconResult?
    private(set) var calibration: [String] = []
    /// The running job's stage and step/of (polled on the main actor).
    private(set) var progress = ReconFFI.Progress()
    private var job: ReconFFI.Job?
    /// The printed tag's black square edge, mm (the kit prints 50 mm).
    var tagMM: Double = 50
    /// Bumped on every start/cancel; a finished call whose generation is stale
    /// is discarded (that is what Cancel does, since the call can't stop).
    private var generation = 0
    private var window: NSPanel?
    private weak var model: EditorModel?

    var running: Bool {
        switch phase { case .checking, .fitting: true; default: false }
    }

    /// Show the panel (File menu / palette) and ask for photos if none yet.
    func show(for model: EditorModel) {
        self.model = model
        presentWindow()
        if photos.isEmpty && !running { choosePhotos() }
    }

    /// Photos dropped on the window: show the panel and start.
    func begin(_ urls: [URL], model: EditorModel) {
        self.model = model
        presentWindow()
        start(urls)
    }

    func choosePhotos() {
        let panel = NSOpenPanel()
        panel.allowedContentTypes = PhotoImportFiles.contentTypes
        panel.allowsMultipleSelection = true
        panel.message = "Choose at least 3 photos of one part with a tag36h11 AprilTag in every frame"
        panel.prompt = "Reconstruct"
        let go = { [weak self] in if !panel.urls.isEmpty { self?.start(panel.urls) } }
        if let window { panel.beginSheetModal(for: window) { if $0 == .OK { go() } } }
        else if panel.runModal() == .OK { go() }
    }

    func start(_ urls: [URL]) {
        let urls = urls.filter(PhotoImportFiles.isPhoto)
        guard !urls.isEmpty else {
            phase = .failed("Those aren't photos. Choose JPEG, PNG or HEIC files.")
            return
        }
        guard urls.count >= PhotoImportFiles.minimum else {
            photos = urls
            result = nil
            phase = .failed(PhotoImportCopy.tooFew(urls.count))
            return
        }
        generation += 1
        let gen = generation
        photos = urls
        result = nil
        calibration = []
        phase = .checking
        let tag = tagMM
        let cache = ReconFFI.cacheDir.appendingPathComponent("photos", isDirectory: true)
        Task.detached(priority: .userInitiated) {
            // Summarised here: the raw JSON dictionaries aren't Sendable.
            let checked = Result { () throws -> ([URL], [String]) in
                let prepared = try ReconFFI.prepare(urls, in: cache)
                let cals = try ReconFFI.check(prepared, tagMM: tag).map { c in
                    String(format: "%@: tag %d found, corner fit %.2f px", c["name"] as? String ?? "photo",
                           (c["tag_id"] as? NSNumber)?.intValue ?? -1, (c["rms_px"] as? NSNumber)?.doubleValue ?? .nan)
                }
                return (prepared, cals)
            }
            await MainActor.run { [weak self] in self?.checked(checked, gen: gen) }
        }
    }

    private func checked(_ r: Result<([URL], [String]), Error>, gen: Int) {
        guard gen == generation else { return }
        switch r {
        case .failure(let e):
            phase = .failed(e.localizedDescription)
        case .success(let (prepared, cals)):
            calibration = cals
            progress = ReconFFI.Progress()
            let job: ReconFFI.Job
            do { job = try ReconFFI.Job(prepared, tagMM: tagMM) } catch {
                phase = .failed(error.localizedDescription)
                return
            }
            self.job = job
            phase = .fitting(since: Date())
            // Progress: poll on the main actor until the job is done (or
            // superseded). The Rust callback never touches Swift state.
            Task { @MainActor [weak self] in
                while let self, gen == self.generation {
                    let (p, done) = job.poll()
                    if p != self.progress { self.progress = p }
                    if done { break }
                    try? await Task.sleep(for: .milliseconds(150))
                }
            }
            // Result: wait off the main actor, hop back with it.
            Task.detached(priority: .userInitiated) {
                let fitted = Result { try job.finish() }
                await MainActor.run { [weak self] in self?.fitted(fitted, gen: gen) }
            }
        }
    }

    private func fitted(_ r: Result<ReconResult, Error>, gen: Int) {
        guard gen == generation else { return }
        job = nil
        switch r {
        case .failure(let e as ReconFFI.Failure) where e.message == ReconFFI.cancelled:
            phase = .idle
        case .failure(let e): phase = .failed(e.localizedDescription)
        case .success(let res):
            result = res
            phase = .done
        }
    }

    func cancel() {
        job?.cancel()
        job = nil
        generation += 1
        phase = .idle
        result = nil
    }

    /// Write the document to the import cache and open it like any `.vcad`.
    @discardableResult
    func openResult() -> URL? {
        guard let result else { return nil }
        let stem = photos.first?.deletingPathExtension().lastPathComponent ?? "photos"
        guard let url = Self.writeDocument(result, named: "\(stem) (from photos)") else {
            phase = .failed("Couldn't write the reconstructed document.")
            return nil
        }
        window?.orderOut(nil)
        if let model { AppInstance.opening(url, from: model) } else { AppInstance.open(document: url) }
        return url
    }

    static func writeDocument(_ result: ReconResult, named name: String) -> URL? {
        let dir = ReconFFI.cacheDir
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let url = dir.appendingPathComponent(name + ".vcad")
        do { try result.documentJSON.write(to: url, options: .atomic) } catch { return nil }
        return url
    }

    private func presentWindow() {
        if window == nil {
            let host = NSHostingController(rootView: PhotoImportSheet(controller: self))
            let panel = NSPanel(contentViewController: host)
            panel.title = "\(PhotoImportCopy.title) (\(PhotoImportCopy.badge))"
            panel.styleMask = [.titled, .closable, .utilityWindow]
            panel.isReleasedWhenClosed = false
            window = panel
            panel.center()
        }
        window?.makeKeyAndOrderFront(nil)
    }
}

struct PhotoImportSheet: View {
    @Bindable var controller: PhotoImportController

    var body: some View {
        VStack(alignment: .leading, spacing: 12) {
            HStack(spacing: 8) {
                Text(PhotoImportCopy.title).font(.headline)
                Text(PhotoImportCopy.badge.uppercased())
                    .font(.caption2.weight(.semibold))
                    .padding(.horizontal, 6).padding(.vertical, 2)
                    .background(Capsule().fill(Color.orange.opacity(0.2)))
                    .foregroundStyle(.orange)
            }
            Text(PhotoImportCopy.requirement).font(.callout)
            Text(PhotoImportCopy.scope).font(.caption).foregroundStyle(.secondary)
            Text(PhotoImportCopy.evidence).font(.caption).foregroundStyle(.secondary)
            Divider()
            if !controller.photos.isEmpty {
                Text(controller.photos.map(\.lastPathComponent).joined(separator: ", "))
                    .font(.caption.monospaced()).lineLimit(2).truncationMode(.middle)
            }
            status
            buttons
        }
        .padding(20)
        .frame(width: 460)
    }

    @ViewBuilder private var status: some View {
        switch controller.phase {
        case .idle:
            EmptyView()
        case .checking:
            HStack { ProgressView().controlSize(.small); Text("Reading photos and finding the tag…") }
        case .fitting(let since):
            VStack(alignment: .leading, spacing: 4) {
                ForEach(controller.calibration, id: \.self) { Text($0).font(.caption).foregroundStyle(.secondary) }
                let p = controller.progress
                if let f = p.fraction {
                    ProgressView(value: f)
                } else {
                    ProgressView().progressViewStyle(.linear)
                }
                TimelineView(.periodic(from: since, by: 1)) { ctx in
                    HStack {
                        Text(p.label).bold()
                        Text(p.stepText).monospacedDigit().foregroundStyle(.secondary)
                        Spacer()
                        Text("\(Int(ctx.date.timeIntervalSince(since))) s (usually a few minutes)")
                            .monospacedDigit().foregroundStyle(.secondary)
                    }
                    .font(.caption)
                }
            }
        case .failed(let message):
            Label(message, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.red).font(.callout).fixedSize(horizontal: false, vertical: true)
        case .done:
            if let r = controller.result { ResultTable(result: r) }
        }
    }

    @ViewBuilder private var buttons: some View {
        HStack {
            if !controller.running {
                Button("Choose Photos…") { controller.choosePhotos() }
                Text("Tag black square").font(.caption).foregroundStyle(.secondary)
                TextField("Tag size", value: $controller.tagMM, format: .number).frame(width: 52)
                    .help("Edge of the printed tag's black square, in mm. Every dimension scales with this.")
                Text("mm").font(.caption).foregroundStyle(.secondary)
            }
            Spacer()
            if controller.running {
                Button("Cancel") { controller.cancel() }.keyboardShortcut(.cancelAction)
            } else if let r = controller.result, controller.phase == .done {
                Button("Open as Document") { controller.openResult() }
                    .keyboardShortcut(.defaultAction)
            }
        }
    }
}

/// The fitted dimensions. No per-dimension ± or confidence flag is shown:
/// both looked like a correctness signal and neither is one.
private struct ResultTable: View {
    let result: ReconResult

    var body: some View {
        VStack(alignment: .leading, spacing: 6) {
            Label(PhotoImportCopy.resultCaveat, systemImage: "exclamationmark.triangle.fill")
                .foregroundStyle(.orange).font(.callout)
                .fixedSize(horizontal: false, vertical: true)
            Text(String(format: "Reconstructed in %.0f s from %d photos. Each dimension opens as a named parameter.", result.seconds, result.photoCount))
                .font(.caption).foregroundStyle(.secondary)
            Grid(alignment: .leading, horizontalSpacing: 16, verticalSpacing: 3) {
                GridRow {
                    Text("Parameter").bold(); Text("Value").bold()
                }.font(.caption)
                ForEach(result.dimensions) { d in
                    GridRow {
                        Text(d.parameter).font(.caption.monospaced())
                        Text(d.valueText).font(.caption.monospacedDigit())
                    }
                }
            }
        }
    }
}
#endif

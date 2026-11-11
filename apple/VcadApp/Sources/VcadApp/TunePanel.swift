import AVFoundation
import CVcadFFI
import Observation
import SwiftUI

// Tune: close a tuned instrument's receipts with the microphone.
//
// The document's `instrument` metadata (written by the design_instrument MCP
// tool) lists every bar's target, predicted and measured pitch. Strike a bar
// near the Mac: the mic tap feeds StrikeDetector (PitchDetector.swift), the
// strike is matched to the nearest bar within 60 cents, and the measurement
// lands on the document through the kernel (`vcad_instrument_call` "record")
// as one named undo step — that bar's claim flips from predicted to measured,
// Pass inside its tolerance and Fail outside.
//
// Fabrication buttons drive the existing print and CAM back ends as DRY RUNS:
// they write G-code files and never talk to a printer or mill.

/// One bar row, decoded from the kernel's receipt JSON.
struct TuneBar: Decodable, Identifiable, Equatable {
    var id: String { note }
    let note: String
    let part: String
    let targetHz: Double
    let predictedHz: Double
    let predictedCents: Double
    let measuredHz: Double?
    let centsError: Double?
    let toleranceCents: Double
    let state: String

    enum CodingKeys: String, CodingKey {
        case note, part, state
        case targetHz = "target_hz", predictedHz = "predicted_hz", predictedCents = "predicted_cents"
        case measuredHz = "measured_hz", centsError = "cents_error", toleranceCents = "tolerance_cents"
    }
}

struct TuneReport: Decodable, Equatable {
    let verdict: String
    let bars: [TuneBar]
}

enum InstrumentFFI {
    static func call(doc: Data, _ request: [String: Any]) throws -> Data {
        try AnalysisFFI.call(vcad_instrument_call, doc: doc, request: try AnalysisFFI.encode(request))
    }

    static func receipt(doc: Data) throws -> TuneReport {
        try JSONDecoder().decode(TuneReport.self, from: call(doc: doc, ["op": "receipt"]))
    }

    /// Record a strike; returns the patched document, the bar's note and the new report.
    static func record(doc: Data, hz: Double, note: String? = nil,
                       instrument: String = "vcad.app Tune (microphone, FFT + parabolic peak)") throws
        -> (document: [String: Any], note: String, report: TuneReport) {
        var req: [String: Any] = ["op": "record", "measured_hz": hz, "instrument": instrument,
                                  "measured_at": ISO8601DateFormatter().string(from: Date())]
        if let note { req["note"] = note }
        let out = try call(doc: doc, req)
        guard let obj = try JSONSerialization.jsonObject(with: out) as? [String: Any],
              let d = obj["document"] as? [String: Any],
              let n = obj["note"] as? String,
              let r = obj["receipt"] else { throw AnalysisFFI.Failure(message: "record returned no document") }
        let report = try JSONDecoder().decode(TuneReport.self, from: JSONSerialization.data(withJSONObject: r))
        return (d, n, report)
    }
}

/// A detection shown in the panel.
struct TuneDetection: Equatable {
    let hz: Double
    let note: String
    let cents: Double
    /// Bar it was recorded against, if any.
    let bar: String?
}

/// Box for the non-Sendable detector, confined to the audio tap's thread.
private final class DetectorBox: @unchecked Sendable {
    var detector: StrikeDetector?
}

@MainActor @Observable
final class TuneStore {
    static let shared = TuneStore()

    enum Mic: Equatable { case unknown, denied, idle, listening }

    var shown = false
    private(set) var mic: Mic = .unknown
    private(set) var report: TuneReport?
    private(set) var last: TuneDetection?
    private(set) var error: String?
    private(set) var fabLog: [String] = []
    private(set) var busy = false
    var selectedBar: String?

    @ObservationIgnored private var engine: AVAudioEngine?
    @ObservationIgnored private let box = DetectorBox()
    @ObservationIgnored weak var model: EditorModel?

    /// Whether the document carries a tuned instrument.
    static func hasInstrument(_ m: EditorModel) -> Bool { m.documentJSON?["instrument"] != nil }

    func refresh(_ m: EditorModel) {
        model = m
        guard Self.hasInstrument(m), let data = AnalysisStore.documentData(m) else { report = nil; return }
        do { report = try InstrumentFFI.receipt(doc: data); error = nil } catch { self.error = "\(error)" }
    }

    // MARK: Microphone

    func checkPermission() {
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: if mic == .unknown || mic == .denied { mic = .idle }
        case .denied, .restricted: mic = .denied
        default: mic = .unknown
        }
    }

    func toggleListening(_ m: EditorModel) {
        model = m
        if mic == .listening { stop(); return }
        switch AVCaptureDevice.authorizationStatus(for: .audio) {
        case .authorized: start()
        case .notDetermined:
            AVCaptureDevice.requestAccess(for: .audio) { granted in
                Task { @MainActor in
                    if granted { self.start() } else { self.mic = .denied }
                }
            }
        default:
            mic = .denied
        }
    }

    private func start() {
        let engine = AVAudioEngine()
        let input = engine.inputNode
        let format = input.outputFormat(forBus: 0)
        guard format.sampleRate > 0, format.channelCount > 0 else {
            error = "No microphone input is available."; mic = .idle; return
        }
        box.detector = StrikeDetector(sampleRate: format.sampleRate)
        let box = self.box
        input.installTap(onBus: 0, bufferSize: 2048, format: format) { buffer, _ in
            guard let ch = buffer.floatChannelData?[0], let det = box.detector else { return }
            let hits = det.process(UnsafeBufferPointer(start: ch, count: Int(buffer.frameLength)))
            for h in hits {
                Task { @MainActor in TuneStore.shared.heard(h) }
            }
        }
        do {
            try engine.start()
            self.engine = engine
            mic = .listening
            error = nil
        } catch {
            input.removeTap(onBus: 0)
            self.error = "Microphone failed to start: \(error.localizedDescription)"
            mic = .idle
        }
    }

    func stop() {
        engine?.inputNode.removeTap(onBus: 0)
        engine?.stop()
        engine = nil
        box.detector = nil
        if mic == .listening { mic = .idle }
    }

    // MARK: Strikes

    /// A strike was heard (or simulated): match and record it.
    func heard(_ e: PitchEstimate) {
        let (name, c) = PitchDetector.nearestNote(e.hz)
        guard let m = model, Self.hasInstrument(m) else {
            last = TuneDetection(hz: e.hz, note: name, cents: c, bar: nil)
            return
        }
        do {
            try record(hz: e.hz, note: nil, m)
            last = TuneDetection(hz: e.hz, note: name, cents: c, bar: lastRecorded)
        } catch {
            last = TuneDetection(hz: e.hz, note: name, cents: c, bar: nil)
            self.error = "\(error)"
        }
    }

    @ObservationIgnored private var lastRecorded: String?

    /// Store a measurement on the document (one named undo step).
    func record(hz: Double, note: String?, _ m: EditorModel) throws {
        guard let data = AnalysisStore.documentData(m) else { throw AnalysisFFI.Failure(message: "No document is open.") }
        let r = try InstrumentFFI.record(doc: data, hz: hz, note: note)
        _ = try AgentEdits.edit(m, name: "Tune \(r.note)") { try $0.loadIR(r.document) }
        lastRecorded = r.note
        selectedBar = r.note
        report = r.report
        error = nil
    }

    // MARK: Fabrication (dry runs)

    static func outputDir(_ m: EditorModel) -> URL {
        let docs = FileManager.default.urls(for: .documentDirectory, in: .userDomainMask)[0]
        let safe = m.documentName.replacingOccurrences(of: "/", with: "-")
        return docs.appendingPathComponent("vcad Fabrication/\(safe)", isDirectory: true)
    }

    func printPlate(_ plate: String, _ m: EditorModel) async {
        await fab(m) { data, dir in
            let path = dir.appendingPathComponent("\(plate)-plate.gcode").path
            let out = try InstrumentFFI.call(doc: data, ["op": "print", "plate": plate, "gcode_path": path,
                                                         "settings": ["printer": "bambu_p1s"]])
            let obj = (try? JSONSerialization.jsonObject(with: out) as? [String: Any]) ?? [:]
            let fits = (obj["fitsBed"] as? Bool) == true ? "fits P1S bed" : "DOES NOT fit the bed"
            return "Print \(plate) plate (dry run) → \(path) · \(fits)"
        }
    }

    func mill(_ note: String, _ m: EditorModel) async {
        await fab(m) { data, dir in
            let out = try InstrumentFFI.call(doc: data, ["op": "mill", "note": note, "out_dir": dir.path])
            let obj = (try? JSONSerialization.jsonObject(with: out) as? [String: Any]) ?? [:]
            let setups = (obj["setups"] as? [[String: Any]]) ?? []
            return "Mill \(note) (dry run) → " + setups.compactMap { $0["path"] as? String }.joined(separator: ", ")
        }
    }

    private func fab(_ m: EditorModel, _ body: @escaping @Sendable (Data, URL) throws -> String) async {
        guard let data = AnalysisStore.documentData(m) else { return }
        let dir = Self.outputDir(m)
        busy = true
        let result = await Task.detached(priority: .userInitiated) { () -> Result<String, Error> in
            Result {
                try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
                return try body(data, dir)
            }
        }.value
        busy = false
        switch result {
        case .success(let line): fabLog.insert(line, at: 0)
        case .failure(let e): error = "\(e)"
        }
    }
}

struct TunePanel: View {
    @Bindable var model: EditorModel
    @Bindable var store = TuneStore.shared

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            PanelHeader(title: "Tune", systemImage: "tuningfork") { store.stop(); store.shown = false }
            if let report = store.report {
                listen
                verdict(report)
                bars(report)
                fabrication
            } else {
                Text("This document has no tuned instrument. Ask the agent to design_instrument (e.g. “a glockenspiel in C major”), then strike the bars here.")
                    .font(.callout).foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
            }
            if let e = store.error {
                Text(e).font(.caption).foregroundStyle(.red).fixedSize(horizontal: false, vertical: true)
            }
        }
        .controlSize(.small)
        .padding(Theme.Space.m).frame(width: Theme.Width.toolInspector).panelSurface()
        .accessibilityIdentifier("tune-panel")
        .task(id: model.documentRevision) { store.refresh(model) }
        .onAppear { store.checkPermission() }
    }

    private var listen: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            HStack {
                Button {
                    store.toggleListening(model)
                } label: {
                    Label(store.mic == .listening ? "Listening…" : "Listen", systemImage: store.mic == .listening ? "waveform" : "mic")
                }
                .buttonStyle(.borderedProminent)
                .tint(store.mic == .listening ? .red : .accentColor)
                .accessibilityIdentifier("tune-listen")
                if store.mic == .listening { ProgressView().controlSize(.small) }
                Spacer()
            }
            if store.mic == .denied {
                Text("Microphone access is off. Turn it on for vcad in System Settings › Privacy & Security › Microphone.")
                    .font(.caption).foregroundStyle(.orange).fixedSize(horizontal: false, vertical: true)
            } else if store.mic == .listening {
                Text("Strike one bar at a time near the Mac.").font(.caption).foregroundStyle(.secondary)
            }
            if let d = store.last {
                HStack(spacing: 6) {
                    Text(String(format: "%.1f Hz", d.hz)).font(.title3.monospacedDigit()).fontWeight(.semibold)
                    Text("\(d.note) \(String(format: "%+.0f", d.cents))¢").foregroundStyle(.secondary)
                    if let b = d.bar { Text("→ \(b)").foregroundStyle(.secondary) }
                }
                .accessibilityIdentifier("tune-last")
            }
        }
    }

    private func verdict(_ r: TuneReport) -> some View {
        let measured = r.bars.filter { $0.state != "predicted" }.count
        let (text, color): (String, Color) = switch r.verdict {
        case "pass": ("All \(r.bars.count) bars measured in tune", .green)
        case "fail": ("Out of tune — see red bars", .red)
        default: ("\(measured)/\(r.bars.count) measured · rest predicted", .secondary)
        }
        return Label(text, systemImage: r.verdict == "pass" ? "checkmark.seal.fill" : r.verdict == "fail" ? "xmark.octagon.fill" : "seal")
            .font(.callout).foregroundStyle(color)
    }

    private func bars(_ r: TuneReport) -> some View {
        VStack(spacing: 2) {
            ForEach(r.bars) { b in
                Button { store.selectedBar = b.note } label: { row(b) }
                    .buttonStyle(.plain)
                    .background(RoundedRectangle(cornerRadius: 6).fill(store.selectedBar == b.note ? Color.accentColor.opacity(0.15) : .clear))
            }
        }
    }

    private func row(_ b: TuneBar) -> some View {
        let color: Color = switch b.state { case "measured_pass": .green; case "measured_fail": .red; default: .secondary }
        let symbol = switch b.state { case "measured_pass": "checkmark.circle.fill"; case "measured_fail": "xmark.circle.fill"; default: "circle.dashed" }
        return HStack(spacing: 8) {
            Image(systemName: symbol).foregroundStyle(color)
            Text(b.note).fontWeight(.semibold).frame(width: 34, alignment: .leading)
            Text(String(format: "%.1f Hz", b.targetHz)).foregroundStyle(.secondary).frame(width: 74, alignment: .trailing)
            Spacer(minLength: 4)
            if let m = b.measuredHz, let c = b.centsError {
                Text(String(format: "%.1f · %+.1f¢", m, c)).foregroundStyle(color)
            } else {
                Text("predicted").foregroundStyle(.tertiary)
            }
        }
        .font(.callout.monospacedDigit())
        .padding(.horizontal, 6).padding(.vertical, 3)
        .contentShape(Rectangle())
        .accessibilityElement(children: .combine)
        .accessibilityIdentifier("tune-bar-\(b.note)")
    }

    private var fabrication: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            Divider()
            Text("Fabricate (dry run — writes files, sends nothing)").font(.caption).foregroundStyle(.secondary)
            HStack {
                Button("Print bars") { Task { await store.printPlate("bars", model) } }
                Button("Print frame") { Task { await store.printPlate("frame", model) } }
                Button("Mill \(store.selectedBar ?? "bar")") {
                    if let n = store.selectedBar ?? store.report?.bars.first?.note { Task { await store.mill(n, model) } }
                }
                if store.busy { ProgressView().controlSize(.small) }
            }
            .disabled(store.busy)
            ForEach(store.fabLog.prefix(4), id: \.self) { l in
                Text(l).font(.caption2.monospaced()).foregroundStyle(.secondary).lineLimit(3).textSelection(.enabled)
            }
        }
    }
}

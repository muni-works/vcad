import AppKit
import MetricKit

// The app side of docs/privacy.md: the "Report a Bug" action and the calm
// next-launch prompt after a crash or hang. macOS delivers MetricKit
// diagnostics to the app on-device; we keep them in Application Support until
// the user decides. Nothing is ever sent — the only outbound step is the user
// opening a GitHub page in their browser and pressing Submit themselves.

@MainActor enum BugReporter {
    static func environment(workspace: String? = nil) -> FeedbackEnvironment {
        let info = Bundle.main.infoDictionary ?? [:]
        let os = ProcessInfo.processInfo.operatingSystemVersion
        return FeedbackEnvironment(
            appVersion: info["CFBundleShortVersionString"] as? String ?? "dev",
            build: info["CFBundleVersion"] as? String ?? "dev",
            macOS: "\(os.majorVersion).\(os.minorVersion).\(os.patchVersion)",
            chip: chip(),
            workspace: workspace ?? AppInstance.currentModel?.workspace.rawValue ?? "none")
    }

    /// "Apple M3 Pro" etc. — the CPU brand string, nothing machine-unique.
    static func chip() -> String {
        var size = 0
        sysctlbyname("machdep.cpu.brand_string", nil, &size, nil, 0)
        guard size > 0 else { return "unknown" }
        var buf = [CChar](repeating: 0, count: size)
        sysctlbyname("machdep.cpu.brand_string", &buf, &size, nil, 0)
        return String(cString: buf)
    }

    /// Open a blank, pre-filled bug report in the browser.
    static func reportBug() {
        open(.init(title: "", environment: environment()))
    }

    static func open(_ draft: FeedbackReport.Draft) {
        let r = FeedbackReport.build(draft)
        if r.needsClipboard || !draft.diagnostics.isEmpty {
            NSPasteboard.general.clearContents()
            NSPasteboard.general.setString(FeedbackReport.scrub(draft.diagnostics), forType: .string)
        }
        NSWorkspace.shared.open(r.url)
    }
}

/// Receives MetricKit payloads and holds crash/hang diagnostics locally until
/// the user reports or dismisses them.
final class CrashDiagnostics: NSObject, MXMetricManagerSubscriber, @unchecked Sendable {
    static let shared = CrashDiagnostics()
    static let dontAskKey = "feedback.crashPrompt.dontAsk"

    static var directory: URL {
        let base = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask)[0]
        return base.appendingPathComponent("vcad/Diagnostics", isDirectory: true)
    }

    @MainActor func start() {
        MXMetricManager.shared.add(self)
        // Already-pending diagnostics from an earlier delivery.
        promptSoon()
    }

    func didReceive(_ payloads: [MXDiagnosticPayload]) {
        let dir = Self.directory
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        var stored = false
        for p in payloads {
            let data = p.jsonRepresentation()
            guard !DiagnosticSummary.parse(payloadJSON: data).isEmpty else { continue }
            let name = "diag-\(Int(p.timeStampEnd.timeIntervalSince1970))-\(UUID().uuidString.prefix(8)).json"
            stored = (try? data.write(to: dir.appendingPathComponent(name))) != nil || stored
        }
        if stored { DispatchQueue.main.async { MainActor.assumeIsolated { self.promptSoon() } } }
    }

    static func pending() -> [URL] {
        let files = (try? FileManager.default.contentsOfDirectory(at: directory, includingPropertiesForKeys: nil)) ?? []
        return files.filter { $0.pathExtension == "json" }.sorted { $0.lastPathComponent < $1.lastPathComponent }
    }

    /// Discard stored diagnostics one file at a time (never a glob).
    static func clear(_ files: [URL]) {
        for f in files { try? FileManager.default.removeItem(at: f) }
    }

    private var prompting = false

    @MainActor func promptSoon() {
        guard !UserDefaults.standard.bool(forKey: Self.dontAskKey), !prompting, !Self.pending().isEmpty else { return }
        // Same reason as RecoveryPrompt: let the window come up first.
        DispatchQueue.main.asyncAfter(deadline: .now() + 1.5) { MainActor.assumeIsolated { self.prompt() } }
    }

    @MainActor func prompt() {
        let files = Self.pending()
        guard !prompting, !files.isEmpty, !UserDefaults.standard.bool(forKey: Self.dontAskKey) else { return }
        prompting = true
        defer { prompting = false }
        let payloads = files.compactMap { try? Data(contentsOf: $0) }
        let summaries = payloads.flatMap { DiagnosticSummary.parse(payloadJSON: $0) }
        guard let first = summaries.first else { Self.clear(files); return }

        let a = NSAlert()
        a.messageText = first.kind == .crash ? "vcad quit unexpectedly last time." : "vcad stopped responding last time."
        a.informativeText = "Report it? This opens a pre-filled GitHub issue in your browser — you review it and decide whether to submit. Nothing is sent automatically."
        a.alertStyle = .informational
        a.addButton(withTitle: "Report…")
        a.addButton(withTitle: "Not Now")
        a.showsSuppressionButton = true
        a.suppressionButton?.title = "Don’t ask again"
        let response = a.runModal()
        if a.suppressionButton?.state == .on { UserDefaults.standard.set(true, forKey: Self.dontAskKey) }
        switch response {
        case .alertFirstButtonReturn:
            let more = summaries.count > 1 ? "\n\n(+\(summaries.count - 1) more diagnostic(s) in the clipboard JSON)" : ""
            let json = payloads.compactMap { String(data: $0, encoding: .utf8) }.joined(separator: "\n")
            BugReporter.open(.init(title: first.title, environment: BugReporter.environment(),
                                   whatHappened: first.text + more, diagnostics: json))
            Self.clear(files)
        default:
            // Not Now keeps them for next launch; Don't ask again drops them.
            if UserDefaults.standard.bool(forKey: Self.dontAskKey) { Self.clear(files) }
        }
    }

    #if DEBUG
    /// Debug-only: plant a fake crash diagnostic and show the prompt
    /// (`VCAD_FAKE_CRASH=1`), for screenshots and manual QA.
    @MainActor static func plantFake() {
        try? FileManager.default.createDirectory(at: directory, withIntermediateDirectories: true)
        try? fakePayload.data(using: .utf8)?.write(to: directory.appendingPathComponent("diag-0-fake.json"))
    }

    static let fakePayload = """
    {"crashDiagnostics":[{"diagnosticMetaData":{"appVersion":"1.0.0","appBuildVersion":"42","osVersion":"macOS 26.1",
    "exceptionType":1,"signal":11,"terminationReason":"Namespace SIGNAL, Code 11"},
    "callStackTree":{"callStacks":[{"threadAttributed":true,"callStackRootFrames":[{"binaryName":"vcad",
    "offsetIntoBinaryTextSegment":123456,"subFrames":[{"binaryName":"libvcad_ffi","offsetIntoBinaryTextSegment":4096,
    "subFrames":[{"binaryName":"libdyld.dylib","offsetIntoBinaryTextSegment":7}]}]}]}]}}]}
    """
    #endif
}

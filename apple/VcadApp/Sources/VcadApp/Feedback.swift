import Foundation

// Privacy-respecting feedback (docs/privacy.md). Everything in this file is
// pure: it builds a GitHub "new issue" URL the USER opens and submits, and
// formats MetricKit diagnostics into a short summary. Nothing here touches the
// network, and no file path or document content is ever included — the user
// adds those themselves if they want to.

/// The facts a bug report carries about the machine and build. No paths, no
/// document names, no identifiers.
struct FeedbackEnvironment: Equatable {
    var appVersion: String
    var build: String
    var macOS: String
    var chip: String
    var workspace: String

    var lines: String {
        """
        vcad \(appVersion) (\(build))
        macOS \(macOS)
        chip: \(chip)
        workspace: \(workspace)
        """
    }
}

/// Builds the pre-filled issue link. The URL targets the
/// `.github/ISSUE_TEMPLATE/bug_report.yml` form, whose field ids are the query
/// keys below, so the user lands on a filled-in form and presses Submit.
enum FeedbackReport {
    static let repo = "muni-works/vcad"
    static let template = "bug_report.yml"
    /// GitHub rejects request lines past ~8 KB; stay well under it.
    static let maxURLLength = 7_500
    static let clipboardNote = "(Full diagnostics were copied to your clipboard — paste them below.)"

    struct Draft: Equatable {
        var title: String
        var environment: FeedbackEnvironment
        /// Pre-filled "what happened" (e.g. a crash summary). Empty for a manual report.
        var whatHappened = ""
        /// Optional long text (full diagnostics). Never put in the URL when it
        /// would push it past the limit; the caller copies it to the clipboard.
        var diagnostics = ""
    }

    struct Result: Equatable {
        let url: URL
        /// True when something was cut to fit: the caller must copy
        /// `Draft.diagnostics` to the clipboard (the body already says so).
        let needsClipboard: Bool
    }

    static func build(_ draft: Draft) -> Result {
        var happened = scrub(draft.whatHappened)
        let diag = scrub(draft.diagnostics)
        let env = scrub(draft.environment.lines)
        var logs = diag
        var needsClipboard = false

        func make() -> URL {
            var c = URLComponents(string: "https://github.com/\(repo)/issues/new")!
            var items = [
                URLQueryItem(name: "template", value: template),
                URLQueryItem(name: "labels", value: "bug"),
                URLQueryItem(name: "title", value: draft.title),
                URLQueryItem(name: "environment", value: env),
            ]
            if !happened.isEmpty { items.append(URLQueryItem(name: "what-happened", value: happened)) }
            if !logs.isEmpty { items.append(URLQueryItem(name: "logs", value: logs)) }
            c.queryItems = items
            // URLComponents leaves `+` and `&`-adjacent characters ambiguous for
            // GitHub's parser; encode them strictly.
            c.percentEncodedQuery = c.percentEncodedQuery?
                .replacingOccurrences(of: "+", with: "%2B")
            return c.url!
        }

        var url = make()
        if url.absoluteString.count > maxURLLength, !logs.isEmpty {
            // Diagnostics go to the clipboard first — they're the long part.
            logs = clipboardNote
            needsClipboard = !diag.isEmpty
            url = make()
        }
        if url.absoluteString.count > maxURLLength {
            // Still too long: trim the summary until it fits.
            let suffix = "\n…(truncated)"
            needsClipboard = needsClipboard || !diag.isEmpty
            var keep = happened.count
            while url.absoluteString.count > maxURLLength, keep > 0 {
                keep = max(0, keep - max(64, (url.absoluteString.count - maxURLLength) / 2))
                happened = String(scrub(draft.whatHappened).prefix(keep)) + suffix
                url = make()
            }
        }
        return Result(url: url, needsClipboard: needsClipboard)
    }

    /// Replace anything that looks like a home-directory path, so a stray
    /// path in a diagnostic never names the user.
    static func scrub(_ s: String) -> String {
        guard s.contains("/Users/") else { return s }
        let re = try! NSRegularExpression(pattern: #"/Users/[^/\s"]+"#)
        return re.stringByReplacingMatches(in: s, range: NSRange(s.startIndex..., in: s), withTemplate: "~")
    }
}

/// A one-screen summary of a MetricKit diagnostic payload (crash or hang),
/// parsed from `MXDiagnosticPayload.jsonRepresentation()`.
struct DiagnosticSummary: Equatable {
    enum Kind: String { case crash, hang }
    var kind: Kind
    var appVersion: String?
    var build: String?
    var osVersion: String?
    /// e.g. "EXC_BAD_ACCESS (1) · SIGSEGV (11)" or "hang 4.2 s"
    var headline: String
    var terminationReason: String?
    /// Top frames of the crashing (or first) thread: "binary +0xoffset".
    var frames: [String]

    static let maxFrames = 12

    var title: String {
        kind == .crash ? "Crash: \(headline)" : "Hang: \(headline)"
    }

    var text: String {
        var out = [kind == .crash ? "vcad quit unexpectedly." : "vcad stopped responding.", headline]
        if let v = appVersion { out.append("app \(v)\(build.map { " (\($0))" } ?? "")") }
        if let os = osVersion { out.append("os \(os)") }
        if let r = terminationReason, !r.isEmpty { out.append("reason: \(r)") }
        if !frames.isEmpty {
            out.append("")
            out.append("top frames:")
            out.append(contentsOf: frames.enumerated().map { "\($0.offset)  \($0.element)" })
        }
        return out.joined(separator: "\n")
    }

    /// Every crash and hang in a payload. Malformed JSON yields an empty list.
    static func parse(payloadJSON data: Data) -> [DiagnosticSummary] {
        guard let root = try? JSONSerialization.jsonObject(with: data) as? [String: Any] else { return [] }
        var out: [DiagnosticSummary] = []
        for (key, kind) in [("crashDiagnostics", Kind.crash), ("hangDiagnostics", .hang)] {
            for d in root[key] as? [[String: Any]] ?? [] { out.append(summarize(d, kind: kind)) }
        }
        return out
    }

    static func summarize(_ d: [String: Any], kind: Kind) -> DiagnosticSummary {
        let meta = d["diagnosticMetaData"] as? [String: Any] ?? [:]
        func str(_ k: String) -> String? {
            if let s = meta[k] as? String { return s }
            if let n = meta[k] as? NSNumber { return n.stringValue }
            return nil
        }
        let headline: String
        if kind == .hang {
            headline = "hang \(str("hangDuration") ?? "unknown duration")"
        } else {
            let exc = str("exceptionType").map { "\(machException($0)) (\($0))" }
            let sig = str("signal").map { "\(signalName($0)) (\($0))" }
            headline = [exc, sig].compactMap { $0 }.joined(separator: " · ").nonEmpty ?? "unknown exception"
        }
        return DiagnosticSummary(
            kind: kind,
            appVersion: str("appVersion"), build: str("appBuildVersion"), osVersion: str("osVersion"),
            headline: headline,
            terminationReason: str("terminationReason").map(FeedbackReport.scrub),
            frames: topFrames(d["callStackTree"] as? [String: Any] ?? [:]))
    }

    /// The first `maxFrames` frames of the attributed (crashing) thread, or of
    /// the first thread. MetricKit nests each frame's caller under `subFrames`.
    static func topFrames(_ tree: [String: Any]) -> [String] {
        let stacks = tree["callStacks"] as? [[String: Any]] ?? []
        guard let stack = stacks.first(where: { ($0["threadAttributed"] as? Bool) == true }) ?? stacks.first,
              var frame = (stack["callStackRootFrames"] as? [[String: Any]])?.first else { return [] }
        var out: [String] = []
        while out.count < maxFrames {
            let name = (frame["binaryName"] as? String).map(FeedbackReport.scrub) ?? "???"
            if let off = (frame["offsetIntoBinaryTextSegment"] as? NSNumber)?.uint64Value {
                out.append("\(name) +0x\(String(off, radix: 16))")
            } else if let addr = (frame["address"] as? NSNumber)?.uint64Value {
                out.append("\(name) 0x\(String(addr, radix: 16))")
            } else {
                out.append(name)
            }
            guard let next = (frame["subFrames"] as? [[String: Any]])?.first else { break }
            frame = next
        }
        return out
    }

    static func machException(_ code: String) -> String {
        ["1": "EXC_BAD_ACCESS", "2": "EXC_BAD_INSTRUCTION", "3": "EXC_ARITHMETIC", "4": "EXC_EMULATION",
         "5": "EXC_SOFTWARE", "6": "EXC_BREAKPOINT", "10": "EXC_CRASH", "11": "EXC_RESOURCE",
         "12": "EXC_GUARD"][code] ?? "EXC"
    }

    static func signalName(_ code: String) -> String {
        ["4": "SIGILL", "5": "SIGTRAP", "6": "SIGABRT", "7": "SIGEMT", "8": "SIGFPE", "9": "SIGKILL",
         "10": "SIGBUS", "11": "SIGSEGV", "13": "SIGPIPE", "15": "SIGTERM"][code] ?? "SIG"
    }
}

private extension String {
    var nonEmpty: String? { isEmpty ? nil : self }
}

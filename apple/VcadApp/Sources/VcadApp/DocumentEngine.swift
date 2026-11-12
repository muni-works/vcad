import CVcadFFI
import Foundation

/// The shared Rust document engine (`vcad_app::DocumentApi`) over the C ABI.
///
/// Speaks the same `DocCommand` vocabulary as the web app's
/// `WasmDocumentEngine.apply`, so a mutation means the same thing on every
/// client. Commands, part info and the result envelope are the typed Swift
/// mirrors in `Generated/EngineTypes.swift`, generated from the Rust types
/// (`npm run swift:gen`); the materialized IR document stays raw JSON for the
/// existing `vcad_scene_from_json` path.
final class DocumentEngine: @unchecked Sendable {
    /// A command the engine refused (`CommandError` in Rust).
    struct Refusal: Error, CustomStringConvertible {
        /// `unknown_id`, `invalid`, `nothing_to`, `unsupported`, `batch`, or
        /// `ffi` when the call itself failed.
        let kind: String
        let message: String
        /// The structured error, when the engine sent one.
        var error: CommandError? = nil
        var description: String { message }
    }

    /// Materialized state after a successful command.
    struct Result {
        /// The materialized IR document — what the editor renders and reads.
        let document: [String: Any]
        /// The document as JSON bytes.
        var documentJSON: Data { (try? JSONSerialization.data(withJSONObject: document)) ?? Data() }
        /// Per materialized feature: its stable id, kind and IR node ids.
        let parts: [EnginePartInfo]
        /// Stable id of the feature the command created, if any.
        let createdFeatureId: String?
        var partCount: Int { parts.count }
        let warnings: [String]
        /// What the command changed, when it came back as a delta: the parts
        /// to re-tessellate (added + updated, by stable id) and those gone.
        /// Nil for a full state (load, restore, undo snapshot) — rebuild all.
        var changes: Changes? = nil

        struct Changes: Equatable {
            var rebuild: Set<String>
            var removed: Set<String>
        }

        /// Stable part id (`"replica:seq"`) of a scene root node, via the
        /// document's `partIds` map. Node ids are renumbered on every
        /// materialize; part ids survive, so they are what commands carry.
        func partId(forRootNode node: Int) -> String? {
            (document["partIds"] as? [String: Any])?[String(node)] as? String
        }

        /// The part that owns `node` and the node's role in its transform
        /// chain: `translate`, `rotate`, `scale`, or `primitive` (the leaf —
        /// primitive, boolean, fillet, …).
        func owner(ofNode node: Int) -> (part: EnginePartInfo, role: String)? {
            for part in parts {
                if part.translateNodeId == node { return (part, "translate") }
                if part.rotateNodeId == node { return (part, "rotate") }
                if part.scaleNodeId == node { return (part, "scale") }
                if part.leafNodeIds.contains(node) { return (part, "primitive") }
            }
            return nil
        }

        /// Material key of the scene root whose part id is `id`.
        func material(ofPart id: String) -> String? {
            let ids = document["partIds"] as? [String: Any] ?? [:]
            let roots = document["roots"] as? [[String: Any]] ?? []
            return roots.first { r in
                (r["root"] as? NSNumber).map { ids[$0.stringValue] as? String == id } ?? false
            }?["material"] as? String
        }

        /// The material new parts get: the document's first defined material,
        /// else the first part's, else `default` (the pre-engine editor rule).
        var defaultMaterial: String {
            if let m = (document["materials"] as? [String: Any])?.keys.sorted().first { return m }
            if let r = (document["roots"] as? [[String: Any]])?.first?["material"] as? String { return r }
            return "default"
        }

        /// Whether the document's `materials` table defines `key`.
        func definesMaterial(_ key: String) -> Bool {
            (document["materials"] as? [String: Any])?[key] != nil
        }
    }

    private let handle: OpaquePointer
    /// The last state handed out — the base the next delta patches.
    private var current: Result?

    init(replicaId: UInt64 = UInt64(Date().timeIntervalSince1970 * 1000)) {
        handle = vcad_engine_new(replicaId)
    }

    deinit {
        // Clears the sink and joins the runtime thread: no frame callback
        // runs after this returns, so the unretained `self` ctx never dangles.
        vcad_engine_free(handle)
    }

    // MARK: Runtime (docs/runtime-design.md)
    //
    // The engine handle is a `vcad_runtime::Runtime`: every call above runs
    // on its thread. `send` queues a fire-and-forget intent; the result
    // comes back as a frame through `onFrame`, decoded OFF the main thread.

    /// A frame, decoded off main: everything the editor adopts is already
    /// built. `@unchecked`: immutable once constructed, handed main-ward once.
    final class Frame: @unchecked Sendable {
        let rev: UInt64
        let ackedSeq: UInt64
        let result: Result?
        let documentBytes: Data
        let graph: DocumentGraph?
        let stateHash: String
        /// `feature_history` rows, decoded here so the tree redraw never
        /// waits on the runtime.
        let history: [HistoryRow]?
        /// Per visible root: subtree fingerprint, equal exactly when that
        /// part's geometry is unchanged ("" when unknown).
        let partKeys: [String]
        let errors: [String]
        let evalError: String?
        /// Visible-root scene, owned; free with `vcad_scene_free` (or let
        /// `deinit` do it).
        private(set) var scene: OpaquePointer?

        init(raw: OpaquePointer) {
            rev = vcad_frame_rev(raw)
            ackedSeq = vcad_frame_acked_seq(raw)
            let state = vcad_frame_state(raw).map { Data(String(cString: $0).utf8) } ?? Data()
            result = try? DocumentEngine.result(from: state)
            var len = 0
            if let p = vcad_frame_document_json(raw, &len) { documentBytes = Data(bytes: p, count: len) }
            else { documentBytes = Data() }
            graph = result.flatMap { DocumentGraph.parse($0.document, allowEmpty: true) }
            let info = vcad_frame_info(raw)
                .flatMap { try? JSONSerialization.jsonObject(with: Data(String(cString: $0).utf8)) as? [String: Any] } ?? [:]
            stateHash = info["stateHash"] as? String ?? ""
            partKeys = info["partKeys"] as? [String] ?? []
            history = (info["history"] as? String).flatMap { try? JSONDecoder().decode([HistoryRow].self, from: Data($0.utf8)) }
            errors = info["errors"] as? [String] ?? []
            evalError = info["evalError"] as? String
            scene = vcad_frame_take_scene(raw)
        }

        /// Take ownership of the scene.
        func takeScene() -> OpaquePointer? {
            defer { scene = nil }
            return scene
        }

        deinit { if let scene { vcad_scene_free(scene) } }
    }

    /// Frame handler, called on `frameQueue` (never main). Install once.
    var onFrame: (@Sendable (Frame) -> Void)? {
        didSet {
            if onFrame == nil { vcad_doc_runtime_set_sink(handle, nil, nil); return }
            let ctx = Unmanaged.passUnretained(self).toOpaque()
            vcad_doc_runtime_set_sink(handle, { ctx, raw in
                guard let ctx, let raw else { return }
                let engine = Unmanaged<DocumentEngine>.fromOpaque(ctx).takeUnretainedValue()
                let ptr = UInt(bitPattern: raw)
                // Decode on our own queue so the runtime thread goes straight
                // back to its inbox.
                engine.frameQueue.async { [weak engine] in
                    guard let raw = OpaquePointer(bitPattern: ptr) else { return }
                    defer { vcad_frame_free(raw) }
                    guard let engine, let handler = engine.onFrame else { return }
                    handler(Frame(raw: raw))
                }
            }, ctx)
        }
    }
    let frameQueue = DispatchQueue(label: "vcad.doc-runtime.frames", qos: .userInitiated)

    /// One fire-and-forget request.
    enum Intent {
        case setDocParam(name: String, value: Double, gesture: UInt64, start: Bool)
        case command(DocCommand, gesture: UInt64, start: Bool, coalesce: String?)
        case gestureEnd(UInt64)
        /// Publish the current state as a frame (no change).
        case publish
    }

    /// Seq of the last intent sent; `adoptedSeq` is the last one a frame the
    /// editor adopted folded in. Unequal = edits in flight.
    private(set) var sentSeq: UInt64 = 0
    var adoptedSeq: UInt64 = 0
    /// Seq of the last intent that changes the document (a `publish` doesn't,
    /// so it never makes the delta base look stale).
    private var lastChangeSeq: UInt64 = 0
    var intentsInFlight: Bool { lastChangeSeq > adoptedSeq }
    /// Runtime revision after this client's last blocking call.
    private(set) var lastRev: UInt64 = 0

    /// Queue an intent; returns its seq (0 if the runtime refused it).
    @discardableResult
    func send(_ intent: Intent) -> UInt64 {
        var body: [String: Any]
        switch intent {
        case let .setDocParam(name, value, gesture, start):
            body = ["type": "set_doc_param", "name": name, "value": value,
                    "gesture": ["id": gesture, "start": start]]
        case let .command(cmd, gesture, start, coalesce):
            guard let data = try? JSONEncoder().encode(cmd),
                  let obj = try? JSONSerialization.jsonObject(with: data) else { return 0 }
            body = ["type": "command", "command": obj, "gesture": ["id": gesture, "start": start]]
            if let coalesce { body["coalesce"] = coalesce }
        case let .gestureEnd(id):
            body = ["type": "gesture_end", "gesture": id]
        case .publish:
            body = ["type": "publish"]
        }
        sentSeq += 1
        if case .publish = intent {} else { lastChangeSeq = sentSeq }
        guard let data = try? JSONSerialization.data(withJSONObject: ["seq": sentSeq, "intent": body]) else { return 0 }
        let ok = data.withUnsafeBytes { vcad_doc_runtime_send(handle, $0.bindMemory(to: UInt8.self).baseAddress, data.count) }
        return ok ? sentSeq : 0
    }

    /// Adopt a frame's state as the base later deltas patch.
    func adopt(_ frame: Frame) {
        adoptedSeq = max(adoptedSeq, frame.ackedSeq)
        if let r = frame.result { current = r }
    }

    /// Undo — or, called again, redo — a whole gesture.
    func swapGesture(_ id: UInt64) throws -> Result {
        try track(Self.unwrap(vcad_doc_runtime_swap_gesture(handle, id)))
    }

    /// The runtime's scene for its current state (blocking; caller frees).
    func currentScene() -> OpaquePointer? { vcad_doc_runtime_scene(handle) }

    /// `vcad_sound_call` on the runtime's document and meshes. Blocks while
    /// it solves: call off main.
    func sound(_ request: Data) throws -> Data {
        let req = String(decoding: request, as: UTF8.self)
        guard let raw = req.withCString({ vcad_doc_runtime_sound(handle, $0) }) else {
            throw AnalysisFFI.Failure(message: AnalysisFFI.lastError() ?? "sound failed")
        }
        defer { vcad_string_free(raw) }
        return Data(bytes: raw, count: strlen(raw))
    }

    /// Drop a gesture's snapshot (its undo entry is gone).
    func forgetGesture(_ id: UInt64) { vcad_doc_runtime_forget_gesture(handle, id) }

    /// Where relative mesh imports resolve when frames evaluate.
    func setBaseDir(_ dir: [UInt8]) {
        dir.withUnsafeBufferPointer { vcad_doc_runtime_set_base_dir(handle, $0.baseAddress, $0.count) }
    }

    /// Apply one `DocCommand`, e.g. `.setMaterial(id: id, material: "steel")`.
    @discardableResult
    func apply(_ command: DocCommand) throws -> Result {
        let json = String(decoding: try JSONEncoder().encode(command), as: UTF8.self)
        guard let base = current, !intentsInFlight else {
            return try track(Self.unwrap(json.withCString { vcad_engine_apply(handle, $0) }))
        }
        return try track(Self.patch(base, json.withCString { vcad_engine_apply_delta(handle, $0) }))
    }

    /// Lower and apply an AI tool call (`create`, `translate`, …) in Rust.
    @discardableResult
    func applyTool(_ call: ToolCall) throws -> Result {
        let json = String(decoding: try JSONEncoder().encode(call.args), as: UTF8.self)
        guard let base = current, !intentsInFlight else {
            return try track(Self.unwrap(call.tool.withCString { t in json.withCString { vcad_engine_apply_tool(handle, t, $0) } }))
        }
        return try track(Self.patch(base, call.tool.withCString { t in
            json.withCString { vcad_engine_apply_tool_delta(handle, t, $0) }
        }))
    }

    /// Answer an agent read tool (`describe_scene`, `inspect`) as text.
    func agentRead(_ tool: String, _ args: [String: Any]) throws -> String {
        let json = String(decoding: try JSONSerialization.data(withJSONObject: args), as: UTF8.self)
        guard let raw = tool.withCString({ t in json.withCString { vcad_engine_agent_read(handle, t, $0) } }) else {
            throw Refusal(kind: "ffi", message: "agent read failed")
        }
        defer { vcad_engine_string_free(raw) }
        let envelope = try JSONSerialization.jsonObject(with: Data(String(cString: raw).utf8)) as? [String: Any] ?? [:]
        if envelope["ok"] as? Bool == true, let text = (envelope["result"] as? [String: Any])?["text"] as? String {
            return text
        }
        let error = envelope["error"] as? [String: Any] ?? [:]
        throw Refusal(kind: error["kind"] as? String ?? "unknown", message: error["message"] as? String ?? "read refused")
    }

    /// The agent's tool manifest + system prompt (`vcad_app::agent`).
    static func agentManifest() -> [String: Any] {
        guard let raw = vcad_agent_manifest() else { return [:] }
        defer { vcad_engine_string_free(raw) }
        return (try? JSONSerialization.jsonObject(with: Data(String(cString: raw).utf8))) as? [String: Any] ?? [:]
    }

    /// The current state (no mutation).
    func state() throws -> Result { try track(Self.unwrap(vcad_engine_state(handle))) }

    /// Replace the document with an IR `Document` (a `.vcad` file's
    /// contents); the undo history starts fresh. Throws, leaving the engine
    /// untouched, when the JSON isn't an IR document.
    @discardableResult
    func loadIR(_ document: [String: Any]) throws -> Result {
        let json = String(decoding: try JSONSerialization.data(withJSONObject: document), as: UTF8.self)
        return try track(Self.unwrap(json.withCString { vcad_engine_load_ir(handle, $0) }))
    }

    /// Snapshot of document + undo/redo history, for `restore`.
    func save() -> Data {
        var len = 0
        guard let bytes = vcad_engine_save(handle, &len) else { return Data() }
        defer { vcad_engine_bytes_free(bytes, len) }
        return Data(bytes: bytes, count: len)
    }

    /// Put back a `save` snapshot (document and history).
    @discardableResult
    func restore(_ snapshot: Data) throws -> Result {
        try track(Self.unwrap(snapshot.withUnsafeBytes {
            vcad_engine_restore(handle, $0.bindMemory(to: UInt8.self).baseAddress, snapshot.count)
        }))
    }

    var canUndo: Bool { vcad_engine_can_undo(handle) }
    var canRedo: Bool { vcad_engine_can_redo(handle) }

    /// Deterministic fingerprint of the materialized state — equal hashes
    /// mean equal documents (the editor's "unsaved changes" test).
    var stateHash: String {
        guard let raw = vcad_engine_state_hash(handle) else { return "" }
        defer { vcad_engine_string_free(raw) }
        return String(cString: raw)
    }

    private func track(_ r: Result) -> Result {
        current = r
        // A blocking edit supersedes every frame minted before it.
        lastRev = vcad_doc_runtime_rev(handle)
        adoptedSeq = sentSeq
        return r
    }

    private static func take(_ raw: UnsafeMutablePointer<CChar>?) throws -> Data {
        guard let raw else {
            var count = 0
            let message = vcad_last_error(&count).map { String(decoding: UnsafeBufferPointer(start: $0, count: count), as: UTF8.self) }
            throw Refusal(kind: "ffi", message: message ?? "engine call failed")
        }
        defer { vcad_engine_string_free(raw) }
        return Data(String(cString: raw).utf8)
    }

    private static func unwrap(_ raw: UnsafeMutablePointer<CChar>?) throws -> Result {
        try result(from: take(raw))
    }

    private static func patch(_ base: Result, _ raw: UnsafeMutablePointer<CChar>?) throws -> Result {
        try result(fromDelta: take(raw), base: base)
    }

    /// Decode an `EngineDeltaEnvelope` and fold it into `base` (the state the
    /// command started from).
    static func result(fromDelta envelope: Data, base: Result) throws -> Result {
        let decoded = try JSONDecoder().decode(EngineDeltaEnvelope.self, from: envelope)
        guard decoded.ok, let delta = decoded.result else {
            let error = decoded.error
            throw Refusal(kind: error?.commandError.kind ?? "unknown",
                          message: error?.message ?? "command refused",
                          error: error?.commandError)
        }
        return Result(
            document: delta.document.applied(to: base.document),
            parts: delta.parts.applied(to: base.parts),
            createdFeatureId: delta.createdFeatureId,
            warnings: delta.warnings,
            changes: .init(rebuild: Set((delta.parts.added + delta.parts.updated).map(\.id)),
                           removed: Set(delta.parts.removed))
        )
    }

    /// Decode an `EngineEnvelope`: a `Result`, or the `Refusal` it carries.
    static func result(from envelope: Data) throws -> Result {
        let decoded = try JSONDecoder().decode(EngineEnvelope.self, from: envelope)
        guard decoded.ok, let result = decoded.result else {
            let error = decoded.error
            throw Refusal(kind: error?.commandError.kind ?? "unknown",
                          message: error?.message ?? "command refused",
                          error: error?.commandError)
        }
        let document = result.document.foundationValue as? [String: Any] ?? [:]
        return Result(
            document: document,
            parts: result.parts,
            createdFeatureId: result.createdFeatureId,
            warnings: result.warnings
        )
    }
}

extension EnginePartInfo {
    /// The IR node ids of this part's leaf (everything but the transform
    /// chain): the primitive, boolean, fillet, sketch, … nodes.
    var leafNodeIds: [Int] {
        guard let payload = Mirror(reflecting: self).children.first?.value else { return [] }
        return Mirror(reflecting: payload).children.flatMap { child -> [Int] in
            guard let label = child.label,
                  label.hasSuffix("NodeId") || label.hasSuffix("NodeIds"),
                  !["translateNodeId", "rotateNodeId", "scaleNodeId"].contains(label) else { return [] }
            return (child.value as? Int).map { [$0] } ?? (child.value as? [Int]) ?? []
        }
    }
}

extension EngineJSON {
    /// The Foundation (`JSONSerialization`) form.
    var foundationValue: Any {
        switch self {
        case .null: return NSNull()
        case .bool(let v): return v
        case .number(let v): return v
        case .string(let v): return v
        case .array(let v): return v.map(\.foundationValue)
        case .object(let v): return v.mapValues(\.foundationValue)
        }
    }
}

extension EngineJSON: ExpressibleByStringLiteral, ExpressibleByFloatLiteral, ExpressibleByIntegerLiteral,
    ExpressibleByBooleanLiteral, ExpressibleByArrayLiteral, ExpressibleByDictionaryLiteral {
    public init(stringLiteral value: String) { self = .string(value) }
    public init(floatLiteral value: Double) { self = .number(value) }
    public init(integerLiteral value: Int) { self = .number(Double(value)) }
    public init(booleanLiteral value: Bool) { self = .bool(value) }
    public init(arrayLiteral elements: EngineJSON...) { self = .array(elements) }
    public init(dictionaryLiteral elements: (String, EngineJSON)...) {
        self = .object(Dictionary(elements, uniquingKeysWith: { _, last in last }))
    }
}

extension DocumentDelta {
    /// `document` with this patch applied (mirrors `DocumentDelta::apply`).
    func applied(to document: [String: Any]) -> [String: Any] {
        var out = document
        for key in removedFields { out.removeValue(forKey: key) }
        for (key, value) in fields { out[key] = value.foundationValue }
        for (key, patch) in maps {
            var map = out[key] as? [String: Any] ?? [:]
            for k in patch.remove { map.removeValue(forKey: k) }
            for (k, v) in patch.set { map[k] = v.foundationValue }
            out[key] = map
        }
        return out
    }
}

extension PartsDelta {
    /// `parts` with this patch applied (mirrors `PartsDelta::apply`).
    func applied(to parts: [EnginePartInfo]) -> [EnginePartInfo] {
        let ids = order ?? parts.map(\.id)
        var byId = Dictionary(parts.map { ($0.id, $0) }, uniquingKeysWith: { _, last in last })
        for p in added + updated { byId[p.id] = p }
        for id in removed { byId.removeValue(forKey: id) }
        return ids.compactMap { byId[$0] }
    }
}

/// One open document's incremental kernel evaluation (`VcadEvalSession`):
/// each `scene(data:dir:)` re-evaluates only the nodes whose subtree changed
/// since the previous call and re-tessellates only changed roots. The scene
/// equals `vcad_scene_from_json_in`.
final class EvalSession {
    private let handle: OpaquePointer

    init() { handle = vcad_eval_session_new() }
    deinit { vcad_eval_session_free(handle) }

    /// Evaluate a document (IR JSON bytes); caller frees with `vcad_scene_free`.
    func scene(data: Data, dir: [UInt8]) -> OpaquePointer? {
        data.withUnsafeBytes { raw in
            dir.withUnsafeBufferPointer { d in
                vcad_scene_from_json_session(handle, raw.bindMemory(to: UInt8.self).baseAddress, data.count,
                                             d.baseAddress, d.count)
            }
        }
    }

    /// Reuse counts from the last evaluation.
    var stats: (reusedSolids: Int, evaluatedSolids: Int, reusedMeshes: Int) {
        var r = 0, e = 0, m = 0
        vcad_eval_session_stats(handle, &r, &e, &m)
        return (r, e, m)
    }
}

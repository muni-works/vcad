import CVcadFFI
import Foundation
import simd

// Assembly + animation authoring as pure edits of the document IR JSON
// (instances, joints, mates, ground, timeline). The web app issues the same
// edits through engine `add_feature` verbs; natively the edited IR is reloaded
// into the engine (`EditorModel.assemblyEdit`), which migrates instances,
// joints, `mates` and `timeline` faithfully (vcad-app migrate.rs).
//
// Shapes are the canonical vcad-ir serde forms:
//   instance  {id, partDefId, name?, transform?{translation,rotation,scale}}
//   joint     {id, name?, parentInstanceId|null, childInstanceId,
//              parentAnchor, childAnchor, kind{type, axis?, limits?}, state}
//   mate      {id, name?, instanceA, instanceB, kind{type: Coaxial|PlanarOffset, …}}
//   timeline  {durationS, fps, tracks:[{target{type:"Joint", jointId}, keys:[{t,value,ease?}]}]}

/// Joint kinds the authoring UI offers (vcad-ir `JointKind`; "Slider" is the
/// prismatic joint).
enum JointKindChoice: String, CaseIterable, Identifiable {
    case fixed = "Fixed", revolute = "Revolute", slider = "Slider", cylindrical = "Cylindrical", ball = "Ball", free = "Free"
    var id: String { rawValue }
    var label: String { self == .slider ? "Prismatic" : self == .free ? "Free (floating)" : rawValue }
    var hasAxis: Bool { self == .revolute || self == .slider || self == .cylindrical }
    var hasLimits: Bool { self == .revolute || self == .slider }
    /// Unit of the driven state.
    var unit: String { self == .slider ? "mm" : "°" }
    var symbol: String {
        switch self {
        case .fixed: return "lock"
        case .revolute: return "arrow.triangle.2.circlepath"
        case .slider: return "arrow.up.and.down"
        case .cylindrical: return "rotate.3d"
        case .ball: return "globe"
        case .free: return "move.3d"
        }
    }
}

/// Parameters of a new or edited joint.
struct JointSpec: Equatable {
    var name: String? = nil
    var kind: JointKindChoice = .revolute
    var parentInstanceId: String? = nil
    var childInstanceId: String = ""
    var parentAnchor: SIMD3<Double> = .zero
    var childAnchor: SIMD3<Double> = .zero
    var axis: SIMD3<Double> = SIMD3(0, 0, 1)
    var limits: ClosedRange<Double>? = nil
    var state: Double = 0
}

enum MateKindChoice: String, CaseIterable, Identifiable {
    case coaxial = "Coaxial", planarOffset = "PlanarOffset"
    var id: String { rawValue }
    var label: String { self == .coaxial ? "Coaxial" : "Planar offset" }
}

struct MateSpec: Equatable {
    var name: String? = nil
    var kind: MateKindChoice = .coaxial
    var instanceA: String = ""
    var instanceB: String = ""
    var axis: SIMD3<Double> = SIMD3(0, 0, 1)
    var offset: Double = 0
}

enum AsmEdit {
    // MARK: reads

    static func instances(_ json: [String: Any]) -> [[String: Any]] {
        json["instances"] as? [[String: Any]] ?? []
    }
    static func joints(_ json: [String: Any]) -> [[String: Any]] {
        json["joints"] as? [[String: Any]] ?? []
    }
    static func mates(_ json: [String: Any]) -> [[String: Any]] {
        json["mates"] as? [[String: Any]] ?? []
    }
    /// Part definitions, sorted by id for a stable picker order.
    static func partDefs(_ json: [String: Any]) -> [(id: String, name: String, root: Int)] {
        ((json["partDefs"] as? [String: Any]) ?? [:]).compactMap { id, v in
            guard let d = v as? [String: Any], let root = (d["root"] as? NSNumber)?.intValue else { return nil }
            return (id, (d["name"] as? String) ?? id, root)
        }.sorted { $0.id < $1.id }
    }
    static func groundInstanceId(_ json: [String: Any]) -> String? { json["groundInstanceId"] as? String }

    static func instanceName(_ json: [String: Any], _ id: String?) -> String {
        guard let id else { return "World" }
        let inst = instances(json).first { $0["id"] as? String == id }
        return (inst?["name"] as? String) ?? id
    }

    /// A fresh id `prefix-N` not used by any instance/joint/mate/part def.
    static func freshId(_ json: [String: Any], prefix: String) -> String {
        var used = Set<String>()
        for k in ["instances", "joints", "mates"] {
            for o in (json[k] as? [[String: Any]]) ?? [] { if let s = o["id"] as? String { used.insert(s) } }
        }
        used.formUnion(((json["partDefs"] as? [String: Any]) ?? [:]).keys)
        var n = 1
        while used.contains("\(prefix)-\(n)") { n += 1 }
        return "\(prefix)-\(n)"
    }

    static func v3(_ v: SIMD3<Double>) -> [String: Any] { ["x": v.x, "y": v.y, "z": v.z] }
    static func vec(_ any: Any?) -> SIMD3<Double> {
        let d = any as? [String: Any] ?? [:]
        return SIMD3((d["x"] as? NSNumber)?.doubleValue ?? 0,
                     (d["y"] as? NSNumber)?.doubleValue ?? 0,
                     (d["z"] as? NSNumber)?.doubleValue ?? 0)
    }

    // MARK: part defs + instances

    /// Display name of the part rooted at `rootNodeId`: the first named node
    /// down the transform chain the engine wraps every feature in.
    static func partName(_ json: [String: Any], rootNodeId: Int) -> String? {
        let nodes = json["nodes"] as? [String: Any] ?? [:]
        var id: Int? = rootNodeId, hops = 0
        while let cur = id, hops < 64 {
            guard let node = nodes[String(cur)] as? [String: Any] else { return nil }
            if let n = node["name"] as? String, !n.isEmpty { return n }
            let op = node["op"] as? [String: Any] ?? [:]
            guard ["Translate", "Rotate", "Scale"].contains(op["type"] as? String ?? "") else { return nil }
            id = (op["child"] as? NSNumber)?.intValue
            hops += 1
        }
        return nil
    }

    /// The part def whose root is `rootNodeId`, creating one when the root has
    /// none (the web's "Create Part Definition"). Returns its id.
    @discardableResult
    static func ensurePartDef(_ json: inout [String: Any], rootNodeId: Int, name: String?) -> String {
        if let hit = partDefs(json).first(where: { $0.root == rootNodeId }) { return hit.id }
        var defs = json["partDefs"] as? [String: Any] ?? [:]
        let id = freshId(json, prefix: "part")
        var def: [String: Any] = ["id": id, "root": rootNodeId]
        if let name { def["name"] = name }
        defs[id] = def
        json["partDefs"] = defs
        return id
    }

    /// Place a new instance of `partDefId` at `translation`. Returns its id.
    @discardableResult
    static func insertInstance(_ json: inout [String: Any], partDefId: String, name: String? = nil,
                               translation: SIMD3<Double> = .zero) -> String {
        let id = freshId(json, prefix: "inst")
        var inst: [String: Any] = ["id": id, "partDefId": partDefId,
                                   "transform": ["translation": v3(translation),
                                                 "rotation": v3(.zero),
                                                 "scale": v3(SIMD3(1, 1, 1))]]
        let defName = partDefs(json).first { $0.id == partDefId }?.name ?? "Part"
        // Names must be unique: the engine re-keys instance ids on every
        // reload, so the name is the handle that survives an edit.
        let taken = Set(instances(json).compactMap { $0["name"] as? String })
        var n = 1
        while taken.contains("\(defName) \(n)") { n += 1 }
        inst["name"] = name.flatMap { taken.contains($0) ? nil : $0 } ?? "\(defName) \(n)"
        json["instances"] = instances(json) + [inst]
        return id
    }

    /// Remove an instance and everything that names it (joints, mates, joint
    /// tracks, ground), like the web's cascade delete.
    static func deleteInstance(_ json: inout [String: Any], id: String) {
        json["instances"] = instances(json).filter { $0["id"] as? String != id }
        let doomed = joints(json).filter {
            $0["childInstanceId"] as? String == id || $0["parentInstanceId"] as? String == id
        }.compactMap { $0["id"] as? String }
        for j in doomed { deleteJoint(&json, id: j) }
        json["mates"] = mates(json).filter {
            $0["instanceA"] as? String != id && $0["instanceB"] as? String != id
        }
        if groundInstanceId(json) == id { json.removeValue(forKey: "groundInstanceId") }
    }

    static func renameInstance(_ json: inout [String: Any], id: String, name: String) {
        json["instances"] = instances(json).map {
            guard $0["id"] as? String == id else { return $0 }
            var o = $0; o["name"] = name; return o
        }
    }

    static func setInstanceTranslation(_ json: inout [String: Any], id: String, _ t: SIMD3<Double>) {
        json["instances"] = instances(json).map {
            guard $0["id"] as? String == id else { return $0 }
            var o = $0
            var tf = o["transform"] as? [String: Any] ?? ["rotation": v3(.zero), "scale": v3(SIMD3(1, 1, 1))]
            tf["translation"] = v3(t)
            o["transform"] = tf
            return o
        }
    }

    static func setGround(_ json: inout [String: Any], id: String?) {
        if let id { json["groundInstanceId"] = id } else { json.removeValue(forKey: "groundInstanceId") }
    }

    // MARK: joints

    static func kindJSON(_ s: JointSpec) -> [String: Any] {
        var k: [String: Any] = ["type": s.kind.rawValue]
        if s.kind.hasAxis {
            let a = simd_length(s.axis) > 1e-12 ? simd_normalize(s.axis) : SIMD3(0, 0, 1)
            k["axis"] = v3(a)
        }
        if s.kind.hasLimits, let l = s.limits { k["limits"] = [l.lowerBound, l.upperBound] }
        return k
    }

    static func jointJSON(id: String, _ s: JointSpec) -> [String: Any] {
        var j: [String: Any] = [
            "id": id,
            "parentInstanceId": s.parentInstanceId.map { $0 as Any } ?? NSNull(),
            "childInstanceId": s.childInstanceId,
            "parentAnchor": v3(s.parentAnchor),
            "childAnchor": v3(s.childAnchor),
            "kind": kindJSON(s),
            "state": s.state,
        ]
        if let n = s.name, !n.isEmpty { j["name"] = n }
        return j
    }

    /// Read a joint back into a spec (canonical camelCase or legacy snake_case).
    static func jointSpec(_ j: [String: Any]) -> JointSpec {
        let kind = j["kind"] as? [String: Any] ?? [:]
        var s = JointSpec()
        s.name = j["name"] as? String
        s.kind = JointKindChoice(rawValue: kind["type"] as? String ?? "") ?? .fixed
        s.parentInstanceId = (j["parentInstanceId"] ?? j["parent_instance_id"]) as? String
        s.childInstanceId = ((j["childInstanceId"] ?? j["child_instance_id"]) as? String) ?? ""
        s.parentAnchor = vec(j["parentAnchor"] ?? j["parent_anchor"])
        s.childAnchor = vec(j["childAnchor"] ?? j["child_anchor"])
        if kind["axis"] != nil { s.axis = vec(kind["axis"]) }
        if let l = kind["limits"] as? [NSNumber], l.count == 2, l[0].doubleValue <= l[1].doubleValue {
            s.limits = l[0].doubleValue...l[1].doubleValue
        }
        s.state = (j["state"] as? NSNumber)?.doubleValue ?? 0
        return s
    }

    /// Validation the kernel FK would otherwise reject at solve time.
    static func validate(_ json: [String: Any], _ s: JointSpec, editing id: String? = nil) -> String? {
        let ids = Set(instances(json).compactMap { $0["id"] as? String })
        guard ids.contains(s.childInstanceId) else { return "Pick a child instance." }
        if let p = s.parentInstanceId {
            guard ids.contains(p) else { return "Unknown parent instance \(p)." }
            guard p != s.childInstanceId else { return "A joint can't connect an instance to itself." }
        }
        // Each child has exactly one joint placing it (FK is a tree).
        let clash = joints(json).first {
            ($0["childInstanceId"] as? String) == s.childInstanceId && ($0["id"] as? String) != id
        }
        if clash != nil { return "\(instanceName(json, s.childInstanceId)) is already placed by another joint." }
        // No cycles: walk parents from the proposed parent up.
        var cur = s.parentInstanceId, seen = Set<String>()
        while let c = cur, !seen.contains(c) {
            if c == s.childInstanceId { return "That joint would make a kinematic loop." }
            seen.insert(c)
            cur = joints(json).first {
                ($0["childInstanceId"] as? String) == c && ($0["id"] as? String) != id
            }.flatMap { $0["parentInstanceId"] as? String }
        }
        if s.kind.hasAxis, simd_length(s.axis) < 1e-9 { return "The joint axis can't be zero." }
        return nil
    }

    @discardableResult
    static func addJoint(_ json: inout [String: Any], _ s: JointSpec) -> String {
        var spec = s
        // The engine keys a joint by its name across reloads (an unnamed one
        // is re-keyed every edit, orphaning its timeline tracks), so every
        // joint gets a unique name.
        let names = Set(joints(json).compactMap { $0["name"] as? String })
        if spec.name?.isEmpty ?? true || names.contains(spec.name!) {
            var n = joints(json).count + 1
            while names.contains("joint-\(n)") { n += 1 }
            spec.name = "joint-\(n)"
        }
        json["joints"] = joints(json) + [jointJSON(id: spec.name!, spec)]
        return spec.name!
    }

    /// Replace a joint's fields, keeping its id (and so its timeline tracks).
    static func updateJoint(_ json: inout [String: Any], id: String, _ s: JointSpec) {
        json["joints"] = joints(json).map { $0["id"] as? String == id ? jointJSON(id: id, s) : $0 }
    }

    static func setJointState(_ json: inout [String: Any], id: String, _ v: Double) {
        json["joints"] = joints(json).map {
            guard $0["id"] as? String == id else { return $0 }
            var o = $0; o["state"] = v; return o
        }
    }

    static func deleteJoint(_ json: inout [String: Any], id: String) {
        json["joints"] = joints(json).filter { $0["id"] as? String != id }
        guard var tl = json["timeline"] as? [String: Any] else { return }
        tl["tracks"] = ((tl["tracks"] as? [[String: Any]]) ?? []).filter { !isJointTrack($0, id) }
        json["timeline"] = tl
    }

    // MARK: mates

    static func mateJSON(id: String, _ s: MateSpec) -> [String: Any] {
        let a = simd_length(s.axis) > 1e-12 ? simd_normalize(s.axis) : SIMD3(0, 0, 1)
        var kind: [String: Any] = ["type": s.kind.rawValue, "axis": v3(a)]
        if s.kind == .planarOffset { kind["offset"] = s.offset }
        var m: [String: Any] = ["id": id, "instanceA": s.instanceA, "instanceB": s.instanceB, "kind": kind]
        if let n = s.name, !n.isEmpty { m["name"] = n }
        return m
    }

    @discardableResult
    static func addMate(_ json: inout [String: Any], _ s: MateSpec) -> String {
        let id = freshId(json, prefix: "mate")
        json["mates"] = mates(json) + [mateJSON(id: id, s)]
        return id
    }

    static func deleteMate(_ json: inout [String: Any], id: String) {
        json["mates"] = mates(json).filter { $0["id"] as? String != id }
    }

    // MARK: timeline (keyframes)

    static let defaultDuration = 2.0
    static let keyEpsilon = 1e-6

    static func isJointTrack(_ t: [String: Any], _ jointId: String) -> Bool {
        let target = t["target"] as? [String: Any]
        return target?["type"] as? String == "Joint" && target?["jointId"] as? String == jointId
    }

    static func timeline(_ json: [String: Any]) -> [String: Any] {
        json["timeline"] as? [String: Any]
            ?? ["durationS": defaultDuration, "fps": 24.0, "tracks": [[String: Any]]()]
    }

    /// Mutate the joint track for `jointId` (created on demand), then
    /// canonicalize the timeline through the kernel's own type.
    private static func editTrack(_ json: inout [String: Any], jointId: String,
                                  _ f: (inout [[String: Any]]) -> Void) {
        var tl = timeline(json)
        var tracks = (tl["tracks"] as? [[String: Any]]) ?? []
        let idx = tracks.firstIndex { isJointTrack($0, jointId) }
        var keys = idx.flatMap { tracks[$0]["keys"] as? [[String: Any]] } ?? []
        f(&keys)
        let track: [String: Any] = ["target": ["type": "Joint", "jointId": jointId], "keys": keys]
        if let idx { tracks[idx] = track } else { tracks.append(track) }
        tl["tracks"] = tracks
        json["timeline"] = normalized(tl) ?? tl
    }

    private static func keyTime(_ k: [String: Any]) -> Double { (k["t"] as? NSNumber)?.doubleValue ?? -1 }

    /// Set (or replace) the key at `t`. Extends the duration to cover `t`.
    static func setKey(_ json: inout [String: Any], jointId: String, t: Double, value: Double,
                       ease: String = "linear") {
        var tl = timeline(json)
        let dur = (tl["durationS"] as? NSNumber)?.doubleValue ?? defaultDuration
        if t > dur { tl["durationS"] = t; json["timeline"] = tl }
        editTrack(&json, jointId: jointId) { keys in
            keys.removeAll { abs(keyTime($0) - t) < keyEpsilon }
            var k: [String: Any] = ["t": max(0, t), "value": value]
            if ease != "linear" { k["ease"] = ease }
            keys.append(k)
        }
    }

    /// Move the key at `from` to `to` (replacing any key already there).
    static func moveKey(_ json: inout [String: Any], jointId: String, from: Double, to: Double) {
        editTrack(&json, jointId: jointId) { keys in
            guard let i = keys.firstIndex(where: { abs(keyTime($0) - from) < keyEpsilon }) else { return }
            var k = keys.remove(at: i)
            keys.removeAll { abs(keyTime($0) - to) < keyEpsilon }
            k["t"] = to
            keys.append(k)
        }
    }

    static func setKeyEase(_ json: inout [String: Any], jointId: String, t: Double, ease: String) {
        editTrack(&json, jointId: jointId) { keys in
            guard let i = keys.firstIndex(where: { abs(keyTime($0) - t) < keyEpsilon }) else { return }
            if ease == "linear" { keys[i].removeValue(forKey: "ease") } else { keys[i]["ease"] = ease }
        }
    }

    static func deleteKey(_ json: inout [String: Any], jointId: String, t: Double) {
        editTrack(&json, jointId: jointId) { keys in
            keys.removeAll { abs(keyTime($0) - t) < keyEpsilon }
        }
        dropEmptyTimeline(&json)
    }

    static func setDuration(_ json: inout [String: Any], _ d: Double) {
        guard d > 0 else { return }
        var tl = timeline(json)
        tl["durationS"] = d
        json["timeline"] = normalized(tl) ?? tl
    }

    /// A timeline with no tracks left is no timeline (the pose studio returns).
    static func dropEmptyTimeline(_ json: inout [String: Any]) {
        guard let tl = json["timeline"] as? [String: Any] else { return }
        if ((tl["tracks"] as? [Any]) ?? []).isEmpty, ((tl["camera"] as? [Any]) ?? []).isEmpty {
            json.removeValue(forKey: "timeline")
        }
    }

    /// Canonical form via `vcad_timeline_normalize` (sorted, clamped, deduped).
    static func normalized(_ tl: [String: Any]) -> [String: Any]? {
        guard let data = try? JSONSerialization.data(withJSONObject: tl) else { return nil }
        let raw = String(decoding: data, as: UTF8.self).withCString { vcad_timeline_normalize($0) }
        guard let raw else { return nil }
        defer { vcad_string_free(raw) }
        return (try? JSONSerialization.jsonObject(with: Data(bytes: raw, count: strlen(raw)))) as? [String: Any]
    }
}

// MARK: mate checking (crates/vcad-ffi/src/assembly.rs)

struct MateVerdict: Decodable, Identifiable, Equatable {
    let id: String
    let kind: String
    let instances: [String]
    let pass: Bool
    let measured: Double
    let expected: Double
    let tolerance: Double
    let unit: String
    let detail: String
    let error: String?
}

enum AssemblyFFI {
    /// Verdicts for every mate in `doc`, or an error string.
    static func checkMates(_ doc: [String: Any]) -> Result<[MateVerdict], AnalysisFFI.Failure> {
        guard let data = try? JSONSerialization.data(withJSONObject: doc) else {
            return .failure(.init(message: "document is not serializable"))
        }
        let raw = String(decoding: data, as: UTF8.self).withCString { vcad_assembly_check_mates($0) }
        guard let raw else { return .failure(.init(message: AnalysisFFI.lastError() ?? "mate check failed")) }
        defer { vcad_string_free(raw) }
        do {
            return .success(try JSONDecoder().decode([MateVerdict].self, from: Data(bytes: raw, count: strlen(raw))))
        } catch {
            return .failure(.init(message: "mate report: \(error)"))
        }
    }
}

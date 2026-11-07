import XCTest
@testable import VcadApp

/// Parity gate: the web app's own example documents (exported verbatim from
/// packages/app/src/data/examples/*.vcad.ts into Tests/Fixtures/web-examples)
/// open natively, survive an edit + undo + save + reopen with their geometry
/// trees unchanged, and an edit that is kept persists through the file.
@MainActor
final class WebRoundTripTests: XCTestCase {
    private var dir: URL {
        URL(fileURLWithPath: #filePath).deletingLastPathComponent().deletingLastPathComponent()
            .appendingPathComponent("Fixtures/web-examples")
    }
    private var fixtures: [URL] {
        ((try? FileManager.default.contentsOfDirectory(at: dir, includingPropertiesForKeys: nil)) ?? [])
            .filter { $0.pathExtension == "vcad" }.sorted { $0.path < $1.path }
    }

    /// Each root as a node-id-free tree: child ids replaced by their subtrees,
    /// names dropped (display only). Engine materializes renumber nodes, so
    /// ids can't be compared; structure and every parameter can.
    static func canonical(_ doc: [String: Any]) -> [String] {
        let nodes = doc["nodes"] as? [String: Any] ?? [:]
        let childKeys: Set<String> = ["child", "left", "right", "sketch", "input", "parent"]
        func tree(_ id: Any?, depth: Int = 0) -> Any {
            guard depth < 256, let id = (id as? NSNumber)?.intValue,
                  let node = nodes[String(id)] as? [String: Any],
                  let op = node["op"] as? [String: Any] else { return NSNull() }
            // Identity transforms (the chain the engine wraps every feature
            // in) and default `closed: false` are geometrically inert.
            if let t = op["type"] as? String, let child = op["child"] as? NSNumber,
               Self.isIdentity(t, op) { return tree(child, depth: depth + 1) }
            var out: [String: Any] = [:]
            for (k, v) in op {
                if k == "closed", (v as? Bool) == false { continue }
                if childKeys.contains(k), v is NSNumber { out[k] = tree(v, depth: depth + 1) }
                else if ["profiles", "children", "sketches"].contains(k), let ids = v as? [Any] {
                    out[k] = ids.map { $0 is NSNumber ? tree($0, depth: depth + 1) : $0 }
                } else { out[k] = v }
            }
            return out
        }
        func json(_ v: Any) -> String {
            String(decoding: (try? JSONSerialization.data(withJSONObject: v, options: [.sortedKeys])) ?? Data(), as: UTF8.self)
        }
        // Assemblies: the part defs (as trees), instances, joints and ground
        // ARE the document; roots are derived from the defs. The engine
        // re-keys defs and instances, so everything is matched by name.
        let instances = doc["instances"] as? [[String: Any]] ?? []
        if !instances.isEmpty {
            let defs = doc["partDefs"] as? [String: Any] ?? [:]
            func defName(_ id: Any?) -> String {
                ((defs[id as? String ?? ""] as? [String: Any])?["name"] as? String) ?? "?"
            }
            func instName(_ id: Any?) -> String {
                (instances.first { ($0["id"] as? String) == (id as? String) }?["name"] as? String) ?? "?"
            }
            var out = defs.values.compactMap { $0 as? [String: Any] }.map { d -> String in
                "def " + json(["name": d["name"] ?? "", "material": d["defaultMaterial"] ?? "", "root": tree(d["root"])])
            }
            out += instances.map { i in
                "inst " + json(["name": i["name"] ?? "", "def": defName(i["partDefId"]), "transform": i["transform"] ?? NSNull()])
            }
            out += (doc["joints"] as? [[String: Any]] ?? []).map { j in
                "joint " + json(["name": j["name"] ?? "", "kind": j["kind"] ?? "", "state": j["state"] ?? 0,
                                 "parent": instName(j["parentInstanceId"]), "child": instName(j["childInstanceId"]),
                                 "pa": j["parentAnchor"] ?? "", "ca": j["childAnchor"] ?? ""])
            }
            out.append("ground " + instName(doc["groundInstanceId"]))
            return out.sorted()
        }
        let roots = doc["roots"] as? [[String: Any]] ?? []
        return roots.map { r in
            let t: [String: Any] = ["tree": tree(r["root"]), "material": r["material"] ?? NSNull(),
                                    "visible": r["visible"] ?? true]
            let d = (try? JSONSerialization.data(withJSONObject: t, options: [.sortedKeys])) ?? Data()
            return String(decoding: d, as: UTF8.self)
        }
    }

    static func isIdentity(_ type: String, _ op: [String: Any]) -> Bool {
        func vec(_ k: String) -> [Double] {
            let d = op[k] as? [String: Any] ?? [:]
            return ["x", "y", "z"].map { (d[$0] as? NSNumber)?.doubleValue ?? .nan }
        }
        switch type {
        case "Translate": return vec("offset") == [0, 0, 0]
        case "Rotate": return vec("angles") == [0, 0, 0]
        case "Scale": return vec("factor") == [1, 1, 1]
        default: return false
        }
    }

    private func load(_ url: URL) throws -> [String: Any] {
        try XCTUnwrap(JSONSerialization.jsonObject(with: Data(contentsOf: url)) as? [String: Any])
    }

    func testAllWebExamplesArePresent() {
        XCTAssertEqual(fixtures.count, 11, "one fixture per web example")
    }

    func testOpenEditSaveReopenIsIdentical() throws {
        for src in fixtures {
            let name = src.lastPathComponent
            let original = Self.canonical(try load(src))
            let tmp = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
            defer { try? FileManager.default.removeItem(at: tmp) }
            try FileManager.default.copyItem(at: src, to: tmp)

            let m = EditorModel()
            m.openDocument(tmp)
            XCTAssertTrue(m.canEditDocument, "\(name): \(m.engineError ?? "view-only")")
            guard m.canEditDocument else { continue }
            XCTAssertEqual(Self.canonical(m.documentJSON ?? [:]), original, "\(name): open changed the document")

            // Edit + undo leaves it untouched; then save and reopen.
            if m.partId(forPart: 0) != nil {
                m.setPartMaterial(0, "brass"); m.undo()
            }
            let out = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
            defer { try? FileManager.default.removeItem(at: out) }
            m.saveDocumentAs(out)
            XCTAssertEqual(Self.canonical(try load(out)), original, "\(name): save changed the document")

            let again = EditorModel()
            again.openDocument(out)
            XCTAssertEqual(Self.canonical(again.documentJSON ?? [:]), original, "\(name): reopen differs")

            // A kept edit persists through the file.
            guard again.partId(forPart: 0) != nil else { continue }
            again.setPartMaterial(0, "brass")
            let edited = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".vcad")
            defer { try? FileManager.default.removeItem(at: edited) }
            again.saveDocumentAs(edited)
            let roots = try load(edited)["roots"] as? [[String: Any]]
            XCTAssertEqual(roots?.first?["material"] as? String, "brass", "\(name): edit lost on save")
        }
    }
}

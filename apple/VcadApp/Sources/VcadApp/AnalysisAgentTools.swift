import Foundation
import CVcadFFI

// Analysis tools on the app's MCP server (AgentBridge.swift): an agent runs
// DFM, applies a resolved DFM fix, solves a static stress case and checks
// clearance on the OPEN document. Results also land in the Analysis panel,
// so the user sees what the agent saw; a fix is one named undo step.

@MainActor
enum AnalysisAgentTools {
    typealias Failure = AgentTools.Failure

    private static func schema(_ props: [String: Any], required: [String] = []) -> [String: Any] {
        ["type": "object", "properties": props, "required": required]
    }
    private static let target: [String: Any] = [
        "type": "object",
        "properties": [
            "faces": ["type": "array", "items": ["type": "integer"],
                      "description": "Face ids on the part (from inspect_faces)."],
            "region": ["type": "object", "description": "Axis-aligned box {min:[x,y,z], max:[x,y,z]} in mm; nodes inside are selected."],
        ],
    ]

    static let definitions: [[String: Any]] = [
        ["name": "dfm_check",
         "description": "Run Design-for-Manufacturing rules on the open document. process: fdm (default), sla, cnc_3axis, injection, sheet_metal, casting_sand, casting_investment. Returns issues with severity, measured vs limit, face ids, and — when the fix can be applied automatically — a patch. Shows in the user's Analysis panel.",
         "inputSchema": schema(["process": ["type": "string"]])],
        ["name": "dfm_apply_fix",
         "description": "Apply the automatic fix for a DFM issue from the latest dfm_check (by issue_id) to the open document — one undo step — then re-run the check and report whether the issue cleared.",
         "inputSchema": schema(["issue_id": ["type": "string"]], required: ["issue_id"])],
        ["name": "inspect_faces",
         "description": "Faces of a part (id, surface kind, centre, normal, area) — the ids analyze_structure loads/supports take.",
         "inputSchema": schema(["part_index": ["type": "integer"]])],
        ["name": "analyze_structure",
         "description": "Static linear-elastic FEA of one part, meshed and solved in-app with a mesh-convergence gate. loads: [{faces|region, force:[Fx,Fy,Fz] N}], supports: [{faces|region, fix?:[bool,bool,bool]}]. material: a preset id (aluminum_6061, steel, stainless_304, titanium, brass, pla, petg, abs, nylon) or omit to infer from the part. Returns max von Mises (MPa), max displacement (mm), safety factor (only when converged), and colours the part by stress in the user's viewport.",
         "inputSchema": schema(["part_index": ["type": "integer"], "material": ["type": "string"],
                                "resolution": ["type": "integer", "description": "coarse lattice cells along the longest axis (default 24; the study also solves at 2×)"],
                                "loads": ["type": "array", "items": target], "supports": ["type": "array", "items": target]],
                               required: ["loads", "supports"])],
        ["name": "check_interference",
         "description": "Minimum clearance and interference (overlap volume, penetration depth) between every pair of parts in the open document; clashes are drawn in red in the user's viewport.",
         "inputSchema": schema([:])],
    ]
    static var toolNames: Set<String> { Set(definitions.compactMap { $0["name"] as? String }) }

    static func call(_ name: String, _ a: [String: Any], model m: EditorModel) async throws -> [String: Any] {
        let store = AnalysisStore.shared
        switch name {
        case "dfm_check":
            if let p = a["process"] as? String, !p.isEmpty { store.dfmProcess = p }
            store.shown = true; store.tab = .dfm
            await store.runDfm(m)
            guard let r = store.dfmReport else { throw Failure(message: store.dfmError ?? "DFM failed") }
            return AgentTools.json(dfmSummary(r))
        case "dfm_apply_fix":
            guard let id = a["issue_id"] as? String else { throw Failure(message: "issue_id is required") }
            guard let issue = store.dfmReport?.issues.first(where: { $0.id == id }) else {
                throw Failure(message: "No issue \(id) in the latest dfm_check — run dfm_check first.")
            }
            guard issue.patch != nil else {
                throw Failure(message: "Issue \(id) has no automatic fix: \(issue.patch_note ?? "fix it by hand"). Edit the geometry with the modeling tools, then re-run dfm_check.")
            }
            let before = store.dfmReport?.issues.filter { $0.rule == issue.rule }.count ?? 0
            guard await store.applyFix(issue, m) else { throw Failure(message: store.dfmError ?? "fix failed") }
            let after = store.dfmReport?.issues.filter { $0.rule == issue.rule }.count ?? 0
            let cleared = !(store.dfmReport?.issues.contains { $0.id == id } ?? false)
            return AgentTools.json(["applied": issue.patch?.summary ?? "", "issue_cleared": cleared,
                                    "rule": issue.rule, "rule_issues_before": before, "rule_issues_after": after,
                                    "undo_label": "DFM: Fix \(issue.ruleTitle)",
                                    "report": store.dfmReport.map(dfmSummary) ?? [:]])
        case "inspect_faces":
            let part = a["part_index"] as? Int ?? 0
            guard let data = AnalysisStore.documentData(m) else { throw Failure(message: "No document is open.") }
            guard let faces = store.picker.faces(part: part, document: data) else {
                throw Failure(message: "Part \(part) has no B-rep faces.")
            }
            return AgentTools.json(faces.map { ["id": $0.id, "surface": $0.surface, "center": $0.center,
                                                "normal": $0.normal, "area": $0.area, "label": $0.label] as [String: Any] })
        case "analyze_structure":
            return try await structure(a, m, store)
        case "check_interference":
            store.shown = true; store.tab = .clearance
            await store.runClearance(m)
            guard let r = store.clearance else { throw Failure(message: store.clearanceError ?? "clearance failed") }
            return AgentTools.json([
                "interference_count": r.interference_count,
                "min_clearance_mm": r.min_clearance_mm.map { $0 as Any } ?? NSNull(),
                "total_overlap_mm3": r.total_overlap_mm3,
                "pairs": r.pairs.map { ["a": $0.a_name, "b": $0.b_name, "status": $0.status,
                                        "distance_mm": $0.distance_mm, "overlap_volume_mm3": $0.overlap_volume_mm3] as [String: Any] },
            ])
        default:
            throw Failure(message: "unknown tool \(name)")
        }
    }

    static func dfmSummary(_ r: DfmReport) -> [String: Any] {
        [
            "process": r.process, "rule_pack": r.rule_pack_name, "score": r.score,
            "errors": r.counts.error, "warnings": r.counts.warning, "info": r.counts.info,
            "issues": r.issues.map { i -> [String: Any] in
                var o: [String: Any] = ["id": i.id, "rule": i.rule, "severity": i.severity, "message": i.message,
                                        "part": i.part_name, "measured": i.measured, "limit": i.limit, "units": i.units,
                                        "face_ids": i.face_ids, "auto_fix": i.patch != nil]
                if let p = i.patch { o["fix"] = p.summary } else if let n = i.patch_note { o["fix_note"] = n }
                return o
            },
        ]
    }

    private static func structure(_ a: [String: Any], _ m: EditorModel, _ store: AnalysisStore) async throws -> [String: Any] {
        guard let data = AnalysisStore.documentData(m) else { throw Failure(message: "No document is open.") }
        // Pass the agent's loads/supports straight through: the FFI accepts
        // faces and/or explicit regions.
        var req: [String: Any] = [
            "part_index": a["part_index"] as? Int ?? 0,
            "resolution": a["resolution"] as? Int ?? 24,
            "levels": 2,
            "loads": a["loads"] ?? [],
            "supports": a["supports"] ?? [],
            "fields": true,
        ]
        if let mat = a["material"] as? String, !mat.isEmpty { req["material"] = mat }
        let body = try AnalysisFFI.encode(req)
        store.shown = true; store.tab = .fea; store.feaBusy = true
        let r = await Task.detached {
            Result { try JSONDecoder().decode(FeaResult.self, from: try AnalysisFFI.call(vcad_fea_solve, doc: data, request: body)) }
        }.value
        store.feaBusy = false
        switch r {
        case .failure(let e):
            store.feaError = "\(e)"
            throw Failure(message: "\(e)")
        case .success(let res):
            store.feaResult = res; store.feaError = nil; store.feaPart = res.part_index
            store.showField()
            store.overlayRevision += 1
            var out: [String: Any] = [
                "part": res.part_name, "material": res.material,
                "max_von_mises_mpa": res.max_von_mises_mpa, "max_stress_at": res.max_stress_at,
                "max_displacement_mm": res.max_displacement_mm, "max_displacement_at": res.max_displacement_at,
                "converged": res.converged,
                "stress_change_between_levels": res.study.stress_change_rel,
            ]
            if let y = res.yield_strength_mpa { out["yield_strength_mpa"] = y }
            if let sf = res.safety_factor { out["safety_factor"] = sf }
            if !res.reasons.isEmpty { out["not_converged_because"] = res.reasons }
            if !res.notes.isEmpty { out["notes"] = res.notes }
            return AgentTools.json(out)
        }
    }
}

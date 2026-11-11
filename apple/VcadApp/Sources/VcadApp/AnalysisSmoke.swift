import Foundation

// VCAD_ANALYSIS_SMOKE=1: drive the analysis stack headlessly through the
// app's own paths (EditorModel + engine + AnalysisStore + the MCP tool
// dispatch) and print one line per gate:
//   DFM   — a 0.5 mm plate is flagged under FDM; the resolved fix is applied
//           as an undo step and the violation clears; ⌘Z brings it back.
//   FEA   — a 100×10×10 aluminum cantilever, fixed at x=0 and loaded −100 N Z
//           at x=100 (faces picked from the kernel face table), solves.
//   CLEAR — two 10 mm cubes overlapping 4 mm report 400 mm³.
//   AGENT — dfm_check / dfm_apply_fix / check_interference over the MCP tool
//           dispatch the HTTP bridge uses.

@MainActor
func analysisSmoke() async {
    func emit(_ s: String) { FileHandle.standardError.write(Data(("[VCAD_ANALYSIS] " + s + "\n").utf8)) }
    func load(_ m: EditorModel, _ src: String, _ name: String) -> Bool {
        do {
            let json = try AgentEdits.compileLoon(src)
            try AgentEdits.edit(m, name: name) { try $0.loadIR(json) }
            return true
        } catch { emit("load \(name) failed: \(AgentEdits.describe(error))"); return false }
    }
    var failures = 0
    func check(_ ok: Bool, _ s: String) { emit((ok ? "PASS " : "FAIL ") + s); if !ok { failures += 1 } }

    let store = AnalysisStore.shared
    let m = EditorModel()
    guard m.agentEnsureEngine() != nil else { emit("FAIL no engine"); exit(1) }

    // DFM
    if load(m, "[pub let plate [cube 40 30 0.5]]", "Plate") {
        store.dfmProcess = "fdm"
        await store.runDfm(m)
        let thin = store.dfmReport?.issues.first { $0.rule == "fdm.thin_wall" }
        check(thin != nil, "dfm flags thin wall (\(store.dfmReport?.issues.count ?? -1) issues, error \(store.dfmError ?? "-"))")
        if let thin {
            check(thin.patch != nil, "dfm fix resolved: \(thin.patch?.summary ?? thin.patch_note ?? "-") faces \(thin.face_ids)")
            let applied = await store.applyFix(thin, m)
            let cleared = !(store.dfmReport?.issues.contains { $0.rule == "fdm.thin_wall" } ?? true)
            check(applied && cleared, "dfm fix applied and violation cleared (undo label \(m.undoActionName ?? "-"))")
            m.undo()
            await store.runDfm(m)
            check(store.dfmReport?.issues.contains { $0.rule == "fdm.thin_wall" } ?? false, "undo restores the violation")
        }
    }

    // FEA
    if load(m, "[pub let bracket [cube 100 10 10]]", "Bracket") {
        store.feaPart = 0
        store.feaMaterial = "aluminum_6061"
        store.feaResolution = 20
        var l = FeaLoadSetup(); l.force = [0, 0, -100]
        let s = FeaSupportSetup()
        store.feaLoads = [l]; store.feaSupports = [s]
        // Viewport picks: kernel rays through the real picker, as a click would.
        store.setPickTarget(.support(s.id), m)
        let fixedHit = m.pickTopology(part: 0, origin: [-50, 5, 5], dir: [1, 0, 0])
        m.picking.select(fixedHit?.ref, additive: false)
        store.setPickTarget(.load(l.id), m)
        let tipHit = m.pickTopology(part: 0, origin: [150, 5, 5], dir: [-1, 0, 0])
        m.picking.select(tipHit?.ref, additive: false)
        store.setPickTarget(nil, m)
        let sup = store.feaSupports[0].faces, ld = store.feaLoads[0].faces
        let faces = store.faces(m)
        let label = { (ids: [UInt32]) in ids.compactMap { id in faces.first { $0.id == id }?.label }.joined(separator: ", ") }
        check(sup.count == 1 && ld.count == 1 && sup != ld && m.picking.filter == .standard,
              "fea faces picked in viewport: support [\(label(sup))], load [\(label(ld))]")
        if sup.count == 1, ld.count == 1 {
            let t0 = Date()
            await store.runFea(m)
            if let r = store.feaResult {
                emit(String(format: "fea σmax %.6f MPa δmax %.6f mm converged %@ SF %@ (%.2fs)", r.max_von_mises_mpa,
                            r.max_displacement_mm, r.converged ? "yes" : "no",
                            r.safety_factor.map { String(format: "%.2f", $0) } ?? "-", Date().timeIntervalSince(t0)))
                // Kernel reference (crates/vcad-ffi fea::tests, same spec): see the PR.
                if let ref = ProcessInfo.processInfo.environment["VCAD_FEA_REFERENCE_MPA"].flatMap(Double.init) {
                    check(abs(r.max_von_mises_mpa - ref) / ref < 1e-6, "fea matches kernel reference \(ref) MPa")
                }
                let layers = FieldOverlayStore.shared.layers.map(\.id)
                check(layers == ["von_mises", "displacement"] && FieldOverlayStore.shared.skin() != nil,
                      "fea field in overlay: \(layers), skin quads \(FieldOverlayStore.shared.skin()?.cells.count ?? 0)")
            } else {
                check(false, "fea solve: \(store.feaError ?? "-")")
            }
        }
    }

    // Clearance
    if load(m, "[root [cube 10 10 10] \"steel\"]\n[root [translate 6 0 0 [cube 10 10 10]] \"steel\"]", "Pair") {
        await store.runClearance(m)
        let p = store.clearance?.pairs.first
        check(p?.status == "interference" && abs((p?.overlap_volume_mm3 ?? 0) - 400) < 1,
              String(format: "clearance overlap %.3f mm³ depth %.3f mm (%@)", p?.overlap_volume_mm3 ?? -1,
                     -(p?.distance_mm ?? 0), p?.status ?? store.clearanceError ?? "-"))
    }

    // Agent (MCP dispatch)
    if load(m, "[pub let plate [cube 40 30 0.5]]", "Plate") {
        var out: [String: Any] = [:]
        var err: String?
        do { out = try await AgentTools.call("dfm_check", ["process": "fdm"], model: m) }
        catch { err = "\(error)" }
        let text = ((out["content"] as? [[String: Any]])?.first?["text"] as? String) ?? ""
        check(err == nil && text.contains("fdm.thin_wall"), "agent dfm_check over MCP dispatch (\(text.count) chars) \(err ?? "")")
        if let id = store.dfmReport?.issues.first(where: { $0.rule == "fdm.thin_wall" })?.id {
            do { out = try await AgentTools.call("dfm_apply_fix", ["issue_id": id], model: m) }
            catch { err = "\(error)" }
            let t = ((out["content"] as? [[String: Any]])?.first?["text"] as? String) ?? ""
            check(t.contains("\"issue_cleared\" : true") || t.contains("\"issue_cleared\":true"), "agent dfm_apply_fix clears it")
        }
        let listed = (AgentTools.definitions + AgentEdits.definitions + AnalysisAgentTools.definitions)
            .compactMap { $0["name"] as? String }
        check(["dfm_check", "dfm_apply_fix", "analyze_structure", "check_interference", "inspect_faces"].allSatisfy(listed.contains),
              "analysis tools listed")
    }
    emit(failures == 0 ? "ALL PASS" : "\(failures) FAILED")
    exit(failures == 0 ? 0 : 1)
}

import SwiftUI
import AppKit
import simd
import CVcadFFI

@main
struct VcadApp: App {
    @NSApplicationDelegateAdaptor(AppDelegate.self) var appDelegate

    init() {
        // No AppKit window restoration: the app owns its windows (a hidden
        // SwiftUI host plus the real editor window) and remembers its own
        // layout. Restoring the host after an unclean exit crashed the next
        // launch inside NSPersistentUIRestorer before any of our code ran, and
        // every launch after that sat behind AppKit's modal "quit unexpectedly
        // while reopening windows" prompt with no editor window at all.
        // `ApplePersistenceIgnoreState` is what skips both the restore and
        // the prompt (it is the switch behind Xcode's "launch without state
        // restoration").
        UserDefaults.standard.set(false, forKey: "NSQuitAlwaysKeepsWindows")
        UserDefaults.standard.set(true, forKey: "ApplePersistenceIgnoreState")

        // Headless smoke hook (matches VCAD_GRIPPER/VCAD_ROUTE): dump the parsed
        // feature tree of a .vcad to stderr and exit — verifies the DAG parser
        // without driving the GUI. e.g. VCAD_DUMP_TREE=examples/plate.vcad
        if let path = ProcessInfo.processInfo.environment["VCAD_DUMP_TREE"] {
            dumpFeatureTree(path: path)
            exit(0)
        }
        if ProcessInfo.processInfo.environment["VCAD_ANALYSIS_SMOKE"] != nil {
            // Runs once the main run loop starts; exits the process itself.
            Task { @MainActor in await analysisSmoke() }
        }
        if ProcessInfo.processInfo.environment["VCAD_NAV_SMOKE"] != nil {
            // Needs the GUI up; runs alongside launch and exits the process itself.
            Task { @MainActor in await navSmoke() }
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_EDIT_SMOKE"] {
            MainActor.assumeIsolated { editSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_AUTHOR_SMOKE"] {
            MainActor.assumeIsolated { authorSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_SKETCH_SMOKE"] {
            MainActor.assumeIsolated { sketchSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_MAT_SMOKE"] {
            MainActor.assumeIsolated { matSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_USDZ_SMOKE"] {
            MainActor.assumeIsolated { usdzSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_PATTERN_SMOKE"] {
            MainActor.assumeIsolated { patternSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_GIZMO_SMOKE"] {
            MainActor.assumeIsolated { gizmoSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_POSE_SMOKE"] {
            MainActor.assumeIsolated { poseSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_GOV_SMOKE"] {
            MainActor.assumeIsolated { govSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_ANTI_SMOKE"] {
            MainActor.assumeIsolated { antiSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_BEEST_SMOKE"] {
            MainActor.assumeIsolated { beestSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_GEARDRAG_SMOKE"] {
            MainActor.assumeIsolated { gearDragSmoke(path: path) }
            exit(0)
        }
        if let path = ProcessInfo.processInfo.environment["VCAD_PLAYBACK_SMOKE"] {
            MainActor.assumeIsolated { playbackSmoke(path: path) }
            exit(0)
        }
    }

    /// The document lives at app scope, not in the window: the WindowGroup's
    /// lifecycle host is hidden; the visible AppKit editor supports both modes. The
    /// menu bar commands need the same model the floating overlay is editing.
    @State private var model = EditorModel()
    @State private var intent = IntentEngine()

    var body: some Scene {
        WindowGroup {
            EditorView(model: model, intent: intent)
                .task { FieldOverlayStore.shared.loadFromEnvironment() }
                .task { await DirectManipDemo.runIfRequested(model) }
                .task { PlasmoidStore.shared.attach(model); await PlasmoidDemo.runIfRequested(model) }
                .task { await FieldsSnapshots.runIfRequested(model) }
                .task { await AssemblyOpenDemo.runIfRequested(model) }
                .task { await JointDragDemo.runIfRequested(model) }
                .task { await RuntimeScrubBench.runIfRequested(model) }
        }
        .windowStyle(.automatic)
        .commands {
            DocumentCommands(model: model, intent: intent)
            FieldCommands()
            AnalysisCommands()
            AssemblyCommands(model: model)
            FabricateCommands()
            UpdateCommands()
        }

        Settings {
            SettingsView()
        }
    }
}

/// Print a document's parsed feature tree (used by the VCAD_DUMP_TREE hook).
private func dumpFeatureTree(path: String) {
    guard let g = DocumentGraph.load(path: path) else {
        FileHandle.standardError.write(Data("[VCAD_TREE] parse failed: \(path)\n".utf8))
        return
    }
    var out = "[VCAD_TREE] \(path) — \(g.visibleRoots.count) part(s)\n"
    func walk(_ node: FeatureNode, _ depth: Int) {
        let pad = String(repeating: "  ", count: depth)
        let part = node.partIndex.map { " [part \($0)]" } ?? ""
        let detail = node.detail.map { " — \($0)" } ?? ""
        out += "\(pad)• \(node.name) (\(node.opType))\(detail)\(part)\n"
        for c in node.children { walk(c, depth + 1) }
    }
    for r in g.featureRoots() { walk(r, 0) }
    FileHandle.standardError.write(Data(out.utf8))
}

/// Exercise the edit → re-eval → undo → export pipeline headlessly.
@MainActor private func editSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data((s + "\n").utf8)) }
    func find(_ nodes: [FeatureNode], _ op: String) -> FeatureNode? {
        for n in nodes {
            if n.opType == op { return n }
            if let f = find(n.children, op) { return f }
        }
        return nil
    }
    let m = EditorModel()
    m.source = .document(path: path, label: "smoke")
    guard m.usesDocumentTree, m.reevalDocumentMeshes() != nil else {
        emit("[VCAD_EDIT] no renderable geometry for \(path) (e.g. an assembly the scene eval can't handle)")
        return
    }
    let before = m.sizeMM
    emit("[VCAD_EDIT] loaded: bbox \(fmt3(before)) · tris \(m.triangleCount) · canUndo \(m.canUndo)")

    guard let cube = find(m.featureNodes, "Cube") else { emit("[VCAD_EDIT] no Cube to edit"); return }
    m.editVec(nodeId: cube.nodeId, key: "size", axis: "x", value: 200, snapshot: true)
    _ = m.reevalDocumentMeshes()
    emit("[VCAD_EDIT] after size.x=200 on '\(cube.name)': bbox \(fmt3(m.sizeMM)) · tris \(m.triangleCount) · canUndo \(m.canUndo)")

    m.undo()
    _ = m.reevalDocumentMeshes()
    emit("[VCAD_EDIT] after undo: bbox \(fmt3(m.sizeMM)) · canRedo \(m.canRedo)")

    let stl = URL(fileURLWithPath: "/tmp/vcad_edit_smoke.stl")
    let ok = m.exportSTL(to: stl)
    let size = (try? Data(contentsOf: stl))?.count ?? 0
    emit("[VCAD_EDIT] STL export: \(ok) · \(size) bytes")
}

private func fmt3(_ v: SIMD3<Float>) -> String {
    String(format: "%.1f×%.1f×%.1f", abs(v.x), abs(v.y), abs(v.z))
}

/// Load a document and export it as USDZ — verifies the ModelIO pipeline
/// headlessly. Writes to /tmp/vcad_usdz_smoke.usdz.
@MainActor private func usdzSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data((s + "\n").utf8)) }
    let m = EditorModel()
    m.source = .document(path: path, label: "usdz")
    _ = m.reevalDocumentMeshes()
    let out = URL(fileURLWithPath: "/tmp/vcad_usdz_smoke.usdz")
    let ok = m.exportUSDZ(to: out)
    let size = (try? Data(contentsOf: out))?.count ?? 0
    emit("[VCAD_USDZ] export: \(ok) · \(size) bytes · parts \(m.partCount) · bbox \(fmt3(m.sizeMM)) mm")
}

/// Material assignment + persistence: set part 0 to copper, verify the resolved
/// color, save, reload, and confirm the assignment + definition round-trip.
@MainActor private func matSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data((s + "\n").utf8)) }
    let m = EditorModel()
    m.source = .document(path: path, label: "mat")
    guard m.usesDocumentTree else { emit("[VCAD_MAT] load failed"); return }
    let before = m.resolvedMaterial(forPart: 0).color
    emit("[VCAD_MAT] part0 material '\(m.materialName(forPart: 0) ?? "—")' color \(rgb(before))")
    m.setPartMaterial(0, "copper")
    let after = m.resolvedMaterial(forPart: 0).color
    emit("[VCAD_MAT] → copper: key '\(m.materialName(forPart: 0) ?? "—")' color \(rgb(after))")
    let out = URL(fileURLWithPath: "/tmp/vcad_mat.vcad")
    m.saveDocumentAs(out)
    if let g = DocumentGraph.load(path: out.path) {
        let hasDef = g.materials["copper"] != nil
        emit("[VCAD_MAT] reloaded: part0 key '\(g.visibleRoots.first?.material ?? "—")' · copper def present \(hasDef)")
    } else { emit("[VCAD_MAT] reload failed") }
}

private func rgb(_ c: NSColor) -> String {
    let s = c.usingColorSpace(.sRGB) ?? c
    return String(format: "(%.2f, %.2f, %.2f)", s.redComponent, s.greenComponent, s.blueComponent)
}

/// Pattern instancing: load a doc with pattern roots through the instanced
/// eval path, then cross-check each instanced part's aggregated bounds and
/// triangle count against a full BAKED kernel eval of the untouched document.
/// Agreement proves the Swift-side transforms reproduce the kernel's pattern
/// placement exactly.
@MainActor private func patternSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data((s + "\n").utf8)) }
    let m = EditorModel()
    m.source = .document(path: path, label: "pattern")
    guard m.usesDocumentTree, m.reevalDocumentMeshes() != nil else {
        emit("[VCAD_PATTERN] load failed"); return
    }
    let instanced = m.docPartInstancing.enumerated().compactMap { i, p in p.map { (i, $0) } }
    emit("[VCAD_PATTERN] \(m.partCount) part(s), \(instanced.count) instanced · tris \(m.triangleCount) · bbox \(fmt3(m.sizeMM))")
    for (i, p) in instanced {
        emit("[VCAD_PATTERN] part\(i): node \(p.patternNodeId) seed \(p.seedNodeId) × \(p.transforms.count) copies")
    }

    // Baked reference: evaluate the ORIGINAL doc (patterns unioned in-kernel).
    guard let json = m.documentJSON, let data = DocEdit.serialize(json) else { return }
    let scene: OpaquePointer? = data.withUnsafeBytes {
        vcad_scene_from_json($0.bindMemory(to: UInt8.self).baseAddress, data.count)
    }
    guard let scene else { emit("[VCAD_PATTERN] baked eval failed"); return }
    defer { vcad_scene_free(scene) }
    var ok = true
    for i in 0..<vcad_scene_part_count(scene) {
        let km = KernelMesh.fromView(vcad_scene_part_mesh(scene, i))
        guard i < m.docPartMeshes.count else { continue }
        let pick = m.docPartMeshes[i]
        let dLo = simd_length(km.minBound - pick.lo), dHi = simd_length(km.maxBound - pick.hi)
        let match = dLo < 1e-3 && dHi < 1e-3
        // Baked pattern tris can differ slightly (the union sews coincident
        // faces); bounds are the placement invariant.
        emit("[VCAD_PATTERN] part\(i) bounds baked vs instanced: Δlo \(String(format: "%.5f", dLo)) Δhi \(String(format: "%.5f", dHi)) → \(match ? "MATCH" : "MISMATCH")")
        if !match { ok = false }
    }
    emit("[VCAD_PATTERN] \(ok ? "PASS" : "FAIL")")
}

/// Gizmo translate: move part 0 by +50 in X via the root Translate, confirm the
/// bbox shifts, then undo back.
@MainActor private func gizmoSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data((s + "\n").utf8)) }
    let m = EditorModel()
    m.source = .document(path: path, label: "gizmo")
    guard m.usesDocumentTree, m.reevalDocumentMeshes() != nil else { emit("[VCAD_GIZMO] load failed"); return }
    let c0 = m.gizmoCenterKernel() ?? .zero
    emit("[VCAD_GIZMO] loaded · part0 center \(fmt3(c0)) · bbox \(fmt3(m.sizeMM))")
    guard var json = m.documentJSON else { return }
    DocEdit.setRootTranslate(&json, partIndex: 0, 50, 0, 0)
    m.documentJSON = json
    _ = m.reevalDocumentMeshes()
    let c1 = m.gizmoCenterKernel() ?? .zero
    emit("[VCAD_GIZMO] +50 X · part0 center \(fmt3(c1)) (Δx \(String(format: "%.1f", c1.x - c0.x)))")

    // Rotate-about-center: 90° about Z should swap the footprint + hold the center.
    let r = EditorModel()
    r.source = .document(path: path, label: "rot")
    _ = r.reevalDocumentMeshes()
    let rc0 = r.gizmoCenterKernel() ?? .zero
    let before = r.sizeMM
    guard var rj = r.documentJSON, let rot = DocEdit.wrapRotate(&rj, partIndex: 0,
                                                                Double(rc0.x), Double(rc0.y), Double(rc0.z)) else { return }
    DocEdit.setRotateAngle(&rj, rotNodeId: rot, axisIndex: 2, degrees: 90)
    r.documentJSON = rj
    _ = r.reevalDocumentMeshes()
    let rc1 = r.gizmoCenterKernel() ?? .zero
    emit("[VCAD_GIZMO] rotate 90°Z · bbox \(fmt3(before)) → \(fmt3(r.sizeMM)) · center \(fmt3(rc0)) → \(fmt3(rc1))")
}

/// Sketch → extrude authoring: draw a 40×40 square on XY, extrude 12mm, verify
/// the kernel builds a 40×40×12 solid.
@MainActor private func sketchSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data((s + "\n").utf8)) }
    let m = EditorModel()
    m.source = .document(path: path, label: "sketch")
    guard m.usesDocumentTree else { emit("[VCAD_SKETCH] load failed"); return }
    let parts0 = m.featureNodes.count
    m.enterSketch()
    m.setSketchTool(.rectangle)
    m.sketchTap(SIMD2(0, 0))         // corner
    m.sketchTap(SIMD2(40, 40))       // opposite corner
    m.sketchExtrudeDepth = 12
    emit("[VCAD_SKETCH] entities \(m.sketch?.entities.count ?? 0) · \(m.sketch?.stateLabel ?? "-") · canFinish \(m.canFinishSketch)")
    m.finishSketch()
    _ = m.reevalDocumentMeshes()
    emit("[VCAD_SKETCH] extruded → \(m.featureNodes.count) parts (was \(parts0)) · last op '\(m.featureNodes.last?.opType ?? "?")' · bbox \(fmt3(m.sizeMM)) · tris \(m.triangleCount) · sketching \(m.sketching)")
}

/// Exercise the authoring pipeline: add a primitive, apply a fillet, save,
/// reload — confirming round-trip authoring through the kernel.
@MainActor private func authorSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data((s + "\n").utf8)) }
    let m = EditorModel()
    m.source = .document(path: path, label: "author")
    guard m.usesDocumentTree else { emit("[VCAD_AUTHOR] load failed"); return }
    let parts0 = m.featureNodes.count
    emit("[VCAD_AUTHOR] loaded \(parts0) part(s)")

    m.addPrimitive(.cylinder)
    _ = m.reevalDocumentMeshes()
    emit("[VCAD_AUTHOR] +cylinder → \(m.featureNodes.count) parts · tris \(m.triangleCount)")

    m.selectedFeatureID = m.featureNodes.first?.id
    m.applyModifierToSelected(.fillet)
    _ = m.reevalDocumentMeshes()
    let rootOp = m.selectedFeatureNode?.opType ?? "?"
    emit("[VCAD_AUTHOR] fillet part 0 → root op now '\(rootOp)' · tris \(m.triangleCount)")

    m.multiSelectedParts = [0, 1]
    m.combineSelected(.difference)
    _ = m.reevalDocumentMeshes()
    emit("[VCAD_AUTHOR] subtract parts 0,1 → \(m.featureNodes.count) part(s), root op '\(m.featureNodes.first?.opType ?? "?")' · tris \(m.triangleCount)")

    let out = URL(fileURLWithPath: "/tmp/vcad_authored.vcad")
    m.saveDocumentAs(out)
    let reloaded = DocumentGraph.load(path: out.path)
    emit("[VCAD_AUTHOR] saved + reloaded: \(reloaded?.visibleRoots.count ?? -1) part(s), parses=\(reloaded != nil)")
}

/// Kinematic joint playback: load a jointed assembly, confirm instances +
/// timeline surface through the model, then scrub to mid-timeline and show
/// the FK-driven instance positions moving.
@MainActor private func playbackSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data((s + "\n").utf8)) }
    let m = EditorModel()
    m.source = .document(path: path, label: "playback")
    let scene = m.buildScene()
    emit("[VCAD_PLAY] instances \(scene.instances.count) · joints \(m.documentGraph?.joints.count ?? 0) · hasPlayback \(m.hasPlayback) · duration \(m.timeline?.durationS ?? 0)s")
    guard m.hasPlayback else { emit("[VCAD_PLAY] no playback surface"); return }
    let dur = m.timeline?.durationS ?? 0
    let before = m.instanceTransforms
    m.setPlaybackTime(dur / 2)
    let mid = m.instanceTransforms
    for i in 0..<min(before.count, mid.count) {
        let a = before[i].columns.3, b = mid[i].columns.3
        emit(String(format: "[VCAD_PLAY] inst%d t=0 (%.1f, %.1f, %.1f) → t=%.2f (%.1f, %.1f, %.1f)",
                    i, a.x, a.y, a.z, dur / 2, b.x, b.y, b.z))
    }
    m.setPlaybackTime(dur)
    let end = m.instanceTransforms
    emit(String(format: "[VCAD_PLAY] inst%d t=%.2f pos (%.1f, %.1f, %.1f)",
                end.count - 1, dur,
                end.last!.columns.3.x, end.last!.columns.3.y, end.last!.columns.3.z))
}

/// Pose studio: open a jointed assembly (a `.loon` file too), drive one joint,
/// explode, bake the pose for the photoreal renderer, and reload from disk.
@MainActor private func poseSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data((s + "\n").utf8)) }
    let m = EditorModel()
    let t0 = Date()
    m.openDocument(URL(fileURLWithPath: path))
    let scene = m.buildScene()
    let mats = Set(scene.instances.map { $0.material.color.description }).count
    emit(String(format: "[VCAD_POSE] instances %d · driven joints %d · distinct materials %d · studio %@ · %.1fs",
                scene.instances.count, m.drivenJoints.count, mats, m.hasPoseStudio ? "yes" : "no",
                Date().timeIntervalSince(t0)))
    if let e = m.loadError { emit("[VCAD_POSE] loadError: \(e)") }
    guard m.hasPoseStudio else { return }
    let rest = m.instanceTransforms
    func moved(_ a: [float4x4], _ b: [float4x4]) -> Int {
        // Bodies are authored about their own pivots, so compare where each
        // part's centre lands, not the transform origin.
        a.indices.filter { i in
            let c = SIMD4<Float>(m.assemblyLocalCenters[i], 1)
            return i < b.count && simd_length(a[i] * c - b[i] * c) > 0.5
        }.count
    }
    let name = ProcessInfo.processInfo.environment["VCAD_POSE_JOINT"] ?? "l_knee_pitch"
    if let j = m.drivenJoints.first(where: { EditorModel.poseKey($0) == name }) {
        m.setJoint(j, (j.limits?.upperBound ?? 60) * 0.8)
        emit("[VCAD_POSE] \(name) → \(m.jointValue(j))° limits \(j.limits.map { "\($0)" } ?? "none"): \(moved(rest, m.instanceTransforms)) instances moved")
    } else { emit("[VCAD_POSE] no joint named \(name)") }
    let posed = m.instanceTransforms
    m.setExplode(1)
    emit("[VCAD_POSE] explode 1: \(moved(posed, m.instanceTransforms)) instances moved")
    for k in ["cover-helmet_i", "cover-chest_i", "cover-shoe_ll_i"] {
        if let i = m.assemblyInstanceIDs.firstIndex(of: k) {
            let a = posed[i].columns.3, b = m.instanceTransforms[i].columns.3
            emit(String(format: "[VCAD_POSE]   %@ (%.0f %.0f %.0f) → (%.0f %.0f %.0f)", k, a.x, a.y, a.z, b.x, b.y, b.z))
        }
    }
    if let data = m.posedDocumentJSON() {
        let out = URL(fileURLWithPath: NSTemporaryDirectory() + "vcad_posed.vcad")
        try? data.write(to: out)
        emit("[VCAD_POSE] posed doc \(data.count) bytes → \(out.path)")
    }
    m.reloadFromDisk()
    _ = m.buildScene()
    emit("[VCAD_POSE] reload: instances \(m.assemblyInstanceCount) · explode kept \(m.explode) · pose kept \(m.jointPose.count) joint(s) · moved vs rest \(moved(rest, m.instanceTransforms))")
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
        MainActor.assumeIsolated { AgentInstallPrompt.runIfFirstLaunch()
            #if DEBUG
            if ProcessInfo.processInfo.environment["VCAD_FAKE_CRASH"] == "1" { CrashDiagnostics.plantFake() }
            #endif
            CrashDiagnostics.shared.start()
        }
    }
    func applicationShouldTerminateAfterLastWindowClosed(_ app: NSApplication) -> Bool { true }

    /// ⌘Q skips the close prompt, and the autosave debounce would never fire:
    /// write unsaved work now so it lands in Recents for next time.
    func applicationWillTerminate(_ notification: Notification) {
        MainActor.assumeIsolated { AppInstance.currentModel?.flushAutosaveNow() }
    }

    /// Implementing `application(_:open:)` below tells AppKit this app opens
    /// documents, which by default suppresses the untitled window at launch —
    /// and this app's whole UI hangs off that window. Say yes explicitly.
    func applicationShouldOpenUntitledFile(_ sender: NSApplication) -> Bool { true }

    /// Finder double-click, drag onto the icon, `open file.vcad`. macOS hands
    /// these to a RUNNING instance rather than launching one, so this is the
    /// path that would otherwise replace the document you are looking at.
    func application(_ application: NSApplication, open urls: [URL]) {
        MainActor.assumeIsolated {
            // `vcad://` deep links (MCP `open_in_app`, retired vcad.io links)
            // resolve to a file first, then open like any other document.
            // `vcad://focus` (MCP Apps "Open in vcad") just brings the window up.
            if urls.contains(where: { $0.scheme?.lowercased() == "vcad" && $0.host?.lowercased() == "focus" }) {
                AgentViewer.focusWindow()
            }
            var urls = urls.compactMap { u -> URL? in
                guard u.scheme?.lowercased() == "vcad" else { return u }
                return u.host?.lowercased() == "focus" ? nil : AppInstance.resolveDeepLink(u)
            }
            // A link to the document already open focuses it instead of
            // opening a second copy beside the live one.
            if let open = AppInstance.currentModel?.documentURL?.standardizedFileURL,
               urls.contains(where: { $0.standardizedFileURL == open }) {
                urls.removeAll { $0.standardizedFileURL == open }
                AgentViewer.focusWindow()
            }
            guard !urls.isEmpty else { return }
            guard let model = AppInstance.currentModel else {
                // The view has not come up yet (we were launched for these
                // files): it opens them the moment it attaches.
                AppInstance.pendingOpen.append(contentsOf: urls)
                return
            }
            // Outlines ride along with the document; the first document may
            // claim this instance, further documents get their own.
            var documents = 0
            for url in urls {
                if url.pathExtension.lowercased() == "dxf" { AppInstance.opening(url, from: model); continue }
                if documents == 0 { AppInstance.opening(url, from: model) } else { AppInstance.open(document: url) }
                documents += 1
            }
        }
    }
}

/// Headless hero path: `VCAD_GEARDRAG_SMOKE=examples/gear-train.vcad`. Opens
/// the train, auto-meshes it, drags the input gear 30° with synthetic kernel
/// rays, checks every gear turned by its ratio, and runs the clearance
/// receipt across a tooth pitch. `[GEARDRAG]` lines; PASS/FAIL at the end.
@MainActor private func gearDragSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data(("[GEARDRAG] " + s + "\n").utf8)) }
    let m = EditorModel()
    m.openDocument(URL(fileURLWithPath: path))
    _ = m.buildScene()
    if let e = m.loadError { emit("loadError: \(e)"); return }
    emit("instances \(m.assemblyInstanceCount)")
    func angle(_ i: Int) -> Double {
        let x = m.instanceTransforms[i].columns.0
        return Double(atan2(x.y, x.x)) * 180 / .pi
    }
    let gears = (0..<m.instanceTransforms.count).compactMap { i in m.gearSpec(instance: i).map { (i, $0.teeth) } }
    emit("gears " + gears.map { "z\($0.1)" }.joined(separator: " "))
    let props = m.inferGearMeshes()
    emit("inferred " + props.map { "\($0.driverTeeth)→\($0.drivenTeeth) ratio \(String(format: "%.4f", $0.ratio)) offset \(String(format: "%.2f", $0.offset))" }.joined(separator: ", "))
    guard !props.isEmpty, m.applyGearMeshes(props) else { emit("FAIL no meshes applied"); return }
    _ = m.buildScene()
    let coupled = m.asmJoints.filter { $0["coupling"] != nil }.count
    emit("couplings after edit \(coupled); re-infer finds \(m.inferGearMeshes().count) (want 0)")
    guard let gi = gears.first(where: { $0.1 == 16 })?.0 else { emit("FAIL no input gear"); return }
    let before = gears.map { angle($0.0) }
    let c = m.instanceTransforms[gi].columns.3
    let ctr = SIMD3<Float>(c.x, c.y, c.z + 3)
    func ray(_ deg: Float) -> (o: SIMD3<Float>, d: SIMD3<Float>) {
        let a = deg * .pi / 180
        return (ctr + SIMD3(12 * cos(a), 12 * sin(a), 300), SIMD3(0, 0, -1))
    }
    guard m.beginJointDrag(instance: gi, at: ctr + SIMD3(12, 0, 0), ray: ray(0)) else { emit("FAIL begin"); return }
    for k in 1...30 { m.jointDragTo(ray: ray(Float(k))) }
    let after = gears.map { angle($0.0) }
    var ok = true
    for (n, g) in gears.enumerated() {
        var d = after[n] - before[n]
        d = (d + 540).truncatingRemainder(dividingBy: 360) - 180
        let want = 30.0 * 16.0 / Double(g.1) * (n % 2 == 0 ? 1 : -1)
        let good = abs(d - want) < 0.5
        ok = ok && good
        emit("z\(g.1) turned \(String(format: "%.2f", d))° (want \(String(format: "%.2f", want))) \(good ? "ok" : "BAD")")
    }
    if let (docs, names, label) = m.meshReceiptInputs(from: m.jointDrag?.driverJointId ?? "") {
        let r = EditorModel.evaluateMeshReceipt(docs: docs, gearNames: names)
        emit("receipt \(label): checked \(r.checked) pairs, clashes \(r.clashes), min gap \(String(format: "%.3f", r.minGap)) mm, failed \(r.failed)")
        ok = ok && r.ok
    } else { emit("receipt inputs unavailable"); ok = false }
    // Negative control: the same train half a tooth out of phase must clash,
    // or the receipt above proves nothing about the phase math.
    if let saved = m.documentJSON, var joints = saved["joints"] as? [[String: Any]] {
        for k in joints.indices {
            guard var c = joints[k]["coupling"] as? [String: Any],
                  let off = (c["offset"] as? NSNumber)?.doubleValue else { continue }
            c["offset"] = off + 180.0 / 28.0
            joints[k]["coupling"] = c
        }
        var shifted = saved
        shifted["joints"] = joints
        m.documentJSON = shifted
        if let (docs, names, _) = m.meshReceiptInputs(from: m.jointDrag?.driverJointId ?? "") {
            let r = EditorModel.evaluateMeshReceipt(docs: docs, gearNames: names)
            emit("control (half-tooth off): clashes \(r.clashes) — \(r.clashes > 0 ? "caught" : "MISSED")")
            ok = ok && r.clashes > 0
        }
        m.documentJSON = saved
    }
    m.endJointDrag()
    _ = m.buildScene()
    // Panel path: driving the IDLER slider back-drives the input; the readout
    // is the derived value, not the authored state FK ignores.
    if let idler = m.asmJoints.first(where: { (($0["name"] as? String) ?? "").hasPrefix("Idler") })?["id"] as? String,
       let input = m.asmJoints.first(where: { (($0["name"] as? String) ?? "").hasPrefix("Input") })?["id"] as? String {
        let in0 = m.currentJointValues()[input] ?? .nan
        let id0 = m.currentJointValues()[idler] ?? .nan
        m.dragJoint(idler, to: id0 - 28.0)
        let in1 = m.currentJointValues()[input] ?? .nan, id1 = m.currentJointValues()[idler] ?? .nan
        let good = abs((in1 - in0) - 49.0) < 0.01 && abs((id1 - id0) + 28.0) < 0.01
        emit("panel idler −28°: idler \(String(format: "%.2f", id1 - id0))°, input \(String(format: "%+.2f", in1 - in0))° (want +49) \(good ? "ok" : "BAD")")
        ok = ok && good
        m.commitJointDrag(idler)
        _ = m.buildScene()
    }
    let committed = m.asmJoints.first { ($0["coupling"] == nil) && (($0["name"] as? String) ?? "").hasPrefix("Input") }
    emit("committed input state \(((committed?["state"]) as? NSNumber)?.doubleValue ?? .nan)")
    emit(ok ? "PASS" : "FAIL")
}

/// Headless Strandbeest: `VCAD_BEEST_SMOKE=examples/strandbeest.vcad`. Sweeps
/// the crank (loops must close and the foot pivot must track Jansen's
/// circle-intersection geometry), then grabs a FOOT — a passive link — and
/// checks the drag drives the crank. `[BEEST]` lines; PASS/FAIL at the end.
@MainActor private func beestSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data(("[BEEST] " + s + "\n").utf8)) }
    let t0 = Date()
    let m = EditorModel()
    m.openDocument(URL(fileURLWithPath: path))
    _ = m.buildScene()
    if let e = m.loadError { emit("loadError: \(e)"); return }
    emit(String(format: "instances %d · build %.1fs", m.assemblyInstanceCount, Date().timeIntervalSince(t0)))
    let docIds = m.asmInstances.compactMap { $0["id"] as? String }
    emit("order matches doc: \(docIds == m.assemblyInstanceIDs) doc=\(docIds.prefix(4)) ffi=\(m.assemblyInstanceIDs.prefix(4))")
    // Ids are re-minted on load; find things by name.
    func instIndex(named n: String) -> Int? {
        m.asmInstances.first { ($0["name"] as? String) == n }.flatMap { $0["id"] as? String }
            .flatMap { m.assemblyInstanceIDs.firstIndex(of: $0) }
    }
    let crankId = m.asmJoints.first { ($0["name"] as? String) == "Crank" }?["id"] as? String ?? "crank_axle"
    guard let foot = instIndex(named: "Foot 1") else { emit("FAIL no foot"); return }
    func jansenF(_ th: Double) -> (Double, Double) {
        func cc(_ p: (Double, Double), _ r1: Double, _ q: (Double, Double), _ r2: Double, _ s: Double) -> (Double, Double) {
            let dx = q.0 - p.0, dy = q.1 - p.1, D = (dx * dx + dy * dy).squareRoot()
            let A = (r1 * r1 - r2 * r2 + D * D) / (2 * D), H = max(0, r1 * r1 - A * A).squareRoot()
            let mx = p.0 + A * dx / D, my = p.1 + A * dy / D
            return (mx + s * H * dy / D, my - s * H * dx / D)
        }
        let B = (-38.0, -7.8), A = (15 * cos(th), 15 * sin(th))
        let C = cc(A, 50, B, 41.5, 1), D = cc(A, 61.9, B, 39.3, -1), E = cc(B, 40.1, C, 55.8, -1)
        return cc(E, 39.4, D, 36.7, 1)
    }
    // Upright: the frame tips the drawing plane onto world XZ and lifts it.
    let lift = Float((((m.asmInstances.first { ($0["name"] as? String) == "Frame" }?["transform"] as? [String: Any])?["translation"] as? [String: Any])?["z"] as? NSNumber)?.doubleValue ?? 0)
    func plane(_ c: SIMD4<Float>) -> (Double, Double) { (Double(c.x), Double(c.z - lift)) }
    var ok = true, worst = 0.0
    let t1 = Date()
    for k in 0...24 {
        let deg = Double(k) * 15
        var v = m.currentJointValues(); v[crankId] = deg
        guard let w = m.applyAssemblyPose(v) else { emit("FAIL fk"); return }
        let f = plane(w[foot].columns.3), (fx, fy) = jansenF(deg * .pi / 180)
        worst = max(worst, hypot(f.0 - fx, f.1 - fy))
    }
    emit(String(format: "crank sweep 0→360°: worst foot error %.4f mm · %.1f ms/pose", worst, Date().timeIntervalSince(t1) * 1000 / 25))
    ok = ok && worst < 0.05
    // Grab the foot pivot and pull it along the stride.
    var v0 = m.currentJointValues(); v0[crankId] = 0
    _ = m.applyAssemblyPose(v0)
    let f0 = m.instanceTransforms[foot].columns.3
    let p0 = SIMD3<Float>(f0.x, f0.y, f0.z)
    // Look at the upright linkage from the front (-Y).
    func ray(_ p: SIMD3<Float>) -> (o: SIMD3<Float>, d: SIMD3<Float>) { (p - SIMD3(0, 300, 0), SIMD3(0, 1, 0)) }
    guard m.beginJointDrag(instance: foot, at: p0, ray: ray(p0)) else { emit("FAIL begin on foot"); return }
    emit("grabbed foot → drives \(m.jointDrag?.driverJointId ?? "-")")
    let target = jansenF(60 * .pi / 180)
    for k in 1...20 {
        let u = Float(k) / 20
        let p = SIMD3<Float>(f0.x + (Float(target.0) - f0.x) * u, f0.y, f0.z + (Float(target.1) + lift - f0.z) * u)
        m.jointDragTo(ray: ray(p))
    }
    let crank = m.jointDrag?.value ?? .nan
    let f1 = plane(m.instanceTransforms[foot].columns.3)
    let miss = hypot(f1.0 - target.0, f1.1 - target.1)
    emit(String(format: "foot pulled to crank-60° spot: crank now %.1f° (want ≈60), foot miss %.2f mm", crank, miss))
    ok = ok && abs(crank - 60) < 3 && miss < 1
    // Branch stability: sweep the crank fast (big jumps, like drag probes)
    // and check the foot never leaves Jansen's branch.
    var jump = 0.0
    for k in 0...36 {
        let deg = Double((k * 97) % 360)
        var v = m.currentJointValues(); v[crankId] = deg
        guard let w = m.applyAssemblyPose(v) else { continue }
        let f = plane(w[foot].columns.3), (fx, fy) = jansenF(deg * .pi / 180)
        jump = max(jump, hypot(f.0 - fx, f.1 - fy))
    }
    emit(String(format: "jumpy crank (97° hops): worst foot error %.4f mm %@", jump, jump < 0.05 ? "ok" : "BAD — wrong branch"))
    ok = ok && jump < 0.05
    m.endJointDrag()
    let t2 = Date()
    let idsBefore = m.asmJoints.compactMap { $0["id"] as? String }
    m.commitJointDrag(crankId)
    emit(String(format: "commit: %.0f ms, ids stable %@", Date().timeIntervalSince(t2) * 1000,
                idsBefore == m.asmJoints.compactMap { $0["id"] as? String } ? "yes" : "NO (rebuilt)"))
    emit(ok ? "PASS" : "FAIL")
}

/// Headless Antikythera: `VCAD_ANTI_SMOKE=examples/antikythera.vcad`. One turn
/// of the year crank must lap the moon pointer 254/19 times, forward; then the
/// clearance receipt runs over the whole train. `[ANTI]` lines.
@MainActor private func antiSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data(("[ANTI] " + s + "\n").utf8)) }
    let t0 = Date()
    let m = EditorModel()
    m.openDocument(URL(fileURLWithPath: path))
    _ = m.buildScene()
    if let e = m.loadError { emit("loadError: \(e)"); return }
    emit(String(format: "instances %d · build %.1fs · couplings %d", m.assemblyInstanceCount, Date().timeIntervalSince(t0),
                m.asmJoints.filter { $0["coupling"] != nil }.count))
    func jid(_ n: String) -> String? { m.asmJoints.first { ($0["name"] as? String) == n }?["id"] as? String }
    guard let year = jid("Year (crank)"), let moon = jid("Moon") else { emit("FAIL joints"); return }
    let base = m.currentJointValues()
    var ok = true
    for deg in [30.0, 360.0] {
        var v = base; v[year] = deg
        m.dragJoint(year, to: deg)
        let mv = m.currentJointValues()[moon]! - base[moon]!
        let want = deg * 254 / 19
        emit(String(format: "year +%.0f° → moon +%.2f° (want +%.2f) %@", deg, mv, want, abs(mv - want) < 0.01 ? "ok" : "BAD"))
        ok = ok && abs(mv - want) < 0.01
    }
    let ids0 = m.asmJoints.compactMap { $0["id"] as? String }
    let tc = Date()
    m.commitJointDrag(year)
    emit(String(format: "release commit: %.0f ms, ids stable %@, moon still +%.2f°", Date().timeIntervalSince(tc) * 1000,
                ids0 == m.asmJoints.compactMap { $0["id"] as? String } ? "yes" : "NO",
                (m.currentJointValues()[jid("Moon") ?? ""] ?? .nan) - base[moon]!))
    _ = m.buildScene()
    if let (docs, names, label, pairs) = m.meshReceiptInputsWithPairs(from: jid("Year (crank)") ?? "") {
        let t0 = Date()
        let rp = EditorModel.evaluateMeshReceipt(docs: docs, gearNames: names, pairs: pairs)
        emit(String(format: "receipt (cross-arbor pairs only): %.1fs, %d checked, clashes %d", Date().timeIntervalSince(t0), rp.checked, rp.clashes))
        let t1 = Date()
        if let rep = try? AnalysisFFI.clearance(doc: docs[0], includeMesh: false) {
            for p in rep.pairs where p.intersecting {
                guard let ga = names[p.a_name], let gb = names[p.b_name], ga != gb else { continue }
                emit(String(format: "  clash %@ × %@ overlap %.3f mm³", p.a_name, p.b_name, p.overlap_volume_mm3))
            }
        }
        let r = EditorModel.evaluateMeshReceipt(docs: docs, gearNames: names)
        emit(String(format: "receipt (all pairs, old way): %.1fs", Date().timeIntervalSince(t1)))
        emit("receipt \(label): \(names.count) arbors, checked \(r.checked) pairs, clashes \(r.clashes), min gap \(String(format: "%.3f", r.minGap)) mm")
        ok = ok && r.ok
    } else { emit("receipt unavailable"); ok = false }
    emit(ok ? "PASS" : "FAIL")
}

/// Headless Watt governor: `VCAD_GOV_SMOKE=examples/watt-governor.vcad`. The
/// loops must close at rest and fully lifted, the balls must swing OUT (not
/// fold in) symmetrically, and spinning the spindle must not move the sleeve.
@MainActor private func govSmoke(path: String) {
    func emit(_ s: String) { FileHandle.standardError.write(Data(("[GOV] " + s + "\n").utf8)) }
    let m = EditorModel()
    m.openDocument(URL(fileURLWithPath: path))
    _ = m.buildScene()
    if let e = m.loadError { emit("loadError: \(e)"); return }
    func jid(_ n: String) -> String? { m.asmJoints.first { ($0["name"] as? String) == n }?["id"] as? String }
    func idx(_ n: String) -> Int? { m.asmInstances.first { ($0["name"] as? String) == n }.flatMap { $0["id"] as? String }.flatMap { m.assemblyInstanceIDs.firstIndex(of: $0) } }
    guard let sleeve = jid("Sleeve"), let spindle = jid("Spindle"), let bl = idx("Ball L"), let br = idx("Ball R") else { emit("FAIL ids"); return }
    func ballX(_ w: [float4x4], _ i: Int) -> SIMD3<Float> { let p = w[i] * SIMD4<Float>(0, 0, -70, 1); return SIMD3(p.x, p.y, p.z) }
    var ok = true
    for lift in [3.0, 30.0, 60.0, 74.0] {
        var v = m.currentJointValues(); v[sleeve] = lift
        guard let w = m.applyAssemblyPose(v) else { emit("FAIL fk"); return }
        let l = ballX(w, bl), r = ballX(w, br)
        let phi = acos(1 - lift / 80) * 180 / .pi
        let good = l.x < -5 && r.x > 5 && abs(l.x + r.x) < 0.1 && abs(l.z - r.z) < 0.1
        emit(String(format: "sleeve %.0f mm (φ %.1f°): balls x %.1f / %.1f, z %.1f %@", lift, phi, l.x, r.x, l.z, good ? "ok" : "BAD"))
        ok = ok && good
    }
    var v = m.currentJointValues(); v[sleeve] = 30; v[spindle] = 90
    if let w = m.applyAssemblyPose(v) {
        let l = ballX(w, bl)
        emit(String(format: "spindle 90°: ball L at (%.1f, %.1f, %.1f) — swung round to ±y", l.x, l.y, l.z))
        ok = ok && abs(l.x) < 1 && abs(l.y) > 5
    }
    emit(ok ? "PASS" : "FAIL")
}

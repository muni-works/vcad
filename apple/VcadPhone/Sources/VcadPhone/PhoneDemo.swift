import SwiftUI
import RealityKit

/// `VCAD_PHONE_DEMO=1`: walk the editor loop headlessly for simulator
/// screenshots and CI-style smoke runs, through the SAME code paths the
/// touch gestures and buttons call (EditorModel orbit, the viewport's
/// ARView hit test for picking, the inspector's parameter edit, the toolbar's
/// undo and save). Each step logs `[phone-demo] <step>` and then holds for
/// `VCAD_PHONE_DEMO_HOLD` seconds (default 3) so an outside
/// `simctl io screenshot` can catch it.
///
/// It does not synthesize UITouches — gesture recognizer wiring is covered
/// by hand on a device / the simulator panel.
@MainActor
enum PhoneDemo {
    static var enabled: Bool { ProcessInfo.processInfo.environment["VCAD_PHONE_DEMO"] == "1" }

    static func run(model: EditorModel) async {
        let hold = Double(ProcessInfo.processInfo.environment["VCAD_PHONE_DEMO_HOLD"] ?? "") ?? 3
        func step(_ s: String) async {
            NSLog("[phone-demo] %@", s)
            try? await Task.sleep(for: .seconds(hold))
        }
        func fail(_ s: String) { NSLog("[phone-demo] FAIL %@", s) }

        // Wait for the opened document's first full scene.
        for _ in 0..<200 where model.source.isSandbox || model.solving || model.geometryDirty
            || ReleaseWindowController.shared.centeringEntity == nil {
            try? await Task.sleep(for: .milliseconds(100))
        }
        model.deselectAll()
        model.resetCamera()
        await step("opened \(model.documentName)")

        // Orbit: what a one-finger drag feeds (PhoneViewport.Coordinator.orbit).
        model.beginOrbit()
        for _ in 0..<40 {
            model.orbitDrag(dx: -9, dy: 1.5)
            try? await Task.sleep(for: .milliseconds(16))
        }
        model.endOrbit()
        model.stopSpin()
        await step("orbited az=\(model.azimuth) el=\(model.elevation)")

        // Tap-select: project part 0's centre to the screen and hit-test there,
        // exactly as a tap at that point would.
        let c = ReleaseWindowController.shared
        guard let p = c.viewPoint(forPart: 0, from: model.cameraPosition, lookAt: model.panOffset) else {
            return fail("part 0 not on screen")
        }
        // A tap picks the face under the finger (the Mac's Fusion-style click)…
        PhonePick.tap(at: p, model: model)      // exactly what the tap recognizer calls
        guard model.picking.selection.count == 1 else { return fail("tap at \(p) picked nothing") }
        await step("tap picked \(model.topologySelectionReadout() ?? "?") at (\(Int(p.x)), \(Int(p.y)))")
        // …and a double tap reaches the whole body (the Mac's double-click).
        PhonePick.doubleTap(at: p, model: model)
        guard model.hasSelection else { return fail("double tap at \(p) selected nothing") }
        try? await Task.sleep(for: .milliseconds(300))
        await step("double tap selected \(model.selectedFeatureNode?.name ?? "?"); inspector \(PhoneDocumentFlow.shared.inspectorShown ? "open" : "CLOSED")")

        // Edit a parameter in the inspector card: the wall's height.
        guard let wall = find(model.featureNodes, where: { $0.opType == "Cube" && ($0.name.contains("wall")) })
                ?? find(model.featureNodes, where: { $0.opType == "Cube" }) else { return fail("no cube feature") }
        model.selectFeature(wall.id)
        await step("inspect \(wall.name)")
        let before = (model.opDict(nodeId: wall.nodeId)?["size"] as? [String: Any])?["z"] as? Double ?? 0
        model.editVec(nodeId: wall.nodeId, key: "size", axis: "z", value: before + 20, snapshot: true,
                      name: "Change Size")
        await step("edited \(wall.name).size.z \(before) -> \(before + 20)")

        // Undo (the top bar's ↶).
        model.undo()
        let afterUndo = (model.opDict(nodeId: wall.nodeId)?["size"] as? [String: Any])?["z"] as? Double ?? -1
        await step("undo -> size.z \(afterUndo) (\(afterUndo == before ? "restored" : "MISMATCH"))")

        // Edit again, then Save (the ⋯ menu's Save) and read the file back.
        model.editVec(nodeId: wall.nodeId, key: "size", axis: "z", value: before + 10, snapshot: true,
                      name: "Change Size")
        PhoneDocumentFlow.shared.inspectorShown = false
        await step("edited again size.z -> \(before + 10)")
        let undoBefore = model.undoStack.count
        model.saveDocument()
        // The file watcher fires ~0.6 s after our own save; undo must survive it.
        try? await Task.sleep(for: .seconds(1.5))
        let undoAfter = model.undoStack.count
        NSLog("[phone-demo] undo depth before save %d, 1.5 s after %d (%@)", undoBefore, undoAfter,
              undoAfter == undoBefore ? "PASS" : "FAIL")
        guard let url = model.documentURL, let data = try? Data(contentsOf: url),
              let json = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
              let nodes = json["nodes"] as? [String: Any],
              let n = nodes["\(wall.nodeId)"] as? [String: Any], let op = n["op"] as? [String: Any],
              let size = op["size"] as? [String: Any], let z = size["z"] as? Double else {
            return fail("saved file unreadable")
        }
        GlassUI.shared.flash("Saved \(model.documentName)", symbol: "checkmark.circle")
        await step("saved \(url.lastPathComponent): on-disk size.z=\(z) dirty=\(model.documentDirty) (\(z == before + 10 ? "PASS" : "FAIL"))")
        model.undo()
        let z2 = (model.opDict(nodeId: wall.nodeId)?["size"] as? [String: Any])?["z"] as? Double ?? -1
        await step("undo after save -> size.z \(z2) (\(z2 == before ? "PASS" : "FAIL"))")
        NSLog("[phone-demo] done")
    }

    private static func find(_ nodes: [FeatureNode], where f: (FeatureNode) -> Bool) -> FeatureNode? {
        for n in nodes {
            if f(n) { return n }
            if let hit = find(n.children, where: f) { return hit }
        }
        return nil
    }
}

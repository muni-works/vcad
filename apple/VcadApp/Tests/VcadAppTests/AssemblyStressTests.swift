import XCTest
import simd
@testable import VcadApp

/// Assembly stress path (docs/assembly-stress.md): open/import → scene →
/// select → joint drag → interference → explode → save/reopen → export, timed
/// per step. K1 always runs; the Voron 2.4 STEP runs when
/// `scripts/fetch-voron-step.sh` has fetched it (it is not in git).
@MainActor
final class AssemblyStressTests: XCTestCase {
    private var temps: [URL] = []
    override func tearDown() async throws {
        for u in temps { try? FileManager.default.removeItem(at: u) }
    }

    private static var repoRoot: URL {
        var dir = URL(fileURLWithPath: #filePath)
        for _ in 0..<8 {
            dir = dir.deletingLastPathComponent()
            if FileManager.default.fileExists(atPath: dir.appendingPathComponent("third_party/booster-k1").path) { return dir }
        }
        return dir
    }

    private func temp(_ ext: String) -> URL {
        let u = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + "." + ext)
        temps.append(u)
        return u
    }

    private var timings: [(String, Double)] = []
    @discardableResult
    private func time<T>(_ step: String, _ body: () throws -> T) rethrows -> T {
        let t0 = Date()
        let r = try body()
        let dt = Date().timeIntervalSince(t0)
        timings.append((step, dt))
        print(String(format: "[stress] %-28@ %8.3f s", step as NSString, dt))
        return r
    }
    private func timeAsync(_ step: String, _ body: () async throws -> Void) async rethrows {
        let t0 = Date()
        try await body()
        let dt = Date().timeIntervalSince(t0)
        timings.append((step, dt))
        print(String(format: "[stress] %-28@ %8.3f s", step as NSString, dt))
    }

    private func runPath(_ m: EditorModel, label: String) async throws {
        _ = time("\(label) buildScene") { m.buildScene() }
        print("[stress] \(label) instances=\(m.assemblyInstanceCount) joints=\(m.asmJoints.count) parts=\(m.clearanceBodyCount)")

        // Drive the first driveable joint, if any.
        let specs = m.asmJoints.map(AsmEdit.jointSpec)
        let ids = m.asmJoints.compactMap { $0["id"] as? String }
        if let i = specs.firstIndex(where: { $0.kind == .revolute }), i < ids.count {
            let r = specs[i].limits ?? -90...90
            let target = min(max(20, r.lowerBound), r.upperBound)
            time("\(label) drag joint ×30") {
                for k in 0..<30 { m.dragJoint(ids[i], to: target * Double(k) / 29) }
            }
            time("\(label) commit joint") { m.commitJointDrag(ids[i]) }
        }

        let s = AnalysisStore.shared
        await timeAsync("\(label) interference") { await s.runClearance(m) }
        if let rep = s.clearance {
            print("[stress] \(label) clearance pairs=\(rep.pairs.count) parts=\(rep.parts.count)")
        } else { print("[stress] \(label) clearance error: \(s.clearanceError ?? "?")") }

        _ = time("\(label) rebuild after edit") { m.buildScene() }
        time("\(label) explode 0→1 ×11") { for k in 0...10 { m.setExplode(Double(k) / 10) } }
        m.setExplode(0)

        let out = temp("vcad")
        time("\(label) save") { m.saveDocumentAs(out) }
        let r = EditorModel()
        time("\(label) reopen") { r.openDocument(out) }
        XCTAssertTrue(r.canEditDocument, r.engineError ?? "reopen failed")
        XCTAssertEqual(r.asmJoints.count, m.asmJoints.count, "joints survive save → reopen")

        for f in [DocumentExportFormat.glb, .step] {
            let url = temp(f.fileExtension)
            let json = try XCTUnwrap(m.exportableDocumentData())
            let res = time("\(label) export \(f.label)") {
                KernelExport.export(json: json, baseDir: m.exportBaseDir, to: url, format: f)
            }
            switch res {
            case .success(let n):
                let size = (try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? Int) ?? 0
                print("[stress] \(label) \(f.label): \(n) parts, \(size) bytes")
                if f == .glb, label == "K1" {
                    // One mesh per link: def roots must not ride along unposed.
                    XCTAssertEqual(n, m.assemblyInstanceCount, "GLB exports instances, not a copy of every def")
                }
            case .failure(let e): print("[stress] \(label) \(f.label) FAILED: \(e.message)")
            }
        }
    }

    func testK1StressPath() async throws {
        let urdf = Self.repoRoot.appendingPathComponent("third_party/booster-k1/K1_22dof_floating.urdf")
        let m = EditorModel()
        let imported = try XCTUnwrap(time("K1 import URDF") { m.importForeign(urdf) }, m.loadError ?? "import")
        temps.append(imported)
        time("K1 open") { m.openDocument(imported) }
        XCTAssertTrue(m.canEditDocument, m.engineError ?? "no engine")
        try await runPath(m, label: "K1")
    }

    func testVoronStressPath() async throws {
        let step = Self.repoRoot.appendingPathComponent("target/stress/voron/Voron_2.4r2_Assembly.step")
        try XCTSkipUnless(FileManager.default.fileExists(atPath: step.path), "run scripts/fetch-voron-step.sh")
        let m = EditorModel()
        let imported = try XCTUnwrap(time("Voron import STEP") { m.importForeign(step) }, m.loadError ?? "import")
        temps.append(imported)
        time("Voron open") { m.openDocument(imported) }
        XCTAssertTrue(m.canEditDocument, m.engineError ?? "no engine")
        try await runPath(m, label: "Voron")
    }
}

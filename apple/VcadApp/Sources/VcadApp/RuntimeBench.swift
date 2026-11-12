import Foundation

/// Scripted parameter scrub + Mallet strike, for profiling the main thread
/// with `sample(1)` (docs/runtime-design.md, slice 1 "done" check):
///
///     VCAD_OPEN=docs/launch/hero-demo/glockenspiel.vcad VCAD_SCRUB_BENCH=1 \
///       apple/VcadApp/dist/vcad.app/Contents/MacOS/vcad
///
/// Waits for the document, idles `VCAD_SCRUB_BENCH_DELAY` seconds (default 4,
/// time to attach `sample`), then drives the same calls the inspector's
/// ScrubField and the Mallet tool make: 180 scrub ticks at ~60 Hz on the
/// first document parameter, then five strikes on the first bar. Markers go
/// to stderr as `[VCAD_BENCH]`. `VCAD_RUNTIME_SCRUB=0` runs the old path.
@MainActor
enum RuntimeScrubBench {
    static func runIfRequested(_ m: EditorModel) async {
        guard ProcessInfo.processInfo.environment["VCAD_SCRUB_BENCH"] != nil else { return }
        func emit(_ s: String) { FileHandle.standardError.write(Data("[VCAD_BENCH] \(s)\n".utf8)) }
        func wait(_ s: Double) async { try? await Task.sleep(nanoseconds: UInt64(s * 1e9)) }
        for _ in 0..<240 where m.docPartMeshes.isEmpty { await wait(0.25) }
        let delay = Double(ProcessInfo.processInfo.environment["VCAD_SCRUB_BENCH_DELAY"] ?? "") ?? 4
        await wait(delay)
        guard let p = m.docParameters.first(where: { $0.isLiteral }), let v0 = p.value else {
            emit("no literal parameter"); return
        }
        emit("scrub start \(p.name) runtime=\(m.runtimeScrubEnabled) rev=\(m.documentRevision)")
        let t0 = Date()
        var worst = 0.0
        for i in 0..<180 {
            let tick = Date()
            let v = v0 + 10 * sin(Double(i) / 180 * 2 * .pi)
            m.editParameter(p.name, value: v, snapshot: i == 0)
            ReleaseWindowController.shared.refreshGeometry(model: m, collisions: i == 0)
            worst = max(worst, Date().timeIntervalSince(tick))
            await wait(1.0 / 60)
        }
        for _ in 0..<400 where m.runtimeEditsInFlight { await wait(0.01) }
        emit(String(format: "scrub end %.2fs worst-tick %.1fms rev=%d", Date().timeIntervalSince(t0),
                    worst * 1000, m.documentRevision))

        SoundStore.syncNames(m)
        let bar = SoundFFI.partNames.firstIndex { $0.hasPrefix("bar_") } ?? 0
        emit("strike start part \(bar) (\(bar < SoundFFI.partNames.count ? SoundFFI.partNames[bar] : "?"))")
        let s0 = Date()
        for k in 0..<5 {
            let tick = Date()
            guard await SoundStore.shared.ensureModes(part: bar, m) != nil else { emit("no modes"); break }
            SoundStore.shared.strike(vertex: k, dir: SIMD3(0, 0, -1))
            emit(String(format: "strike %d %.1fms", k, Date().timeIntervalSince(tick) * 1000))
            await wait(0.4)
        }
        emit(String(format: "strike end %.2fs", Date().timeIntervalSince(s0)))
        emit("done")
    }
}

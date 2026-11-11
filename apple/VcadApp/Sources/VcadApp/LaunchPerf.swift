// Launch and viewport timing for the launch speed pass, behind VCAD_PERF=1.
//
// Marks are milliseconds since the PROCESS started (kinfo_proc start time, so
// dyld + static init are included — what a user waits through after a click),
// appended to /tmp/vcad_perf.log next to the click-path lines.
//
// VCAD_PERF_ORBIT=<seconds> spins the camera around the part once the first
// full scene is on screen and reports mean / p5 frame rate from RealityKit's
// own scene-update events; VCAD_PERF_EXIT=1 quits after the report, so
// scripts/launch-perf.sh can time a cold launch end to end.
import AppKit
import Combine
import Foundation
import RealityKit

@MainActor
enum LaunchPerf {
    static let enabled = ProcessInfo.processInfo.environment["VCAD_PERF"] == "1"
    static let orbitSeconds = Double(ProcessInfo.processInfo.environment["VCAD_PERF_ORBIT"] ?? "") ?? 0
    static let exitWhenDone = ProcessInfo.processInfo.environment["VCAD_PERF_EXIT"] == "1"

    private static var seen: Set<String> = []

    /// Wall-clock time the process was exec'd.
    static let processStart: Date = {
        var info = kinfo_proc()
        var size = MemoryLayout<kinfo_proc>.stride
        var mib: [Int32] = [CTL_KERN, KERN_PROC, KERN_PROC_PID, getpid()]
        guard sysctl(&mib, 4, &info, &size, nil, 0) == 0 else { return Date() }
        let tv = info.kp_proc.p_un.__p_starttime
        return Date(timeIntervalSince1970: Double(tv.tv_sec) + Double(tv.tv_usec) / 1e6)
    }()

    static var sinceLaunchMs: Double { Date().timeIntervalSince(processStart) * 1000 }

    /// Log `label` once per process (later calls are ignored).
    static func once(_ label: String, _ extra: String = "") {
        guard enabled, !seen.contains(label) else { return }
        seen.insert(label)
        write(String(format: "[PERF] %@ %.0f ms since launch%@", label, sinceLaunchMs, extra))
    }

    static func write(_ line: String) {
        let data = Data((line + "\n").utf8)
        FileHandle.standardError.write(data)
        if let fh = FileHandle(forWritingAtPath: "/tmp/vcad_perf.log") {
            fh.seekToEndOfFile(); fh.write(data); try? fh.close()
        } else {
            try? data.write(to: URL(fileURLWithPath: "/tmp/vcad_perf.log"))
        }
    }

    // MARK: orbit benchmark

    private static var orbitSub: (any Combine.Cancellable)?
    private static var frameTimes: [Double] = []
    private static var orbitStart: CFAbsoluteTime = 0
    private static var lastFrame: CFAbsoluteTime = 0

    /// Start the orbit benchmark on `ar` (once). The camera circles `center`
    /// at its current distance; frame intervals come from scene updates.
    static func startOrbit(_ ar: ARView, camera: PerspectiveCamera, center: SIMD3<Float>, label: String) {
        guard enabled, orbitSeconds > 0, orbitSub == nil else { return }
        RenderGate.wake(for: orbitSeconds + 1)   // the orbit is driven by frames
        let start = camera.position
        let radius = max(simd_length(SIMD2(start.x - center.x, start.z - center.z)), 0.05)
        let height = start.y
        orbitStart = CFAbsoluteTimeGetCurrent(); lastFrame = orbitStart
        frameTimes.reserveCapacity(Int(orbitSeconds * 130))
        orbitSub = ar.scene.subscribe(to: SceneEvents.Update.self) { _ in
            MainActor.assumeIsolated {
                let now = CFAbsoluteTimeGetCurrent()
                frameTimes.append(now - lastFrame); lastFrame = now
                let t = Float(now - orbitStart)
                let a = t * 2 * .pi / 4                     // one turn per 4 s
                let p = SIMD3(center.x + radius * sin(a), height, center.z + radius * cos(a))
                camera.look(at: center, from: p, relativeTo: nil)
                if now - orbitStart >= orbitSeconds { finishOrbit(label: label) }
            }
        }
    }

    private static func finishOrbit(label: String) {
        orbitSub?.cancel(); orbitSub = nil
        let frames = Array(frameTimes.dropFirst(5))          // settle
        guard !frames.isEmpty else { return }
        let mean = Double(frames.count) / frames.reduce(0, +)
        let sorted = frames.sorted()
        let p95 = sorted[min(sorted.count - 1, Int(Double(sorted.count) * 0.95))]
        write(String(format: "[PERF] orbit %@ frames %d mean %.1f fps p5 %.1f fps worst %.1f ms",
                     label, frames.count, mean, 1 / p95, (sorted.last ?? 0) * 1000))
        if exitWhenDone { NSApp.terminate(nil) }
    }
}

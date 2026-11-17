#if os(macOS)
import AppKit
import simd

// VCAD_NAV_SMOKE=1 (with VCAD_OPEN=<doc> and `-vcad.prefs.presentation YES`):
// once the editor window is up, push synthesized mouse events through
// NSApp.sendEvent, so they reach the same local monitors a real mouse does
// (the middle-button monitor, HaloInput's right button). For each navigation
// preset it checks that the camera orbits or pans the way the preset says.
// The pointer never moves: the window is centred under wherever it already is.
// Prints one PASS/FAIL line per case and exits 1 if any case fails.

@MainActor
func navSmoke() async {
    func emit(_ s: String) { FileHandle.standardError.write(Data(("[VCAD_NAV] " + s + "\n").utf8)) }
    var failures = 0
    func check(_ ok: Bool, _ s: String) { emit((ok ? "PASS " : "FAIL ") + s); if !ok { failures += 1 } }

    // Wait for the editor window, its scene, and the document model.
    var window: NSWindow?, model: EditorModel?
    for _ in 0..<300 {
        window = ReleaseWindowController.shared.window
        model = AppInstance.currentModel
        if window != nil, ReleaseWindowController.shared.arView != nil, model != nil { break }
        try? await Task.sleep(for: .milliseconds(100))
    }
    guard let w = window, let m = model, let content = w.contentView else { emit("FAIL no editor window"); exit(1) }
    try? await Task.sleep(for: .seconds(2))

    var centre = CGPoint.zero

    func send(_ type: NSEvent.EventType, button: Int64, flags: NSEvent.ModifierFlags = [],
              clicks: Int = 1, dx: Double = 0, dy: Double = 0, at offset: CGFloat = 0) -> Bool {
        // AppKit window coords are y-up; a +dy (downward) drag lowers y.
        let at = CGPoint(x: centre.x + offset, y: centre.y - offset * 3 / 8)
        guard let base = NSEvent.mouseEvent(with: type, location: at, modifierFlags: flags,
                                            timestamp: ProcessInfo.processInfo.systemUptime,
                                            windowNumber: w.windowNumber, context: nil, eventNumber: 0,
                                            clickCount: clicks, pressure: type == .otherMouseUp || type == .rightMouseUp ? 0 : 1),
              let cg = base.cgEvent?.copy() else { return false }
        cg.setIntegerValueField(.mouseEventButtonNumber, value: button)
        cg.setDoubleValueField(.mouseEventDeltaX, value: dx)
        cg.setDoubleValueField(.mouseEventDeltaY, value: dy)
        guard let e = NSEvent(cgEvent: cg), e.window === w else { return false }
        NSApp.sendEvent(e)
        return true
    }
    /// Put the window under the pointer so the scene owns it (pointerOverScene
    /// follows the real pointer), then let the hover pass run.
    func hover() async {
        let mouse = NSEvent.mouseLocation
        w.setFrame(NSRect(x: mouse.x - 400, y: mouse.y - 300, width: 800, height: 600), display: true)
        try? await Task.sleep(for: .milliseconds(100))
        centre = content.convert(w.convertPoint(fromScreen: mouse), from: nil)
        _ = send(.mouseMoved, button: 0)
        try? await Task.sleep(for: .milliseconds(150))
    }

    enum Moved: String { case orbit, pan, nothing }
    /// One drag: down, a few moves, up. Reports what the camera did.
    func drag(_ button: NSEvent.EventType, flags: NSEvent.ModifierFlags = []) async -> Moved {
        await hover()
        m.stopSpin()
        let az = m.azimuth, el = m.elevation, pan = m.panOffset
        let (down, dragged, up): (NSEvent.EventType, NSEvent.EventType, NSEvent.EventType) =
            button == .rightMouseDown ? (.rightMouseDown, .rightMouseDragged, .rightMouseUp)
                                      : (.otherMouseDown, .otherMouseDragged, .otherMouseUp)
        let b: Int64 = button == .rightMouseDown ? 1 : 2
        guard send(down, button: b, flags: flags) else { return .nothing }
        // The location moves with the deltas: HaloInput tells a right-drag
        // from a right-click by how far the pointer travelled.
        for i in 1...6 {
            _ = send(dragged, button: b, flags: flags, dx: 8, dy: 3, at: CGFloat(8 * i))
            try? await Task.sleep(for: .milliseconds(16))
        }
        _ = send(up, button: b, flags: flags, at: 48)
        m.stopSpin()
        if abs(m.azimuth - az) > 1e-4 || abs(m.elevation - el) > 1e-4 { return .orbit }
        if simd_length(m.panOffset - pan) > 1e-6 { return .pan }
        return .nothing
    }

    let key = NavigationScheme.prefsKey
    func use(_ s: NavigationScheme) {
        // Volatile: never written to the user's real preferences.
        var args = UserDefaults.standard.volatileDomain(forName: UserDefaults.argumentDomain)
        args[key] = s.rawValue
        UserDefaults.standard.setVolatileDomain(args, forName: UserDefaults.argumentDomain)
    }

    let cases: [(NavigationScheme, NSEvent.EventType, NSEvent.ModifierFlags, Moved, String)] = [
        (.fusion360, .otherMouseDown, [], .pan, "middle-drag"),
        (.fusion360, .otherMouseDown, .shift, .orbit, "⇧ middle-drag"),
        (.solidworks, .otherMouseDown, [], .orbit, "middle-drag"),
        (.solidworks, .otherMouseDown, .control, .pan, "⌃ middle-drag"),
        (.onshape, .rightMouseDown, [], .orbit, "right-drag"),
        (.onshape, .rightMouseDown, .control, .pan, "⌃ right-drag"),
        (.onshape, .otherMouseDown, [], .pan, "middle-drag"),
        (.blender, .otherMouseDown, [], .orbit, "middle-drag"),
        (.blender, .otherMouseDown, .shift, .pan, "⇧ middle-drag"),
    ]
    for (scheme, button, flags, want, label) in cases {
        use(scheme)
        guard NavigationScheme.current == scheme else { check(false, "\(scheme.label): preset did not apply"); continue }
        let got = await drag(button, flags: flags)
        check(got == want, "\(scheme.label) \(label) → \(got.rawValue) (want \(want.rawValue))")
    }

    // Double-click middle fits everything (resetCamera puts azimuth at π/5).
    use(.fusion360)
    await hover()
    m.azimuth = 1.0
    _ = send(.otherMouseDown, button: 2, clicks: 1); _ = send(.otherMouseUp, button: 2, clicks: 1)
    _ = send(.otherMouseDown, button: 2, clicks: 2); _ = send(.otherMouseUp, button: 2, clicks: 2)
    try? await Task.sleep(for: .milliseconds(800))
    check(abs(m.azimuth - .pi / 5) < 1e-3, "double-click middle → fit all (azimuth \(m.azimuth))")

    emit(failures == 0 ? "ALL PASS" : "\(failures) FAILED")
    exit(failures == 0 ? 0 : 1)
}
#endif

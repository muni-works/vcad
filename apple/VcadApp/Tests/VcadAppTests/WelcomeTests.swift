import XCTest
@testable import VcadApp

/// The first-run samples: every one is listed, has a thumbnail, and opens
/// through the engine with geometry.
@MainActor
final class WelcomeTests: XCTestCase {
    private let samplesDir = URL(fileURLWithPath: #filePath)
        .deletingLastPathComponent().deletingLastPathComponent().deletingLastPathComponent()
        .appendingPathComponent("Samples")

    func testManifestListsTutorialAndSamples() throws {
        let samples = SampleLibrary.load(from: samplesDir)
        XCTAssertEqual(samples.count, 17, "every listed sample's file must exist")
        XCTAssertEqual(samples.filter { $0.tutorial == true }.map(\.file), ["first-part.loon"])
        for s in samples {
            XCTAssertNotNil(NSImage(contentsOf: samplesDir.appendingPathComponent(s.thumbnail)), s.thumbnail)
        }
    }

    func testDirectoryFoundThroughEnvironment() {
        XCTAssertEqual(SampleLibrary.directory(environment: ["VCAD_SAMPLES": samplesDir.path])?.standardizedFileURL,
                       samplesDir.standardizedFileURL)
    }

    func testEverySampleOpensWithGeometry() throws {
        let workspace = FileManager.default.temporaryDirectory
            .appendingPathComponent("vcad-samples-\(UUID().uuidString)")
        addTeardownBlock { try? FileManager.default.removeItem(at: workspace) }
        for s in SampleLibrary.load(from: samplesDir) {
            let url = try SampleLibrary.materialize(s, from: samplesDir, into: workspace)
            let attrs = try FileManager.default.attributesOfItem(atPath: url.path)
            XCTAssertEqual(attrs[.type] as? FileAttributeType, .typeRegular, "\(s.file) copied as a file, not a link")
            let model = EditorModel()
            model.openDocument(url)
            XCTAssertNil(model.loonDiagnosticOrNil, s.file)
            let scene = model.buildScene()
            XCTAssertGreaterThan(scene.partCount, 0, "\(s.file) evaluates to parts")
            XCTAssertGreaterThan(model.triangleCount, 0, "\(s.file) has triangles")
            if s.assets != nil {
                // Mesh imports resolve against the copied assets, not the bundle:
                // the K1's 23 STL links land (a missing mesh still yields the
                // one placeholder cube, so count triangles, not parts).
                XCTAssertGreaterThan(model.triangleCount, 100_000, "\(s.file) loads its imported meshes")
            }
        }
    }

    func testMaterializeNeverOverwritesAnEditedCopy() throws {
        let workspace = FileManager.default.temporaryDirectory
            .appendingPathComponent("vcad-samples-\(UUID().uuidString)")
        addTeardownBlock { try? FileManager.default.removeItem(at: workspace) }
        let tutorial = try XCTUnwrap(SampleLibrary.load(from: samplesDir).first { $0.tutorial == true })
        let url = try SampleLibrary.materialize(tutorial, from: samplesDir, into: workspace)
        try "; edited\n".write(to: url, atomically: true, encoding: .utf8)
        _ = try SampleLibrary.materialize(tutorial, from: samplesDir, into: workspace)
        XCTAssertEqual(try String(contentsOf: url, encoding: .utf8), "; edited\n")
    }

    func testWelcomeShowsAtLaunchUntilTurnedOffAndNotUnderDevHooks() throws {
        let defaults = try XCTUnwrap(UserDefaults(suiteName: "vcad-welcome-test-\(UUID().uuidString)"))
        let model = EditorModel()
        XCTAssertTrue(WelcomeWindowController.shouldShowAtLaunch(model: model, defaults: defaults, environment: [:]))
        // Shown once is no longer "never again": a returning user gets Recent.
        XCTAssertTrue(WelcomeWindowController.shouldShowAtLaunch(model: model, defaults: defaults, environment: [:]))
        XCTAssertFalse(WelcomeWindowController.shouldShowAtLaunch(model: model, defaults: defaults,
                                                                  environment: ["VCAD_OPEN": "/x.vcad"]))
        XCTAssertFalse(WelcomeWindowController.shouldShowAtLaunch(model: model, defaults: defaults,
                                                                  environment: ["VCAD_NO_WELCOME": "1"]))
        XCTAssertTrue(WelcomeWindowController.shouldShowAtLaunch(model: model, defaults: defaults,
                                                                 environment: ["VCAD_WELCOME": "1", "VCAD_OPEN": "/x"]))
        XCTAssertTrue(WelcomeWindowController.shouldShowAtLaunch(model: model, defaults: defaults,
                                                                 environment: ["VCAD_SAMPLES": "/s"]))
        WelcomePrefs.setShowAtLaunch(false, defaults)
        XCTAssertFalse(WelcomeWindowController.shouldShowAtLaunch(model: model, defaults: defaults, environment: [:]))
        XCTAssertFalse(WelcomePrefs.shouldShowAtLaunch(isScratch: false, defaults: try XCTUnwrap(
            UserDefaults(suiteName: "vcad-welcome-test-\(UUID().uuidString)")), environment: [:]),
                       "an instance opened for a document never shows it")
    }

    func testShowAtLaunchPersists() throws {
        let suite = "vcad-welcome-test-\(UUID().uuidString)"
        let defaults = try XCTUnwrap(UserDefaults(suiteName: suite))
        addTeardownBlock { UserDefaults().removePersistentDomain(forName: suite) }
        XCTAssertTrue(WelcomePrefs.showAtLaunch(defaults), "on by default")
        WelcomePrefs.setShowAtLaunch(false, defaults)
        XCTAssertFalse(WelcomePrefs.showAtLaunch(try XCTUnwrap(UserDefaults(suiteName: suite))), "survives a reload")
        WelcomePrefs.setShowAtLaunch(true, defaults)
        XCTAssertTrue(WelcomePrefs.showAtLaunch(defaults))
    }

    func testRecentsDropMissingAndDuplicatesAndCapAtFour() throws {
        let dir = FileManager.default.temporaryDirectory.appendingPathComponent("vcad-recents-\(UUID().uuidString)")
        try FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        addTeardownBlock { try? FileManager.default.removeItem(at: dir) }
        let files = (0..<6).map { dir.appendingPathComponent("p\($0).vcad") }
        for f in files { try "{}".write(to: f, atomically: true, encoding: .utf8) }
        let paths = [files[0].path, dir.appendingPathComponent("gone.vcad").path, files[0].path,
                     files[1].path, dir.appendingPathComponent("./p2.vcad").path, files[3].path, files[4].path]
        let list = WelcomeRecents.list(paths: paths)
        XCTAssertEqual(list.map(\.name), ["p0", "p1", "p2", "p3"])
        XCTAssertNotNil(list.first?.modified)
        XCTAssertEqual(WelcomeRecents.list(paths: []).count, 0)
    }

    func testFirstLaunchVersusReturning() {
        XCTAssertEqual(WelcomeMode(recents: []), .firstLaunch)
        XCTAssertTrue(WelcomeMode.firstLaunch.examplesTitle.contains("teaches"))
        let r = WelcomeRecent(url: URL(fileURLWithPath: "/tmp/a.vcad"), modified: nil)
        XCTAssertEqual(WelcomeMode(recents: [r]), .returning)
        XCTAssertEqual(WelcomeMode.returning.examplesTitle, "Examples")
    }

    func testRecentContextLine() throws {
        var cal = Calendar(identifier: .gregorian)
        cal.timeZone = try XCTUnwrap(TimeZone(identifier: "UTC"))
        let now = try XCTUnwrap(cal.date(from: DateComponents(year: 2026, month: 11, day: 12, hour: 15)))
        let yesterday = now.addingTimeInterval(-86_400)
        XCTAssertEqual(WelcomeRecents.context(parts: 9, modified: yesterday, now: now, calendar: cal), "9 parts · yesterday")
        XCTAssertEqual(WelcomeRecents.context(parts: 1, modified: now, now: now, calendar: cal), "1 part · today")
        XCTAssertEqual(WelcomeRecents.context(parts: nil, modified: now.addingTimeInterval(-3 * 86_400), now: now,
                                              calendar: cal), "3 days ago")
        XCTAssertEqual(WelcomeRecents.context(parts: 0, modified: nil, now: now, calendar: cal), "")
    }

    func testExamplesAreTheFourTeachingSamples() {
        let examples = WelcomeExamples.pick(SampleLibrary.load(from: samplesDir))
        XCTAssertEqual(examples.map(\.id), ["bracket", "spur-gear", "enclosure", "tuned-bar"])
        XCTAssertTrue(examples.allSatisfy { $0.lesson?.isEmpty == false })
    }

    func testAgentState() {
        let now = Date()
        XCTAssertEqual(WelcomeAgentState.from(statuses: [("Claude Code", .available), ("Codex", .notDetected)],
                                              lastCall: nil, now: now), .notConnected(detected: ["Claude Code"]))
        XCTAssertEqual(WelcomeAgentState.from(statuses: [("Claude Code", .connected)], lastCall: nil, now: now),
                       .connected("Claude Code"))
        XCTAssertEqual(WelcomeAgentState.from(statuses: [("Claude Code", .connected)],
                                              lastCall: now.addingTimeInterval(-10), now: now), .active("Claude Code"))
        XCTAssertEqual(WelcomeAgentState.from(statuses: [("Claude Code", .connected)],
                                              lastCall: now.addingTimeInterval(-600), now: now), .connected("Claude Code"))
        XCTAssertEqual(WelcomeAgentState.from(statuses: [("Cursor", .foreign("vcad"))], lastCall: nil, now: now),
                       .connected("Cursor"))
        XCTAssertEqual(WelcomeAgentState.connected("Claude Code").title, "Claude Code connected")
        XCTAssertFalse(WelcomeAgentState.notConnected(detected: []).isConnected)
    }

    func testGridKeyboardNavigation() {
        XCTAssertEqual(WelcomeGridNav.move(0, .right, columns: 4, count: 4), 1)
        XCTAssertNil(WelcomeGridNav.move(3, .right, columns: 4, count: 4), "no wrap off the row end")
        XCTAssertNil(WelcomeGridNav.move(0, .left, columns: 4, count: 4))
        XCTAssertNil(WelcomeGridNav.move(1, .down, columns: 4, count: 4), "leaves the grid → next section")
        XCTAssertEqual(WelcomeGridNav.move(1, .down, columns: 4, count: 8), 5)
        XCTAssertEqual(WelcomeGridNav.move(5, .up, columns: 4, count: 8), 1)
        XCTAssertNil(WelcomeGridNav.move(2, .right, columns: 4, count: 3))
        XCTAssertNil(WelcomeGridNav.move(0, .right, columns: 4, count: 0))
    }

    func testWhatsNewLinksToTheGitHubRelease() {
        XCTAssertEqual(WelcomeLinks.whatsNew(version: "1.0.0-beta.1").absoluteString,
                       "https://github.com/muni-works/vcad/releases/tag/desktop-v1.0.0-beta.1")
        XCTAssertEqual(WelcomeLinks.whatsNew(version: "1.2.0").absoluteString,
                       "https://github.com/muni-works/vcad/releases/tag/desktop-v1.2.0")
        XCTAssertEqual(WelcomeLinks.whatsNew(version: "0.1"), WelcomeLinks.releases, "dev build")
        XCTAssertEqual(WelcomeLinks.whatsNew(version: nil), WelcomeLinks.releases)
        XCTAssertEqual(WelcomeLinks.whatsNew(version: "1.0 x/../"), WelcomeLinks.releases)
    }

    func testThumbnailKeyChangesWithTheFile() {
        let a = WelcomeThumbnailKey.key(path: "/x.vcad", modified: Date(timeIntervalSince1970: 100), size: 10)
        XCTAssertEqual(a, WelcomeThumbnailKey.key(path: "/x.vcad", modified: Date(timeIntervalSince1970: 100), size: 10))
        XCTAssertNotEqual(a, WelcomeThumbnailKey.key(path: "/x.vcad", modified: Date(timeIntervalSince1970: 101), size: 10))
        XCTAssertNotEqual(a, WelcomeThumbnailKey.key(path: "/y.vcad", modified: Date(timeIntervalSince1970: 100), size: 10))
        XCTAssertEqual(a.count, 24)
    }

    func testThumbnailRendersASample() throws {
        let png = try XCTUnwrap(WelcomeThumbnailRenderer.render(url: samplesDir.appendingPathComponent("tuned-bar.loon")))
        XCTAssertEqual(png.parts, 1)
        XCTAssertNotNil(NSImage(data: png.png))
        XCTAssertNil(WelcomeThumbnailRenderer.render(url: URL(fileURLWithPath: "/nonexistent.vcad")))
    }
}

private extension EditorModel {
    /// `loadError` on this branch (the loon diagnostic lands separately).
    var loonDiagnosticOrNil: String? { loadError }
}

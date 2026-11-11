import XCTest
@testable import VcadApp

/// v1 ships Design + Agent; Electronics, Manufacture and simulation sit
/// behind Settings ▸ Labs (off by default).
@MainActor
final class LabsTests: XCTestCase {
    private var saved: Any?

    override func setUp() async throws { saved = UserDefaults.standard.object(forKey: Prefs.labsKey) }
    override func tearDown() async throws {
        if let saved { UserDefaults.standard.set(saved, forKey: Prefs.labsKey) }
        else { UserDefaults.standard.removeObject(forKey: Prefs.labsKey) }
    }

    func testLabsOffByDefaultOffersOnlyDesign() {
        UserDefaults.standard.removeObject(forKey: Prefs.labsKey)
        XCTAssertFalse(Prefs.labs)
        XCTAssertEqual(Workspace.available, [.design, .drawing])
    }

    /// A launch never comes back to a workspace that doesn't mount the 3D
    /// viewport: opening a document straight into Drawing broke its first
    /// viewport build (live QA 2026-11-11).
    func testLaunchNeverRestoresAViewportlessWorkspace() {
        UserDefaults.standard.set(true, forKey: Prefs.labsKey)
        XCTAssertEqual(Workspace.launchRestorable(.design), .design)
        XCTAssertEqual(Workspace.launchRestorable(.manufacture), .manufacture)
        XCTAssertEqual(Workspace.launchRestorable(.drawing), .design)
        XCTAssertEqual(Workspace.launchRestorable(.electronics), .design)
        UserDefaults.standard.set(false, forKey: Prefs.labsKey)
        XCTAssertNil(Workspace.launchRestorable(.manufacture), "a Labs workspace isn't restored with Labs off")
    }

    /// Frame All fits the model: a tall part comes closer than a cube, and
    /// both sit inside the old fixed 1.5 (which left a 240 mm-tall assembly
    /// in a quarter of the view).
    func testFrameAllFitsTheModel() {
        let cube = EditorModel.frameAllDistance(size: [30, 30, 30], aspect: 1.5)
        let tall = EditorModel.frameAllDistance(size: [60, 60, 240], aspect: 1.5)
        XCTAssertLessThan(cube, 1.5)
        XCTAssertLessThan(tall, cube)
        XCTAssertEqual(EditorModel.frameAllDistance(size: .zero, aspect: 1.5), 1.5)
    }

    func testLabsOnOffersEveryWorkspace() {
        UserDefaults.standard.set(true, forKey: Prefs.labsKey)
        XCTAssertEqual(Workspace.available, Workspace.allCases)
    }

    func testDXFOpenIsRefusedWithoutLabs() throws {
        UserDefaults.standard.set(false, forKey: Prefs.labsKey)
        let url = FileManager.default.temporaryDirectory.appendingPathComponent(UUID().uuidString + ".dxf")
        try "0\nEOF\n".write(to: url, atomically: true, encoding: .utf8)
        defer { try? FileManager.default.removeItem(at: url) }
        let m = EditorModel()
        m.openDocument(url)
        XCTAssertEqual(m.workspace, .design)
        XCTAssertTrue(m.loadError?.contains("Labs") == true, m.loadError ?? "nil")
    }

    func testSimulationUIIsGated() {
        UserDefaults.standard.set(false, forKey: Prefs.labsKey)
        let m = EditorModel()
        XCTAssertFalse(m.showsSimulation)
    }
}

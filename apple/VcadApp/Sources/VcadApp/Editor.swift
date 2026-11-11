import SwiftUI
import RealityKit
#if canImport(AppKit)
import AppKit
#endif
import simd
import CVcadFFI

// Model layer. The SwiftUI shell is in Shell.swift; kernel/streaming in Kernel.swift.

/// One motion vocabulary for the whole app, so every surface moves with the same
/// personality. Reach for these instead of ad-hoc `.snappy`/`.smooth` literals.
enum Motion {
    /// Selection, tab swaps, small state flips — quick and crisp.
    static let snappy = Animation.snappy(duration: 0.22, extraBounce: 0.04)
    /// Panels appearing / docking — a soft settle.
    static let panel = Animation.spring(response: 0.4, dampingFraction: 0.84)
    /// Content morphs, value-driven layout shifts.
    static let smooth = Animation.smooth(duration: 0.3)
    /// A small confident "pop" for things that materialize.
    static let pop = Animation.spring(response: 0.32, dampingFraction: 0.7)
}

/// The app's workspaces — one exclusive mode at a time, each with its own
/// panels and its own remembered camera.
enum Workspace: String, CaseIterable, Identifiable, Codable {
    case design, electronics, manufacture, drawing
    var id: String { rawValue }
    /// The workspace a launch may come back to. Drawing and Electronics don't
    /// mount the 3D viewport, and a document opened straight into one of them
    /// first built its viewport only on leaving it — that late first build came
    /// up wrong (an "Opening" card that never cleared; a model drawn as a huge
    /// black silhouette around the camera). Those launches start in Design;
    /// switching into Drawing afterwards is unaffected.
    static func launchRestorable(_ ws: Workspace) -> Workspace? {
        guard available.contains(ws) else { return nil }
        switch ws {
        case .design, .manufacture: return ws
        case .drawing, .electronics: return .design
        }
    }
    var label: String {
        switch self {
        case .design: return "Design"
        case .electronics: return "Electronics"
        case .manufacture: return "Manufacture"
        case .drawing: return "Drawing"
        }
    }
    var symbol: String {
        switch self {
        case .design: return "cube"
        case .electronics: return "cpu"
        case .manufacture: return "wrench.and.screwdriver"
        case .drawing: return "ruler"
        }
    }
    /// Workspaces the UI offers: Design, plus the Labs ones when enabled.
    static var available: [Workspace] { Prefs.labs ? allCases : [.design, .drawing] }
    var keyEquivalent: KeyEquivalent {
        switch self {
        case .design: return "1"
        case .electronics: return "2"
        case .manufacture: return "3"
        case .drawing: return "4"
        }
    }
}

/// The standard camera views.
enum CameraPreset {
    case isometric, front, right, top
    var azimuth: Float {
        switch self {
        case .isometric: return .pi / 5
        case .front, .top: return 0
        case .right: return .pi / 2
        }
    }
    var elevation: Float {
        switch self {
        case .isometric: return .pi / 7
        case .front, .right: return 0
        case .top: return 1.45
        }
    }
}

/// One undo step, and what the edit was called so the Edit menu can say
/// "Undo Extrude" rather than "Undo".
struct UndoEntry: Equatable {
    enum Step: Equatable {
        /// Exactly one engine history group: undone with the engine's own
        /// `undo` command.
        case engine
        /// An engine snapshot (document + history) from before a compound
        /// edit — a drag, a scrub, or an IR edit the command vocabulary has
        /// no verb for yet.
        case snapshot(Data)
        /// Serialized document JSON, for documents edited outside the engine
        /// (electronics, which the command vocabulary doesn't cover yet).
        case json(Data)
    }
    let step: Step
    let name: String

    init(step: Step, name: String) { self.step = step; self.name = name }
    /// A detached-JSON step (the electronics workspace's grouping).
    init(data: Data, name: String) { self.init(step: .json(data), name: name) }
}

enum GeometrySource: Hashable, Identifiable {
    case sandbox
    case document(path: String, label: String)
    /// Geometry authored by the AI intent bar: a loon program the kernel
    /// compiles + evaluates exactly like a loaded `.vcad` file.
    case generated(loon: String, label: String)
    /// The cross-domain slice: a resident parametric doc where one `connector_x`
    /// couples the enclosure cutout (mechanical) + the board connector (electrical).
    case gripper

    var id: String {
        switch self {
        case .sandbox: return "sandbox"
        case .document(let path, _): return path
        case .generated: return "generated"
        case .gripper: return "gripper"
        }
    }
    var label: String {
        switch self {
        case .sandbox: return "Untitled"
        case .document(_, let label): return label
        case .generated(_, let label): return label
        case .gripper: return "Gripper"
        }
    }
    var isSandbox: Bool { if case .sandbox = self { return true }; return false }
    var isGenerated: Bool { if case .generated = self { return true }; return false }
    var isGripper: Bool { if case .gripper = self { return true }; return false }
}

/// The sandbox primitive — chosen from the Create tab of the tool palette.
enum BaseShape: String, CaseIterable {
    case cube, cylinder, sphere, cone, torus, wedge, prism
    var label: String { rawValue.capitalized }
    var symbol: String {
        switch self {
        case .cube: return "cube"
        case .cylinder: return "cylinder"
        case .sphere: return "globe"
        case .cone: return "cone"
        case .torus: return "circle.circle"
        case .wedge: return "triangle.righthalf.filled"
        case .prism: return "hexagon"
        }
    }
    /// The sandbox preview only builds these three; the rest promote to a
    /// document first.
    var sandboxPreview: Bool { [.cube, .cylinder, .sphere].contains(self) }
}

/// The sandbox modifier — chosen from the Modify tab.
enum Modifier: String, CaseIterable {
    case none, fillet, chamfer
    var label: String { self == .none ? "None" : rawValue.capitalized }
    var symbol: String {
        switch self {
        case .none: return "minus.circle"
        case .fillet: return "square.on.circle.fill"
        case .chamfer: return "triangle"
        }
    }
    var paramLabel: String { self == .chamfer ? "Distance" : "Radius" }
}

/// A tab in the tool palette (the native reinterpretation of the web app's
/// Borland tool picker — same model, native skin).
enum ToolTab: String, CaseIterable, Identifiable {
    case create, modify, combine
    var id: String { rawValue }
    var label: String { rawValue.capitalized }
    var symbol: String {
        switch self {
        case .create: return "plus.square.on.square"
        case .modify: return "wand.and.rays"
        case .combine: return "square.on.square.dashed"
        }
    }
    /// The palette's resting hint — what this page of the palette is for, shown
    /// when the pointer is not over a tool.
    var paletteHint: String {
        switch self {
        case .create: return "Add geometry to the document"
        case .modify: return "Edit the selected part"
        case .combine: return "Boolean two selected parts"
        }
    }
}

/// A boolean combine operation (the Combine tab — acts on two selected parts).
enum BooleanOp: String, CaseIterable, Identifiable {
    case union, difference, intersection
    var id: String { rawValue }
    var label: String {
        switch self {
        case .union: return "Union"
        case .difference: return "Subtract"
        case .intersection: return "Intersect"
        }
    }
    /// The IR op tag emitted into the document.
    var opType: String {
        switch self {
        case .union: return "Union"
        case .difference: return "Difference"
        case .intersection: return "Intersection"
        }
    }
    var symbol: String {
        switch self {
        case .union: return "plus.circle"
        case .difference: return "minus.circle"
        case .intersection: return "square.on.square.intersection.dashed"
        }
    }
}

/// A 2D sketch drawing tool (mirrors the web app's line/rectangle/circle tools).

/// An axis-aligned sketch plane (origin at world 0). Basis vectors place 2D
/// sketch coords in 3D; `normal` is the extrude direction.
enum SketchPlane: String, CaseIterable, Identifiable {
    case xy, xz, yz
    var id: String { rawValue }
    var label: String { rawValue.uppercased() }
    var xDir: (Double, Double, Double) {
        switch self { case .xy: return (1, 0, 0); case .xz: return (1, 0, 0); case .yz: return (0, 1, 0) }
    }
    var yDir: (Double, Double, Double) {
        switch self { case .xy: return (0, 1, 0); case .xz: return (0, 0, 1); case .yz: return (0, 0, 1) }
    }
    var normal: (Double, Double, Double) {
        switch self { case .xy: return (0, 0, 1); case .xz: return (0, 1, 0); case .yz: return (1, 0, 0) }
    }
    var xDirF: SIMD3<Float> { SIMD3(Float(xDir.0), Float(xDir.1), Float(xDir.2)) }
    var yDirF: SIMD3<Float> { SIMD3(Float(yDir.0), Float(yDir.1), Float(yDir.2)) }
    var normalF: SIMD3<Float> { SIMD3(Float(normal.0), Float(normal.1), Float(normal.2)) }
}

/// One tool button within a tab.
struct Tool: Identifiable {
    let id: String
    let label: String
    let symbol: String
    let isActive: Bool
    var enabled: Bool = true
    var hint: String = ""
    /// A bare-letter key (Fusion/KiCad style) — fires only when nothing else
    /// handles the key, so it can never steal typing from a field.
    var shortcut: Character? = nil
    /// Tools sharing a group collapse into one dock button with a popover.
    var group: String? = nil
    /// Like `group`, but only when the dock is short of room.
    var compactGroup: String? = nil
    let action: () -> Void
}

/// A node in the feature tree (the parametric DAG, shown in the sidebar).
struct Feature: Identifiable, Hashable {
    enum Kind { case base, modifier, part }
    let id: String
    let name: String
    let symbol: String
    let kind: Kind
}

/// Stats captured when a generated program is validated — drives the intent
/// bar's "Built · N parts · W×H×D mm" confirmation.
struct GenStats {
    var parts: Int
    var size: SIMD3<Float>
    var triangles: Int
}

@MainActor
@Observable
final class EditorModel {
    let cnc = CNCWorkspace()
    /// Face/edge selection + hover (Picking.swift) — shared with the
    /// modeling ops and the analysis lanes.
    let picking = PickingState()
    /// The feature dialog on screen (FeatureOps.swift), if any.
    var activeFeatureOp: FeatureOp?
    /// Measure tool on (Measure.swift): clicks pick points/edges/faces.
    var measuring = false {
        didSet { if measuring { picking.filter = .faceOrEdge; picking.clear() } }
    }
    /// Section plane (Section.swift); nil = off.
    var section: SectionPlane?
    /// Command palette / parameter table sheets.
    var showsCommandPalette = false
    var sketchPlaneChosen = false
    /// The external MCP session this window shows, and the engine state it last pushed.
    var agentLinkID: String?
    var agentLinkRevision: String?
    /// The linked document exists only because an agent pushed it into an empty
    /// window: another agent session may take the window over while it is
    /// unedited. False for a document the user opened and an agent attached to.
    var agentLinkOwned = false
    /// When the linked session last touched this window. A session may only
    /// take the window from one that has gone quiet (`AgentEdits.takeoverIdleSeconds`).
    var agentLinkActiveAt: Date?
    /// Folder a linked document's relative mesh paths resolve from.
    var agentLinkBaseDir: String?
    /// The link a pose has already moved to a posing workspace, so a pose
    /// stream switches at most once and the user can walk away.
    var agentPoseSurfacedLink: String?
    /// A simulated floating base: its first pose this episode, and its motion since.
    var agentBaseFirst: simd_float4x4?
    var agentBaseDelta: simd_float4x4?
    var showsParameterTable = false
    var isWindowed = false { didSet { saveLayout() } }
    init() {
        bridgeSimulationToRenderer()
        restoreLayout()
        cnc.onLayoutChange = { [weak self] in self?.saveLayout() }
    }

    // Orbit camera (radians / scene meters).
    var azimuth: Float = .pi / 5
    var elevation: Float = .pi / 7
    var distance: Float = 1.5

    let electronics = ElectronicsWorkspace()

    // MARK: workspaces

    /// The active workspace. Switching saves the outgoing workspace's camera
    /// and restores the incoming one's, so Manufacture's auto-fit never moves
    /// the camera you were designing with.
    var workspace: Workspace = .design {
        didSet {
            guard workspace != oldValue else { return }
            savedCameras[oldValue] = CameraState(azimuth: azimuth, elevation: elevation,
                                                 distance: distance, pan: panOffset)
            electronicsShown = workspace == .electronics
            cnc.shown = workspace == .manufacture
            if let c = savedCameras[workspace] {
                stopSpin()
                azimuth = c.azimuth; elevation = c.elevation
                distance = c.distance; pinchBaseline = c.distance; panOffset = c.pan
            }
            saveLayout()
        }
    }
    /// The workspace flags the renderer and panels read. Derived from
    /// `workspace`; kept as stored properties because the CNC controller owns
    /// `cnc.shown` and pauses its preview on it.
    private(set) var electronicsShown = false
    struct CameraState { var azimuth: Float; var elevation: Float; var distance: Float; var pan: SIMD3<Float> }
    @ObservationIgnored private var savedCameras: [Workspace: CameraState] = [:]

    // MARK: layout memory

    @ObservationIgnored private var restoringLayout = false
    /// Remember which panels are open and which workspace is active.
    func saveLayout() {
        guard !restoringLayout else { return }
        LayoutMemory(workspace: workspace.rawValue, showsTree: showsTree, showsInspector: showsInspector,
                     cncLeft: cnc.leftPanelShown, cncRight: cnc.rightPanelShown, cncBottom: cnc.bottomPanelShown,
                     measurementsShown: measurementsShown, windowed: isWindowed).save()
    }
    private func restoreLayout() {
        let m = LayoutMemory.load()
        restoringLayout = true
        defer { restoringLayout = false }
        showsTree = m.showsTree
        showsInspector = m.showsInspector
        cnc.leftPanelShown = m.cncLeft
        cnc.rightPanelShown = m.cncRight
        cnc.bottomPanelShown = m.cncBottom
        measurementsShown = m.measurementsShown
        if let ws = Workspace(rawValue: m.workspace).flatMap(Workspace.launchRestorable) { workspace = ws }
    }
    /// Whether the last session ran in a window (the presentation to restore).
    var remembersWindowed: Bool { LayoutMemory.load().windowed }
    /// The inspector's Measurements disclosure, remembered across launches.
    var measurementsShown = false { didSet { saveLayout() } }

    /// Save As re-points `source` at the same document: keep the sketch etc.
    @ObservationIgnored private var sourceRenameOnly = false
    var source: GeometrySource = .sandbox {
        didSet {
            geometryDirty = true
            if source.id != oldValue.id {
                jointPose = [:]; explode = 0
                // A different document is not the one any session was linked to.
                agentLinkID = nil; agentLinkRevision = nil; agentLinkOwned = false
                agentLinkBaseDir = nil; agentPoseSurfacedLink = nil
                agentBaseFirst = nil; agentBaseDelta = nil
                cameraUserMoved = false; agentFramedSize = nil; agentFramePending = false
                if !sourceRenameOnly { resetTransientDocumentState() }
            }
            watchDocumentFile()                   // hot reload on regenerate
            loadDocumentTree()                    // parse the DAG for a .vcad
            if !availableTabs.contains(toolTab) { toolTab = .create }
            selectedFeatureID = defaultSelection()
            resetCamera()        // frame the new part cleanly
        }
    }

    // Sandbox model: primitive + modifier, driven by the tool palette.
    var baseShape: BaseShape = .cube { didSet { if source.isSandbox { geometryDirty = true } } }
    var modifier: Modifier = .fillet { didSet { if source.isSandbox { geometryDirty = true } } }
    var modifierValue: Double = 3.0 {
        didSet {
            if source.isSandbox { parameterDirty = true }
            if Int(modifierValue.rounded()) != Int(oldValue.rounded()) { hapticDetent() }
        }
    }

    /// A haptic detent on the trackpad — only when the user has them on.
    func hapticDetent(_ pattern: HapticPattern = .alignment) {
        #if os(macOS)
        guard Prefs.haptics else { return }
        let p: NSHapticFeedbackManager.FeedbackPattern = pattern == .alignment ? .alignment : .levelChange
        NSHapticFeedbackManager.defaultPerformer.perform(p, performanceTime: .default)
        #endif
    }
    enum HapticPattern { case alignment, levelChange }

    var toolTab: ToolTab = .create

    // Selection binds tree -> inspector AND rolls history (selecting "base"
    // shows the modifier's input).
    var selectedFeatureID: String? = "modifier" {
        didSet {
            if source.isSandbox { geometryDirty = true }
            else if selectedFeatureID != oldValue { selectionDirty = true }
        }
    }

    // Live readouts.
    var triangleCount: Int = 0
    var partCount: Int = 1
    var solveMillis: Double = 0
    var sizeMM: SIMD3<Float> = .zero

    // Picking.
    /// Why the last evaluation produced nothing, when it produced nothing.
    var loadError: String?
    /// A new document opening (import + first solve): drives the progress
    /// card. Nil for re-solves of the open document.
    var loading: LoadProgress?
    /// What was open before `loading` began — Cancel returns to it.
    @ObservationIgnored var loadingPrevious: GeometrySource?
    /// Shown when the first solve lands (STEP faces skipped).
    @ObservationIgnored var pendingLoadWarning: LoadProgress.Phase?
    @ObservationIgnored private var stepJob: OpaquePointer?
    @ObservationIgnored private var stepPoll: Task<Void, Never>?
    var pickPoint: SIMD3<Float>?
    var pickInfo: String?
    var pickDirty = false
    var displayScale: Float = 0.02
    var displayCenter: SIMD3<Float> = .zero
    /// Native RealityKit picking: the live `centering` entity (weak) provides
    /// the scene for collision raycasts and world↔kernel conversion.
    @ObservationIgnored weak var centeringEntity: Entity?
    /// Per-entity collider build tokens — an async static-mesh collider only
    /// lands if it is still the newest request for that entity name.
    @ObservationIgnored var colliderTokens: [String: Int] = [:]

    var geometryDirty = true
    var parameterDirty = false
    let streaming = StreamingMesh()
    let chime = Chime()
    var lastDrag: CGSize = .zero
    var pinchBaseline: Float = 1.5
    var draggingHandle = false
    var handleBaseline: Double = 0

    // MARK: document feature tree
    // The parsed DAG behind a loaded .vcad — drives the hierarchical sidebar,
    // per-part visibility, and selection ↔ viewport highlight. Geometry still
    // comes from the kernel; this is the *operations* view of the same file.

    var documentGraph: DocumentGraph?
    var featureNodes: [FeatureNode] = []
    var expandedFeatureIDs: Set<String> = []
    /// Part indices hidden via the eye toggle (empty = all shown).
    var hiddenParts: Set<Int> = []
    /// Isolate: when set, only this part shows (supersedes `hiddenParts`).
    var isolatedPart: Int?
    /// Viewport needs to re-apply part visibility / re-highlight the selection.
    var visibilityDirty = false
    var selectionDirty = false
    /// Parts ⌘-clicked for a multi-part action (booleans). Ordered: first = base.
    var multiSelectedParts: [Int] = []
    /// Part under the cursor (documents) — drives a subtle hover highlight.
    var hoveredPartIndex: Int?
    /// Assembly instance under the cursor / selected, by render index.
    ///
    /// Assemblies draw one entity per INSTANCE, not per root part, so the
    /// part-index selection path could never touch them: every link of a robot
    /// was inert — no hover, no click, and an inspector that said "select a
    /// feature in the tree" while you were pointing right at one.
    var hoveredInstanceIndex: Int?
    var selectedInstanceIndex: Int?
    /// Instances currently lit. A viewport hover lights exactly one; hovering a
    /// tree row lights every instance drawn from that row's part def, because
    /// the row *is* the definition — one row, four elbows on a quadruped.
    var hoveredInstances: Set<Int> = []
    /// Instances an agent (over the app's MCP bridge) is pointing at — painted
    /// like a selection so the user sees what the agent means.
    var agentHighlights: Set<Int> = []
    /// Parts a pending agent suggestion would touch (soft shimmer preview).
    var agentShimmer: Set<Int> = []
    /// Labels an agent pinned to instances, drawn in the viewport overlay.
    var agentAnnotations: [AgentAnnotation] = []
    /// The tree row that is hot, whichever side the pointer is on. Hovering
    /// geometry highlights its row; hovering a row highlights its geometry.
    var hoveredFeatureID: String?
    var hoverDirty = false
    /// Zebra surface analysis: striped environment reflected off chromed parts,
    /// so stripe continuity reveals curvature/tangency defects.
    var zebraMode = false { didSet { if zebraMode != oldValue { zebraDirty = true } } }
    var zebraDirty = false
    /// Panels the shell can close and re-open from the View menu.
    var showsTree = true { didSet { saveLayout() } }
    var showsInspector = true { didSet { saveLayout() } }
    var anyPanelShown: Bool { showsTree || showsInspector }

    /// Show or hide every floating panel at once.
    func setPanels(shown: Bool) {
        showsTree = shown
        showsInspector = shown
    }
    /// Per-part kernel-space triangle meshes (+ AABB), index-aligned with the
    /// rendered parts — the basis for precise ray-triangle hover/pick. The AABB
    /// is a broadphase cull before the triangle test.
    struct PickMesh {
        var positions: [SIMD3<Float>]
        var indices: [UInt32]
        var lo: SIMD3<Float>
        var hi: SIMD3<Float>
    }
    @ObservationIgnored var docPartMeshes: [PickMesh] = []
    /// Feature-edge segments per part (index-aligned with docPartMeshes),
    /// refreshed on every evaluate/re-evaluate for the CAD edge overlay.
    @ObservationIgnored var docPartEdges: [[SIMD3<Float>]] = []
    /// Per-part instancing (index-aligned with docPartMeshes): non-nil when the
    /// part is a pattern root rendered as one shared mesh + N instance entities.
    /// The in-place re-eval path reads this to sync instance entities without a
    /// full rebuild; pick/edge data is pre-aggregated so everything downstream
    /// (hover, pick, gizmo, auto-fit) is instancing-agnostic.
    @ObservationIgnored var docPartInstancing: [PatternInstancing?] = []
    /// Crease threshold for the edge overlay: creases sharper than this many
    /// degrees draw; cylinder facets (~6°) stay invisible.
    static let edgeAngleDeg: Float = 25.0
    /// Largest scene dimension currently displayed (mm) — sizes the edge
    /// overlay ribbon width during in-place scrub swaps.
    var displayedSceneSize: Float { max(sizeMM.x, max(sizeMM.y, sizeMM.z)) }

    // MARK: kinematic joint playback (assemblies)
    // Assembly documents carry joints + (optionally) a timeline of time-major
    // joint keyframes. The evaluated scene handle stays resident so each
    // playback frame is one kernel FK solve (`vcad_scene_solve_fk`) — the
    // exact solver the web evaluator runs, so native motion matches it by
    // construction. Entities keep their part-def-local meshes; only their
    // transforms move.

    /// The resident evaluated assembly scene (owns instance meshes + the
    /// parsed doc the FK solver re-poses). Freed on rebuild / source change.
    nonisolated(unsafe) var residentAssemblyScene: OpaquePointer?

    // MARK: dynamics (physics simulation)
    //
    // Playback above is *kinematic*: the document says where each joint is and
    // FK says where that puts the links. The simulation below is *dynamic*:
    // gravity, contact, and actuator torques decide, and the document only
    // supplies the plant. Both end at the same place — `instanceTransforms` in
    // scene order — so the renderer draws either without knowing which it got.

    /// The physics simulation model. Inert until `enableSimulation()`.
    let sim = SimController()
    /// Assembly / joint / keyframe authoring UI state (AssemblyModel.swift).
    let assembly = AssemblyUIState()

    /// Whether the current document can be simulated: an assembly with bodies.
    var canSimulate: Bool { assemblyInstanceCount > 0 && documentJSON != nil }
    /// Simulation UI is a Labs feature; the capability stays for hooks/tests.
    var showsSimulation: Bool { Prefs.labs && canSimulate }

    /// Build a simulation for the current document and bind it to the resident
    /// scene, so simulated transforms arrive in the renderer's index order.
    func enableSimulation() {
        // No silent guards on this path. Every early return here is a press of
        // the Simulate button that visibly does nothing, which is the worst
        // possible failure mode for a button whose whole job is to start
        // something.
        guard canSimulate else {
            sim.reportUnavailable(
                "This document has no simulable assembly (instances: \(assemblyInstanceCount)).")
            return
        }
        guard let dict = documentJSON else {
            sim.reportUnavailable("The document JSON is not loaded yet.")
            return
        }
        let data: Data
        do {
            data = try JSONSerialization.data(withJSONObject: dict)
        } catch {
            sim.reportUnavailable("Could not re-serialize the document: \(error)")
            return
        }
        // Physics and kinematic playback both write `instanceTransforms`;
        // letting them run together means two writers racing for the pose.
        pausePlayback()
        sim.prepare(documentJSON: data,
                    scene: residentAssemblyScene,
                    authored: authoredInstanceTransforms,
                    baseDirectory: documentDirectory)
    }

    /// Directory of the currently open document, when it came from a file —
    /// the base for resolving its relative mesh references.
    var documentDirectory: String? {
        guard case .document(let path, _) = source else { return agentLinkBaseDir }
        return (path as NSString).deletingLastPathComponent
    }

    /// Monotonic counter over published poses, for view layers that need an
    /// explicit per-frame input rather than an observation closure.
    ///
    /// Reading this in a `body` is what makes the released `NSViewRepresentable`
    /// re-pose; the studio's `RealityView` observes `playbackDirty` directly and
    /// does not need it.
    var poseTick = 0

    /// Route simulated poses into the same channel kinematic playback uses.
    /// Called from `init`, once.
    private func bridgeSimulationToRenderer() {
        sim.onTransforms = { [weak self] transforms in
            guard let self else { return }
            self.instanceTransforms = transforms
            self.playbackDirty = true
            self.poseTick &+= 1
        }
    }
    /// FFI instance count of the resident scene (0 = not an assembly).
    var assemblyInstanceCount = 0
    /// Per-instance kernel-frame world transforms at the current playback
    /// time, FFI-index order. Applied to "inst<i>" entities when
    /// `playbackDirty` is set.
    @ObservationIgnored var instanceTransforms: [float4x4] = []
    /// The instances' authored (document) transforms, scene order. Seeds the
    /// simulation's output for bodies physics doesn't own.
    @ObservationIgnored var authoredInstanceTransforms: [float4x4] = []
    var playbackDirty = false

    // MARK: pose studio (interactive joints + explode + hot reload)
    // Implemented in PoseStudio.swift; the stored state lives here because an
    // extension can't hold it.

    /// Interactive joint values, keyed by joint NAME (falling back to id) so a
    /// pose survives the file being regenerated with renumbered joint ids.
    var jointPose: [String: Double] = [:]
    /// Explode amount, 0 (assembled) … 1 (fully exploded).
    var explode: Double = 0
    /// Instance ids + part-local AABB centres, scene order — what explode
    /// reasons about. Filled when an assembly scene is adopted.
    @ObservationIgnored var assemblyInstanceIDs: [String] = []
    @ObservationIgnored var assemblyLocalCenters: [SIMD3<Float>] = []
    /// Watches the open document's file; a regenerate reloads it in place.
    @ObservationIgnored var fileWatch: DispatchSourceFileSystemObject?
    @ObservationIgnored var fileWatchPath: String?
    @ObservationIgnored var reloadPending: DispatchWorkItem?
    /// The loon loop (LoonLoop.swift): the open program's compile error, the
    /// directory watch, and a just-compiled document awaiting adoption.
    var loonDiagnostic: LoonDiagnostic?
    @ObservationIgnored var loonWatcher: LoonWatcher?
    @ObservationIgnored var loonWatchPath: String?
    @ObservationIgnored var precompiledLoon: (path: String, data: Data)?
    /// When the open document last reloaded from disk (shown in the pose panel).
    var lastReload: Date?
    /// Photoreal snapshot state.
    var photorealBusy = false
    var photorealStatus: String?

    // MARK: path-traced viewport (PathTracer.swift)
    /// The live irys path-traced view is on.
    var pathTraceEnabled = false
    /// Why it isn't running, or what it's doing (nil when converging normally).
    var pathTraceStatus: String?
    /// Samples per pixel in the image on screen.
    var pathTraceSamples: UInt32 = 0
    /// A still was asked for and is waiting for enough samples.
    var pathTraceStillPending = false
    @ObservationIgnored let pathTracer = PathTracer()
    /// Bumped whenever a new scene's meshes are adopted, so the path tracer
    /// knows to rebuild its scene (pose changes go through `poseTick`).
    @ObservationIgnored var sceneGeneration = 0

    var isPlaying = false
    /// Current playback time in seconds (drives the scrubber).
    var playbackTime: Double = 0
    nonisolated(unsafe) private var playbackTimer: Timer?

    var timeline: DocTimeline? { documentGraph?.timeline }
    /// Transport UI shows only when there is something to play.
    var hasPlayback: Bool { assemblyInstanceCount > 0 && timeline != nil }

    func togglePlayback() { isPlaying ? pausePlayback() : startPlayback() }

    func startPlayback() {
        guard hasPlayback else { return }
        isPlaying = true
        playbackTimer?.invalidate()
        let dt = 1.0 / 60.0
        playbackTimer = Timer.scheduledTimer(withTimeInterval: dt, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self, let tl = self.timeline else { return }
                var t = self.playbackTime + dt
                if t > tl.durationS { t = 0 }              // loop
                self.playbackTime = t
                self.applyPlaybackPose()
            }
        }
    }

    func pausePlayback() {
        isPlaying = false
        playbackTimer?.invalidate()
        playbackTimer = nil
    }

    /// Scrub: jump to `t` (clamped to the timeline) and re-pose.
    func setPlaybackTime(_ t: Double) {
        let dur = timeline?.durationS ?? 0
        playbackTime = min(max(t, 0), dur)
        applyPlaybackPose()
    }

    /// Sample the joint tracks at the current time and run the kernel FK
    /// solver for fresh instance transforms.
    private func applyPlaybackPose() {
        guard let scene = residentAssemblyScene, let tl = timeline,
              assemblyInstanceCount > 0 else { return }
        let values = tl.jointValues(at: playbackTime)
        guard !values.isEmpty,
              let data = try? JSONSerialization.data(withJSONObject: values) else { return }
        var out = [Double](repeating: 0, count: assemblyInstanceCount * 16)
        let n = data.withUnsafeBytes { raw in
            vcad_scene_solve_fk(scene, raw.bindMemory(to: UInt8.self).baseAddress,
                                data.count, &out, out.count)
        }
        guard n == assemblyInstanceCount else { return }
        instanceTransforms = (0..<n).map { Self.mat4(out, at: $0 * 16) }
        playbackDirty = true
        poseTick &+= 1
    }

    /// Column-major 16-double slice → float4x4 (the FFI transform layout).
    static func mat4(_ d: [Double], at o: Int = 0) -> float4x4 {
        func col(_ c: Int) -> SIMD4<Float> {
            SIMD4<Float>(Float(d[o + c * 4]), Float(d[o + c * 4 + 1]),
                         Float(d[o + c * 4 + 2]), Float(d[o + c * 4 + 3]))
        }
        return float4x4(columns: (col(0), col(1), col(2), col(3)))
    }

    /// Drop the resident assembly scene + playback state (source change or a
    /// rebuild about to adopt a fresh scene).
    private func clearAssembly() {
        pausePlayback()
        sim.teardown()
        authoredInstanceTransforms = []
        if let s = residentAssemblyScene {
            vcad_scene_free(s)
            residentAssemblyScene = nil
        }
        assemblyInstanceCount = 0
        instanceTransforms = []
        playbackTime = 0
        playbackDirty = false
    }

    // MARK: ray-traced still mode (pixel-perfect: rays vs analytic BRep)

    /// Whether the ray-traced refinement pass is enabled (toolbar toggle).
    var raytraceEnabled = false
    /// The latest completed ray-traced frame, shown as an overlay once the
    /// camera settles. Cleared by any camera/geometry/parameter change.
    /// (macOS-only: the overlay composites over a fixed orbit camera, which a
    /// visionOS volume doesn't have.)
    #if os(macOS)
    var raytraceImage: NSImage?
    #endif
    /// Monotonic token so a stale in-flight render can't overwrite a newer one.
    @ObservationIgnored var raytraceToken = 0

    /// Convert the display-space orbit camera into kernel coordinates
    /// (Z-up, mm). Display world = zUpRot(kernel − displayCenter) · displayScale,
    /// so kernel = zUpRotInv(world / displayScale) + displayCenter, where
    /// zUpRotInv maps (x, y, z)world → (x, −z, y)kernel.
    func kernelCamera() -> (cam: SIMD3<Double>, target: SIMD3<Double>)? {
        guard displayScale > 1e-9 else { return nil }
        func toKernel(_ w: SIMD3<Float>) -> SIMD3<Double> {
            let u = w / displayScale
            let k = SIMD3<Float>(u.x, -u.z, u.y) + displayCenter
            return SIMD3<Double>(Double(k.x), Double(k.y), Double(k.z))
        }
        return (toKernel(cameraPosition), toKernel(panOffset))
    }

    #if os(macOS)
    /// Async ray-traced still: gathers the doc bytes, camera, and part colors
    /// on the main actor, then runs the blocking tracer on a background
    /// thread. Returns nil when there is no evaluable document.
    func raytraceStillAsync(width: Int, height: Int) async -> NSImage? {
        guard usesDocumentTree, let (cam, target) = kernelCamera() else { return nil }
        let data: Data?
        if let json = documentJSON { data = DocEdit.serialize(json) }
        else if case let .document(path, _) = source { data = Self.documentBytes(path: path) }
        else if case let .generated(loon, _) = source { data = Data(loon.utf8) }
        else { data = nil }
        guard let data else { return nil }
        let isLoon = { if case .generated = source { return true } else { return false } }()
        var colors: [Float] = []
        for i in 0..<partCount {
            let c = resolvedMaterial(forPart: i).color.usingColorSpace(.sRGB) ?? .gray
            colors.append(contentsOf: [Float(c.redComponent), Float(c.greenComponent), Float(c.blueComponent)])
        }
        return await Task.detached(priority: .userInitiated) {
            Self.renderRaytraceFrame(data: data, isLoon: isLoon, cam: cam, target: target,
                                     colors: colors, width: width, height: height)
        }.value
    }

    /// Blocking direct-BRep render — everything it touches is passed in, so
    /// it is safe off the main actor.
    nonisolated private static func renderRaytraceFrame(
        data: Data, isLoon: Bool,
        cam: SIMD3<Double>, target: SIMD3<Double>,
        colors: [Float], width: Int, height: Int
    ) -> NSImage? {
        let scene: OpaquePointer? = data.withUnsafeBytes { raw in
            let base = raw.bindMemory(to: UInt8.self).baseAddress
            return isLoon ? vcad_scene_from_loon(base, data.count) : vcad_scene_from_json(base, data.count)
        }
        guard let scene else { return nil }
        defer { vcad_scene_free(scene) }

        var camArr = [cam.x, cam.y, cam.z]
        var targetArr = [target.x, target.y, target.z]
        // RealityKit's PerspectiveCameraComponent defaults to a 60° vertical
        // field of view — match it so the still lands exactly on the raster.
        // GPU first (wgpu → Metal: full-frame with SSAO, analytic edges, and
        // its own sky/ground environment); CPU studio tracer as the
        // composited fallback when no adapter is available.
        var img: OpaquePointer? = vcad_scene_raytrace_gpu(
            scene, &camArr, &targetArr, 60.0,
            UInt32(width), UInt32(height), colors, colors.count)
        if img == nil {
            img = vcad_scene_raytrace(
                scene, &camArr, &targetArr, 60.0,
                UInt32(width), UInt32(height), colors, colors.count)
        }
        guard let img else { return nil }
        defer { vcad_image_free(img) }
        let view = vcad_image_view(img)
        guard let px = view.pixels, view.pixels_len == Int(view.width * view.height * 4) else { return nil }

        let w = Int(view.width), h = Int(view.height)
        guard let rep = NSBitmapImageRep(
            bitmapDataPlanes: nil, pixelsWide: w, pixelsHigh: h, bitsPerSample: 8,
            samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
            colorSpaceName: .deviceRGB, bytesPerRow: w * 4, bitsPerPixel: 32),
            let dst = rep.bitmapData else { return nil }
        dst.update(from: px, count: view.pixels_len)
        let image = NSImage(size: NSSize(width: w, height: h))
        image.addRepresentation(rep)
        return image
    }
    #endif

    var usesDocumentTree: Bool { documentGraph != nil && (!featureNodes.isEmpty || engine != nil) }

    /// Parse the DAG when a `.vcad` document loads; clear it for every other
    /// source. Resets visibility + expansion so each document opens clean.
    private func loadDocumentTree() {
        hiddenParts.removeAll()
        isolatedPart = nil
        multiSelectedParts = []
        hoveredPartIndex = nil
        expandedFeatureIDs.removeAll()
        docPartMeshes = []
        documentJSON = nil
        engine = nil; engineState = nil; engineError = nil; savedHash = nil
        evalSession = EvalSession()          // a new document starts a fresh session
        undoStack.removeAll(); redoStack.removeAll()
        renamingFeatureID = nil
        documentDirty = false
        savedSnapshot = nil
        if case let .document(path, _) = source,
           let data = loonAwareDocumentBytes(path: path),
           let dict = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any],
           DocumentGraph.parse(dict) != nil {
            // The engine owns the document from here on. A file it can't
            // round-trip faithfully opens view-only rather than being
            // silently rewritten on the first edit.
            let opened = Self.openInEngine(dict)
            let live = opened?.state.document ?? dict
            guard let g = DocumentGraph.parse(live) else { return }
            engine = opened?.engine
            engineState = opened?.state
            savedHash = opened?.engine.stateHash
            if opened == nil { engineError = "This document opens view-only: the document engine can't edit it yet." }
            documentJSON = live
            documentGraph = g
            featureNodes = g.featureRoots()
            savedSnapshot = DocEdit.serialize(live)
            // Auto-expand a single-root doc so its history reads at a glance.
            if featureNodes.count == 1, let only = featureNodes.first {
                expandedFeatureIDs.insert(only.id)
            }
        } else {
            documentGraph = nil
            featureNodes = []
        }
    }

    /// The document's JSON; a loon program reports where it failed.
    private func loonAwareDocumentBytes(path: String) -> Data? {
        guard path.lowercased().hasSuffix(".loon") else {
            loonDiagnostic = nil
            return Self.documentBytes(path: path)
        }
        if let pre = precompiledLoon, pre.path == path {
            precompiledLoon = nil
            return pre.data
        }
        switch LoonCompiler.compile(path: path) {
        case .success(let data):
            loonDiagnostic = nil
            return data
        case .failure(let d):
            loonDiagnostic = d
            return nil
        }
    }

    private func defaultSelection() -> String? {
        switch source {
        case .sandbox: return "modifier"
        case .document: return featureNodes.first?.id
        default: return "part0"
        }
    }

    func toggleExpanded(_ id: String) {
        if expandedFeatureIDs.contains(id) { expandedFeatureIDs.remove(id) }
        else { expandedFeatureIDs.insert(id) }
    }

    // MARK: per-part visibility

    func isPartVisible(_ i: Int) -> Bool {
        if let iso = isolatedPart { return i == iso }
        return !hiddenParts.contains(i)
    }
    var hasHiddenParts: Bool { isolatedPart != nil || !hiddenParts.isEmpty }

    func toggleVisibility(part i: Int) {
        isolatedPart = nil                       // an explicit toggle exits isolate
        if hiddenParts.contains(i) { hiddenParts.remove(i) } else { hiddenParts.insert(i) }
        visibilityDirty = true
    }
    func isolate(part i: Int) {
        isolatedPart = (isolatedPart == i) ? nil : i
        hiddenParts.removeAll()
        visibilityDirty = true
    }
    func showAllParts() {
        hiddenParts.removeAll()
        isolatedPart = nil
        visibilityDirty = true
    }

    // MARK: selection ↔ part mapping

    /// The viewport part index implied by the current selection — the owning
    /// root of the selected feature row, or nil (selecting deep operands still
    /// highlights the part they build).
    var selectedPartIndex: Int? {
        guard let sel = selectedFeatureID else { return nil }
        for node in featureNodes {
            if let pi = node.partIndex, Self.subtree(node, contains: sel) { return pi }
        }
        return nil
    }
    private static func subtree(_ node: FeatureNode, contains id: String) -> Bool {
        if node.id == id { return true }
        return node.children.contains { subtree($0, contains: id) }
    }

    /// Single-select a row (clears any multi-selection).
    func selectFeature(_ id: String) {
        if !multiSelectedParts.isEmpty { multiSelectedParts = [] }
        // A row click supersedes a click on geometry: leaving the old instance
        // set would keep an unrelated body lit.
        selectedInstanceIndex = nil
        selectedFeatureID = id
    }
    /// Whether anything is selected (drives Escape/empty-click deselect).
    var hasSelection: Bool {
        selectedFeatureID != nil || !multiSelectedParts.isEmpty || selectedInstanceIndex != nil
    }
    /// Clear all selection — empty-space click or Escape (documents only).
    func deselectAll() {
        picking.clear()
        multiSelectedParts = []
        selectedInstanceIndex = nil
        selectedFeatureID = nil
        selectionDirty = true
    }
    // MARK: assembly instances

    /// The document's instance list, as authored (index-aligned with the
    /// rendered instances — the evaluator preserves order).
    private var instanceDicts: [[String: Any]] {
        (documentJSON?["instances"] as? [[String: Any]]) ?? []
    }

    /// An instance's display name, falling back to its id.
    func instanceName(_ i: Int) -> String? {
        guard i < instanceDicts.count else { return nil }
        let d = instanceDicts[i]
        return (d["name"] as? String) ?? (d["id"] as? String)
    }

    /// The part definition an instance is an instance OF.
    func instancePartDefName(_ i: Int) -> String? {
        guard i < instanceDicts.count,
              let key = instanceDicts[i]["partDefId"] as? String else { return nil }
        let defs = documentJSON?["partDefs"] as? [String: Any]
        let def = defs?[key] as? [String: Any]
        return (def?["name"] as? String) ?? key
    }

    /// The feature-tree row for the geometry an instance draws: its part def's
    /// root node. Selecting an instance therefore lands you on the DAG that
    /// produced it — edit the link's cube there and every instance follows.
    func instanceFeatureNode(_ i: Int) -> FeatureNode? {
        guard i < instanceDicts.count,
              let key = instanceDicts[i]["partDefId"] as? String,
              let defs = documentJSON?["partDefs"] as? [String: Any],
              let def = defs[key] as? [String: Any],
              let root = (def["root"] as? NSNumber)?.intValue else { return nil }
        return Self.findNode(featureNodes, nodeId: root)
    }

    private static func findNode(_ nodes: [FeatureNode], nodeId: Int) -> FeatureNode? {
        for n in nodes {
            if n.nodeId == nodeId { return n }
            if let f = findNode(n.children, nodeId: nodeId) { return f }
        }
        return nil
    }

    /// Every instance drawn from a given DAG node (a part def's root).
    func instanceIndices(forNodeId nodeId: Int) -> [Int] {
        guard let defs = documentJSON?["partDefs"] as? [String: Any] else { return [] }
        let keys = defs.compactMap { key, value -> String? in
            guard let def = value as? [String: Any],
                  (def["root"] as? NSNumber)?.intValue == nodeId else { return nil }
            return key
        }
        guard !keys.isEmpty else { return [] }
        return instanceDicts.enumerated().compactMap { i, d in
            (d["partDefId"] as? String).map { keys.contains($0) ? i : nil } ?? nil
        }
    }

    /// The instances the viewport should show as selected.
    ///
    /// Clicking geometry selects one instance. Clicking a TREE ROW selects a
    /// part definition, which may have been instanced many times — so the
    /// selection is a set, derived from whichever side did the selecting.
    /// Without this an assembly row click changed `selectedFeatureID` and
    /// nothing else: the tree highlighted, the viewport sat there.
    var selectedInstances: Set<Int> {
        if let i = selectedInstanceIndex { return [i] }
        guard assemblyInstanceCount > 0, let node = selectedFeatureNode else { return [] }
        return Set(instanceIndices(forNodeId: node.nodeId))
    }

    /// Hover driven from the feature tree: light the row and everything it drew.
    func hoverFeature(_ node: FeatureNode?) {
        hoveredFeatureID = node?.id
        hoveredPartIndex = node?.partIndex
        hoveredInstances = node.map { Set(instanceIndices(forNodeId: $0.nodeId)) } ?? []
    }

    /// Hover driven from the viewport: light the body and its row.
    func hoverBody(part: Int?, instance: Int?) {
        hoveredPartIndex = part
        hoveredInstanceIndex = instance
        hoveredInstances = instance.map { [$0] } ?? []
        if let part {
            hoveredFeatureID = Self.findNode(featureNodes, partIndex: part)?.id
        } else if let instance {
            hoveredFeatureID = instanceFeatureNode(instance)?.id
        } else {
            hoveredFeatureID = nil
        }
    }

    private static func findNode(_ nodes: [FeatureNode], partIndex: Int) -> FeatureNode? {
        for n in nodes {
            if n.partIndex == partIndex { return n }
            if let f = findNode(n.children, partIndex: partIndex) { return f }
        }
        return nil
    }

    /// Click an instance: it becomes the selection, and the tree follows to the
    /// part def it draws.
    func selectInstance(_ i: Int) {
        multiSelectedParts = []
        selectedInstanceIndex = i
        selectedFeatureID = instanceFeatureNode(i)?.id
        selectionDirty = true
    }

    /// ⌘-click: toggle a part in the multi-selection; the last click stays primary.
    func toggleMultiSelect(part pi: Int, featureID id: String) {
        if let idx = multiSelectedParts.firstIndex(of: pi) { multiSelectedParts.remove(at: idx) }
        else { multiSelectedParts.append(pi) }
        selectedFeatureID = id
        selectionDirty = true
    }
    /// The parts to highlight: the multi-selection if any, else the primary part.
    var highlightedParts: Set<Int> {
        multiSelectedParts.isEmpty ? Set([selectedPartIndex].compactMap { $0 }) : Set(multiSelectedParts)
    }

    var selectedFeatureNode: FeatureNode? { Self.find(featureNodes, id: selectedFeatureID) }
    private static func find(_ nodes: [FeatureNode], id: String?) -> FeatureNode? {
        guard let id else { return nil }
        for n in nodes {
            if n.id == id { return n }
            if let f = find(n.children, id: id) { return f }
        }
        return nil
    }

    /// Material key assigned to a rendered part (root order), if any.
    func materialName(forPart i: Int) -> String? {
        guard let g = documentGraph, i < g.visibleRoots.count else { return nil }
        return g.visibleRoots[i].material
    }

    /// Resolve a part's render material: the doc's own definition → a preset by
    /// key → a neutral fallback (so unknown-material parts look as before).
    func resolvedMaterial(forPart i: Int) -> ResolvedMaterial {
        let key = materialName(forPart: i) ?? ""
        if let d = documentGraph?.materials[key] {
            return ResolvedMaterial(
                color: NSColor(srgbRed: d.color.0, green: d.color.1, blue: d.color.2, alpha: 1),
                metallic: d.metallic, roughness: d.roughness, transmission: d.transmission)
        }
        if let p = MaterialPreset.byKey(key) {
            return ResolvedMaterial(color: p.nsColor, metallic: p.metallic,
                                    roughness: p.roughness, transmission: p.transmission)
        }
        return ResolvedMaterial(color: documentBaseColor(i), metallic: 0.55, roughness: 0.34, transmission: 0)
    }

    /// Assign a material to a part — recolors live (no geometry rebuild).
    func setPartMaterial(_ partIndex: Int, _ key: String) {
        guard let id = partId(forPart: partIndex) else { return }
        // The root material lives on the scene entry. A preset the document
        // doesn't define yet gets its definition written into `materials` in
        // the same step, so it renders and survives save/reload.
        var commands: [DocCommand] = []
        if engineState?.definesMaterial(key) == false, let p = MaterialPreset.byKey(key) {
            commands.append(.defineMaterial(key: key, definition: MaterialDef(
                name: key, color: [p.color.0, p.color.1, p.color.2],
                metallic: Double(p.metallic), roughness: Double(p.roughness),
                density: 1000, friction: 0.5, transmission: Double(p.transmission))))
        }
        commands.append(.setMaterial(id: id, material: key))
        send(commands.count == 1 ? commands[0] : .batch(commands: commands), name: "Change Material", reeval: .none)
        selectionDirty = true
    }

    func documentBaseColor(_ i: Int) -> NSColor { Self.partColors[i % Self.partColors.count] }
    static let brandPink = NSColor(srgbRed: 0.976, green: 0.149, blue: 0.447, alpha: 1.0)
    /// Viewport selection accent — brand orange (action color). Selection must
    /// never repaint a part's material; it only adds a subtle emissive lift.
    static let brandOrange = NSColor(srgbRed: 1.0, green: 0.45, blue: 0.10, alpha: 1.0)

    // Part picking is native RealityKit now: part entities carry generated
    // static-mesh colliders and scene raycasts do the hit testing (Shell.swift).

    // MARK: document editing
    // The Rust document engine (`DocumentEngine` → `vcad_app::DocumentApi`)
    // owns the document: every mutation is a `DocCommand`, and `documentJSON`
    // is the engine's materialized IR, re-read after each command. Swift keeps
    // view state only. Commands address parts by stable part id
    // (`"replica:seq"`, the document's `partIds`), never node ids — the engine
    // renumbers IR nodes on every materialize.

    /// The engine for the open document; nil for sources with no editable
    /// document and for files it can't round-trip (opened view-only).
    @ObservationIgnored private(set) var engine: DocumentEngine?
    /// The engine's last result (document + part table).
    @ObservationIgnored private(set) var engineState: DocumentEngine.Result?
    /// The last refusal, or why the document is view-only.
    var engineError: String?
    /// The engine's materialized document — read-only from Swift's side
    /// except for the detached electronics path.
    @ObservationIgnored var documentJSON: [String: Any]? {
        // Any document swap except an engine delta (`adopt`) invalidates the
        // per-part tessellation cache.
        didSet {
            if !adoptingDelta { partMeshCache.removeAll() }
            documentRevision &+= 1
            noteEditForAutosave()
        }
    }
    /// Bumped on every document swap or edit — the observable signal for
    /// state derived from `documentJSON` (live receipts re-verify on it).
    var documentRevision = 0
    /// Tessellated parts by stable part id, reused across edits: an engine
    /// delta drops exactly the parts it added, updated or removed, so an edit
    /// re-tessellates only those.
    @ObservationIgnored private var partMeshCache: [String: TessellatedPart] = [:]
    @ObservationIgnored private var adoptingDelta = false
    /// Part ids whose tessellation is cached (for tests and diagnostics).
    var cachedPartIds: Set<String> { Set(partMeshCache.keys) }
    var undoStack: [UndoEntry] = []
    var redoStack: [UndoEntry] = []
    var documentDirty = false
    /// Last save failure, shown to the user (nil when the last save worked).
    var saveError: String?
    /// The version being previewed in the viewport (VersionBrowser), if any.
    var previewingVersion: DocumentVersion?
    @ObservationIgnored var autosaver: Autosaver?
    @ObservationIgnored var autosavePending = false
    /// Engine state hash as last saved (or loaded): undoing back to it clears
    /// the edited state instead of leaving a phantom dot in the title bar.
    @ObservationIgnored private var savedHash: String?
    /// Same, for detached (non-engine) documents.
    @ObservationIgnored private var savedSnapshot: Data?
    /// The source the intent bar replaced, so a generation can be undone as a
    /// whole even though the generated program has no document JSON.
    @ObservationIgnored private var generationUndo: (source: GeometrySource, json: [String: Any]?,
                                                     engine: DocumentEngine?, state: DocumentEngine.Result?,
                                                     savedHash: String?,
                                                     undo: [UndoEntry], redo: [UndoEntry], dirty: Bool)?
    /// What ⌘Z would undo, for the Edit menu.
    var undoActionName: String? {
        if let sketch = sketchHistory { return sketch.canUndo ? "Sketch Edit" : nil }
        if let last = undoStack.last { return last.name }
        return generationUndo != nil ? "Generate" : nil
    }
    var redoActionName: String? {
        if let sketch = sketchHistory { return sketch.canRedo ? "Sketch Edit" : nil }
        return redoStack.last?.name
    }
    /// While sketching, undo/redo are scoped to the sketch (Fusion/Onshape
    /// style): the open sketch's own history, never the document's. Document
    /// history comes back once the sketch is finished or cancelled, so an
    /// undo pressed mid-sketch can never remove a feature under it.
    var sketchHistory: SketchSession? { sketching ? sketch : nil }
    /// One scrub gesture = one undo entry; skip the materialize-pop on edits.
    @ObservationIgnored var suppressMaterializePop = false
    /// In-place re-eval of a parameter edit (mesh swap, no full rebuild / pop).
    var docParamDirty = false
    /// The feature row currently being renamed inline, if any.
    var renamingFeatureID: String?
    /// Whether the open document accepts edits (it has an engine).
    var canEditDocument: Bool { engine != nil }

    /// Open `dict` in a fresh engine, or nil when the engine can't carry it
    /// faithfully: a refusal, materialize warnings, a different scene shape,
    /// or electronics (schematic/board JSON the vocabulary doesn't cover).
    static func openInEngine(_ dict: [String: Any]) -> (engine: DocumentEngine, state: DocumentEngine.Result)? {
        func count(_ d: [String: Any], _ k: String) -> Int { (d[k] as? [Any])?.count ?? 0 }
        let nodes = dict["nodes"] as? [String: Any] ?? [:]
        let hasBoard = nodes.values.contains {
            (($0 as? [String: Any])?["op"] as? [String: Any])?["type"] as? String == "PcbBoard"
        }
        guard dict["schematic"] == nil, dict["pcb"] == nil, !hasBoard else { return nil }
        let engine = DocumentEngine()
        // An assembly written by the web stores no roots (its parts are the
        // part defs); the engine materializes one root per def, which is the
        // same document, not a lossy one.
        let derivedRoots = count(dict, "roots") == 0 && !(dict["partDefs"] as? [String: Any] ?? [:]).isEmpty
        guard let state = try? engine.loadIR(dict), state.warnings.isEmpty,
              derivedRoots || count(state.document, "roots") == count(dict, "roots"),
              count(state.document, "instances") == count(dict, "instances"),
              count(state.document, "joints") == count(dict, "joints")
        else { return nil }
        return (engine, state)
    }

    /// Stable part id of the `partIndex`-th visible root.
    func partId(forPart partIndex: Int) -> String? {
        guard let g = documentGraph, partIndex >= 0, partIndex < g.visibleRoots.count else { return nil }
        return engineState?.partId(forRootNode: g.visibleRoots[partIndex].nodeId)
    }

    enum Reeval { case inPlace, rebuild, none }
    /// How an edit lands on the undo stack.
    enum UndoMode {
        /// The edit is exactly one engine command → one engine history group.
        case step
        /// Snapshot the engine first (compound edits, first event of a gesture).
        case snapshot
        /// Continue the current gesture: no new entry.
        case none
    }

    func pushUndo(_ step: UndoEntry.Step, name: String) {
        undoStack.append(UndoEntry(step: step, name: name))
        if undoStack.count > 64 { undoStack.removeFirst() }
        redoStack.removeAll()
    }

    /// Render an engine result: it is the document now.
    private func adopt(_ r: DocumentEngine.Result, reeval: Reeval) {
        engineState = r
        if let changes = r.changes {
            for id in changes.rebuild.union(changes.removed) { partMeshCache.removeValue(forKey: id) }
            adoptingDelta = true
        }
        documentJSON = r.document
        adoptingDelta = false
        documentDirty = engine.map { $0.stateHash != savedHash } ?? true
        // The engine's document is always valid, including with nothing in it.
        if let g = DocumentGraph.parse(r.document, allowEmpty: true) {
            documentGraph = g
            featureNodes = g.featureRoots()
        }
        markReeval(reeval)
    }

    func markReeval(_ reeval: Reeval) {
        switch reeval {
        case .inPlace:
            docParamDirty = true
        case .rebuild:
            suppressMaterializePop = true
            geometryDirty = true; selectionDirty = true; visibilityDirty = true
        case .none:
            break
        }
    }

    /// Run engine commands as one edit. With `.step` the body must issue
    /// exactly one command (use a `batch` for several). A refusal leaves the
    /// document and the undo stack untouched and lands in `engineError`.
    @discardableResult
    func engineEdit(_ name: String, undo mode: UndoMode, reeval: Reeval,
                            _ body: (DocumentEngine) throws -> DocumentEngine.Result) -> DocumentEngine.Result? {
        guard let engine else { return nil }
        let before = mode == .snapshot ? engine.save() : nil
        do {
            let r = try body(engine)
            switch mode {
            case .step: pushUndo(.engine, name: name)
            case .snapshot: pushUndo(.snapshot(before ?? Data()), name: name)
            case .none: break
            }
            engineError = nil
            adopt(r, reeval: reeval)
            return r
        } catch {
            if let before { _ = try? engine.restore(before) }
            engineError = String(describing: error)
            return nil
        }
    }

    /// One `DocCommand`, one undo step.
    @discardableResult
    private func send(_ command: DocCommand, name: String, reeval: Reeval) -> DocumentEngine.Result? {
        engineEdit(name, undo: .step, reeval: reeval) { try $0.apply(command) }
    }

    /// An edit the command vocabulary has no verb for yet (sketch extrude,
    /// rotate-about-center, document parameters, raw node fields): edit the
    /// materialized IR and reload it into the engine. Always snapshot-undone,
    /// since a reload starts the engine's history over.
    /// Assembly / keyframe authoring (AssemblyModel.swift): an IR edit, one
    /// undo step. `rebuild` re-evaluates the scene (instances, joints, ground);
    /// timeline-only edits leave the resident scene and playhead alone.
    @discardableResult
    func assemblyEdit(_ name: String, rebuild: Bool, _ mutate: (inout [String: Any]) -> Void) -> Bool {
        irEdit(name, reeval: rebuild ? .rebuild : .none, mutate) != nil
    }

    @discardableResult
    func irEdit(_ name: String, gestureStart: Bool = true, reeval: Reeval,
                        base: [String: Any]? = nil,
                        _ mutate: (inout [String: Any]) -> Void) -> DocumentEngine.Result? {
        guard var json = base ?? documentJSON else { return nil }
        mutate(&json)
        return engineEdit(name, undo: gestureStart ? .snapshot : .none, reeval: reeval) { try $0.loadIR(json) }
    }

    /// Engine command for a node-field edit, when the field maps onto the
    /// owning feature (transform chain, primitive dimensions, modifier
    /// size). Nil → the field has no command yet; fall back to an IR edit.
    private func nodeFieldCommand(nodeId: Int, key: String, axis: String?, value: Double) -> DocCommand? {
        guard let (part, role) = engineState?.owner(ofNode: nodeId) else { return nil }
        let id = part.id, kind = part.kind
        func vec(_ field: String) -> [Double] {
            let v = (DocEdit.op(documentJSON ?? [:], nodeId: nodeId)?[field] as? [String: Any]) ?? [:]
            return ["x", "y", "z"].map { (v[$0] as? NSNumber)?.doubleValue ?? 0 }
        }
        func with(_ field: String) -> [Double]? {
            guard let axis, let i = ["x", "y", "z"].firstIndex(of: axis) else { return nil }
            var v = vec(field); v[i] = value
            return v
        }
        func param(_ k: String) -> DocCommand {
            .setParam(id: id, key: k, value: .f64(value))
        }
        switch (role, key) {
        case ("translate", "offset"): return with("offset").map { .setTranslation(id: id, offset: $0) }
        case ("rotate", "angles"): return with("angles").map { .setRotation(id: id, angles: $0) }
        case ("scale", "factor"): return with("factor").map { .setScale(id: id, factor: $0) }
        case ("primitive", "size") where kind == "cube" || kind == "wedge":
            guard let axis, ["x", "y", "z"].contains(axis) else { return nil }
            return param("size_\(axis)")
        case ("primitive", _) where axis == nil:
            let keys: [String: Set<String>] = [
                "cylinder": ["radius", "height", "segments"],
                "sphere": ["radius", "segments"],
                "cone": ["radius_bottom", "radius_top", "height", "segments"],
                "torus": ["major_radius", "minor_radius", "segments"],
                "prism": ["sides", "radius", "height"],
                "sweep-rotate": ["angle_lo_deg", "angle_hi_deg", "step_deg", "clearance"],
                "fillet": ["radius"], "chamfer": ["distance"],
            ]
            return keys[kind]?.contains(key) == true ? param(key) : nil
        default:
            return nil
        }
    }

    /// Scrub one node field: the owning feature's command when there is one,
    /// otherwise an IR edit. `snapshot` marks the first event of a gesture.
    private func editNodeField(nodeId: Int, key: String, axis: String?, value: Double,
                               snapshot: Bool, name: String,
                               fallback: (inout [String: Any]) -> Void) {
        if let cmd = nodeFieldCommand(nodeId: nodeId, key: key, axis: axis, value: value) {
            engineEdit(name, undo: snapshot ? .snapshot : .none, reeval: .inPlace) { try $0.apply(cmd) }
        } else {
            irEdit(name, gestureStart: snapshot, reeval: .inPlace, fallback)
        }
    }

    /// Edits outside the engine: the electronics workspace's schematic/board
    /// JSON has no `DocCommand` vocabulary yet, so the document detaches from
    /// the engine (engine history can't follow it) and edits the JSON directly.
    private func detachedEdit(name: String, _ mutate: (inout [String: Any]) -> Void) {
        if engine != nil {
            engine = nil; engineState = nil
            undoStack.removeAll { if case .json = $0.step { return false }; return true }
            redoStack.removeAll()
        }
        guard var json = documentJSON else { return }
        if let data = DocEdit.serialize(json) { pushUndo(.json(data), name: name) }
        mutate(&json)
        documentJSON = json
        documentDirty = true
        if let g = DocumentGraph.parse(json) { documentGraph = g; featureNodes = g.featureRoots() }
        markReeval(.rebuild)
    }

    var canUndo: Bool {
        if let sketch = sketchHistory { return sketch.canUndo }
        return !undoStack.isEmpty || generationUndo != nil
    }
    var canRedo: Bool {
        if let sketch = sketchHistory { return sketch.canRedo }
        return !redoStack.isEmpty
    }
    /// Export is available for anything with exportable geometry (not the gripper).
    var canExport: Bool { if case .gripper = source { return false }; return true }

    // MARK: authoring (Create/Modify tools on a loaded document)

    /// Add a fresh primitive as a new part in the loaded document.
    func addPrimitive(_ shape: BaseShape) {
        guard addFeature(Self.addCommand(shape), name: "Add \(shape.label)",
                         material: engineState?.defaultMaterial) != nil else { return }
        selectedFeatureID = featureNodes.last?.id        // focus the new part
    }

    /// `add_feature` for a palette primitive (sizes in mm).
    private static func addCommand(_ shape: BaseShape) -> DocCommand {
        switch shape {
        case .cube:
            return .addFeature(feature: .cube(sizeX: 30, sizeY: 30, sizeZ: 30), name: "Box")
        case .cylinder:
            return .addFeature(feature: .cylinder(radius: 15, height: 30, segments: 64), name: "Cylinder")
        case .sphere:
            return .addFeature(feature: .sphere(radius: 15, segments: 48), name: "Sphere")
        case .cone:
            return .addFeature(feature: .cone(radiusBottom: 15, radiusTop: 0, height: 30, segments: 64), name: "Cone")
        case .torus:
            return .addFeature(feature: .torus(majorRadius: 20, minorRadius: 5, segments: 64), name: "Torus")
        case .wedge:
            return .addFeature(feature: .wedge(sizeX: 30, sizeY: 30, sizeZ: 30), name: "Wedge")
        case .prism:
            return .addFeature(feature: .prism(sides: 6, radius: 15, height: 30), name: "Prism")
        }
    }

    /// Add a feature as one undo step, giving the new part `material`
    /// (otherwise the engine's `default`) and optionally moving it.
    @discardableResult
    private func addFeature(_ add: DocCommand, name: String, material: String?,
                            offset: [Double]? = nil) -> DocumentEngine.Result? {
        let material = material == "default" ? nil : material
        guard material != nil || offset != nil else { return send(add, name: name, reeval: .rebuild) }
        return engineEdit(name, undo: .snapshot, reeval: .rebuild) { engine in
            var r = try engine.apply(add)
            guard let id = r.createdFeatureId else { return r }
            if let material { r = try engine.apply(.setMaterial(id: id, material: material)) }
            if let offset { r = try engine.apply(.setTranslation(id: id, offset: offset)) }
            return r
        }
    }

    /// Add a primitive and move it to a point in kernel space (mm) — the
    /// palette's click-to-place. Two edits, one undo step apart: the add is the
    /// creation, the move is where you put it.
    func addPrimitive(_ shape: BaseShape, at p: SIMD3<Float>) {
        let placed = addFeature(Self.addCommand(shape), name: "Add \(shape.label)",
                                material: engineState?.defaultMaterial,
                                offset: [Double(p.x), Double(p.y), Double(p.z)])
        if placed != nil { selectedFeatureID = featureNodes.last?.id }
    }

    // MARK: armed tool (the Borland palette idiom: pick a component, then place it)

    /// The primitive the palette has armed, waiting for a click in the scene.
    /// Nil means the pointer is a pointer.
    var armedShape: BaseShape?

    func arm(_ shape: BaseShape) {
        armedShape = (armedShape == shape) ? nil : shape
    }
    func disarm() { armedShape = nil }

    /// Replace the selection with everything a marquee caught.
    func selectParts(_ parts: [Int]) {
        guard !parts.isEmpty else { deselectAll(); return }
        multiSelectedParts = parts.count == 1 ? [] : parts
        selectedFeatureID = featureNodes.first(where: { $0.partIndex == parts[0] })?.id
        selectionDirty = true
    }

    /// A boolean waiting for its second part: the selected part is the target,
    /// the next part clicked is the tool (Fusion's target/tool idiom). ⌘-click
    /// multi-select still works, but nothing on screen taught it.
    var pendingBoolean: (op: BooleanOp, target: Int)?

    /// Modeling is possible: a document, or the Untitled sandbox that the first
    /// modeling action promotes.
    var canModel: Bool { usesDocumentTree || (source.isSandbox && documentJSON == nil) }

    /// Start a boolean: two parts already selected combine now; one selected
    /// waits for a click on the other.
    func beginCombine(_ op: BooleanOp) {
        if multiSelectedParts.count == 2 { combineSelected(op); return }
        guard let target = selectedPartIndex else { return }
        pendingBoolean = (op, target)
    }

    /// The click that completes a pending boolean. Returns whether it consumed
    /// the click.
    func completePendingBoolean(withPart pi: Int) -> Bool {
        guard let p = pendingBoolean else { return false }
        // Clicking the target again is a miss, not a cancel: keep waiting
        // (Cancel / Esc in the prompt pill ends it).
        guard pi != p.target else { return true }
        pendingBoolean = nil
        multiSelectedParts = [p.target, pi]
        combineSelected(p.op)
        return true
    }

    /// Combine the two ⌘-selected parts with a boolean op (first = base).
    func combineSelected(_ op: BooleanOp) {
        guard multiSelectedParts.count == 2,
              let a = partId(forPart: multiSelectedParts[0]),
              let b = partId(forPart: multiSelectedParts[1]), a != b else { return }
        guard let type = BooleanType(rawValue: op.opType.lowercased()) else { return }
        // The combined part keeps the base part's material.
        guard addFeature(.addFeature(feature: .boolean(booleanType: type, inputA: a, inputB: b), name: op.opType),
                         name: op.label,
                         material: engineState?.material(ofPart: a) ?? engineState?.defaultMaterial) != nil
        else { return }
        multiSelectedParts = []
        hiddenParts.removeAll(); isolatedPart = nil       // indices collapsed
        selectedFeatureID = featureNodes.last?.id          // the new combined part
    }

    // MARK: feature ops (FeatureOps.swift drives these)

    /// Replace the selected parts with their convex hull, as an editable feature.
    @discardableResult
    func hullSelected() -> Bool {
        let picked = multiSelectedParts.compactMap { partId(forPart: $0) }
        guard picked.count >= 2 else { return false }
        guard addOp(.hull(inputs: picked, points: []), name: "Convex Hull", materialFrom: picked[0]) else { return false }
        multiSelectedParts = []
        hiddenParts.removeAll(); isolatedPart = nil
        return true
    }

    /// The hull feature owning `nodeId`: its id, input part ids and loose points.
    func hullFeature(nodeId: Int) -> (id: String, inputs: [String], points: [Double])? {
        guard let (part, _) = engineState?.owner(ofNode: nodeId),
              case .hull(let id, _, _, _, _, _, let inputs) = part else { return nil }
        let points = (opDict(nodeId: nodeId)?["points"] as? [NSNumber] ?? []).map(\.doubleValue)
        return (id, inputs, points)
    }

    /// Replace a hull's inputs and points. One undo step.
    @discardableResult
    func setHull(id: String, inputs: [String], points: [Double], name: String) -> Bool {
        runCommand(.updateFeature(id: id, feature: .hull(inputs: inputs, points: points)), name: name)
    }

    /// Set one field of a constant edge blend (`size` or `shape`).
    func editBlend(nodeId: Int, field: String, value: Double, snapshot: Bool) {
        irEdit("Change Blend", gestureStart: snapshot, reeval: .inPlace) { doc in
            guard var nodes = doc["nodes"] as? [String: Any],
                  var node = nodes[String(nodeId)] as? [String: Any],
                  var op = node["op"] as? [String: Any],
                  var profile = op["profile"] as? [String: Any], profile["type"] as? String == "Constant" else { return }
            profile[field] = value
            op["profile"] = profile
            node["op"] = op
            nodes[String(nodeId)] = node
            doc["nodes"] = nodes
        }
    }

    /// The current offset or rotation on a part's transform chain.
    func partVector(_ id: String, key: String) -> [Double]? {
        guard let part = engineState?.parts.first(where: { $0.id == id }),
              let op = opDict(nodeId: key == "offset" ? part.translateNodeId : part.rotateNodeId),
              let v = op[key] as? [String: Any] else { return nil }
        return ["x", "y", "z"].map { (v[$0] as? NSNumber)?.doubleValue ?? 0 }
    }

    /// Add any engine feature as one undo step; the new part takes
    /// `materialFrom`'s material (or the default). Selects the result.
    @discardableResult
    func addOp(_ feature: FeatureInput, name: String, materialFrom partId: String? = nil,
               offset: [Double]? = nil) -> Bool {
        let mat = partId.flatMap { engineState?.material(ofPart: $0) } ?? engineState?.defaultMaterial
        guard addFeature(.addFeature(feature: feature, name: name), name: name,
                         material: mat, offset: offset) != nil else { return false }
        selectedFeatureID = featureNodes.last?.id
        return true
    }

    /// Run a raw engine command (scale, reorder, rename…) as one undo step.
    @discardableResult
    func runCommand(_ command: DocCommand, name: String) -> Bool {
        send(command, name: name, reeval: .rebuild) != nil
    }

    /// Append IR through `vcad_feature_append` (ops the engine vocabulary
    /// lacks: per-edge blends, extrude-to-face) and reload, as ONE undo step.
    /// A feature whose `child` is -1 chains onto the previous one's result.
    @discardableResult
    func appendIRFeatures(_ features: [[String: Any]], name: String) -> Bool {
        guard var doc = documentJSON, !features.isEmpty else { return false }
        var prev: Int?
        for var f in features {
            for key in ["child", "target"] where (f[key] as? Int) == -1 {
                guard let prev else { return false }
                f[key] = prev
            }
            guard let docData = try? JSONSerialization.data(withJSONObject: doc),
                  let fData = try? JSONSerialization.data(withJSONObject: f),
                  let docStr = String(data: docData, encoding: .utf8),
                  let fStr = String(data: fData, encoding: .utf8) else { return false }
            guard let raw = vcad_feature_append(docStr, fStr) else {
                engineError = SimError.pending() ?? "\(name) failed"
                return false
            }
            let text = String(cString: raw)
            vcad_string_free(raw)
            guard let obj = try? JSONSerialization.jsonObject(with: Data(text.utf8)) as? [String: Any],
                  let next = obj["document"] as? [String: Any] else { return false }
            doc = next
            prev = (obj["node_id"] as? NSNumber)?.intValue
        }
        guard irEdit(name, reeval: .rebuild, base: doc) { _ in } != nil else { return false }
        // A feature the kernel can't build (a fillet whose edge reference no
        // longer resolves, an over-size radius) was committed anyway and the
        // whole part vanished with no message. Check, and take it back.
        if let failed = newEvaluationErrors(), !failed.isEmpty {
            undo()
            redoStack.removeAll()
            engineError = "\(name) couldn't be built, so it was undone:\n\(failed)"
            return false
        }
        return true
    }

    /// The kernel's per-part build failures for the current document, or nil
    /// when this kernel build can't report them.
    func newEvaluationErrors() -> String? {
        guard let engine else { return nil }
        return (try? engine.agentRead("evaluation_errors", [:]))?
            .trimmingCharacters(in: .whitespacesAndNewlines)
    }

    /// Mutate the IR document directly and reload, as ONE undo step — for
    /// op families the feature builder lacks (sheet-metal chains).
    @discardableResult
    func editIR(_ name: String, _ mutate: (inout [String: Any]) -> Void) -> Bool {
        irEdit(name, reeval: .rebuild, mutate) != nil
    }

    /// Root IR node id of a viewport part.
    func rootNodeId(forPart i: Int) -> Int? {
        guard let roots = documentJSON?["roots"] as? [[String: Any]], i >= 0, i < roots.count else { return nil }
        return (roots[i]["root"] as? NSNumber)?.intValue
    }

    /// Tabs available in the tool palette — Combine only applies to documents.
    var availableTabs: [ToolTab] { usesDocumentTree ? [.create, .modify, .combine] : [.create, .modify] }

    /// Wrap the selected part with a fillet/chamfer over all its edges.
    func applyModifierToSelected(_ mod: Modifier) {
        guard mod != .none, let pi = selectedPartIndex, let id = partId(forPart: pi) else { return }
        let selBefore = selectedPartIndex
        let feature: FeatureInput = mod == .fillet ? .fillet(input: id, radius: 2) : .chamfer(input: id, distance: 2)
        // The wrapped part keeps its material.
        guard addFeature(.addFeature(feature: feature, name: mod.label), name: mod.label,
                         material: engineState?.material(ofPart: id)) != nil else { return }
        // Keep the same part selected (its root node id changed → select its row).
        if let pi = selBefore, pi < featureNodes.count { selectedFeatureID = featureNodes[pi].id }
    }

    // MARK: sketch mode — constraint sketcher (SketchSession.swift) → feature

    var sketching = false
    var sketchPlane: SketchPlane = .xy
    /// The live constraint sketch, while sketching.
    var sketch: SketchSession?
    /// What Finish does with the closed profile.
    var sketchFinish: SketchFinish = .extrude
    var sketchExtrudeDepth: Double = 10
    var sketchRevolveAngle: Double = 360
    /// Sweep path: a helix when `sweepTurns > 0` (world-Z axis, rising
    /// `sweepLength` over `sweepTurns` turns at `sweepHelixRadius`, profile
    /// re-centred so its centroid rides the helix — see `SweepPlan`), else a
    /// straight line `sweepLength` along the sketch normal.
    var sweepHelixRadius: Double = 20
    var sweepLength: Double = 50
    var sweepTurns: Double = 0
    /// Profiles collected for a loft, in order.
    var loftProfiles: [[String: Any]] = []
    /// The planar face a sketch-on-face started from (extrude-to-face target too).
    var sketchFace: (part: Int, point: SIMD3<Double>, normal: SIMD3<Double>)?
    /// The picked face's durable IR FaceRef, when it carries a persistent
    /// name: the finished sketch attaches to it (IR `SketchOnFace`) so it
    /// follows the face when earlier features change.
    @ObservationIgnored var sketchFaceRef: Any?
    /// Viewport needs to rebuild the sketch ink.
    var sketchDirty = false
    var sketchOffsetPrompt = false
    var sketchOffsetDistance: Double = 2

    var sketchCursor: SIMD2<Float>? {
        get { sketch?.cursor.map { SIMD2(Float($0.x), Float($0.y)) } }
        set { sketch?.cursor = newValue.map { sketch?.gridded(SIMD2(Double($0.x), Double($0.y))) ?? SIMD2(Double($0.x), Double($0.y)) }; sketchDirty = true }
    }
    var canFinishSketch: Bool { sketch != nil && sketchFinishBlocker == nil }

    /// Done is hovered: the status field says why it can't finish.
    var sketchDoneHovered = false
    /// Done was clicked while blocked: the reason stays up until then.
    var sketchBlockerShownUntil: Date?

    /// The sketch bar's Done. Never silently dead: when the sketch can't
    /// finish it puts the reason in the status field and authors nothing.
    @discardableResult
    func attemptFinishSketch(now: Date = .now) -> Bool {
        guard sketch != nil else { return false }
        if sketchFinishBlocker != nil {
            sketchBlockerShownUntil = now.addingTimeInterval(4)
            return false
        }
        sketchBlockerShownUntil = nil
        return finishSketch()
    }

    /// The status field's second line while sketching: the active tool's
    /// prompt and the degrees of freedom — or, while Done is hovered or just
    /// clicked, why the sketch can't finish.
    func sketchStatusLine(at now: Date = .now) -> String? {
        guard sketching, let s = sketch else { return nil }
        if let why = sketchFinishBlocker,
           sketchDoneHovered || (sketchBlockerShownUntil.map { now < $0 } ?? false) {
            return "Can't finish — \(why)"
        }
        // Drawing: the snapped cursor first (it's what you place by), then a
        // short prompt, so the 340 pt field truncates the prompt, not the numbers.
        if let c = s.cursor, s.tool.draws {
            return "\(Self.cursorText(c)) · \(s.tool.shortHint) · \(s.stateLabel)"
        }
        return "\(s.tool.hint) · \(s.stateLabel)"
    }

    /// "x 12.0, y -4.0 mm" (never "-0.0").
    static func cursorText(_ c: SIMD2<Double>) -> String {
        String(format: "x %.1f, y %.1f mm", c.x + 0, c.y + 0)
            .replacingOccurrences(of: "-0.0,", with: "0.0,")
            .replacingOccurrences(of: "y -0.0 ", with: "y 0.0 ")
    }

    /// Why Finish is disabled, in words — the button's tooltip and the sketch
    /// status line. Nil when Finish can run.
    var sketchFinishBlocker: String? {
        guard let sketch else { return "Not sketching" }
        if sketchFinish == .loft, loftProfiles.count >= 2 { return nil }
        if sketch.isEmpty { return "Draw a closed profile to finish" }
        if sketch.canFinish { return nil }
        switch (sketchFinish, sketch.loopRegions()) {
        case (.extrude, .success), (.cut, .success): return nil
        case (_, .failure(let p)): return p.reason
        case (_, .success(let profiles)):
            let what = profiles.contains { $0["holes"] != nil } ? "a profile with holes" : "several separate loops"
            return "\(sketchFinish.label) can't use \(what) — Extrude or Cut can"
        }
    }

    /// Start a sketch: on the picked planar face when exactly one is picked,
    /// else on `sketchPlane`.
    @ObservationIgnored private var cameraBeforeSketch: CameraState?
    func enterSketch() {
        guard usesDocumentTree else { return }
        sketchPlaneChosen = false
        var frame = SketchFrame.principal(sketchPlane)
        sketchFace = nil
        sketchFaceRef = nil
        if picking.selectedFaces.count == 1, let f = picking.selectedFaces.first,
           let face = partTopology(f.part)?.face(f.id), face.isPlanar {
            let c = SIMD3(face.center[0], face.center[1], face.center[2])
            let n = SIMD3(face.normal[0], face.normal[1], face.normal[2])
            frame = .onFace(center: c, normal: n)
            sketchFace = (f.part, c, n)
            sketchFaceRef = face.selector.map(\.any).flatMap { ref in
                ((ref as? [String: Any])?["name"] as? String) != nil ? ref : nil
            }
        }
        guard let s = SketchSession(frame: frame) else { engineError = "Sketch solver unavailable"; return }
        if sketch == nil { cameraBeforeSketch = CameraState(azimuth: azimuth, elevation: elevation, distance: distance, pan: panOffset) }
        sketch = s
        sketching = true
        hoveredPartIndex = nil; hoverDirty = true
        aimCamera(at: frame)
        sketchDirty = true
    }
    /// Per-document transient UI state that must not survive a document
    /// switch: an open sketch (discarded like the sketch bar's ✕ — it was never
    /// part of the old document's DAG), the armed palette tool, a pending
    /// boolean, an open feature dialog, measure/section, face/edge picks and
    /// hover. The workspace is a remembered preference and stays.
    func resetTransientDocumentState() {
        if sketching || sketch != nil { exitSketch() }
        cameraBeforeSketch = nil
        sketchPlaneChosen = false
        loftProfiles = []
        sketchOffsetPrompt = false
        sketchDoneHovered = false; sketchBlockerShownUntil = nil
        armedShape = nil
        pendingBoolean = nil
        activeFeatureOp = nil
        measuring = false
        section = nil
        picking.clear()
        selectedInstanceIndex = nil
        hoveredPartIndex = nil; hoveredInstanceIndex = nil; hoveredInstances = []
        hoveredFeatureID = nil; hoveredGizmoHandle = nil; hoveredHandle = nil
        hoverDirty = true
    }
    func exitSketch() {
        sketching = false
        sketch = nil
        sketchFace = nil
        sketchDirty = true
        if let c = cameraBeforeSketch {
            azimuth = c.azimuth; elevation = c.elevation
            distance = c.distance; pinchBaseline = c.distance; panOffset = c.pan
            cameraBeforeSketch = nil
        }
    }
    /// Nothing drawn and no plane chosen yet: the next click may pick a face.
    var sketchAwaitsPlane: Bool {
        guard let k = sketch, sketchFace == nil, !sketchPlaneChosen else { return false }
        // A click with a drawing tool armed places geometry (Fusion/Shapr3D:
        // the plane is chosen before drawing). Only the Select tool picks a
        // face to move an untouched sketch onto.
        guard k.tool == .select else { return false }
        return k.isEmpty && !k.hasInProgress
    }
    /// Restart the sketch on the face just picked. False when it isn't planar.
    func adoptPickedFaceAsSketchPlane() -> Bool {
        guard picking.selectedFaces.count == 1, let f = picking.selectedFaces.first,
              partTopology(f.part)?.face(f.id)?.isPlanar == true else { picking.clear(); return false }
        let tool = sketch?.tool ?? .line
        enterSketch()
        sketch?.tool = tool
        picking.clear()
        if sketchFace != nil { GlassUI.shared.flash("Sketch moved onto the picked face", symbol: "square.on.square") }
        return sketchFace != nil
    }
    func setSketchPlane(_ p: SketchPlane) {
        sketchPlane = p
        sketchPlaneChosen = true
        guard sketching, sketchFace == nil else { return }
        sketch = SketchSession(frame: .principal(p))
        aimCamera(at: .principal(p))
        sketchDirty = true
    }
    /// Look at the sketch plane head-on.
    private func aimCamera(at f: SketchFrame) {
        let n = f.normal
        elevation = Float(asin(max(-1, min(1, n.z)))).clamped(to: -1.45...1.45)
        azimuth = abs(n.z) > 0.99 ? 0 : Float(atan2(n.x, -n.y))
        // Open close enough that the 0.5 mm grid can be hit, centred on the plane.
        let o = SIMD3<Float>(f.origin) - displayCenter
        // An empty document has no scene yet, so its display centre is stale.
        let empty = (documentJSON?["roots"] as? [Any])?.isEmpty ?? true
        panOffset = empty ? .zero : SIMD3(o.x, o.z, -o.y) * displayScale
        distance = max(0.45, min(1.5, displayScale * 20))
        pinchBaseline = distance
    }

    func setSketchTool(_ t: SketchToolKind) {
        // A tool's in-progress clicks don't carry over: a line chain left open
        // would otherwise resume from its old end on the next Line click.
        if sketch?.tool != t { sketch?.cancelPending() }
        sketch?.tool = t
        sketchDirty = true
    }

    /// A tap on the sketch plane (2D plane coords) — drives the active tool.
    func sketchTap(_ p: SIMD2<Float>, additive: Bool = false) {
        // Option places freely: no grid, no inference.
        sketch?.suppressInference = NSEvent.modifierFlags.contains(.option)
        sketch?.tap(SIMD2(Double(p.x), Double(p.y)), additive: additive)
        sketchDirty = true
    }

    /// A viewport click on the sketch plane, given as the pick ray and the ray
    /// one screen point to its right. The pair sets the picking scale (plane
    /// mm per screen point), so edges and points hit within a few points of
    /// their ink at any zoom. Picking tools (Select, Dimension) take the raw
    /// hit; drawing tools land on the cursor grid.
    func sketchTap(ray: (o: SIMD3<Float>, d: SIMD3<Float>), neighbour: (o: SIMD3<Float>, d: SIMD3<Float>)?,
                   additive: Bool = false) {
        guard let sketch else { return }
        guard let raw = sketch.frame.intersect(origin: SIMD3<Double>(ray.o), dir: SIMD3<Double>(ray.d)) else { return }
        if let n = neighbour, let q = sketch.frame.intersect(origin: SIMD3<Double>(n.o), dir: SIMD3<Double>(n.d)) {
            let k = simd_distance(raw, q)
            sketch.pickScale = k.isFinite && k > 0 ? k : nil
        }
        if !sketch.tool.draws {
            sketch.tap(raw, additive: additive)
            sketchDirty = true
        } else if let p = sketchPlanePoint(originKernel: ray.o, dirKernel: ray.d) {
            sketchTap(p, additive: additive)
        }
    }

    /// Drag a sketch point in Select: true when a point was under the ray.
    func beginSketchDrag(ray: (o: SIMD3<Float>, d: SIMD3<Float>)) -> Bool {
        guard let sketch, sketch.tool == .select,
              let p = sketch.frame.intersect(origin: SIMD3<Double>(ray.o), dir: SIMD3<Double>(ray.d)) else { return false }
        return sketch.beginDrag(p)
    }
    func sketchDrag(ray: (o: SIMD3<Float>, d: SIMD3<Float>)) {
        guard let sketch, let p = sketch.frame.intersect(origin: SIMD3<Double>(ray.o), dir: SIMD3<Double>(ray.d)) else { return }
        sketch.dragTo(p); sketchDirty = true
    }
    func endSketchDrag() { sketch?.endDrag(); sketchDirty = true }

    /// Spacing of the sketch cursor grid, in mm.
    static let sketchGridStep = 0.5

    /// Project a kernel-space ray onto the sketch plane → 2D plane coords.
    func sketchPlanePoint(originKernel o: SIMD3<Float>, dirKernel d: SIMD3<Float>) -> SIMD2<Float>? {
        let frame = sketch?.frame ?? .principal(sketchPlane)
        guard let p = frame.intersect(origin: SIMD3<Double>(o), dir: SIMD3<Double>(d)) else { return nil }
        // Sketch points land on a grid measured from the sketch origin, so a
        // click can hit a real dimension. Hold Option to place freely.
        guard !NSEvent.modifierFlags.contains(.option) else { return SIMD2(Float(p.x), Float(p.y)) }
        let step = Self.sketchGridStep
        return SIMD2(Float((p.x / step).rounded() * step), Float((p.y / step).rounded() * step))
    }

    /// 2D plane coords → 3D kernel coords (for drawing the ink).
    func sketchWorld(_ v: SIMD2<Float>) -> SIMD3<Float> {
        let frame = sketch?.frame ?? .principal(sketchPlane)
        return SIMD3<Float>(frame.world(SIMD2(Double(v.x), Double(v.y))))
    }

    /// `profile` attached to the sketched-on face (`on_face`), when that face
    /// has a persistent name — so the feature re-resolves its plane on every
    /// rebuild instead of freezing it where the face was.
    private func attachedToSketchFace(_ profile: [String: Any]) -> [String: Any] {
        guard let ref = sketchFaceRef, let part = sketchFace?.part,
              let target = rootNodeId(forPart: part) else { return profile }
        var p = profile
        p["on_face"] = ["target": target, "face": ref]
        return p
    }

    /// Realize the profile as the chosen feature, then leave sketch mode
    /// (a loft section stays in sketch mode for the next profile).
    @discardableResult
    func finishSketch() -> Bool {
        guard let sketch, canEditDocument else { return false }
        let n = sketch.frame.normal
        func vec(_ v: SIMD3<Double>) -> [String: Double] { ["x": v.x, "y": v.y, "z": v.z] }
        let dir = vec(n * sketchExtrudeDepth)
        // Several loops — separate regions, or an outer loop with holes (a
        // ring): one feature per region, in one undo step.
        if !sketch.canFinish, sketchFinish == .extrude || sketchFinish == .cut {
            let cutting = sketchFinish == .cut
            let profiles: [[String: Any]]
            switch sketch.loopRegions() {
            case .success(let p): profiles = p
            case .failure(let problem): engineError = problem.reason; return false
            }
            var features: [[String: Any]]
            if cutting {
                guard let target = sketchFace.flatMap({ rootNodeId(forPart: $0.part) })
                        ?? selectedPartIndex.flatMap({ rootNodeId(forPart: $0) }) else {
                    engineError = "Select the part to cut"; return false
                }
                features = profiles.enumerated().map { i, p in
                    ["kind": "cut_extrude", "target": i == 0 ? target : -1, "profile": attachedToSketchFace(p),
                     "direction": vec(-n * sketchExtrudeDepth), "name": "Cut"]
                }
            } else {
                features = profiles.map { ["kind": "extrude", "profile": attachedToSketchFace($0), "direction": dir, "name": "Extrude"] }
            }
            guard appendIRFeatures(features, name: sketchFinish.label) else { return false }
            selectedFeatureID = featureNodes.last?.id
            exitSketch()
            return true
        }
        var feature: [String: Any]
        switch sketchFinish {
        case .loft:
            if sketch.canFinish, let p = sketch.profileJSON() { loftProfiles.append(p) }
            guard loftProfiles.count >= 2 else {
                // Next section: a fresh sketch offset along the normal.
                var f = sketch.frame; f.origin += n * sketchExtrudeDepth
                self.sketch = SketchSession(frame: f); sketchDirty = true
                return true
            }
            feature = ["kind": "loft", "profiles": loftProfiles, "name": "Loft"]
        default:
            guard let profile = sketch.profileJSON() else { engineError = sketch.lastError; return false }
            switch sketchFinish {
            case .extrude:
                feature = ["kind": "extrude", "profile": attachedToSketchFace(profile), "direction": dir, "name": "Extrude"]
            case .cut:
                guard let target = sketchFace.flatMap({ rootNodeId(forPart: $0.part) })
                        ?? selectedPartIndex.flatMap({ rootNodeId(forPart: $0) }) else {
                    engineError = "Select the part to cut"; return false
                }
                feature = ["kind": "cut_extrude", "target": target, "profile": attachedToSketchFace(profile),
                           "direction": vec(-n * sketchExtrudeDepth), "name": "Cut"]
            case .extrudeToFace:
                guard picking.selectedFaces.count == 1, let f = picking.selectedFaces.first,
                      let face = partTopology(f.part)?.face(f.id) else {
                    engineError = "Pick the face to extrude up to"; return false
                }
                let fp = SIMD3(face.center[0], face.center[1], face.center[2])
                let fn = SIMD3(face.normal[0], face.normal[1], face.normal[2])
                let towards = simd_dot(fp - sketch.frame.origin, n) >= 0 ? n : -n
                feature = ["kind": "extrude_to_face", "profile": profile, "direction": vec(towards),
                           "face_point": vec(fp), "face_normal": vec(fn), "name": "Extrude to Face"]
                if let t = rootNodeId(forPart: f.part) { feature["target"] = t }
            case .revolve:
                // Axis: the sketch's own Y axis through its origin.
                feature = ["kind": "revolve", "profile": profile, "axis_origin": vec(sketch.frame.origin),
                           "axis_dir": vec(sketch.frame.yDir), "angle_deg": sketchRevolveAngle, "name": "Revolve"]
            case .sweep:
                let plan = SweepPlan(profile: profile, turns: sweepTurns,
                                     radius: sweepHelixRadius, length: sweepLength)
                if let problem = plan.problem { engineError = problem; return false }
                let path: [String: Any] = plan.isHelix
                    ? ["type": "Helix", "radius": sweepHelixRadius, "pitch": plan.pitch,
                       "height": sweepLength, "turns": sweepTurns]
                    : ["type": "Line", "start": vec(.zero), "end": vec(n * sweepLength)]
                feature = ["kind": "sweep", "profile": plan.profile, "path": path, "name": "Sweep"]
            case .loft:
                return false
            }
        }
        guard appendIRFeatures([feature], name: sketchFinish.label) else { return false }
        loftProfiles = []
        selectedFeatureID = featureNodes.last?.id
        exitSketch()
        return true
    }

    // MARK: transform gizmo (drag a part to translate it — axis + plane handles)

    /// Last viewport size, so a targeted handle drag can rebuild the kernel ray.
    @ObservationIgnored var viewSize: CGSize = .zero
    /// Viewport should reposition / re-highlight the gizmo.
    var gizmoDirty = false
    /// The handle under the cursor ("gizmoX" / "planeXY" / …), for hover + cursor.
    var hoveredGizmoHandle: String?
    /// Direct-manipulation drag state (DirectManipulation.swift).
    @ObservationIgnored var manip = DirectManipState()
    /// The live on-canvas dimension (value + anchor), shown while dragging
    /// and briefly after, clickable to type an exact value.
    var manipLabel: ManipLabel?
    @ObservationIgnored private var gizmoPart: Int?
    @ObservationIgnored private var gizmoActive: GizmoHandle?
    @ObservationIgnored private var gizmoBase: (Double, Double, Double) = (0, 0, 0)
    @ObservationIgnored private var gizmoStartCenter: SIMD3<Float> = .zero
    @ObservationIgnored private var gizmoT0: Float = 0
    @ObservationIgnored private var gizmoStartPlane: SIMD3<Float> = .zero
    // Rotate-ring drag.
    @ObservationIgnored private var gizmoRotNode: Int?
    @ObservationIgnored private var gizmoRotAxisIndex = 0
    @ObservationIgnored private var gizmoRotU1: SIMD3<Float> = .zero
    @ObservationIgnored private var gizmoRotU2: SIMD3<Float> = .zero
    @ObservationIgnored private var gizmoRotPrev: Float = 0
    @ObservationIgnored private var gizmoRotAccum: Float = 0
    /// Stable id of the dragged part when its feature carries a translate
    /// (drags send `set_translation`); nil → IR edits against `gizmoWorkJSON`.
    @ObservationIgnored private var gizmoPartId: String?
    /// The pre-drag IR the fallback path edits each event (its node ids stay
    /// put, unlike the engine's renumbered output).
    @ObservationIgnored private var gizmoWorkJSON: [String: Any]?
    /// Live displacement of the part during a drag, so the viewport can slide the
    /// gizmo with it instead of leaving it at the grab point.
    @ObservationIgnored var gizmoLiveOffset: SIMD3<Float> = .zero

    enum GizmoHandle {
        case axis(SIMD3<Float>)
        case plane(SIMD3<Float>, SIMD3<Float>)   // two in-plane unit axes
        case rotate(SIMD3<Float>)                // spin axis
        var isRotate: Bool { if case .rotate = self { return true }; return false }
    }
    func gizmoHandle(for name: String) -> GizmoHandle? {
        if let h = manipHandle(for: name) { return h }
        switch name {
        case "gizmoX": return .axis(SIMD3(1, 0, 0))
        case "gizmoY": return .axis(SIMD3(0, 1, 0))
        case "gizmoZ": return .axis(SIMD3(0, 0, 1))
        case "planeXY": return .plane(SIMD3(1, 0, 0), SIMD3(0, 1, 0))
        case "planeYZ": return .plane(SIMD3(0, 1, 0), SIMD3(0, 0, 1))
        case "planeXZ": return .plane(SIMD3(1, 0, 0), SIMD3(0, 0, 1))
        case "rotX": return .rotate(SIMD3(1, 0, 0))
        case "rotY": return .rotate(SIMD3(0, 1, 0))
        case "rotZ": return .rotate(SIMD3(0, 0, 1))
        default: return nil
        }
    }
    static let gizmoRotDefs: [(name: String, axis: SIMD3<Float>)] = [
        ("rotX", SIMD3(1, 0, 0)), ("rotY", SIMD3(0, 1, 0)), ("rotZ", SIMD3(0, 0, 1)),
    ]
    var gizmoRingRadius: Float { gizmoArmLength() * 0.82 }
    /// Two orthonormal in-plane reference axes for a rotation axis.
    static func ringBasis(_ axis: SIMD3<Float>) -> (SIMD3<Float>, SIMD3<Float>) {
        if axis.x != 0 { return (SIMD3(0, 1, 0), SIMD3(0, 0, 1)) }
        if axis.y != 0 { return (SIMD3(0, 0, 1), SIMD3(1, 0, 0)) }
        return (SIMD3(1, 0, 0), SIMD3(0, 1, 0))
    }
    static let gizmoAxisDefs: [(name: String, dir: SIMD3<Float>)] = [
        ("gizmoX", SIMD3(1, 0, 0)), ("gizmoY", SIMD3(0, 1, 0)), ("gizmoZ", SIMD3(0, 0, 1)),
    ]
    static let gizmoPlaneDefs: [(name: String, a: SIMD3<Float>, b: SIMD3<Float>)] = [
        ("planeXY", SIMD3(1, 0, 0), SIMD3(0, 1, 0)),
        ("planeYZ", SIMD3(0, 1, 0), SIMD3(0, 0, 1)),
        ("planeXZ", SIMD3(1, 0, 0), SIMD3(0, 0, 1)),
    ]

    /// Show the gizmo for a single selected part (not while sketching/multi-select).
    var showsGizmo: Bool {
        usesDocumentTree && !sketching && multiSelectedParts.isEmpty
            && (selectedPartIndex != nil || pullFace != nil)
    }
    /// Selected part center in kernel coords (gizmo origin).
    func gizmoCenterKernel() -> SIMD3<Float>? {
        guard let pi = selectedPartIndex, pi < docPartMeshes.count else { return nil }
        let m = docPartMeshes[pi]
        return (m.lo + m.hi) / 2
    }
    /// Arm length — reaches past the part so the arrowheads clear the geometry.
    func gizmoArmLength() -> Float {
        guard let pi = selectedPartIndex, pi < docPartMeshes.count else { return 14 }
        let d = docPartMeshes[pi].hi - docPartMeshes[pi].lo
        return max(10, 0.5 * max(d.x, max(d.y, d.z)) + 0.22 * (d.x + d.y + d.z) / 3)
    }
    var gizmoPlaneOffset: Float { gizmoArmLength() * 0.34 }
    var gizmoPlaneSize: Float { gizmoArmLength() * 0.17 }

    func beginGizmoDrag(handle name: String, ray: (o: SIMD3<Float>, d: SIMD3<Float>)) {
        if beginManip(handle: name, ray: ray) { return }
        guard let pi = selectedPartIndex, let engine, let json = documentJSON,
              let c = gizmoCenterKernel(), let h = gizmoHandle(for: name) else { return }
        // The whole drag is one undo step: snapshot now, edit freely after.
        if case .rotate = h { pushUndo(.snapshot(engine.save()), name: "Rotate Part") }
        else { pushUndo(.snapshot(engine.save()), name: "Move Part") }
        gizmoPart = pi
        gizmoActive = h
        manip.activeName = name
        gizmoStartCenter = c
        gizmoWorkJSON = json
        gizmoPartId = nil
        gizmoBase = DocEdit.rootTranslateOffset(json, partIndex: pi) ?? (0, 0, 0)
        if case .rotate = h {} else if let id = partId(forPart: pi),
           let tn = engineState?.parts.first(where: { $0.id == id })?.translateNodeId {
            gizmoPartId = id
            let o = DocEdit.op(json, nodeId: tn)?["offset"] as? [String: Any] ?? [:]
            gizmoBase = ((o["x"] as? NSNumber)?.doubleValue ?? 0,
                         (o["y"] as? NSNumber)?.doubleValue ?? 0,
                         (o["z"] as? NSNumber)?.doubleValue ?? 0)
        }
        gizmoLiveOffset = .zero
        switch h {
        case .axis(let ax):
            gizmoT0 = Self.axisParam(rayO: ray.o, rayD: ray.d, center: c, axis: ax)
        case .plane(let a, let b):
            let n = simd_normalize(simd_cross(a, b))
            gizmoStartPlane = Self.rayPlane(o: ray.o, d: ray.d, point: c, normal: n) ?? c
        case .rotate(let ax):
            let (u1, u2) = Self.ringBasis(ax)
            gizmoRotU1 = u1; gizmoRotU2 = u2
            gizmoRotAxisIndex = ax.x != 0 ? 0 : (ax.y != 0 ? 1 : 2)
            gizmoRotAccum = 0
            gizmoRotPrev = Self.ringAngle(o: ray.o, d: ray.d, center: c, axis: ax, u1: u1, u2: u2) ?? 0
            var j = json
            gizmoRotNode = DocEdit.wrapRotate(&j, partIndex: pi, Double(c.x), Double(c.y), Double(c.z))
            gizmoWorkJSON = j
        }
    }
    func gizmoDragTo(ray: (o: SIMD3<Float>, d: SIMD3<Float>)) {
        if manipDragTo(ray: ray) { return }
        guard let pi = gizmoPart, var json = gizmoWorkJSON, let h = gizmoActive else { return }
        switch h {
        case .rotate(let ax):
            guard let rn = gizmoRotNode,
                  let a = Self.ringAngle(o: ray.o, d: ray.d, center: gizmoStartCenter,
                                         axis: ax, u1: gizmoRotU1, u2: gizmoRotU2) else { return }
            var d = a - gizmoRotPrev
            if d > .pi { d -= 2 * .pi } else if d < -.pi { d += 2 * .pi }   // unwrap
            gizmoRotAccum += d
            gizmoRotPrev = a
            let deg = snapAngle(Double(gizmoRotAccum * 180 / .pi))
            DocEdit.setRotateAngle(&json, rotNodeId: rn, axisIndex: gizmoRotAxisIndex, degrees: deg)
            showManipLabel(String(format: "%.1f°", deg), at: gizmoStartCenter)
        default:
            var off = SIMD3<Float>.zero
            switch h {
            case .axis(let ax):
                off = ax * (Self.axisParam(rayO: ray.o, rayD: ray.d, center: gizmoStartCenter, axis: ax) - gizmoT0)
            case .plane(let a, let b):
                let n = simd_normalize(simd_cross(a, b))
                guard let p = Self.rayPlane(o: ray.o, d: ray.d, point: gizmoStartCenter, normal: n) else { return }
                off = p - gizmoStartPlane
            case .rotate:
                break
            }
            if let pi = gizmoPart { off = snapMove(off, part: pi, handle: h) }
            gizmoLiveOffset = off
            showManipLabel(moveLabel(off), at: gizmoStartCenter + off)
            let target = [gizmoBase.0 + Double(off.x), gizmoBase.1 + Double(off.y), gizmoBase.2 + Double(off.z)]
            if let id = gizmoPartId {
                engineEdit("Move Part", undo: .none, reeval: .inPlace) {
                    try $0.apply(.setTranslation(id: id, offset: target))
                }
                return
            }
            DocEdit.setRootTranslate(&json, partIndex: pi, target[0], target[1], target[2])
        }
        // Edit the stable pre-drag IR, then reload: smooth in-place re-eval.
        gizmoWorkJSON = json
        irEdit(h.isRotate ? "Rotate Part" : "Move Part", gestureStart: false, reeval: .inPlace, base: json) { _ in }
    }
    /// Where the gizmo root sits mid-drag: it rides the dragged part, or the
    /// pulled face.
    func gizmoLivePosition() -> SIMD3<Float>? {
        if manip.kind != nil {
            if pullFace == nil { return gizmoCenterKernel() }
            return manip.axisOrigin + manip.axisDir * Float(manip.value / manip.gain)
        }
        return gizmoCenterKernel().map { $0 + gizmoLiveOffset }
    }
    func endGizmoDrag() {
        if endManip() { return }
        manip.activeName = nil
        if gizmoPart != nil { fadeManipLabel() }
        gizmoPart = nil
        gizmoActive = nil
        gizmoRotNode = nil
        gizmoPartId = nil
        gizmoWorkJSON = nil
        gizmoLiveOffset = .zero
        gizmoDirty = true
    }

    // Gizmo hover/tap hit testing is native now: the gizmo's invisible grab
    // proxies carry CollisionComponents, so scene raycasts find them directly.

    /// Param `t` along the axis line (center + t·axis) closest to the ray — the
    /// standard closest-point-between-two-lines solution (axis is a unit vector).
    static func axisParam(rayO: SIMD3<Float>, rayD: SIMD3<Float>,
                                  center: SIMD3<Float>, axis: SIMD3<Float>) -> Float {
        let w0 = rayO - center
        let b = simd_dot(rayD, axis)
        let d = simd_dot(rayD, w0)
        let e = simd_dot(axis, w0)
        let denom = 1 - b * b
        if abs(denom) < 1e-5 { return e }       // ray ∥ axis → fall back to projection
        return (e - b * d) / denom
    }

    /// Ray ∩ plane (through `point`, with `normal`) → world point, or nil if ∥.
    static func rayPlane(o: SIMD3<Float>, d: SIMD3<Float>,
                                 point: SIMD3<Float>, normal: SIMD3<Float>) -> SIMD3<Float>? {
        let denom = simd_dot(d, normal)
        if abs(denom) < 1e-6 { return nil }
        return o + d * (simd_dot(point - o, normal) / denom)
    }

    /// Angle of the cursor around `axis` in the ring plane (atan2 in the u1/u2
    /// basis) — drives the rotate-ring drag.
    private static func ringAngle(o: SIMD3<Float>, d: SIMD3<Float>, center: SIMD3<Float>,
                                  axis: SIMD3<Float>, u1: SIMD3<Float>, u2: SIMD3<Float>) -> Float? {
        guard let p = rayPlane(o: o, d: d, point: center, normal: axis) else { return nil }
        let v = p - center
        return atan2(simd_dot(v, u2), simd_dot(v, u1))
    }

    // MARK: save

    func saveDocument() {
        if source.isSandbox, documentJSON != nil {
            #if os(macOS)
            saveAsPanel(self)
            #endif
            return
        }
        if isLoonDocument {
            // Never overwrite generated loon source with compiled JSON.
            #if os(macOS)
            saveAsPanel(self)
            #endif
            return
        }
        guard case let .document(path, _) = source,
              let json = documentJSON, let data = DocEdit.serializePretty(json) else { return }
        // Atomic + fsynced + verified: the file on disk is never half-written
        // if the app dies or the disk fills mid-save (LocalHistory.swift).
        do {
            try SafeWriter.write(data, to: URL(fileURLWithPath: path))
        } catch {
            saveError = "Couldn’t save “\(documentName)”: \(error). The file on disk is unchanged; your work is still autosaved."
            return
        }
        do {
            didSaveDocument(data, at: URL(fileURLWithPath: path))
            documentDirty = false
            savedSnapshot = DocEdit.serialize(json)
            savedHash = engine?.stateHash
            writeMeshBundle(for: URL(fileURLWithPath: path))
        }
    }

    /// Save to a new location and keep editing it there — the undo history
    /// survives, the same as Save As in any Mac app.
    func saveDocumentAs(_ url: URL) {
        guard let json = documentJSON, let data = DocEdit.serializePretty(json) else { return }
        do { try SafeWriter.write(data, to: url) } catch {
            saveError = "Couldn’t save to \(url.lastPathComponent): \(error)"
            return
        }
        didSaveDocument(data, at: url)
        // Keep the engine (and with it the history the undo stack points
        // into) rather than reopening the file we just wrote.
        let keptUndo = undoStack, keptRedo = redoStack
        let keptEngine = engine, keptState = engineState
        sourceRenameOnly = true
        source = .document(path: url.path, label: url.deletingPathExtension().lastPathComponent)
        sourceRenameOnly = false
        if let keptEngine, let keptState {
            engine = keptEngine; engineError = nil
            savedHash = keptEngine.stateHash
            adopt(keptState, reeval: .none)
        }
        undoStack = keptUndo; redoStack = keptRedo
        writeMeshBundle(for: url)
        rememberRecent(url)
    }

    /// The document's file, when it has one.
    var documentURL: URL? {
        if case let .document(path, _) = source { return URL(fileURLWithPath: path) }
        return nil
    }

    #if os(macOS)
    func showInFinder() {
        guard let url = documentURL else { return }
        NSWorkspace.shared.activateFileViewerSelecting([url])
    }
    #endif

    /// Replace the whole document with `dict` as ONE undoable edit (version
    /// preview / restore). Engine documents go through the engine so the
    /// result is evaluated exactly like an opened file; detached documents
    /// take a JSON undo step.
    @discardableResult
    func replaceDocument(with dict: [String: Any], name: String) -> Bool {
        if engine != nil {
            return irEdit(name, reeval: .rebuild, base: dict) { _ in } != nil
        }
        guard let before = documentJSON.flatMap({ DocEdit.serialize($0) }),
              let data = DocEdit.serialize(dict) else { return false }
        pushUndo(.json(before), name: name)
        restoreJSON(data)
        return true
    }

    /// Discard edits and reload the document from disk.
    func revertDocument() {
        guard case .document = source else { return }
        loadDocumentTree()
        geometryDirty = true; selectionDirty = true; visibilityDirty = true
        selectedFeatureID = featureNodes.first?.id
    }

    /// Current op dict for a node — feeds the inspector's parameter editors.
    func opDict(nodeId: Int) -> [String: Any]? {
        documentJSON.flatMap { DocEdit.op($0, nodeId: nodeId) }
    }

    /// Document-level named parameters of the loaded doc (empty for sandbox).
    var docParameters: [DocParameter] { documentGraph?.parameters ?? [] }

    /// Scrub a document-level parameter: clamp to its declared range, write it
    /// into the live JSON, and re-evaluate in place — bindings fan the value
    /// out to every bound node field inside the kernel.
    func editParameter(_ name: String, value: Double, snapshot: Bool) {
        guard let p = docParameters.first(where: { $0.name == name }), p.isLiteral else { return }
        var v = value
        if let lo = p.min { v = Swift.max(lo, v) }
        if let hi = p.max { v = Swift.min(hi, v) }
        irEdit("Change \(name)", gestureStart: snapshot, reeval: .inPlace) {
            DocEdit.setParameter(&$0, name: name, value: v)
        }
    }

    func editScalar(nodeId: Int, key: String, value: Double, snapshot: Bool, name: String = "Change Value") {
        editNodeField(nodeId: nodeId, key: key, axis: nil, value: value, snapshot: snapshot, name: name) {
            DocEdit.setScalar(&$0, nodeId: nodeId, key: key, value: value)
        }
    }
    func editVec(nodeId: Int, key: String, axis: String, value: Double, snapshot: Bool, name: String = "Change Value") {
        editNodeField(nodeId: nodeId, key: key, axis: axis, value: value, snapshot: snapshot, name: name) {
            DocEdit.setVecComponent(&$0, nodeId: nodeId, key: key, axis: axis, value: value)
        }
    }
    func editInt(nodeId: Int, key: String, value: Int, snapshot: Bool, name: String = "Change Value") {
        editNodeField(nodeId: nodeId, key: key, axis: nil, value: Double(value), snapshot: snapshot, name: name) {
            DocEdit.setInt(&$0, nodeId: nodeId, key: key, value: value)
        }
    }
    func renameFeature(_ nodeId: Int, to name: String) {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !trimmed.isEmpty else { return }
        if let id = engineState?.owner(ofNode: nodeId)?.part.id {
            send(.rename(id: id, name: trimmed), name: "Rename", reeval: .none)
        } else {
            irEdit("Rename", reeval: .none) { DocEdit.setName(&$0, nodeId: nodeId, name: trimmed) }
        }
    }
    func deletePart(_ partIndex: Int) {
        guard let id = partId(forPart: partIndex),
              send(.deleteFeature(id: id), name: "Delete Part", reeval: .rebuild) != nil else { return }
        // Indices shifted; clear visibility/multi-select and re-anchor selection.
        hiddenParts.removeAll(); isolatedPart = nil; multiSelectedParts = []
        if selectedFeatureNode == nil { selectedFeatureID = featureNodes.first?.id }
    }

    // MARK: selection commands (the Edit menu)

    /// Every visible part.
    func selectAllParts() {
        guard usesDocumentTree else { return }
        selectParts((0..<partCount).filter { isPartVisible($0) })
    }
    var hasDeletableSelection: Bool { usesDocumentTree && !highlightedParts.isEmpty }
    /// Delete every highlighted part as one undo step. Highest index first so
    /// the remaining indices stay valid while removing.
    func deleteSelectedParts() {
        if sketching {
            sketch?.deleteSelectionOrConstraint()
            sketchDirty = true
            return
        }
        let ids = highlightedParts.compactMap { partId(forPart: $0) }
        guard !ids.isEmpty else { return }
        guard send(.batch(commands: ids.map { .deleteFeature(id: $0) }),
                   name: ids.count == 1 ? "Delete Part" : "Delete \(ids.count) Parts",
                   reeval: .rebuild) != nil else { return }
        hiddenParts.removeAll(); isolatedPart = nil; multiSelectedParts = []
        selectedFeatureID = featureNodes.first?.id
    }
    func beginRenamingSelection() {
        renamingFeatureID = selectedFeatureNode?.id
    }
    func toggleSelectedVisibility() {
        guard let pi = selectedPartIndex else { return }
        toggleVisibility(part: pi)
    }
    func toggleSelectedIsolate() {
        guard let pi = selectedPartIndex else { return }
        isolate(part: pi)
    }

    func undo() {
        if let sketch = sketchHistory {
            if sketch.undo() { sketchDirty = true }
            else { GlassUI.shared.flash("Nothing to undo in this sketch", symbol: "pencil.and.outline") }
            return
        }
        guard let entry = undoStack.popLast() else { undoGeneration(); return }
        guard let back = step(entry, forward: false) else { return }
        redoStack.append(back)
        GlassUI.shared.flash("Undo \(entry.name)", symbol: "arrow.uturn.backward")
    }
    func redo() {
        if let sketch = sketchHistory {
            if sketch.redo() { sketchDirty = true }
            return
        }
        guard let entry = redoStack.popLast() else { return }
        guard let back = step(entry, forward: true) else { return }
        undoStack.append(back)
        GlassUI.shared.flash("Redo \(entry.name)", symbol: "arrow.uturn.forward")
    }

    /// Apply one history entry; returns the entry that reverses it.
    private func step(_ entry: UndoEntry, forward: Bool) -> UndoEntry? {
        switch entry.step {
        case .engine:
            guard let engine, let r = try? engine.apply(forward ? .redo : .undo) else { return nil }
            adopt(r, reeval: .rebuild)
            afterHistoryStep()
            return entry
        case .snapshot(let data):
            guard let engine else { return nil }
            let current = engine.save()
            guard let r = try? engine.restore(data) else { return nil }
            adopt(r, reeval: .rebuild)
            afterHistoryStep()
            return UndoEntry(step: .snapshot(current), name: entry.name)
        case .json(let data):
            guard let cur = documentJSON.flatMap({ DocEdit.serialize($0) }) else { return nil }
            restoreJSON(data)
            return UndoEntry(step: .json(cur), name: entry.name)
        }
    }

    private func afterHistoryStep() {
        hiddenParts.removeAll(); isolatedPart = nil
        if selectedFeatureNode == nil { selectedFeatureID = featureNodes.first?.id }
    }

    /// Put back whatever the intent bar replaced.
    private func undoGeneration() {
        guard let g = generationUndo else { return }
        generationUndo = nil
        source = g.source
        if let json = g.json {
            engine = g.engine; engineState = g.state; savedHash = g.savedHash
            documentJSON = json
            if let graph = DocumentGraph.parse(json) { documentGraph = graph; featureNodes = graph.featureRoots() }
            geometryDirty = true
        }
        undoStack = g.undo; redoStack = g.redo
        documentDirty = g.dirty
    }
    private func restoreJSON(_ data: Data) {
        guard let dict = (try? JSONSerialization.jsonObject(with: data)) as? [String: Any] else { return }
        documentJSON = dict
        documentDirty = savedSnapshot != data
        if let g = DocumentGraph.parse(dict) { documentGraph = g; featureNodes = g.featureRoots() }
        markReeval(.rebuild)
        afterHistoryStep()
    }

    /// Re-evaluate the edited doc and return fresh part meshes for an in-place
    /// swap (no entity rebuild → smooth scrubbing). Keeps display scale/center
    /// fixed so the part doesn't jump mid-edit; updates the live readouts +
    /// pick bounds. Returns nil if the edit produced no geometry.
    func reevalDocumentMeshes() -> [MeshResource]? {
        guard documentJSON != nil else { return nil }
        let plan = documentInstancingPlan()
        let start = Date()
        guard let parts = tessellateDocumentParts(plan: plan, dir: []) else {
            // The kernel refused the document. This used to return nil and
            // leave the app showing an empty viewport with a fully populated
            // feature tree — the Swift-side DAG parser is lenient, the kernel is
            // not, so a schema-stale file looked like a renderer bug. Say so.
            loadError = SimError.pending() ?? "The kernel could not evaluate this document"
            return nil
        }
        loadError = nil
        var meshes: [MeshResource] = []
        var instancing: [PatternInstancing?] = []
        var picks: [PickMesh] = []
        var edges: [[SIMD3<Float>]] = []
        var lo = SIMD3<Float>(repeating: .greatestFiniteMagnitude)
        var hi = SIMD3<Float>(repeating: -.greatestFiniteMagnitude)
        var tris = 0
        for (i, part) in parts.enumerated() {
            guard let part else { continue }
            let km = part.mesh, segs = part.edges
            meshes.append(part.resource)
            if let inst = plan[i] {
                let agg = Self.aggregateInstances(km, edgeSegments: segs,
                                                  transforms: inst.transforms)
                lo = simd_min(lo, agg.lo); hi = simd_max(hi, agg.hi)
                tris += km.triangleCount * inst.transforms.count
                instancing.append(inst)
                picks.append(agg.pick)
                edges.append(agg.edges)
            } else {
                lo = simd_min(lo, km.minBound); hi = simd_max(hi, km.maxBound)
                tris += km.triangleCount
                instancing.append(nil)
                picks.append(PickMesh(positions: km.positions, indices: km.indices,
                                      lo: km.minBound, hi: km.maxBound))
                edges.append(segs)
            }
        }
        guard !meshes.isEmpty else { return nil }
        docPartMeshes = picks
        sceneGeneration &+= 1
        docPartEdges = edges
        docPartInstancing = instancing
        solveMillis = Date().timeIntervalSince(start) * 1000
        triangleCount = tris
        partCount = meshes.count
        sizeMM = hi - lo
        return meshes
    }

    /// The open document's incremental-evaluation session (`VcadEvalSession`):
    /// kernel evaluations of this document reuse every solid an edit didn't
    /// touch. Replaced when another document opens.
    @ObservationIgnored private(set) var evalSession = EvalSession()

    /// One visible root's tessellation.
    struct TessellatedPart {
        let mesh: KernelMesh
        let edges: [SIMD3<Float>]
        let resource: MeshResource
        /// Root-cache key (what a mesh bundle is written from).
        let rootKey: String?
    }

    nonisolated private static func rootKey(_ scene: OpaquePointer, _ i: Int) -> String? {
        guard let c = vcad_scene_root_key(scene, i) else { return nil }
        defer { vcad_cam_free(c) }
        return String(cString: c)
    }

    /// Tessellate the visible roots of the live document, index-aligned with
    /// `documentGraph.visibleRoots` (nil = empty mesh). Parts still in
    /// `partMeshCache` are reused; the kernel only evaluates the rest. Nil
    /// when the kernel refuses the document.
    private func tessellateDocumentParts(plan: [Int: PatternInstancing], dir: [UInt8]) -> [TessellatedPart?]? {
        guard var json = documentJSON, let g = documentGraph else { return nil }
        let partIds: [String?] = engine == nil ? g.visibleRoots.map { _ in nil }
            : g.visibleRoots.map { engineState?.partId(forRootNode: $0.nodeId) }
        let missing = partIds.indices.filter { partIds[$0].map { partMeshCache[$0] == nil } ?? true }
        var out: [TessellatedPart?] = partIds.map { $0.flatMap { partMeshCache[$0] } }
        guard !missing.isEmpty else { return out }
        // Evaluate only the missing roots (pattern roots re-pointed at their
        // seed, as `evalData` does).
        let allRoots = json["roots"] as? [[String: Any]] ?? []
        let visible = allRoots.filter { ($0["visible"] as? Bool) ?? true }
        guard visible.count == partIds.count else { return nil }
        json["roots"] = missing.map { i -> [String: Any] in
            var r = visible[i]
            if let inst = plan[i] { r["root"] = inst.seedNodeId }
            return r
        }
        guard let data = DocEdit.serialize(json),
              let scene = evalSession.scene(data: data, dir: dir) else { return nil }
        defer { vcad_scene_free(scene) }
        let count = vcad_scene_part_count(scene)
        for (k, i) in missing.enumerated() where k < count {
            let km = KernelMesh.fromView(vcad_scene_part_mesh(scene, k))
            if km.isEmpty { out[i] = nil; continue }
            var segs: [SIMD3<Float>] = []
            if let eh = vcad_scene_part_edges(scene, k, Self.edgeAngleDeg) {
                segs = EdgeOverlay.segments(fromView: vcad_edges_view(eh))
                vcad_edges_free(eh)
            }
            let part = TessellatedPart(mesh: km, edges: segs, resource: km.resource(name: "part" + String(i)),
                                       rootKey: Self.rootKey(scene, k))
            out[i] = part
            if let id = partIds[i] { partMeshCache[id] = part }
        }
        return out
    }

    // MARK: export
    // No export FFI today, so STL is written here from the current part meshes
    // (binary STL = just triangles). Works for the sandbox + any loaded/edited doc.

    /// Kernel meshes for whatever is on screen right now (sandbox primitive or
    /// the evaluated document), for export.
    private func currentKernelMeshes() -> [KernelMesh] {
        switch source {
        case .sandbox:
            return sandboxKernelMesh().map { [$0] } ?? []
        case .document, .generated:
            let data: Data?
            if let json = documentJSON { data = DocEdit.serialize(json) }
            else if case let .document(path, _) = source { data = Self.documentBytes(path: path) }
            else if case let .generated(loon, _) = source { data = Data(loon.utf8) }
            else { data = nil }
            guard let data else { return [] }
            let isLoon = { if case .generated = source { return true } else { return false } }()
            let scene: OpaquePointer? = data.withUnsafeBytes { raw in
                let base = raw.bindMemory(to: UInt8.self).baseAddress
                return isLoon ? vcad_scene_from_loon(base, data.count) : vcad_scene_from_json(base, data.count)
            }
            guard let scene else { return [] }
            defer { vcad_scene_free(scene) }
            var out: [KernelMesh] = []
            for i in 0..<vcad_scene_part_count(scene) {
                let km = KernelMesh.fromView(vcad_scene_part_mesh(scene, i))
                if !km.isEmpty { out.append(km) }
            }
            return out
        case .gripper:
            return []
        }
    }

    @discardableResult
    func exportSTL(to url: URL) -> Bool {
        let meshes = currentKernelMeshes()
        guard !meshes.isEmpty else { return false }
        var triCount = 0
        for m in meshes { triCount += m.triangleCount }

        var data = Data()
        data.append(Data(count: 80))                    // header
        var n = UInt32(triCount).littleEndian
        withUnsafeBytes(of: &n) { data.append(contentsOf: $0) }

        func put(_ f: Float) { var v = f.bitPattern.littleEndian; withUnsafeBytes(of: &v) { data.append(contentsOf: $0) } }
        for m in meshes {
            var i = 0
            while i + 2 < m.indices.count {
                let a = m.positions[Int(m.indices[i])]
                let b = m.positions[Int(m.indices[i + 1])]
                let c = m.positions[Int(m.indices[i + 2])]
                let nrm = normalize(cross(b - a, c - a))
                put(nrm.x.isFinite ? nrm.x : 0); put(nrm.y.isFinite ? nrm.y : 0); put(nrm.z.isFinite ? nrm.z : 1)
                put(a.x); put(a.y); put(a.z)
                put(b.x); put(b.y); put(b.z)
                put(c.x); put(c.y); put(c.z)
                data.append(contentsOf: [0, 0])         // attribute byte count
                i += 3
            }
        }
        return (try? data.write(to: url)) != nil
    }

    /// Export the current scene as USDZ — one prim per part, mirroring the
    /// viewport's resolved materials, at physical (millimeter) scale.
    @discardableResult
    func exportUSDZ(to url: URL) -> Bool {
        let meshes = currentKernelMeshes()
        guard !meshes.isEmpty else { return false }
        let parts = meshes.enumerated().map { i, km -> UsdzExport.Part in
            if usesDocumentTree {
                let m = resolvedMaterial(forPart: i)
                let name = i < featureNodes.count ? featureNodes[i].name : "part\(i)"
                return UsdzExport.Part(mesh: km, name: sanitizePrimName(name),
                                       color: m.color, metallic: m.metallic, roughness: m.roughness)
            }
            let color = source.isSandbox ? Self.heroColor : Self.partColors[i % Self.partColors.count]
            return UsdzExport.Part(mesh: km, name: "part\(i)", color: color)
        }
        return UsdzExport.write(parts: parts, to: url)
    }

    /// USD prim names must be identifiers — replace anything else with `_`.
    private func sanitizePrimName(_ s: String) -> String {
        let cleaned = String(s.map { $0.isLetter || $0.isNumber || $0 == "_" ? $0 : "_" })
        let first = cleaned.first
        return (first?.isNumber == true || cleaned.isEmpty) ? "_" + cleaned : cleaned
    }

    // Hover affordance for the draggable handles (cursor + a subtle scale pop).
    // Scene raycasts against the handles' colliders hit-test the pointer;
    // `hoveredHandle` drives the highlight + cursor.
    var hoveredHandle: String?

    /// Frame the geometry at a clean 3/4 view. Parts auto-fit to a constant
    /// display size, so fixed camera params frame any part well.
    func resetCamera(animated: Bool = false) {
        stopSpin()
        let d = Self.frameAllDistance(size: sizeMM, aspect: viewportAspect)
        let apply = {
            self.azimuth = .pi / 5
            self.elevation = .pi / 7
            self.distance = d
            self.panOffset = .zero
        }
        if animated { withAnimation(Motion.smooth) { apply() } } else { apply() }
        pinchBaseline = d
    }

    /// Width / height of the 3D viewport, kept current by the viewport.
    @ObservationIgnored var viewportAspect: Float = 1.6

    /// Frame All / Home: fit the model's bounds in the view (Fusion), not a
    /// fixed distance — a 240 mm-tall assembly sat in a quarter of the
    /// viewport (live QA 2026-11-11). No model yet → the old fixed 1.5.
    static func frameAllDistance(size: SIMD3<Float>, aspect: Float) -> Float {
        guard size.max() > 0 else { return 1.5 }
        return fittedDistance(size: size, azimuth: .pi / 5, elevation: .pi / 7, aspect: aspect)
    }

    /// Turn the camera to a standard view, animated.
    func animateCamera(to preset: CameraPreset) {
        var p = cameraPose
        p.azimuth = preset.azimuth
        p.elevation = preset.elevation
        NavigationUI.shared.glide(self, to: p)
    }

    #if os(macOS)
    /// Switch the ray-traced still on or off (the View menu's toggle).
    func setRaytrace(_ on: Bool) {
        raytraceEnabled = on
        if !on { raytraceImage = nil }
    }
    #endif

    /// Pan the look-at target in the camera's screen plane (⇧-drag).
    func panBy(dx: Float, dy: Float) {
        cameraUserMoved = true
        let forward = normalize(-orbitVector)
        let right = normalize(cross(forward, SIMD3<Float>(0, 1, 0)))
        let up = cross(right, forward)
        panOffset += (-dx * right + dy * up) * (distance * 0.0016)
    }

    // MARK: inertial orbit — flick to spin, momentum decays to rest
    nonisolated(unsafe) private var spinTimer: Timer?
    @ObservationIgnored private var azVel: Float = 0      // rad/s
    @ObservationIgnored private var elVel: Float = 0
    @ObservationIgnored private var lastOrbitTime: Date?

    /// A drag started — kill any running momentum and reset velocity tracking.
    func beginOrbit() {
        stopSpin()
        orbitPivot = orbitPivotProvider?()
        cameraUserMoved = true
        lastOrbitTime = Date()
        azVel = 0; elVel = 0
    }

    /// Incremental orbit from a drag delta; tracks angular velocity (low-pass
    /// filtered) so the flick at release carries momentum.
    func orbitDrag(dx: Float, dy: Float) {
        let now = Date()
        let dt = Float(now.timeIntervalSince(lastOrbitTime ?? now))
        lastOrbitTime = now
        let daz = -dx * 0.01
        let before = elevation
        let o0 = orbitVector
        azimuth += daz
        elevation = max(-1.45, min(1.45, elevation + dy * 0.01))
        let del = elevation - before
        swingAboutPivot(from: o0)
        if dt > 1e-4, dt < 0.1 {
            azVel = 0.6 * azVel + 0.4 * (daz / dt)
            elVel = 0.6 * elVel + 0.4 * (del / dt)
        }
    }

    /// Fusion-style orbit about the selection: the view's selection centre in
    /// world space (part bbox centre, face centroid, edge midpoint), asked
    /// for at the start of each orbit. Nil → orbit the current target.
    @ObservationIgnored var orbitPivotProvider: (() -> SIMD3<Float>?)?
    /// The pivot of the orbit in progress (world space), if any.
    @ObservationIgnored private(set) var orbitPivot: SIMD3<Float>?

    /// After the orbit angles changed from `o0`, move the target so the camera
    /// swung about `orbitPivot` rather than about the target.
    private func swingAboutPivot(from o0: SIMD3<Float>) {
        guard let p = orbitPivot else { return }
        panOffset = SelectionMath.panAfterOrbit(pan: panOffset, from: o0, to: orbitVector, pivot: p)
    }

    /// Release — coast if the flick was fast enough.
    func endOrbit() {
        if abs(azVel) > 0.2 || abs(elVel) > 0.2 { startSpin() }
    }

    private func startSpin() {
        stopSpin()
        let dt: Float = 1.0 / 120.0
        spinTimer = Timer.scheduledTimer(withTimeInterval: TimeInterval(dt), repeats: true) { [weak self] _ in
            MainActor.assumeIsolated {
                guard let self else { return }
                let o0 = self.orbitVector
                self.azimuth += self.azVel * dt
                let ne = max(-1.45, min(1.45, self.elevation + self.elVel * dt))
                if ne != self.elevation { self.elevation = ne } else { self.elVel = 0 }
                self.swingAboutPivot(from: o0)
                let decay: Float = 0.94
                self.azVel *= decay; self.elVel *= decay
                if abs(self.azVel) < 0.05, abs(self.elVel) < 0.05 { self.stopSpin() }
            }
        }
    }

    /// Stop coasting (also called when the user grabs / zooms / taps).
    func stopSpin() { spinTimer?.invalidate(); spinTimer = nil }

    // Escape → deselect (documents only; never while renaming, so the rename
    // field's own Esc-cancel still works). Returns the event so other handlers
    // still see it.
    nonisolated(unsafe) private var keyMonitor: Any?
    #if os(macOS)
    func installKeyMonitor() {
        guard keyMonitor == nil else { return }
        keyMonitor = NSEvent.addLocalMonitorForEvents(matching: .keyDown) { [weak self] event in
            MainActor.assumeIsolated {
                guard let self else { return }
                if event.keyCode == 53 {                       // Escape
                    if self.sketching {
                        // Escape backs out one step: the stroke, then the tool. It never
                        // discards drawn geometry; the toolbar's X does that.
                        if let k = self.sketch, k.hasInProgress { k.cancelPending() }
                        else if let k = self.sketch, k.tool != .select { self.setSketchTool(.select) }
                        else if self.sketch?.isEmpty ?? true { self.exitSketch() }
                    } else if GlassUI.shared.shortcutsShown {
                        GlassUI.shared.shortcutsShown = false
                    } else if self.activeFeatureOp != nil {
                        // A focused number field swallows the panel's own Esc shortcut.
                        self.activeFeatureOp = nil
                    } else if self.showsCommandPalette || GlassUI.shared.panel != nil {
                        // Their own Cancel handles Esc; keep the selection.
                    } else if self.usesDocumentTree, self.renamingFeatureID == nil, self.hasSelection {
                        self.deselectAll()
                    }
                }
                // The ray-traced still lives in View ▸ Ray-Traced Preview
                // (⌥⌘R) — a bare letter must never be a global shortcut.
            }
            return event
        }
    }

    #endif

    // Two-finger / wheel scroll → zoom. Installed once when the viewport appears.
    nonisolated(unsafe) private var scrollMonitor: Any?
    #if os(macOS)
    func installScrollZoom() {
        guard scrollMonitor == nil else { return }
        scrollMonitor = NSEvent.addLocalMonitorForEvents(matching: .scrollWheel) { [weak self] event in
            MainActor.assumeIsolated {
                guard let self, !self.draggingHandle else { return }
                self.stopSpin()                       // zooming interrupts coasting
                self.cameraUserMoved = true
                let dy = Float(event.scrollingDeltaY)
                let k = Float(event.hasPreciseScrollingDeltas ? 0.004 : 0.04)
                self.distance = max(0.45, min(8.0, self.distance * (1 - dy * k)))
                self.pinchBaseline = self.distance
            }
            return event
        }
    }
    #endif

    // MARK: cross-domain slice — the gripper
    // One `connector_x` couples the enclosure cutout (mechanical) + the board
    // connector (electrical); see docs/plans/2026-06-23-gripper-vertical-slice.md.

    nonisolated(unsafe) private var gripperDoc: OpaquePointer?
    var connectorX: Double = 40
    let connectorRange: ClosedRange<Double> = 4...76
    @ObservationIgnored private var connectorBaseline: Double = 40
    @ObservationIgnored private var lastConnectorTick = 40
    @ObservationIgnored private var lastConnectorOK = true

    deinit {
        if let d = gripperDoc { vcad_doc_free(d) }
        if let s = residentAssemblyScene { vcad_scene_free(s) }
        playbackTimer?.invalidate()
        #if os(macOS)
        if let m = scrollMonitor { NSEvent.removeMonitor(m) }
        if let m = keyMonitor { NSEvent.removeMonitor(m) }
        #endif
        spinTimer?.invalidate()
    }

    /// Clearance from the connector cutout to the nearest enclosure side wall —
    /// the REAL geometric min-wall, measured by the kernel (`vcad_doc_min_wall`)
    /// from the resolved `box − cutout`, not arithmetic on the box's literals.
    /// Refreshed live on every cheap per-frame re-solve and on settle; seeded so
    /// the centered open-state reads correctly before the first drag.
    var connectorMinWall: Double = 34
    var connectorOK: Bool { connectorMinWall >= 6 }
    /// Pull the real min-wall from the kernel at the current `connector_x`.
    /// `vcad_doc_min_wall` resolves the resident doc (whose `connector_x` was
    /// just written by the cheap/full re-solve) and measures the box−cutout
    /// geometry — no fold, no routing, safe every drag frame.
    func refreshMinWall() {
        guard let doc = gripperDoc else { return }
        let w = vcad_doc_min_wall(doc)
        if w.isFinite { connectorMinWall = w }
    }
    var showsConnectorHandle: Bool { source.isGripper }
    func connectorHandlePosition() -> SIMD3<Float> { SIMD3(Float(connectorX), 9, 13) }

    func openGripper() {
        connectorX = 40
        lastConnectorTick = 40
        lastConnectorOK = true
        copperStale = false
        copperDirty = false
        copperUnrouted = 0
        // A consistent 3/4 view tilted toward the board top, so all four domains
        // (cutout, board + copper, bracket) read at once instead of edge-on.
        azimuth = .pi / 5
        elevation = 0.62
        distance = 1.45
        panOffset = .zero
        source = .gripper
        // Run the full solve once after the first rebuild so the Receipt's
        // settle-fields (quote, lead time, bracket DFM) start populated instead
        // of showing $0.00 until the first drag.
        copperDirty = true
    }

    func beginConnectorDrag() { connectorBaseline = connectorX }
    var connectorDragBaseline: Double { connectorBaseline }

    /// Drag handler: clamp, flag a re-solve, and fire the felt detents — a tick
    /// per millimetre, a firm "wall" the instant min-wall is violated.
    func setConnectorX(_ x: Double) {
        let clamped = min(max(x, connectorRange.lowerBound), connectorRange.upperBound)
        connectorX = clamped
        parameterDirty = true
        // The shown copper + the expensive Receipt rows now lag the connector —
        // mark them stale ("recomputing"); they re-solve on settle.
        copperStale = true
        receiptStale = true
        let tick = Int(clamped.rounded())
        if tick != lastConnectorTick {
            lastConnectorTick = tick
            hapticDetent()
        }
        let ok = connectorOK
        if ok != lastConnectorOK {
            lastConnectorOK = ok
            if !ok {
                hapticDetent(.levelChange)
                chime.play(.warning)
            } else {
                chime.play(.solved)
            }
        }
    }

    private func ensureGripperDoc() -> OpaquePointer? {
        if gripperDoc == nil { gripperDoc = vcad_doc_gripper_slice1() }
        return gripperDoc
    }

    /// Re-solve the coupled doc at the current `connector_x` and gather both
    /// parts. Re-evaluating sets `connector_x`; bindings move the cutout AND the
    /// connector together — one finger, two domains.
    func gripperScene() -> RenderScene {
        guard let doc = ensureGripperDoc() else { return .empty }
        let start = Date()
        let scene: OpaquePointer? = "connector_x".withCString {
            vcad_doc_set_param(doc, $0, connectorX)
        }
        guard let scene else { return .empty }
        defer { vcad_scene_free(scene) }
        return sceneFromHandle(scene, start: start)
    }

    /// Per-FRAME re-solve: cheap roots only. The mechanical cutout + board
    /// connector follow the finger live (~15 ms); the expensive sheet-metal fold
    /// is skipped here and re-folded once on settle (`gripperScene`). Same
    /// `connector_x` writes through to the resident doc, so the settle solve sees
    /// it — one DAG, staged by cost.
    func gripperSceneCheap() -> RenderScene {
        guard let doc = ensureGripperDoc() else { return .empty }
        let start = Date()
        let scene: OpaquePointer? = "connector_x".withCString {
            vcad_doc_set_param_cheap(doc, $0, connectorX)
        }
        guard let scene else { return .empty }
        defer { vcad_scene_free(scene) }
        // The cheap path wrote connector_x to the resident doc; pull the real
        // min-wall back so the live Receipt row tracks geometry, not arithmetic.
        refreshMinWall()
        return sceneFromHandle(scene, start: start)
    }

    /// The unified cross-domain solve — ONE FFI call returns every domain at the
    /// current `connector_x`: the meshes (enclosure + board + sheet-metal fold),
    /// the routed copper, and the receipt scalars, all descending from the same
    /// resolved parameter. The SETTLE path: the kernel fans connector_x out, not
    /// this view layer.
    struct GripperSolve {
        var meshes: [MeshResource]
        var copper: [CopperSeg]
        var minWall: Double
        var unrouted: Int
    }

    func gripperSolve() -> GripperSolve? {
        guard source.isGripper, let doc = ensureGripperDoc() else { return nil }
        let start = Date()
        let s: OpaquePointer? = "connector_x".withCString { vcad_doc_solve(doc, $0, connectorX) }
        guard let s else { return nil }
        defer { vcad_solve_free(s) }

        var meshes: [MeshResource] = []
        let pc = Int(vcad_solve_part_count(s))
        for i in 0..<pc {
            let km = KernelMesh.fromView(vcad_solve_part_mesh(s, i))
            meshes.append(km.isEmpty ? .generateBox(size: 0.001) : km.resource(name: "part\(i)"))
        }

        var copper: [CopperSeg] = []
        let ox: Float = 5, oy: Float = 5, z: Float = 7.1
        let tc = Int(vcad_solve_trace_count(s))
        for i in 0..<tc {
            let t = vcad_solve_trace(s, i)
            copper.append(CopperSeg(
                a: SIMD3<Float>(Float(t.start.0) + ox, Float(t.start.1) + oy, z),
                b: SIMD3<Float>(Float(t.end.0) + ox, Float(t.end.1) + oy, z),
                width: Float(t.width), net: t.net_id))
        }
        copperUnrouted = Int(vcad_solve_unrouted(s))
        // Settle: refresh the Receipt's expensive verdicts (these came from the
        // same one solve as the meshes + copper).
        bracketOK = vcad_solve_bracket_ok(s) == 1
        bracketSeverity = Int(vcad_solve_bracket_severity(s))
        quoteCents = vcad_solve_quote_cost_cents(s)
        quoteEnclosureCents = vcad_solve_quote_enclosure_cents(s)
        quoteBoardCents = vcad_solve_quote_board_cents(s)
        quoteBracketCents = vcad_solve_quote_bracket_cents(s)
        quoteHasEstimate = vcad_solve_quote_has_estimate(s) == 1
        leadDays = Int(vcad_solve_lead_days(s))
        // Authoritative min-wall from the full solve (same value the cheap path
        // approximates per-frame). Keeps the settled Receipt row exact.
        let solvedWall = vcad_solve_min_wall(s)
        if solvedWall.isFinite { connectorMinWall = solvedWall }
        let held = vcad_solve_all_held(s) == 1
        receiptStale = false
        // Chime on the gate transition (the felt "verified" / "violated" moment).
        if held != allHeld { chime.play(held ? .solved : .failed) }
        allHeld = held
        solveMillis = Date().timeIntervalSince(start) * 1000
        return GripperSolve(meshes: meshes, copper: copper,
                            minWall: vcad_solve_min_wall(s), unrouted: copperUnrouted)
    }

    // MARK: copper — slice 2, the electrical domain of the Connector Drag

    /// One routed copper segment, mapped into the board plate's frame.
    struct CopperSeg { var a: SIMD3<Float>; var b: SIMD3<Float>; var width: Float; var net: UInt32 }

    /// Copper is showing a pre-drag route — drawn dim until it re-routes on settle.
    var copperStale = false
    /// Set on drag-settle to request exactly ONE re-route (never per drag frame).
    var copperDirty = false
    /// Nets that failed to route at the current connector_x (0 = fully routed).
    /// Observable (NOT @ObservationIgnored) so the Receipt's copper row refreshes.
    var copperUnrouted = 0

    // Receipt verdicts — the expensive ones settle on drag-release; min-wall is
    // recomputed live from connectorX. `receiptStale` marks the expensive rows
    // "recomputing" mid-drag (mirrors copperStale).
    var receiptStale = false
    var bracketOK = true
    var bracketSeverity = 0
    /// Total quote (cents) = enclosure + board + bracket. Per-domain breakdown
    /// below so the Receipt can show three labeled line items with an "est."
    /// tag on the board (the one labeled estimate; the other two are kernel-real).
    var quoteCents: UInt64 = 0
    var quoteEnclosureCents: UInt64 = 0
    var quoteBoardCents: UInt64 = 0
    var quoteBracketCents: UInt64 = 0
    var quoteHasEstimate = false
    var leadDays = 0
    var allHeld = true

    /// Route the slice-2 board at the current `connector_x` and map the copper
    /// into the board plate's frame. ONE `route_all` per call — heeding the tween
    /// lesson, this runs on settle, never per drag frame.
    func routeGripperCopper() -> [CopperSeg] {
        guard source.isGripper, let r = vcad_route_traces(connectorX, 0.25) else {
            copperUnrouted = 0
            return []
        }
        defer { vcad_route_result_free(r) }
        copperUnrouted = Int(vcad_route_result_unrouted_count(r))
        let n = Int(vcad_route_result_trace_count(r))
        // Board-local mm → kernel frame: the board plate is translated (5,5,5) and
        // is 2 mm thick, so its top face is z=7; copper sits a hair above it.
        let ox: Float = 5, oy: Float = 5, z: Float = 7.1
        var segs: [CopperSeg] = []
        segs.reserveCapacity(n)
        for i in 0..<n {
            let t = vcad_route_result_trace(r, i)
            let a = SIMD3<Float>(Float(t.start.0) + ox, Float(t.start.1) + oy, z)
            let b = SIMD3<Float>(Float(t.end.0) + ox, Float(t.end.1) + oy, z)
            segs.append(CopperSeg(a: a, b: b, width: Float(t.width), net: t.net_id))
        }
        return segs
    }

    /// Sample documents from the vcad repository, when one can be found: the
    /// `VCAD_EXAMPLES` environment variable, the bundle's `Resources/examples`,
    /// or a checkout above the running binary. Only files that exist are
    /// offered, so the menu is never a list of dead paths on another machine.
    let examples: [(name: String, path: String)] = {
        let candidates: [(String, String)] = [
            ("Pulley", "mecheval/tasks/a6-pulley-01.vcad"),
            ("Counterbore", "mecheval/tasks/a4-counterbore-plate-01.vcad"),
            ("Ribbed plate", "mecheval/tasks/a5-ribbed-plate-01.vcad"),
            ("Robot arm", "examples/robot-arm-2dof.vcad"),
            // The flagship simulation sample: a 22-DOF floating-base humanoid
            // with its meshes vendored under third_party/booster-k1.
            ("Booster K1", "examples/k1-floating.vcad"),
            // A floating-base robot with something to simulate; primitives
            // only, so its geometry resolves with no vendored meshes.
            ("Floating arm", "examples/floating-arm.vcad"),
            ("Sensor mast", "examples/sensor-mast.vcad"),
        ]
        var roots: [URL] = []
        if let env = ProcessInfo.processInfo.environment["VCAD_EXAMPLES"], !env.isEmpty {
            roots.append(URL(fileURLWithPath: env))
        }
        roots.append(Bundle.main.resourceURL ?? Bundle.main.bundleURL)
        var dir = Bundle.main.bundleURL
        for _ in 0..<8 {
            dir = dir.deletingLastPathComponent()
            roots.append(dir)
        }
        let fm = FileManager.default
        guard let root = roots.first(where: { fm.fileExists(atPath: $0.appendingPathComponent("examples").path) })
        else { return [] }
        return candidates.compactMap { name, rel in
            let p = root.appendingPathComponent(rel).path
            return fm.fileExists(atPath: p) ? (name, p) : nil
        }
    }()

    var recents: [URL] = (UserDefaults.standard.array(forKey: "vcad.recents") as? [String] ?? [])
        .map { URL(fileURLWithPath: $0) }

    func clearRecents() {
        recents = []
        UserDefaults.standard.removeObject(forKey: "vcad.recents")
        #if os(macOS)
        NSDocumentController.shared.clearRecentDocuments(nil)
        #endif
    }

    var documentName: String { source.label }

    func newDocument() { source = .sandbox }

    /// True when this instance holds nothing worth keeping on screen: a fresh
    /// sandbox nobody has touched. Opening a document into one of these reuses
    /// the instance, the way a Mac app reuses its empty Untitled window; opening
    /// into anything else would throw away work, so that gets a new instance.
    var isDisposableScratch: Bool {
        source.isSandbox && !canUndo && !documentDirty
    }

    func openDocument(_ url: URL) {
        LaunchPerf.once("open", " (\(url.lastPathComponent))")
        let label = url.deletingPathExtension().lastPathComponent
        // A DXF is an outline to machine, not a document: it lands in
        // Manufacture as contour operations on the current part.
        if url.pathExtension.lowercased() == "dxf" {
            guard Prefs.labs else {
                loadError = "Opening DXF outlines needs the Manufacture workspace (Settings ▸ Labs)."
                return
            }
            workspace = .manufacture
            do {
                try cnc.importOutline(try CNCOutline.parseDXF(String(contentsOf: url, encoding: .utf8), name: url.lastPathComponent))
            } catch { cnc.error = error.localizedDescription }
            return
        }
        // A loon program opens the way the intent bar's output does: compiled
        // by the kernel, shown as generated geometry. It has no node DAG to
        // edit, so it is not a `.document`.
        //
        // A loon FILE opens as a document: the kernel compiles it to `.vcad`
        // JSON with its `[use]` modules and relative mesh imports resolved
        // against its own directory, so a generated assembly gets the full
        // document path — progressive solve, materials, joints, posing — and
        // is watched for regeneration. It is read-only: Save goes to Save As.
        //
        // A folder opens its entry program (`main.loon`, `<folder>.loon`, or
        // its only `.loon`). A program that doesn't compile still opens: the
        // error shows inline, and the next save that fixes it rebuilds.
        var isDir: ObjCBool = false
        if FileManager.default.fileExists(atPath: url.path, isDirectory: &isDir), isDir.boolValue {
            guard let entry = LoonCompiler.entryFile(inFolder: url) else {
                loadError = "\(url.lastPathComponent) has no main.loon (or single .loon file) to open"
                return
            }
            openDocument(entry)
            return
        }
        if url.pathExtension.lowercased() == "loon" {
            beginFirstSolve(url)
            source = .document(path: url.path, label: label)
            rememberRecent(url)
            return
        }
        // STEP in the live window: imported on the kernel's thread with a
        // progress card (Cancel returns to whatever was open before).
        if evaluatesOffMainThread, ["step", "stp"].contains(url.pathExtension.lowercased()) {
            beginStepImport(url)
            return
        }
        // STEP / URDF: convert through the kernel into a .vcad beside the
        // app's import cache, then open that as an ordinary document.
        if ["step", "stp", "urdf"].contains(url.pathExtension.lowercased()) {
            guard let converted = importForeign(url) else { return }
            beginFirstSolve(url)
            source = .document(path: converted.path, label: label)
            rememberRecent(url)
            return
        }
        importMeshBundle(for: url)          // instant open when the file shipped with its meshes
        beginFirstSolve(url)
        source = .document(path: url.path, label: label)
        rememberRecent(url)
    }

    // MARK: open progress (LoadProgress.swift)

    /// Show the open-progress card for `url`'s first solve (live window only;
    /// a re-solve of the open document never gets one).
    private func beginFirstSolve(_ url: URL) {
        guard evaluatesOffMainThread else { return }
        if loading == nil || loading?.url != url {
            stopStepImport()
            loadingPrevious = source
            loading = LoadProgress(url: url, phase: .evaluating(done: 0, total: 0))
        } else {
            loading?.phase = .evaluating(done: 0, total: 0)
        }
    }

    /// Start a cancellable STEP import; the document switches when it lands.
    func beginStepImport(_ url: URL) {
        stopStepImport()
        if loading?.isEvaluating == true { abandonEvaluation() }
        let job = url.path.withCString { vcad_import_step_start($0) }
        guard let job else {
            loadError = "Couldn't import \(url.lastPathComponent): \(SimError.pending() ?? "unknown error")"
            loading = LoadProgress(url: url, phase: LoadProgress.failure(loadError, fallback: "Import failed"))
            return
        }
        if loading == nil || loading?.isFailed == true || loading?.url != url { loadingPrevious = source }
        loading = LoadProgress(url: url)
        stepJob = job
        geometryDirty = true             // the stage drops the previous model
        let label = url.deletingPathExtension().lastPathComponent
        stepPoll = Task { @MainActor [weak self] in
            while !Task.isCancelled {
                guard let self, self.stepJob == job else { return }
                var stage: UInt32 = 0, done = 0, total = 0
                let finished = vcad_import_step_poll(job, &stage, &done, &total)
                let phase = LoadProgress.importPhase(stage: stage, done: done, total: total)
                if !finished, self.loading?.phase != phase, self.loading?.isImporting == true {
                    self.loading?.phase = phase
                }
                if finished {
                    self.stepJob = nil
                    let raw = vcad_import_step_finish(job)     // joins the thread
                    guard let raw else {
                        let err = SimError.pending()
                        if err?.contains("cancelled") == true { return }
                        self.loading?.phase = LoadProgress.failure(err.map { "Couldn't import \(url.lastPathComponent): \($0)" },
                                                                   fallback: "Couldn't import \(url.lastPathComponent)")
                        self.geometryDirty = true
                        return
                    }
                    let text = String(cString: raw)
                    vcad_string_free(raw)
                    guard let written = self.writeImportedStep(text, from: url) else {
                        self.loading?.phase = LoadProgress.failure(self.loadError, fallback: "Couldn't write the imported document")
                        self.geometryDirty = true
                        return
                    }
                    self.pendingLoadWarning = written.warning
                    self.loading?.phase = .evaluating(done: 0, total: 0)
                    self.source = .document(path: written.url.path, label: label)
                    self.rememberRecent(url)
                    return
                }
                try? await Task.sleep(for: .milliseconds(80))
            }
        }
    }

    /// Cancel the in-flight import or first solve and go back to what was
    /// open before (or the empty sandbox).
    func cancelLoading() {
        guard let load = loading, load.isRunning else { return }
        let previous = loadingPrevious ?? .sandbox
        loading = nil
        pendingLoadWarning = nil
        loadingPrevious = nil
        if load.isImporting {
            stopStepImport()
            geometryDirty = true         // redraw the document we never left
        } else {
            abandonEvaluation()
            if source != previous { source = previous } else { geometryDirty = true }
        }
    }

    /// Run the failed open again.
    func retryLoading() {
        guard let url = loading?.url else { return }
        let previous = loadingPrevious
        loading = nil
        loadError = nil
        openDocument(url)
        if loadingPrevious == nil || loadingPrevious == source { loadingPrevious = previous }
    }

    /// Close the failure / warning card. A failed import leaves the previous
    /// document as it was; a failed solve leaves the new document open.
    func dismissLoading() {
        loading = nil
        loadingPrevious = nil
        pendingLoadWarning = nil
        geometryDirty = true
    }

    /// Cancel the STEP job (if any) and join its thread off the main actor.
    private func stopStepImport() {
        stepPoll?.cancel(); stepPoll = nil
        guard let job = stepJob else { return }
        stepJob = nil
        vcad_import_step_cancel(job)
        let token = UInt(bitPattern: job)
        Task.detached(priority: .utility) {
            guard let j = OpaquePointer(bitPattern: token) else { return }
            if let raw = vcad_import_step_finish(j) { vcad_string_free(raw) }
        }
    }

    /// Drop the in-flight background evaluation (the kernel thread finishes
    /// on its own and its result is discarded).
    private func abandonEvaluation() {
        evalPoll?.cancel(); evalPoll = nil
        if let job = evalJob { vcad_eval_abandon(job); evalJob = nil }
        evaluating = nil
        solving = false
        solveProgress = (0, 0)
        partialMeshes = []; partialBounds = nil
    }

    /// The first solve landed (or failed): fold the card away, or keep it up
    /// with what went wrong.
    private func finishFirstSolve(error: String?) {
        guard loading?.isEvaluating == true else { return }
        if let error {
            loading?.phase = LoadProgress.failure(error, fallback: "Couldn't open")
        } else if let w = pendingLoadWarning {
            loading?.phase = w
        } else {
            loading = nil
            loadingPrevious = nil
        }
        pendingLoadWarning = nil
    }

    /// Convert a STEP or URDF file to a .vcad document in
    /// ~/Library/Caches/vcad/imports and return its URL (nil + loadError on
    /// failure). The user saves it wherever they like with Save As.
    func importForeign(_ url: URL) -> URL? {
        let ext = url.pathExtension.lowercased()
        let raw = url.path.withCString { ext == "urdf" ? vcad_import_urdf($0) : vcad_import_step($0) }
        guard let raw else {
            loadError = "Couldn't import \(url.lastPathComponent): \(SimError.pending() ?? "unknown error")"
            return nil
        }
        defer { vcad_string_free(raw) }
        let text = String(cString: raw)
        if ext == "urdf" { return writeImported(Data(text.utf8), from: url) }
        return writeImportedStep(text, from: url)?.url
    }

    /// Unwrap a STEP import result (`{"document", "solids", "skipped_faces",
    /// "warnings"?}`), write the document, and say whether faces were dropped.
    func writeImportedStep(_ text: String, from url: URL) -> (url: URL, warning: LoadProgress.Phase?)? {
        var data = Data(text.utf8)
        var warning: LoadProgress.Phase?
        if let obj = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let doc = obj["document"],
           let d = try? JSONSerialization.data(withJSONObject: doc) {
            data = d
            warning = LoadProgress.importWarning(skippedFaces: obj["skipped_faces"] as? Int ?? 0,
                                                 report: obj["warnings"] as? String)
        }
        guard let out = writeImported(data, from: url) else { return nil }
        return (out, warning)
    }

    private func writeImported(_ data: Data, from url: URL) -> URL? {
        let dir = FileManager.default.urls(for: .cachesDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("vcad/imports", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        let out = dir.appendingPathComponent(url.deletingPathExtension().lastPathComponent + ".vcad")
        do { try data.write(to: out, options: .atomic) } catch {
            loadError = "Couldn't write the imported document: \(error.localizedDescription)"
            return nil
        }
        return out
    }

    // MARK: feature tree edits (FeatureTree.swift)

    /// Move a part to `index` in the tree (engine `reorder`).
    func reorderPart(_ partIndex: Int, to index: Int) {
        guard let id = partId(forPart: partIndex), index >= 0, index < featureNodes.count else { return }
        runCommand(.reorder(id: id, index: index), name: "Reorder")
    }

    /// Parts hidden in the document: saved with the file (IR
    /// `roots[i].visible = false`), still built, just not drawn. Unlike the
    /// view-only Hide, this survives reopening. It is not a true suppress —
    /// the part is still evaluated, and a part another feature consumes has
    /// no scene root to hide — so the menu says "Hide in Document".
    func isSuppressed(part i: Int) -> Bool {
        guard let roots = documentJSON?["roots"] as? [[String: Any]], i < roots.count else { return false }
        return (roots[i]["visible"] as? Bool) == false
    }
    func toggleSuppressed(part i: Int) {
        guard let id = partId(forPart: i) else { return }
        let suppressed = isSuppressed(part: i)
        runCommand(.setVisible(id: id, visible: suppressed), name: suppressed ? "Show in Document" : "Hide in Document")
    }

    /// Parameter table: add / change / remove named parameters. `value` is a
    /// number or a formula string (IR `Expr`).
    func setDocParameter(_ name: String, value: Any, unit: String? = "mm") {
        let trimmed = name.trimmingCharacters(in: .whitespaces)
        guard !trimmed.isEmpty else { return }
        irEdit("Set \(trimmed)", reeval: .rebuild) { json in
            var params = json["parameters"] as? [String: Any] ?? [:]
            var p = params[trimmed] as? [String: Any] ?? [:]
            p["value"] = value
            if let unit { p["unit"] = unit }
            params[trimmed] = p
            json["parameters"] = params
        }
    }
    func removeDocParameter(_ name: String) {
        irEdit("Remove \(name)", reeval: .rebuild) { json in
            var params = json["parameters"] as? [String: Any] ?? [:]
            params[name] = nil
            json["parameters"] = params
        }
    }

    /// Add `url` to the recents list.
    ///
    /// Re-reads the stored list first rather than editing this instance's copy:
    /// several instances run at once now, and each one holds the list as it
    /// looked when IT launched. Writing that stale copy back would drop every
    /// document the other instances opened in the meantime. Concurrent writes
    /// are still last-writer-wins — this narrows the window to the write itself
    /// instead of the whole lifetime of the process.
    private func rememberRecent(_ url: URL) {
        let stored = (UserDefaults.standard.array(forKey: "vcad.recents") as? [String] ?? [])
            .map { URL(fileURLWithPath: $0) }
        var merged = stored
        merged.removeAll { $0 == url }
        merged.insert(url, at: 0)
        if merged.count > 8 { merged = Array(merged.prefix(8)) }
        recents = merged
        UserDefaults.standard.set(merged.map { $0.path }, forKey: "vcad.recents")
        #if os(macOS)
        // The system list too: the Dock menu, Apple ▸ Recent Items, and the
        // Open panel's sidebar all read from it.
        NSDocumentController.shared.noteNewRecentDocumentURL(url)
        #endif
    }

    /// Pick up recents opened by other instances since this one launched.
    func refreshRecents() {
        recents = (UserDefaults.standard.array(forKey: "vcad.recents") as? [String] ?? [])
            .map { URL(fileURLWithPath: $0) }
    }

    // MARK: tool palette

    func tools(for tab: ToolTab) -> [Tool] {
        let docMode = usesDocumentTree
        switch tab {
        case .create:
            var out = BaseShape.allCases.map { shape in
                // Sandbox: pick the primitive. Document: ARM the tool — the
                // palette hands the click to the scene, where it lands where you
                // point (Borland's pick-then-place, which is also how every CAD
                // app places geometry).
                Tool(id: "shape.\(shape.rawValue)",
                     label: docMode ? "Add \(shape.label)" : shape.label, symbol: shape.symbol,
                     isActive: docMode ? armedShape == shape : baseShape == shape,
                     hint: docMode ? "Click in the scene to place it" : "") { [weak self] in
                    guard let self else { return }
                    if docMode { self.arm(shape) }
                    else if shape.sandboxPreview { self.baseShape = shape }
                    else if self.promoteSandboxToDocument(blank: true) { self.arm(shape) }
                }
            }
            if docMode {
                out.append(Tool(id: "sketch", label: "Sketch", symbol: "scribble.variable",
                                isActive: false) { [weak self] in self?.enterSketch() })
                out += FeatureOp.createOps.map { op in
                    Tool(id: "op.\(op.rawValue)", label: op.label, symbol: op.symbol,
                         isActive: false) { [weak self] in self?.activeFeatureOp = op }
                }
            }
            return out
        case .modify:
            if docMode {
                let ok = selectedPartIndex != nil || picking.selectionPart != nil
                return FeatureOp.modifyOps.map { op in
                    Tool(id: "op.\(op.rawValue)", label: op.label, symbol: op.symbol,
                         isActive: activeFeatureOp == op, enabled: ok,
                         hint: ok ? "" : "Select a part first") { [weak self] in self?.activeFeatureOp = op }
                }
            }
            return Modifier.allCases.compactMap { mod in
                if docMode {
                    guard mod != .none else { return nil }     // no "None" tool when authoring
                    let ok = selectedPartIndex != nil
                    return Tool(id: "mod.\(mod.rawValue)", label: mod.label, symbol: mod.symbol,
                                isActive: false, enabled: ok,
                                hint: ok ? "" : "Select a part first") { [weak self] in
                        self?.applyModifierToSelected(mod)
                    }
                }
                // Fillet/chamfer are no-ops on a sphere (no edges) — surface that.
                let ok = mod == .none || baseShape != .sphere
                return Tool(id: "mod.\(mod.rawValue)", label: mod.label, symbol: mod.symbol,
                            isActive: modifier == mod, enabled: ok,
                            hint: ok ? "" : "No edges on a sphere") { [weak self] in self?.modifier = mod }
            }
        case .combine:
            guard docMode else { return [] }
            let ready = multiSelectedParts.count == 2
            return BooleanOp.allCases.map { b in
                Tool(id: "bool.\(b.rawValue)", label: b.label, symbol: b.symbol,
                     isActive: false, enabled: ready,
                     hint: ready ? "" : "⌘-click 2 parts") { [weak self] in self?.combineSelected(b) }
            }
        }
    }

    /// Whether the active modifier actually changes the geometry (a fillet on a
    /// sphere has no edges to round).
    var modifierEffective: Bool { !(baseShape == .sphere && modifier != .none) }

    // MARK: feature tree

    var features: [Feature] {
        switch source {
        case .sandbox:
            return [
                Feature(id: "base", name: baseShape.label, symbol: baseShape.symbol, kind: .base),
                Feature(id: "modifier", name: modifier == .none ? "No modifier" : modifier.label,
                        symbol: modifier.symbol, kind: .modifier),
            ]
        case .document(_, let label), .generated(_, let label):
            let n = max(partCount, 1)
            return (0..<n).map { i in
                Feature(id: "part\(i)", name: n == 1 ? label : "Part \(i + 1)",
                        symbol: "cube.transparent", kind: .part)
            }
        case .gripper:
            return [
                Feature(id: "part0", name: "Enclosure", symbol: "cube", kind: .part),
                Feature(id: "part1", name: "Board", symbol: "cpu", kind: .part),
            ]
        }
    }
    var selectedFeature: Feature? { features.first { $0.id == selectedFeatureID } }

    /// Whether the grabbable 3D handle is shown (only the cube+fillet case has
    /// a parametric anchor today).
    var showsHandle: Bool {
        source.isSandbox && baseShape == .cube && modifier == .fillet && selectedFeatureID == "modifier"
    }

    var panOffset: SIMD3<Float> = .zero
    /// The user has orbited, panned, zoomed or picked a view of this document.
    /// From then on the camera is theirs and an agent's build never moves it.
    @ObservationIgnored var cameraUserMoved = false
    /// An agent changed the geometry and the next finished scene should be
    /// considered for framing.
    @ObservationIgnored var agentFramePending = false
    /// The content size (mm) the camera was last framed on for an agent.
    @ObservationIgnored var agentFramedSize: SIMD3<Float>?
    var orbitVector: SIMD3<Float> {
        let r = distance
        return SIMD3<Float>(
            r * cos(elevation) * sin(azimuth),
            r * sin(elevation),
            r * cos(elevation) * cos(azimuth)
        )
    }
    var cameraPosition: SIMD3<Float> { panOffset + orbitVector }

    func handlePosition(radius: Double) -> SIMD3<Float> {
        let mid = SIMD3<Float>(15, 0, 30)   // top-front edge midpoint of the 30mm cube
        let outward = normalize(SIMD3<Float>(0, -1, 1))
        return mid + outward * (2.5 + Float(radius))
    }

    // MARK: scene building

    func buildScene() -> RenderScene {
        let scene: RenderScene
        switch source {
        case .sandbox: scene = documentJSON == nil ? sandboxScene() : documentScene(path: "")
        case .document(let path, _): scene = documentScene(path: path)
        case .generated(let loon, _): scene = generatedScene(loon: loon)
        case .gripper: scene = gripperScene()
        }
        // The first solve's result is on screen (or the document refused):
        // the open-progress card is done. Deferred: this runs inside a view
        // update, which must not mutate observed state.
        if !scene.solving, loading?.isEvaluating == true {
            let err = scene.meshes.isEmpty ? loadError : nil
            Task { @MainActor [weak self] in self?.finishFirstSolve(error: err) }
        }
        return scene
    }

    /// Validate a generated loon program; if it evaluates to geometry, switch the
    /// active source to it (which re-renders via `generatedScene`). Returns the
    /// evaluated part count, or nil if the program failed — leaving the current
    /// scene untouched so a bad generation never blanks the studio.
    @discardableResult
    func applyGenerated(loon: String, label: String) -> GenStats? {
        let data = Data(loon.utf8)
        guard !data.isEmpty else { return nil }
        let stats: GenStats? = data.withUnsafeBytes { raw -> GenStats? in
            guard let base = raw.bindMemory(to: UInt8.self).baseAddress,
                  let scene = vcad_scene_from_loon(base, data.count) else { return nil }
            defer { vcad_scene_free(scene) }
            let n = vcad_scene_part_count(scene)
            guard n > 0 else { return nil }
            var lo = SIMD3<Float>(repeating: .greatestFiniteMagnitude)
            var hi = SIMD3<Float>(repeating: -.greatestFiniteMagnitude)
            var tris = 0, real = 0
            for i in 0..<n {
                let km = KernelMesh.fromView(vcad_scene_part_mesh(scene, i))
                if km.isEmpty { continue }
                lo = simd_min(lo, km.minBound); hi = simd_max(hi, km.maxBound)
                tris += km.triangleCount; real += 1
            }
            guard real > 0 else { return nil }
            return GenStats(parts: real, size: hi - lo, triangles: tris)
        }
        guard let stats else { return nil }
        // A generation replaces the document; remember what it replaced so
        // ⌘Z brings it back — the intent bar must never be a one-way door.
        generationUndo = (source, documentJSON, engine, engineState, savedHash, undoStack, redoStack, documentDirty)
        source = .generated(loon: loon, label: label)
        return stats
    }

    private func makeBase() -> OpaquePointer? {
        switch baseShape {
        case .cube: return vcad_solid_cube(30, 30, 30)
        case .cylinder: return vcad_solid_cylinder(15, 30, 64)
        case .sphere: return vcad_solid_sphere(15, 48)
        case .cone, .torus, .wedge, .prism: return vcad_solid_cube(30, 30, 30)   // never previewed
        }
    }

    private var modifierApplies: Bool { selectedFeatureID != "base" && modifier != .none && modifierValue > 0.05 }

    /// Build the sandbox solid (primitive + modifier), tessellate it, and hand
    /// the kernel mesh handle to `body`; every FFI handle is freed on return.
    private func withSandboxMesh<T>(_ body: (OpaquePointer) -> T?) -> T? {
        guard let base = makeBase() else { return nil }
        defer { vcad_solid_free(base) }
        var target = base
        var owned: OpaquePointer?
        if modifierApplies {
            switch modifier {
            case .fillet: if let f = vcad_solid_fillet(base, modifierValue) { target = f; owned = f }
            case .chamfer: if let c = vcad_solid_chamfer(base, modifierValue) { target = c; owned = c }
            case .none: break
            }
        }
        defer { if let o = owned { vcad_solid_free(o) } }
        guard let mesh = vcad_solid_to_mesh(target, 64) else { return nil }
        defer { vcad_mesh_free(mesh) }
        return body(mesh)
    }

    /// CPU-side copy of the sandbox mesh — export path (STL needs the arrays).
    private func sandboxKernelMesh() -> KernelMesh? {
        withSandboxMesh { KernelMesh.fromView(vcad_mesh_view($0)) }
    }

    /// Re-solve the sandbox and stream the FFI tessellation buffers directly
    /// into the LowLevelMesh — no CPU-side array copy on the scrub hot path.
    private func solveSandboxStream() -> (recreated: Bool, stats: StreamingMesh.StreamStats)? {
        let start = Date()
        let result: (recreated: Bool, stats: StreamingMesh.StreamStats)? = withSandboxMesh { mesh in
            guard let r = streaming.update(fromView: vcad_mesh_view(mesh)) else { return nil }
            if let eh = vcad_mesh_edges(mesh, Self.edgeAngleDeg) {
                docPartEdges = [EdgeOverlay.segments(fromView: vcad_edges_view(eh))]
                vcad_edges_free(eh)
            } else {
                docPartEdges = []
            }
            return r
        }
        guard let (_, stats) = result else { return nil }
        docPartInstancing = []
        solveMillis = Date().timeIntervalSince(start) * 1000
        triangleCount = stats.triangleCount
        partCount = 1
        sizeMM = stats.maxBound - stats.minBound
        return result
    }

    private func sandboxScene() -> RenderScene {
        clearAssembly()
        guard let r = solveSandboxStream(), let res = streaming.resource else { return .empty }
        let center = (r.stats.minBound + r.stats.maxBound) / 2
        let size = Self.extent(r.stats.minBound, r.stats.maxBound)
        displayCenter = center
        displayScale = 0.6 / max(size, 0.0001)
        return RenderScene(meshes: [(res, Self.heroColor)], center: center, size: size,
                           triangleCount: r.stats.triangleCount, partCount: 1, edges: docPartEdges)
    }

    /// Hot path: re-solve the sandbox and stream into the GPU buffers in place.
    /// Returns true when the LowLevelMesh was recreated (capacity growth) and
    /// the caller must reassign `streaming.resource` onto its entity.
    func streamSandbox() -> Bool {
        solveSandboxStream()?.recreated ?? false
    }

    struct PickHit { var point: SIMD3<Float>; var normal: SIMD3<Float> }

    // MARK: pattern instancing (shared seed mesh + N transformed entities)

    /// Kernel part index → instancing plan, for visible roots whose node is a
    /// Linear/CircularPattern. Only the editable-JSON path instances (legacy
    /// docs the parser couldn't load fall back to baked kernel meshes).
    private func documentInstancingPlan() -> [Int: PatternInstancing] {
        guard let json = documentJSON, let g = documentGraph else { return [:] }
        var plan: [Int: PatternInstancing] = [:]
        for (i, root) in g.visibleRoots.enumerated() {
            if let inst = DocEdit.patternInstancing(json, rootNodeId: root.nodeId),
               inst.transforms.count > 1 {
                plan[i] = inst
            }
        }
        return plan
    }

    /// Serialize the live doc for kernel eval with each instanced pattern root
    /// re-pointed at its seed child — the kernel then tessellates the seed ONCE
    /// (no N-way boolean union); the viewport places the N copies.
    private func evalData(applying plan: [Int: PatternInstancing]) -> Data? {
        guard var json = documentJSON else { return nil }
        if !plan.isEmpty, var roots = json["roots"] as? [[String: Any]] {
            var vis = 0
            for idx in roots.indices where (roots[idx]["visible"] as? Bool) ?? true {
                if let inst = plan[vis] { roots[idx]["root"] = inst.seedNodeId }
                vis += 1
            }
            json["roots"] = roots
        }
        return DocEdit.serialize(json)
    }

    /// Aggregate a seed mesh + edge segments over instance transforms into one
    /// kernel-space pick mesh / edge list, so ray pick, hover, the gizmo, and
    /// the edge overlay see exactly the geometry the instanced entities show.
    private static func aggregateInstances(
        _ km: KernelMesh, edgeSegments: [SIMD3<Float>], transforms: [simd_float4x4]
    ) -> (pick: PickMesh, edges: [SIMD3<Float>], lo: SIMD3<Float>, hi: SIMD3<Float>) {
        var positions: [SIMD3<Float>] = []
        var indices: [UInt32] = []
        var segs: [SIMD3<Float>] = []
        positions.reserveCapacity(km.positions.count * transforms.count)
        indices.reserveCapacity(km.indices.count * transforms.count)
        segs.reserveCapacity(edgeSegments.count * transforms.count)
        var lo = SIMD3<Float>(repeating: .greatestFiniteMagnitude)
        var hi = SIMD3<Float>(repeating: -.greatestFiniteMagnitude)
        for t in transforms {
            let base = UInt32(positions.count)
            for p in km.positions {
                let w = t * SIMD4(p, 1)
                let v = SIMD3(w.x, w.y, w.z)
                positions.append(v)
                lo = simd_min(lo, v); hi = simd_max(hi, v)
            }
            for idx in km.indices { indices.append(base + idx) }
            for s in edgeSegments {
                let w = t * SIMD4(s, 1)
                segs.append(SIMD3(w.x, w.y, w.z))
            }
        }
        return (PickMesh(positions: positions, indices: indices, lo: lo, hi: hi), segs, lo, hi)
    }

    /// Evaluate on a background thread — set by the live window; headless
    /// smoke hooks and tests keep the synchronous path.
    var evaluatesOffMainThread = false
    /// True while the kernel is evaluating the document off the main thread.
    var solving = false
    /// Progress of the background solve: visible roots landed / total.
    var solveProgress: (done: Int, total: Int) = (0, 0)
    /// The last background evaluation, waiting for the viewport to adopt it.
    @ObservationIgnored private var preparedScene: (data: Data, handle: OpaquePointer)?
    @ObservationIgnored private var evaluating: Data?
    /// The in-flight kernel job and the poll that watches it.
    @ObservationIgnored private var evalJob: OpaquePointer?
    @ObservationIgnored private var evalPoll: Task<Void, Never>?
    /// Parts that have landed so far, drawn while the rest of the document
    /// is still solving.
    @ObservationIgnored private var partialMeshes: [(MeshResource, NSColor)] = []
    @ObservationIgnored private var partialBounds: (lo: SIMD3<Float>, hi: SIMD3<Float>)?
    /// Root-cache keys of the last full evaluation, index-aligned with the
    /// parts — what a saved document's mesh bundle is written from.
    @ObservationIgnored private var lastRootKeys: [String] = []

    /// What the viewport draws while solving: whatever has landed.
    private func partialScene() -> RenderScene {
        var s = RenderScene.pending
        guard let b = partialBounds, !partialMeshes.isEmpty else { return s }
        s.meshes = partialMeshes
        s.center = (b.lo + b.hi) / 2
        s.size = Self.extent(b.lo, b.hi)
        s.partCount = partialMeshes.count
        return s
    }

    nonisolated private static func evaluateScene(data: Data, dir: [UInt8]) -> OpaquePointer? {
        data.withUnsafeBytes { raw in
            dir.withUnsafeBufferPointer { d in
                vcad_scene_from_json_in(raw.bindMemory(to: UInt8.self).baseAddress, data.count,
                                        d.baseAddress, d.count)
            }
        }
    }

    /// Evaluate on the kernel's own thread, drawing parts as they land.
    private func startBackgroundEvaluation(data: Data, dir: [UInt8]) {
        guard evaluating != data else { return }          // already in flight
        if let old = evalJob { vcad_eval_abandon(old); evalJob = nil }   // superseded
        evalPoll?.cancel()
        evaluating = data
        solving = true
        solveProgress = (0, 0)
        partialMeshes = []; partialBounds = nil
        let started = Date()
        let job: OpaquePointer? = data.withUnsafeBytes { raw in
            dir.withUnsafeBufferPointer { d in
                vcad_eval_begin(raw.bindMemory(to: UInt8.self).baseAddress, data.count, d.baseAddress, d.count)
            }
        }
        guard let job else {
            solving = false; evaluating = nil
            loadError = SimError.pending() ?? "The kernel could not read this document"
            let err = loadError
            Task { @MainActor [weak self] in self?.finishFirstSolve(error: err) }
            return
        }
        evalJob = job
        evalPoll = Task { @MainActor [weak self] in
            var landed = 0
            while !Task.isCancelled {
                guard let self, self.evalJob == job else { return }
                var done = 0, total = 0
                let finished = vcad_eval_progress(job, &done, &total)
                if total > 0, done != self.solveProgress.done || total != self.solveProgress.total {
                    self.solveProgress = (done, total)
                    if self.loading?.isEvaluating == true {
                        self.loading?.phase = .evaluating(done: done, total: total)
                    }
                }
                // Draw newly landed parts now rather than after the last one.
                var newParts = false
                while landed < done {
                    let km = KernelMesh.fromView(vcad_eval_part_mesh(job, landed))
                    if !km.isEmpty {
                        self.partialMeshes.append((km.resource(name: "part\(landed)"),
                                                   Self.partColors[landed % Self.partColors.count]))
                        var b = self.partialBounds ?? (km.minBound, km.maxBound)
                        b.lo = simd_min(b.lo, km.minBound); b.hi = simd_max(b.hi, km.maxBound)
                        self.partialBounds = b
                        newParts = true
                    }
                    landed += 1
                }
                if newParts {
                    LaunchPerf.once("first part landed")
                    if self.loading?.isEvaluating == true, self.loading?.landedGeometry == false {
                        self.loading?.landedGeometry = true
                    }
                }
                if finished {
                    LaunchPerf.once("solved", String(format: " (solve %.0f ms, %d parts)",
                                                     Date().timeIntervalSince(started) * 1000, total))
                    self.evalJob = nil
                    let handle = vcad_eval_finish(job)
                    self.evaluating = nil
                    self.solving = false
                    self.partialMeshes = []; self.partialBounds = nil
                    if let handle {
                        self.preparedScene = (data, handle)
                    } else {
                        self.loadError = SimError.pending() ?? "The kernel could not evaluate this document"
                        self.finishFirstSolve(error: self.loadError)
                    }
                    self.solveMillis = Date().timeIntervalSince(started) * 1000
                    self.geometryDirty = true                // the viewport adopts it
                    return
                }
                if newParts { self.geometryDirty = true }
                try? await Task.sleep(for: .milliseconds(100))
            }
        }
    }

    // MARK: mesh bundles — a document ships with its solved geometry

    /// `<doc>.vcadmesh` next to `<doc>.vcad`.
    static func meshBundleURL(for document: URL) -> URL {
        document.deletingPathExtension().appendingPathExtension("vcadmesh")
    }

    /// Feed a document's shipped meshes to the root cache so opening it is a
    /// cache hit instead of a kernel walk. Returns how many roots it covered.
    @discardableResult
    func importMeshBundle(for document: URL) -> Int {
        let bundle = Self.meshBundleURL(for: document)
        guard FileManager.default.fileExists(atPath: bundle.path) else { return 0 }
        let path = Array(bundle.path.utf8)
        return path.withUnsafeBufferPointer { vcad_mesh_bundle_import($0.baseAddress, $0.count) }
    }

    /// Write the bundle for the document at `url` from the last evaluation's
    /// root keys. Returns how many roots it holds (0 = nothing cacheable yet).
    @discardableResult
    func writeMeshBundle(for document: URL) -> Int {
        guard !lastRootKeys.isEmpty else { return 0 }
        let keys = Array(lastRootKeys.joined(separator: "\n").utf8)
        let path = Array(Self.meshBundleURL(for: document).path.utf8)
        return keys.withUnsafeBufferPointer { k in
            path.withUnsafeBufferPointer { p in
                vcad_mesh_bundle_write(k.baseAddress, k.count, p.baseAddress, p.count)
            }
        }
    }

    private func documentScene(path: String) -> RenderScene {
        let start = Date()
        // Prefer the live (possibly edited) doc; fall back to the file on disk
        // for formats the JSON parser couldn't load (legacy terse `.vcad`).
        let plan = documentInstancingPlan()
        let data: Data?
        if documentJSON != nil { data = evalData(applying: plan) }
        else { data = Self.documentBytes(path: path) }
        guard let data, !data.isEmpty else { return .empty }
        // Resolve the document's relative mesh references against its own
        // directory. A robot document ships its meshes alongside itself; with
        // no base directory they resolve against the process working directory
        // and the whole assembly evaluates to empty meshes, which presents as
        // "the file didn't load" rather than "the meshes weren't found".
        let dir = Array((documentDirectory ?? "").utf8)
        let scene: OpaquePointer?
        if evaluatesOffMainThread {
            // A slow document must never beachball the app: the kernel runs
            // on a background thread and the viewport shows "Solving…" (or the
            // previous geometry) until the handle is ready, then rebuilds.
            if let p = preparedScene, p.data == data {
                preparedScene = nil
                scene = p.handle
            } else {
                if let p = preparedScene { vcad_scene_free(p.handle); preparedScene = nil }
                startBackgroundEvaluation(data: data, dir: dir)
                return partialScene()
            }
        } else if engine != nil, documentJSON != nil,
                  ((documentJSON?["instances"] as? [Any])?.isEmpty ?? true),
                  let parts = tessellateDocumentParts(plan: plan, dir: dir) {
            // Engine-backed part document: only the parts the last deltas
            // touched are re-tessellated.
            loadError = nil
            return sceneFromParts(parts, rootKeys: nil, start: start, plan: plan)
        } else {
            scene = Self.evaluateScene(data: data, dir: dir)
        }
        guard let scene else {
            // The kernel refused the document. Returning `.empty` here left the
            // app showing a fully populated feature tree over an empty viewport:
            // the Swift-side DAG parser is lenient, the kernel is not, so a
            // schema-stale file read as a renderer bug. Say what happened.
            loadError = SimError.pending() ?? "The kernel could not evaluate this document"
            return .empty
        }
        loadError = nil
        if vcad_scene_instance_count(scene) > 0 {
            return assemblyScene(adopting: scene, start: start)
        }
        defer { vcad_scene_free(scene) }
        return sceneFromHandle(scene, start: start, plan: plan)
    }

    /// Render the AI-generated loon program. Same evaluator + part-gather as a
    /// loaded document, so the studio, feature tree, and materialize-pop are
    /// shared for free.
    private func generatedScene(loon: String) -> RenderScene {
        let start = Date()
        let data = Data(loon.utf8)
        guard !data.isEmpty else { return .empty }
        let scene: OpaquePointer? = data.withUnsafeBytes { raw in
            vcad_scene_from_loon(raw.bindMemory(to: UInt8.self).baseAddress, data.count)
        }
        guard let scene else { return .empty }
        if vcad_scene_instance_count(scene) > 0 {
            return assemblyScene(adopting: scene, start: start)
        }
        defer { vcad_scene_free(scene) }
        return sceneFromHandle(scene, start: start)
    }

    /// Visible roots of an assembly document that no part definition points
    /// at, with their index among the evaluated parts (one per visible root).
    private func freeRoots() -> [(part: Int, material: String)] {
        guard let json = documentJSON, let roots = json["roots"] as? [[String: Any]],
              let defs = json["partDefs"] as? [String: [String: Any]] else { return [] }
        let defRoots = Set(defs.values.compactMap { ($0["root"] as? NSNumber)?.intValue })
        return roots.filter { ($0["visible"] as? Bool) ?? true }.enumerated().compactMap { i, r in
            guard let root = (r["root"] as? NSNumber)?.intValue, !defRoots.contains(root) else { return nil }
            return (i, r["material"] as? String ?? "default")
        }
    }

    /// Gather an ASSEMBLY scene: one entity per instance, mesh in part-def
    /// local coordinates + a world transform, rendered instead of the root
    /// parts (mirrors the web viewport). Takes ownership of `scene` and keeps
    /// it resident so playback can re-solve FK per frame without re-parsing.
    private func assemblyScene(adopting scene: OpaquePointer, start: Date) -> RenderScene {
        clearAssembly()
        residentAssemblyScene = scene
        let count = vcad_scene_instance_count(scene)

        func instString(_ f: (OpaquePointer?, Int, UnsafeMutablePointer<Int>?) -> UnsafePointer<UInt8>?,
                        _ i: Int) -> String {
            var len = 0
            guard let p = f(scene, i, &len), len > 0 else { return "" }
            return String(decoding: UnsafeBufferPointer(start: p, count: len), as: UTF8.self)
        }

        var instances: [RenderInstance] = []
        var transforms: [float4x4] = []
        var lo = SIMD3<Float>(repeating: .greatestFiniteMagnitude)
        var hi = SIMD3<Float>(repeating: -.greatestFiniteMagnitude)
        var tris = 0
        var centers = [SIMD3<Float>](repeating: .zero, count: count)
        for i in 0..<count {
            var m16 = [Double](repeating: 0, count: 16)
            _ = vcad_scene_instance_transform(scene, i, &m16)
            let world = Self.mat4(m16)
            transforms.append(world)

            let km = KernelMesh.fromView(vcad_scene_instance_mesh(scene, i))
            guard !km.isEmpty else { continue }
            centers[i] = (km.minBound + km.maxBound) / 2
            tris += km.triangleCount
            // World bounds: transform the local AABB's 8 corners.
            for cx in [km.minBound.x, km.maxBound.x] {
                for cy in [km.minBound.y, km.maxBound.y] {
                    for cz in [km.minBound.z, km.maxBound.z] {
                        let w = world * SIMD4<Float>(cx, cy, cz, 1)
                        let p = SIMD3<Float>(w.x, w.y, w.z)
                        lo = simd_min(lo, p); hi = simd_max(hi, p)
                    }
                }
            }
            let matKey = instString(vcad_scene_instance_material, i)
            instances.append(RenderInstance(
                index: i,
                id: instString(vcad_scene_instance_id, i),
                mesh: km.resource(name: "inst\(i)"),
                material: resolvedMaterial(named: matKey, fallbackIndex: i),
                transform: world))
        }
        // A root no part definition uses (a frozen topology_optimize mesh added
        // to an assembly) has no instance to draw it. Index past the instances
        // so FK, playback and picking never see it.
        let partTotal = vcad_scene_part_count(scene)
        for (k, root) in freeRoots().enumerated() where root.part < partTotal {
            let km = KernelMesh.fromView(vcad_scene_part_mesh(scene, root.part))
            guard !km.isEmpty else { continue }
            tris += km.triangleCount
            lo = simd_min(lo, km.minBound); hi = simd_max(hi, km.maxBound)
            instances.append(RenderInstance(
                index: count + k,
                id: "root\(root.part)",
                mesh: km.resource(name: "free\(k)"),
                material: resolvedMaterial(named: root.material, fallbackIndex: count + k),
                transform: matrix_identity_float4x4))
        }
        guard !instances.isEmpty else {
            clearAssembly()
            return .empty
        }
        assemblyInstanceCount = count
        instanceTransforms = transforms
        // Keep the AUTHORED poses: the simulation only writes instances that
        // have a physics body, and everything else (static scenery, fixtures)
        // must keep the pose the document gave it rather than collapse to the
        // origin.
        authoredInstanceTransforms = transforms
        // Instance picking isn't wired yet — clear the part pick meshes so a
        // stale root-part hit can't select the wrong thing.
        docPartMeshes = []
        docPartEdges = []

        solveMillis = Date().timeIntervalSince(start) * 1000
        triangleCount = tris
        partCount = instances.count
        sizeMM = hi - lo
        let center = (lo + hi) / 2
        let size = Self.extent(lo, hi)
        displayCenter = center
        displayScale = 0.6 / max(size, 0.0001)
        sceneGeneration &+= 1
        assemblyInstanceIDs = (0..<count).map { instString(vcad_scene_instance_id, $0) }
        assemblyLocalCenters = centers
        // Land on the authored pose at t=0 when a timeline exists.
        if timeline != nil { applyPlaybackPose() }
        // A reload keeps the pose you were looking at.
        // So does a pose an agent sent while this scene was still solving.
        if timeline == nil, !jointPose.isEmpty || explode > 0 || agentBaseDelta != nil { applyStudioPose() }
        return RenderScene(meshes: [], center: center, size: size,
                           triangleCount: tris, partCount: instances.count,
                           instances: instances)
    }

    /// Resolve a material KEY (as carried by an instance) the same way the
    /// per-part path does: document materials → presets → index palette.
    func resolvedMaterial(named key: String, fallbackIndex: Int) -> ResolvedMaterial {
        if let d = documentGraph?.materials[key] {
            return ResolvedMaterial(
                color: NSColor(srgbRed: d.color.0, green: d.color.1, blue: d.color.2, alpha: 1),
                metallic: d.metallic, roughness: d.roughness, transmission: d.transmission)
        }
        if let p = MaterialPreset.byKey(key) {
            return ResolvedMaterial(color: p.nsColor, metallic: p.metallic,
                                    roughness: p.roughness, transmission: p.transmission)
        }
        return ResolvedMaterial(color: documentBaseColor(fallbackIndex),
                                metallic: 0.55, roughness: 0.34, transmission: 0)
    }

    /// Gather every part mesh out of an evaluated scene handle into a centered,
    /// auto-fit `RenderScene`, updating the live readouts. Shared by the
    /// document and AI-generated paths.
    private func sceneFromHandle(_ scene: OpaquePointer, start: Date,
                                 plan: [Int: PatternInstancing] = [:]) -> RenderScene {
        let count = vcad_scene_part_count(scene)
        let parts: [TessellatedPart?] = (0..<count).map { i in
            let km = KernelMesh.fromView(vcad_scene_part_mesh(scene, i))
            if km.isEmpty { return nil }
            // Feature edges for the CAD edge overlay (same index alignment).
            var segs: [SIMD3<Float>] = []
            if let eh = vcad_scene_part_edges(scene, i, Self.edgeAngleDeg) {
                segs = EdgeOverlay.segments(fromView: vcad_edges_view(eh))
                vcad_edges_free(eh)
            }
            return TessellatedPart(mesh: km, edges: segs, resource: km.resource(name: "part" + String(i)),
                                   rootKey: Self.rootKey(scene, i))
        }
        let keys: [String] = (0..<count).compactMap { i in
            guard let c = vcad_scene_root_key(scene, i) else { return nil }
            defer { vcad_cam_free(c) }
            return String(cString: c)
        }
        return sceneFromParts(parts, rootKeys: keys, start: start, plan: plan)
    }

    /// Gather a part-mode scene from per-root tessellations (index-aligned
    /// with the evaluated roots; nil = empty).
    private func sceneFromParts(_ parts: [TessellatedPart?], rootKeys: [String]?, start: Date,
                                plan: [Int: PatternInstancing]) -> RenderScene {
        clearAssembly()                      // part-mode scene: drop any resident assembly
        var meshes: [(MeshResource, NSColor)] = []
        var instancing: [PatternInstancing?] = []
        var picks: [PickMesh] = []
        var edges: [[SIMD3<Float>]] = []
        var lo = SIMD3<Float>(repeating: .greatestFiniteMagnitude)
        var hi = SIMD3<Float>(repeating: -.greatestFiniteMagnitude)
        var tris = 0
        for (i, part) in parts.enumerated() {
            guard let part else { continue }
            let km = part.mesh, segs = part.edges
            meshes.append((part.resource, Self.partColors[i % Self.partColors.count]))
            if let inst = plan[i] {
                // Pattern root: the kernel returned the SEED (the eval doc was
                // re-pointed) — aggregate picks/edges/bounds over the instances.
                let agg = Self.aggregateInstances(km, edgeSegments: segs,
                                                  transforms: inst.transforms)
                lo = simd_min(lo, agg.lo); hi = simd_max(hi, agg.hi)
                tris += km.triangleCount * inst.transforms.count
                instancing.append(inst)
                picks.append(agg.pick)
                edges.append(agg.edges)
            } else {
                lo = simd_min(lo, km.minBound); hi = simd_max(hi, km.maxBound)
                tris += km.triangleCount
                instancing.append(nil)
                // Index-aligned with `meshes` (and thus the rendered part
                // entities), so a viewport ray maps back to the right tree row.
                picks.append(PickMesh(positions: km.positions, indices: km.indices,
                                      lo: km.minBound, hi: km.maxBound))
                edges.append(segs)
            }
        }
        guard !meshes.isEmpty else { return .empty }
        docPartMeshes = picks
        sceneGeneration &+= 1
        docPartEdges = edges
        docPartInstancing = instancing
        lastRootKeys = rootKeys ?? parts.compactMap { $0?.rootKey }

        solveMillis = Date().timeIntervalSince(start) * 1000
        triangleCount = tris
        partCount = meshes.count
        sizeMM = hi - lo
        let center = (lo + hi) / 2
        let size = Self.extent(lo, hi)
        displayCenter = center
        displayScale = 0.6 / max(size, 0.0001)
        return RenderScene(meshes: meshes, center: center, size: size,
                           triangleCount: tris, partCount: meshes.count, edges: edges,
                           instancing: instancing)
    }

    static func extent(_ lo: SIMD3<Float>, _ hi: SIMD3<Float>) -> Float {
        let d = hi - lo
        return max(d.x, max(d.y, d.z))
    }

    static let heroColor = NSColor(red: 0.40, green: 0.60, blue: 0.80, alpha: 1.0)
    static let partColors: [NSColor] = [
        NSColor(red: 0.62, green: 0.66, blue: 0.70, alpha: 1.0),
        NSColor(red: 0.82, green: 0.62, blue: 0.30, alpha: 1.0),
        NSColor(red: 0.45, green: 0.62, blue: 0.82, alpha: 1.0),
        NSColor(red: 0.72, green: 0.46, blue: 0.46, alpha: 1.0),
    ]
}

extension EditorModel {
    func editElectronics(name: String = "Edit Circuit", _ mutate: (inout ECObject) -> Void) {
        if documentJSON == nil {
            documentJSON = ["version": "0.1", "nodes": ECObject(), "materials": ECObject(), "part_materials": ECObject(), "roots": [ECObject]()]
        }
        detachedEdit(name: name, mutate)
    }
}

// MARK: - Agent hooks (AgentPanel.swift / FrogAgent.swift)

extension EditorModel {
    /// Render an agent tool's engine result live, without an undo entry —
    /// the whole agent turn becomes one entry via `agentEndTurn`.
    func agentApplied(_ r: DocumentEngine.Result) {
        engineError = nil
        adopt(r, reeval: .rebuild)
        agentFramePending = true
    }

    /// Whether content going from `old` to `new` (mm) is worth reframing for:
    /// its first geometry, or any axis moving by over a third of the larger
    /// of the two bodies' longest sides.
    static func agentShouldFrame(from old: SIMD3<Float>?, to new: SIMD3<Float>) -> Bool {
        let longest = new.max()
        guard longest > 0 else { return false }
        guard let old else { return true }
        let change = simd_abs(new - old).max()
        return change > max(longest, old.max()) / 3
    }

    /// Camera distance that puts a box of `size` (mm, Z-up), scaled to the
    /// scene's display normalization and centred, inside `margin` of a 60°
    /// view from this orbit direction. Leaves room for the window's chrome.
    static func fittedDistance(size: SIMD3<Float>, azimuth: Float, elevation: Float,
                               aspect: Float, margin: Float = 0.8) -> Float {
        let world = SIMD3(size.x, size.z, size.y) * (0.6 / max(size.max(), 0.0001)) / 2
        let toCamera = SIMD3(cos(elevation) * sin(azimuth), sin(elevation), cos(elevation) * cos(azimuth))
        let right = normalize(cross(-toCamera, SIMD3<Float>(0, 1, 0)))
        let up = cross(right, -toCamera)
        let tanHalf = tan(Float.pi / 6) * margin
        var needed: Float = 0
        for sx: Float in [-1, 1] {
            for sy: Float in [-1, 1] {
                for sz: Float in [-1, 1] {
                    let c = world * SIMD3(sx, sy, sz)
                    let reach = max(abs(dot(c, up)) / tanHalf,
                                    abs(dot(c, right)) / (tanHalf * max(aspect, 0.1)))
                    needed = max(needed, dot(c, toCamera) + reach)
                }
            }
        }
        return max(0.45, min(8.0, needed))
    }

    /// Where the camera should go for the agent build that just finished, or
    /// nil to leave it alone. Consumes the pending request.
    func takeAgentFrameDistance(aspect: Float) -> Float? {
        guard agentFramePending else { return nil }
        agentFramePending = false
        guard !cameraUserMoved, !sketching, workspace != .manufacture, workspace != .electronics,
              Self.agentShouldFrame(from: agentFramedSize, to: sizeMM) else { return nil }
        agentFramedSize = sizeMM
        return Self.fittedDistance(size: sizeMM, azimuth: azimuth, elevation: elevation, aspect: aspect)
    }

    /// Close an agent turn that changed the document: one undo step back to
    /// the snapshot taken before the turn's first edit.
    func agentEndTurn(before: Data, name: String = "Agent Edit") {
        pushUndo(.snapshot(before), name: name)
    }

    /// Drop undo entries an agent edit pushed above `depth` (AgentTurn.swift).
    func agentDropUndo(above depth: Int) {
        if undoStack.count > depth { undoStack.removeSubrange(depth...) }
    }

    /// Land an agent edit as one snapshot entry: entries the edit pushed
    /// itself (above `depth`) fold into it. `coalesce` extends the turn's
    /// existing top entry instead — it already snapshots the turn's start.
    func agentCommitUndo(before: Data, above depth: Int, coalesce: Bool, name: String) {
        agentDropUndo(above: depth)
        if coalesce, let last = undoStack.last, case .snapshot = last.step {
            undoStack[undoStack.count - 1] = UndoEntry(step: last.step, name: name)
            redoStack.removeAll()
        } else {
            pushUndo(.snapshot(before), name: name)
        }
    }

    /// The agent can work here: an engine document, or a blank canvas that
    /// can be given one.
    var agentCanEdit: Bool { engine != nil || (source.isSandbox && documentJSON == nil) }

    /// Turn the Untitled sandbox (a primitive + one modifier, not a document)
    /// into a real engine document holding the same shape, so the modeling
    /// tools work. Without this every Design tool sat disabled behind an
    /// "Open a document" hover hint, and a fresh install could not model
    /// anything. One undo step back to the sandbox. Returns whether the
    /// window now has an editable document.
    ///
    /// `silent`: the viewport's face/edge picking needs a B-rep document to
    /// pick against, so the first pointer move over the demo shape promotes
    /// it without an undo step, without marking the window dirty, and
    /// without selecting the body — nothing the user did changed the model.
    @discardableResult
    func promoteSandboxToDocument(blank: Bool = false, silent: Bool = false) -> Bool {
        if usesDocumentTree, engine != nil { return true }
        guard source.isSandbox, engine == nil, documentJSON == nil else { return usesDocumentTree }
        // A sketch starts a part from nothing; it shouldn't inherit the demo cube.
        if blank { return agentEnsureEngine() != nil && usesDocumentTree }
        let applies = modifierApplies && modifierEffective
        let (shape, mod, amount) = (baseShape, modifier, modifierValue)
        guard let e = agentEnsureEngine() else { return false }
        let before = e.save()
        func call(_ tool: String, _ args: [String: Any]) throws -> DocumentEngine.Result {
            let typed = try JSONDecoder().decode([String: EngineJSON].self,
                                                 from: JSONSerialization.data(withJSONObject: args))
            return try e.applyTool(ToolCall(tool: tool, args: typed))
        }
        do {
            let primitive: [String: Any] = switch shape {
            case .cube: ["type": "cube", "name": "Cube", "params": ["size": ["x": 30, "y": 30, "z": 30]]]
            case .cylinder: ["type": "cylinder", "name": "Cylinder", "params": ["radius": 15, "height": 30]]
            case .sphere: ["type": "sphere", "name": "Sphere", "params": ["radius": 15]]
            case .cone, .torus, .wedge, .prism: ["type": "cube", "name": "Cube", "params": ["size": ["x": 30, "y": 30, "z": 30]]]   // never previewed
            }
            var r = try call("create", primitive)
            if applies, let base = r.createdFeatureId {
                let primitiveOnly = e.save()
                switch mod {
                case .fillet: r = try call("create", ["type": "fillet", "name": "Fillet", "parent_part_id": base, "params": ["radius": amount]])
                case .chamfer: r = try call("create", ["type": "chamfer", "name": "Chamfer", "parent_part_id": base, "params": ["distance": amount]])
                case .none: break
                }
                // The kernel can refuse the demo's modifier (a fillet on a
                // cylinder's circular edges diverges); keep the plain primitive
                // rather than promote to a part that doesn't build and vanishes.
                if let failed = (try? e.agentRead("evaluation_errors", [:]))?
                    .trimmingCharacters(in: .whitespacesAndNewlines), !failed.isEmpty {
                    r = try e.restore(primitiveOnly)
                }
            }
            adopt(r, reeval: .rebuild)
            if silent {
                savedHash = e.stateHash
                documentDirty = false
                selectedFeatureID = nil
                return usesDocumentTree
            }
            pushUndo(.snapshot(before), name: "Start Document")
            if let first = featureNodes.first { selectFeature(first.id) }
            return usesDocumentTree
        } catch {
            _ = try? e.restore(before)
            engineError = "Couldn't start a document from the sketch pad: \(error)"
            return false
        }
    }

    /// The engine the agent edits. A blank canvas (Untitled, nothing in it)
    /// gets an empty engine document on first use; Save then goes to Save As
    /// like any untitled document. Nil for view-only documents.
    func agentEnsureEngine() -> DocumentEngine? {
        if let engine { return engine }
        guard source.isSandbox, documentJSON == nil else { return nil }
        let empty: [String: Any] = ["version": "0.1", "nodes": [String: Any](), "materials": [String: Any](),
                                    "part_materials": [String: Any](), "roots": [Any]()]
        let fresh = DocumentEngine()
        guard let state = try? fresh.loadIR(empty) else { return nil }
        engine = fresh
        engineError = nil
        savedHash = fresh.stateHash
        adopt(state, reeval: .rebuild)
        return fresh
    }
}

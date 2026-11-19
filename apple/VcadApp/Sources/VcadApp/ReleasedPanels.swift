import RealityKit
import SwiftUI

// The released window's floating panels and viewport overlays that don't
// depend on the Mac window itself — shared so the iPhone target hosts the
// SAME inspector, simulation panel, section view and topology highlight.
// Moved verbatim from ReleasedDesktop.swift.

// MARK: - Floating panels

/// The Object Inspector: what the selected feature is, its live parameters,
/// the document's named parameters, and measurements.
struct ObjectInspectorWindow: View {
    @Bindable var model: EditorModel
    private var title: String {
        if let n = model.selectedFeatureNode { return n.name }
        // A document's title comes only from its tree. The sandbox feature list
        // (Cube / Fillet) outlives promotion to a document (source stays
        // .sandbox), which titled an empty inspector "Fillet".
        if model.usesDocumentTree { return "Inspector" }
        return model.features.first { $0.id == model.selectedFeatureID }?.name ?? "Inspector"
    }
    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            PanelHeader(title: title)
            ScrollView {
                VStack(alignment: .leading, spacing: Theme.Space.m) {
                    if model.usesDocumentTree { documentSections } else { sandboxSection }
                    if !model.docParameters.isEmpty {
                        Divider()
                        Eyebrow("Document Parameters")
                        docParameterRows
                        SensitivitySection(model: model)
                    }
                    Divider()
                    DisclosureGroup("Measurements", isExpanded: $model.measurementsShown) {
                        VStack(alignment: .leading, spacing: 7) { measurementRows }
                            .padding(.top, Theme.Space.s)
                    }.font(.callout)
                }
            }.frame(maxHeight: 420).fixedSize(horizontal: false, vertical: true)
        }.padding(Theme.Space.m).frame(width: Theme.Width.inspector).panelSurface()
    }

    // MARK: document (a .vcad's feature tree)

    /// The selected feature: what it is, what it is made of, and — for the ops
    /// that declare editable parameters — live scrub fields.
    @ViewBuilder private var documentSections: some View {
        if let ii = model.selectedInstanceIndex {
            Eyebrow(model.instanceName(ii) ?? "Instance \(ii)")
            VStack(alignment: .leading, spacing: 7) {
                KeyValueRow("Kind", "Assembly instance")
                if let def = model.instancePartDefName(ii) { KeyValueRow("Part", def) }
            }
            if let node = model.instanceFeatureNode(ii) {
                Divider()
                // The geometry an instance draws lives in its part def, so the
                // editors act on the definition: change the link's cube here and
                // every instance of that link follows.
                Eyebrow("\(node.name) — shared by every instance")
                VStack(alignment: .leading, spacing: 7) {
                    KeyValueRow("Operation", DocumentGraph.label(node.opType))
                    if FeatureParamEditors.editableOps.contains(node.opType) {
                        FeatureParamEditors(model: model, node: node) { snapshot in
                            ReleaseWindowController.shared.refreshGeometry(model: model,
                                                                           collisions: snapshot)
                        }
                    }
                }
            }
        } else if let node = model.selectedFeatureNode {
            Eyebrow(DocumentGraph.label(node.opType))
            VStack(alignment: .leading, spacing: 7) {
                if let pi = node.partIndex {
                    HStack {
                        Text("Material").font(.callout).foregroundStyle(.secondary)
                        Spacer(minLength: Theme.Space.s)
                        materialMenu(pi)
                    }
                    if !model.isPartVisible(pi) {
                        HStack {
                            Text("Visibility").font(.callout).foregroundStyle(.secondary)
                            Spacer()
                            Label("Hidden", systemImage: "eye.slash").font(.callout).foregroundStyle(.secondary)
                        }
                    }
                }
            }
            if FeatureParamEditors.editableOps.contains(node.opType) {
                Divider()
                Eyebrow("Parameters")
                VStack(alignment: .leading, spacing: 7) {
                    // Edits re-solve the bound nodes; the released scene swaps
                    // meshes in place (collisions only on commit — per-tick
                    // regen stutters big documents).
                    FeatureParamEditors(model: model, node: node) { snapshot in
                        ReleaseWindowController.shared.refreshGeometry(model: model,
                                                                       collisions: snapshot)
                    }
                }
            } else if let d = node.detail {
                KeyValueRow("Value", d)
            }
            // A placed shape: its row edits the position, so its own size
            // (a cylinder's radius and height) was unreachable from the UI.
            if let hid = node.heldNodeId, let hop = node.heldOpType,
               FeatureParamEditors.editableOps.contains(hop) {
                Divider()
                Eyebrow(DocumentGraph.label(hop))
                VStack(alignment: .leading, spacing: 7) {
                    FeatureParamEditors(model: model,
                                        node: FeatureNode(id: "held\(hid)", nodeId: hid, name: node.name,
                                                          opType: hop, symbol: node.symbol, detail: nil,
                                                          partIndex: node.partIndex, children: [])) { snapshot in
                        ReleaseWindowController.shared.refreshGeometry(model: model,
                                                                       collisions: snapshot)
                    }
                }
            }
        } else {
            EmptyPanelState(title: "Nothing selected", systemImage: "cursorarrow.click.2",
                            detail: "Select a feature in the navigator or a part in the viewport.")
        }
    }

    // MARK: sandbox (the built-in primitive + modifier)

    @ViewBuilder private var sandboxSection: some View {
        VStack(alignment: .leading, spacing: 7) {
            KeyValueRow("Shape", model.baseShape.label)
            KeyValueRow("Modifier", model.modifier.label)
            if model.modifier != .none && model.modifierEffective {
                // Label on its own line, slider flexes: the one-line version
                // (label + 90 pt slider + field) was wider than the glass
                // inspector and clipped every row's right edge.
                VStack(alignment: .leading, spacing: 4) {
                    Text(model.modifier.paramLabel).font(.callout).foregroundStyle(.secondary)
                    HStack(spacing: 6) {
                        Slider(value: $model.modifierValue, in: 0...12)
                            .controlSize(.mini)
                            .accessibilityLabel(model.modifier.paramLabel)
                        ScrubField(label: "", value: model.modifierValue, sensitivity: 0.05, minValue: 0) { v, _ in
                            model.modifierValue = min(12, max(0, v))
                        }
                    }
                }
            } else if model.modifier != .none {
                Label("No edges on a sphere", systemImage: "info.circle")
                    .font(.callout).foregroundStyle(.secondary)
            }
        }
    }

    @ViewBuilder private var docParameterRows: some View {
        VStack(alignment: .leading, spacing: 7) {
            ForEach(model.docParameters) { p in
                if let v = p.value {
                    ScrubField(label: p.name, value: v, unit: p.unit ?? "mm",
                               sensitivity: FeatureParamEditors.paramSensitivity(p),
                               minValue: p.min ?? -.greatestFiniteMagnitude) { v, s in
                        model.editParameter(p.name, value: v, snapshot: s)
                        // Collisions on typed commits / scrub start only —
                        // per-tick regen would stutter big docs.
                        ReleaseWindowController.shared.refreshGeometry(model: model, collisions: s)
                    }
                    .help(p.description ?? p.name)
                } else if let f = p.formula {
                    KeyValueRow(p.name, "= \(f)").help(p.description ?? p.name)
                }
            }
        }
    }

    @ViewBuilder private var measurementRows: some View {
        if let p = model.openingParts {
            // First solve still running: nothing measured yet.
            KeyValueRow("Solve", p.total > 1 ? "\(p.done) of \(p.total) parts" : "Evaluating…")
        } else {
            settledMeasurementRows
        }
    }

    @ViewBuilder private var settledMeasurementRows: some View {
        KeyValueRow("Triangles", model.triangleCount.formatted())
        KeyValueRow("Bounds", String(format: "%.1f × %.1f × %.1f mm",
                                     abs(model.sizeMM.x), abs(model.sizeMM.y), abs(model.sizeMM.z)))
        KeyValueRow("Solve", String(format: "%.1f ms", model.solveMillis))
        if let info = model.pickInfo {
            KeyValueRow("Picked", info)
        }
    }

    /// Material assignment for a part — swatch + grouped presets. A Menu
    /// rather than a Picker: the presets are grouped by category, and the
    /// checkmark must reflect the part's real assignment (a part with no
    /// material shows "Default", never a preset it does not have).
    private func materialMenu(_ pi: Int) -> some View {
        let current = model.materialName(forPart: pi) ?? "default"
        let resolved = model.resolvedMaterial(forPart: pi)
        return Menu {
            ForEach(MaterialPreset.grouped, id: \.category) { group in
                Section(group.category.capitalized) {
                    ForEach(group.items) { m in
                        Button { model.setPartMaterial(pi, m.key) } label: {
                            if m.key == current { Label(m.name, systemImage: "checkmark") }
                            else { Text(m.name) }
                        }
                    }
                }
            }
        } label: {
            HStack(spacing: 6) {
                Circle().fill(Color(portedColor: resolved.color)).frame(width: 10, height: 10)
                    .overlay(Circle().strokeBorder(.separator, lineWidth: 0.5))
                Text(MaterialPreset.byKey(current)?.name ?? current.capitalized)
                    .font(.callout)
            }
        }
        .menuStyle(.borderlessButton)
        .menuIndicator(.visible)
        .fixedSize()
        .accessibilityLabel("Material")
    }
}

/// The cross-domain Receipt, in the inspector's slot. The gripper is neither a
/// sandbox primitive nor a feature-tree document, so the Object Inspector has
/// nothing true to say about it.
struct ReceiptLedgerWindow: View {
    @Bindable var model: EditorModel

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            PanelHeader(title: "Receipt", systemImage: "checklist", onClose: { model.showsInspector = false })
            ReceiptLedger(model: model)
        }
        .padding(Theme.Space.m).frame(width: Theme.Width.inspector).panelSurface()
    }
}

/// The Simulation panel. Wraps the same `SimInspector` every platform uses, so
/// every readout, control, and fail-closed message is defined once.
///
/// **Anything added here must carry a `ChromeRegion`.** Released mode passes
/// the mouse through to the desktop everywhere the hit test says the pixel is
/// not ours, so chrome that does not report its frame renders normally and
/// then silently ignores every click.
struct SimulationWindow: View {
    @Bindable var model: EditorModel

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            PanelHeader(title: "Simulation", systemImage: "atom", onClose: { model.sim.teardown() })
            SimInspector(model: model)
        }
        .padding(Theme.Space.m).frame(width: Theme.Width.navigator).panelSurface()
    }
}


/// Swap every part's mesh for a copy clipped by the section plane (or put the
/// originals back), and draw the cut outline. Display-only.
@MainActor
func applySectionView(model: EditorModel) {
    guard let centering = ReleaseWindowController.shared.centeringEntity else { return }
    centering.findEntity(named: "sectionRoot")?.removeFromParent()
    func parts(_ e: Entity) -> [ModelEntity] {
        e.children.flatMap { c -> [ModelEntity] in
            var out = parts(c)
            if let m = c as? ModelEntity, c.name.hasPrefix("part") || c.name.hasPrefix("inst") { out.append(m) }
            return out
        }
    }
    let bodies = parts(centering)
    guard let sec = model.section else {
        for b in bodies { b.isEnabled = true }
        return
    }
    let root = Entity(); root.name = "sectionRoot"
    var outline: [SIMD3<Float>] = []
    for b in bodies {
        guard let mesh = b.model?.mesh else { continue }
        b.isEnabled = false
        let g = SectionPlane.geometry(of: mesh)
        // Clip in the kernel frame: transform through the body's pose.
        let m = b.transformMatrix(relativeTo: centering)
        let world = g.positions.map { p -> SIMD3<Float> in
            let v = m * SIMD4(p, 1); return SIMD3(v.x, v.y, v.z)
        }
        let (tris, cut) = sec.clip(positions: world, indices: g.indices)
        outline += cut
        if let r = SectionPlane.resource(tris, name: b.name + "-section") {
            root.addChild(ModelEntity(mesh: r, materials: b.model?.materials ?? []))
        }
    }
    let w = max(model.displayedSceneSize, 10) * 0.004
    if let r = EdgeOverlay.ribbonResource(segments: outline, width: w, name: "sectionOutline") {
        root.addChild(ModelEntity(mesh: r, materials: [UnlitMaterial(color: NSColor(red: 1, green: 0.62, blue: 0.12, alpha: 1))]))
    }
    centering.addChild(root)
}

/// Face/edge highlight under the kernel frame: selected faces get a
/// translucent brand-orange fill, the hovered face a lighter one; selected and
/// hovered edges are coloured screen-space lines (3 px / 2 px, ribbons when
/// the shader library is absent); measure span teal. Nil when the pick filter
/// isn't topological. Shared by the pick refresh and the geometry rebuild, so
/// a rebuild keeps it.
@MainActor
func topologyOverlayEntity(model: EditorModel) -> Entity? {
    guard model.picking.filter.picksTopology else { return nil }
    let (sel, hov) = model.topologyHighlight()
    let root = Entity(); root.name = "topoOverlay"
    let w = max(model.displayedSceneSize, 1) * 0.006

    // Face fills: the face's own triangles, nudged off the surface along their
    // normals (a hair, scene-relative) so they never z-fight it.
    if let data = model.pickingDocumentData {
        let cache = AnalysisMeshCache.shared
        let lift = Float(max(model.displayedSceneSize, 1)) * 0.0008
        func fill(_ refs: [TopoRef], _ color: NSColor, _ alpha: CGFloat, _ name: String) {
            for part in Set(refs.map(\.part)) {
                let faces = Set(refs.filter { $0.part == part && $0.kind == .face }.map(\.id))
                guard let mesh = cache.faceMesh(part: part, faces: faces, data: data, lift: lift) else { continue }
                var m = UnlitMaterial(color: color.withAlphaComponent(alpha))
                m.blending = .transparent(opacity: .init(floatLiteral: Float(alpha)))
                m.faceCulling = .none
                let e = ModelEntity(mesh: mesh, materials: [m]); e.name = name
                root.addChild(e)
            }
        }
        fill(model.picking.selectedFaces, SelectionStyle.selected, SelectionStyle.faceFillSelectedAlpha, "topoSelFill")
        if let h = model.picking.hovered, h.kind == .face, !model.picking.isSelected(h) {
            fill([h], SelectionStyle.hover, SelectionStyle.faceFillHoverAlpha, "topoHoverFill")
        }
    }

    let view = ReleaseWindowController.shared.arView
    let hPx = Float((view?.bounds.height ?? 900) * (view?.window?.backingScaleFactor ?? 2))
    let scale = Float(view?.window?.backingScaleFactor ?? 2)
    func lines(_ segs: [SIMD3<Float>], _ color: NSColor, px: Float, ribbon: Float, _ name: String) {
        if let mat = SceneAssets.highlightLineMaterial(viewportHeightPx: hPx, widthPx: px * scale, color: color),
           let r = EdgeOverlay.screenLineResource(segments: segs, name: name) {
            root.addChild(ModelEntity(mesh: r, materials: [mat]))
        } else if let r = EdgeOverlay.ribbonResource(segments: segs, width: ribbon, name: name) {
            root.addChild(ModelEntity(mesh: r, materials: [UnlitMaterial(color: color)]))
        }
    }
    lines(sel, SelectionStyle.selected, px: SelectionStyle.edgeSelectedPx, ribbon: w * 1.4, "topoSel")
    lines(hov, SelectionStyle.hover, px: SelectionStyle.edgeHoverPx, ribbon: w, "topoHover")
    if model.measuring, let span = model.measurement?.span,
       let m = EdgeOverlay.ribbonResource(segments: [span.0, span.1], width: w, name: "measureSpan") {
        root.addChild(ModelEntity(mesh: m, materials: [UnlitMaterial(color: .systemTeal)]))
    }
    return root
}

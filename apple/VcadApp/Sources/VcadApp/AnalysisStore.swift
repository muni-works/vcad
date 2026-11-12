import Foundation
import SwiftUI

// State + actions behind the Analysis panel: DFM (run a rule pack, apply a
// resolved fix as one named undo step), FEA (loads/supports on picked faces,
// material, in-process solve, results into the field overlay) and
// clearance/interference between parts.
//
// The kernel work runs off the main actor on a snapshot of the document
// bytes; a result whose document has since changed is marked stale rather
// than drawn over geometry it no longer describes.

enum AnalysisTab: String, CaseIterable, Identifiable {
    case dfm = "DFM", fea = "Stress", clearance = "Clearance"
    var id: String { rawValue }
}

/// A load or support being set up: faces on the analysed part.
struct FeaLoadSetup: Identifiable, Equatable {
    let id = UUID()
    var faces: [UInt32] = []
    /// Total force, N (kernel Z-up frame).
    var force: SIMD3<Double> = [0, 0, -100]
}

struct FeaSupportSetup: Identifiable, Equatable {
    let id = UUID()
    var faces: [UInt32] = []
    var fix: [Bool] = [true, true, true]
}

enum FeaFieldKind: String, CaseIterable, Identifiable {
    case vonMises = "von Mises", displacement = "Displacement"
    var id: String { rawValue }
}

@MainActor @Observable
final class AnalysisStore {
    static let shared = AnalysisStore()

    var shown = false
    var tab: AnalysisTab = .dfm

    // DFM
    var dfmProcess = "fdm"
    var dfmReport: DfmReport?
    var dfmBusy = false
    var dfmError: String?
    var focusedIssue: String?
    /// Last fix applied (for the status line).
    var lastFix: String?

    // FEA
    var feaPart = 0
    /// Preset id; nil = from the part's material.
    var feaMaterial: String?
    var feaResolution = 24
    var feaLoads: [FeaLoadSetup] = [FeaLoadSetup()]
    var feaSupports: [FeaSupportSetup] = [FeaSupportSetup()]
    var feaResult: FeaResult?
    var feaBusy = false
    var feaError: String?
    var feaField: FeaFieldKind = .vonMises
    /// Which setup row the next face pick goes to.
    var pickTarget: PickTarget?
    enum PickTarget: Equatable { case load(UUID), support(UUID) }

    // Clearance
    var clearance: ClearanceReport?
    var clearanceBusy = false
    var clearanceError: String?

    /// Document bytes each result was computed from.
    @ObservationIgnored private var dfmKey: Data?
    @ObservationIgnored private var feaKey: Data?
    @ObservationIgnored private var clearanceKey: Data?
    /// Bumped whenever something the viewport overlay draws changes.
    var overlayRevision = 0

    @ObservationIgnored var picker: AnalysisFacePicker = KernelFaceTable()

    static func documentData(_ m: EditorModel) -> Data? {
        m.pickingDocumentData   // same bytes, memoized per document revision
    }

    func isStale(_ key: Data?, _ m: EditorModel) -> Bool { key != nil && key != Self.documentData(m) }
    func dfmStale(_ m: EditorModel) -> Bool { isStale(dfmKey, m) }
    func feaStale(_ m: EditorModel) -> Bool { isStale(feaKey, m) }
    func clearanceStale(_ m: EditorModel) -> Bool { isStale(clearanceKey, m) }

    // MARK: DFM

    func runDfm(_ m: EditorModel) async {
        guard let data = Self.documentData(m) else { dfmError = "No document is open."; return }
        dfmBusy = true; dfmError = nil
        let process = dfmProcess
        let r = await Task.detached { Result { try AnalysisFFI.dfmCheck(doc: data, process: process) } }.value
        dfmBusy = false
        switch r {
        case .success(let report): dfmReport = report; dfmKey = data
        case .failure(let e): dfmError = "\(e)"; dfmReport = nil
        }
        overlayRevision += 1
    }

    /// Apply an issue's resolved patch as one undo step, then re-check.
    @discardableResult
    func applyFix(_ issue: DfmIssue, _ m: EditorModel) async -> Bool {
        guard let patch = issue.patch else { dfmError = issue.patch_note ?? "No automatic fix."; return false }
        guard let data = Self.documentData(m) else { return false }
        do {
            let patched = try AnalysisFFI.dfmApply(doc: data, patch: patch)
            try AgentEdits.edit(m, name: "DFM: Fix \(issue.ruleTitle)") { try $0.loadIR(patched) }
            lastFix = patch.summary
        } catch {
            dfmError = AgentEdits.describe(error)
            return false
        }
        await runDfm(m)
        return true
    }

    // MARK: FEA

    func faces(_ m: EditorModel) -> [AnalysisFace] {
        guard let data = Self.documentData(m) else { return [] }
        return picker.faces(part: feaPart, document: data) ?? []
    }

    func toggle(face: UInt32) {
        switch pickTarget {
        case .load(let id):
            guard let i = feaLoads.firstIndex(where: { $0.id == id }) else { return }
            if let k = feaLoads[i].faces.firstIndex(of: face) { feaLoads[i].faces.remove(at: k) } else { feaLoads[i].faces.append(face) }
        case .support(let id):
            guard let i = feaSupports.firstIndex(where: { $0.id == id }) else { return }
            if let k = feaSupports[i].faces.firstIndex(of: face) { feaSupports[i].faces.remove(at: k) } else { feaSupports[i].faces.append(face) }
        case nil: return
        }
        overlayRevision += 1
    }

    // MARK: Viewport picking (Picking.swift, model.picking)

    @ObservationIgnored private var pickHandler: UUID?

    /// Arm (or disarm, with nil) viewport face picking for a setup row. While
    /// armed, the viewport picks faces and the row mirrors the selection on
    /// the analysed part; the face list stays as a precise alternative.
    func setPickTarget(_ t: PickTarget?, _ m: EditorModel) {
        if let h = pickHandler { m.picking.removeHandler(h); pickHandler = nil }
        pickTarget = t
        guard let t else { m.picking.filter = .standard; overlayRevision += 1; return }
        m.picking.filter = .face
        m.picking.setSelection(faces(of: t).map { .face($0, part: feaPart) })
        pickHandler = m.picking.onSelectionChange { [weak self] refs in
            MainActor.assumeIsolated {
                guard let self, self.pickTarget == t else { return }
                if let part = refs.last?.part, part != self.feaPart { self.feaPart = part }
                self.setFaces(refs.compactMap { $0.kind == .face && $0.part == self.feaPart ? $0.id : nil }, for: t)
            }
        }
        overlayRevision += 1
    }

    private func faces(of t: PickTarget) -> [UInt32] {
        switch t {
        case .load(let id): feaLoads.first { $0.id == id }?.faces ?? []
        case .support(let id): feaSupports.first { $0.id == id }?.faces ?? []
        }
    }

    func setFaces(_ f: [UInt32], for t: PickTarget) {
        switch t {
        case .load(let id): if let i = feaLoads.firstIndex(where: { $0.id == id }) { feaLoads[i].faces = f }
        case .support(let id): if let i = feaSupports.firstIndex(where: { $0.id == id }) { feaSupports[i].faces = f }
        }
        overlayRevision += 1
    }

    func request() -> FeaRequest {
        FeaRequest(
            part_index: feaPart, material: feaMaterial, resolution: feaResolution, levels: 2,
            loads: feaLoads.filter { !$0.faces.isEmpty }.map { .init(faces: $0.faces, force: [$0.force.x, $0.force.y, $0.force.z]) },
            supports: feaSupports.filter { !$0.faces.isEmpty }.map { .init(faces: $0.faces, fix: $0.fix) })
    }

    func runFea(_ m: EditorModel) async {
        guard let data = Self.documentData(m) else { feaError = "No document is open."; return }
        let req = request()
        guard !req.loads.isEmpty else { feaError = "Pick at least one face for a load."; return }
        guard !req.supports.isEmpty else { feaError = "Pick at least one face for a support."; return }
        feaBusy = true; feaError = nil
        let r = await Task.detached { Result { try AnalysisFFI.feaSolve(doc: data, request: req) } }.value
        feaBusy = false
        switch r {
        case .success(let res):
            feaResult = res; feaKey = data
            showField()
        case .failure(let e):
            feaError = "\(e)"; feaResult = nil
        }
        overlayRevision += 1
    }

    /// Push the solved field into the shared field overlay (surface + slice).
    func showField() {
        guard let res = feaResult, let f = res.field else { return }
        FieldOverlayStore.shared.showSolved(res, field: f, kind: feaField)
    }

    // MARK: Clearance

    func runClearance(_ m: EditorModel) async {
        guard let data = Self.documentData(m) else { clearanceError = "No document is open."; return }
        clearanceBusy = true; clearanceError = nil
        let r = await Task.detached { Result { try AnalysisFFI.clearance(doc: data) } }.value
        clearanceBusy = false
        switch r {
        case .success(let rep): clearance = rep; clearanceKey = data
        case .failure(let e): clearanceError = "\(e)"; clearance = nil
        }
        overlayRevision += 1
    }

    func clearAll() {
        dfmReport = nil; feaResult = nil; clearance = nil
        dfmKey = nil; feaKey = nil; clearanceKey = nil
        FieldOverlayStore.shared.clear()
        overlayRevision += 1
    }
}

// MARK: - FEA → field overlay

extension FieldOverlayStore {
    /// Show an in-app FEA solve as a two-layer stack (von Mises, displacement)
    /// on the lattice cell grid. The solid mask is expressed as the stack's
    /// geometry boxes (one per run of solid cells along x), so surface skin,
    /// slice ghosting and colormap ranges all work exactly as for a file stack.
    func showSolved(_ res: FeaResult, field f: FeaResult.Field, kind: FeaFieldKind) {
        guard f.dims_zyx.count == 3, f.origin_mm.count == 3, f.voxel_mm.count == 3 else { return }
        let (nz, ny, nx) = (f.dims_zyx[0], f.dims_zyx[1], f.dims_zyx[2])
        let o = f.origin_mm, h = f.voxel_mm
        var boxes: [FieldStackFile.Box] = []
        for k in 0..<nz {
            for j in 0..<ny {
                var i = 0
                while i < nx {
                    guard f.mask[(k * ny + j) * nx + i] == 1 else { i += 1; continue }
                    let start = i
                    while i < nx, f.mask[(k * ny + j) * nx + i] == 1 { i += 1 }
                    boxes.append(.init(min: [o[0] + Double(start) * h[0], o[1] + Double(j) * h[1], o[2] + Double(k) * h[2]],
                                       max: [o[0] + Double(i) * h[0], o[1] + Double(j + 1) * h[1], o[2] + Double(k + 1) * h[2]]))
                }
            }
        }
        let stressDelta = res.max_von_mises_mpa * res.study.stress_change_rel
        func layer(_ id: String, _ label: String, _ data: [Float], _ range: [Double], _ units: String, _ bound: Double) -> FieldLayer {
            let meta = FieldMeta(field: label, units: units, plane: nil, dims_zyx: f.dims_zyx, origin_mm: o, voxel_mm: h,
                                 min: range.first ?? 0, max: range.last ?? 0,
                                 bound: .init(kind: "fea", value: bound, units: units), native_fnv1a: nil, format: "fea")
            let decoded = DecodedField(name: id, meta: meta, data: data, dims: (nz, ny, nx), hash: "",
                                       streamBytes: 0, readMs: 0, decodeMs: 0)
            return FieldLayer(id: id, label: label, field: decoded, range: (range.first ?? 0, range.last ?? 0))
        }
        let vm = layer("von_mises", "von Mises (MPa)", f.von_mises_mpa, f.von_mises_range, "MPa", stressDelta)
        let disp = layer("displacement", "Displacement (mm)", f.displacement_mm, f.displacement_range, "mm",
                         res.max_displacement_mm * res.study.displacement_change_rel)
        let notes = String(format: "%@ · %@ · σmax %.2f MPa · δmax %.4f mm%@%@", res.part_name, res.material,
                           res.max_von_mises_mpa, res.max_displacement_mm,
                           res.safety_factor.map { String(format: " · SF %.2f", $0) } ?? "",
                           res.converged ? "" : " · NOT CONVERGED")
        stack = FieldStackFile(name: "fea-\(res.part_index)", title: "Stress — \(res.part_name)", notes: notes,
                               geometry: .init(boxes: boxes),
                               layers: [.init(id: vm.id, label: vm.label, file: ""), .init(id: disp.id, label: disp.label, file: "")])
        layers = [vm, disp]
        activeId = kind == .vonMises ? vm.id : disp.id
        mode = .surface
        axis = .z
        slice = nz / 2
        error = nil
        totalMs = 0
        revision += 1
    }
}

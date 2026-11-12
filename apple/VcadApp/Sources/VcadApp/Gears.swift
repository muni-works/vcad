import Foundation
import simd

// Spur Gear tool: writes one parametric `SpurGear` IR node (the kernel builds
// the exact involute; see `vcad_ir::gear`) under a Rotate + Translate, so the
// gear's module, teeth, pressure angle, face width and bore stay editable in
// the inspector. "Mesh with selected gear" places the new gear at the standard
// centre distance with tooth-into-gap phase.

/// Parameters for a new spur gear.
struct SpurGearParams: Equatable {
    var module: Double = 2
    var teeth: Int = 20
    var pressureAngle: Double = 20
    var faceWidth: Double = 10
    var bore: Double = 8

    var pitchDiameter: Double { module * Double(teeth) }
    var tipDiameter: Double { module * Double(teeth + 2) }
    var rootDiameter: Double { module * (Double(teeth) - 2.5) }

    /// Why these parameters can't make a gear, or nil. Mirrors
    /// `GearSpec::validate` + `validate_bore` so the dialog says it up front.
    var problem: String? {
        if module <= 0 { return "Module must be greater than 0." }
        if teeth < 4 { return "A gear needs at least 4 teeth." }
        if !(10...35).contains(pressureAngle) { return "Pressure angle must be 10°–35°." }
        if faceWidth <= 0 { return "Face width must be greater than 0." }
        if bore < 0 { return "Bore can't be negative." }
        if bore >= rootDiameter - module {
            return "Bore must be under \(SpurGearMath.fmt(rootDiameter - module)) mm to leave a rim."
        }
        return nil
    }
}

/// Swift twins of `vcad_ir::gear::{center_distance, mesh_rotation_deg}`.
/// `GearTests` pins them to the Rust formulas.
enum SpurGearMath {
    static func centerDistance(module: Double, z1: Int, z2: Int) -> Double {
        module * Double(z1 + z2) / 2
    }

    /// Rotation (degrees) that meshes gear 2 with gear 1: tooth 0 of each
    /// gear is centred on its local +X; gear 1 is turned `rot1` and gear 2
    /// sits in direction `direction` from it.
    static func meshRotation(z1: Int, rot1: Double, z2: Int, direction: Double) -> Double {
        let tau1 = 360 / Double(z1), tau2 = 360 / Double(z2)
        let f = remEuclid((direction - rot1) / tau1, 1)
        return remEuclid(direction + 180 + (f - 0.5) * tau2, 360)
    }

    static func remEuclid(_ a: Double, _ b: Double) -> Double {
        let r = a.truncatingRemainder(dividingBy: b)
        return r < 0 ? r + b : r
    }

    static func fmt(_ v: Double) -> String { v.formatted(.number.precision(.fractionLength(0...3))) }
}

/// A gear already in the document: where it sits and what it is.
struct PlacedSpurGear: Equatable {
    var gearNodeId: Int
    var module: Double
    var teeth: Int
    var pressureAngle: Double
    var faceWidth: Double
    /// World centre (translation of the part).
    var center: SIMD3<Double>
    /// Rotation about Z, degrees.
    var rotationZ: Double
}

extension EditorModel {
    /// The spur gear behind viewport part `i`, when the part is one: a
    /// SpurGear under any chain of Translates, Z-only Rotates and unit Scales
    /// (the engine wraps every part in its own Scale → Rotate → Translate).
    func placedSpurGear(part i: Int) -> PlacedSpurGear? {
        guard let json = documentJSON, let root = rootNodeId(forPart: i) else { return nil }
        return Self.placedSpurGear(json, root: root)
    }

    static func placedSpurGear(_ json: [String: Any], root: Int) -> PlacedSpurGear? {
        var id = root
        var center = SIMD3<Double>(0, 0, 0)
        var rotZ = 0.0
        func vec(_ v: Any?) -> SIMD3<Double>? {
            guard let d = v as? [String: Any] else { return nil }
            return SIMD3(["x", "y", "z"].map { (d[$0] as? NSNumber)?.doubleValue ?? 0 })
        }
        for _ in 0..<12 {
            guard let op = DocEdit.op(json, nodeId: id), let type = op["type"] as? String else { return nil }
            switch type {
            case "Translate":
                // Walking root → leaf, an inner translate is turned by every
                // rotate above it.
                guard let o = vec(op["offset"]) else { return nil }
                let t = rotZ * .pi / 180
                center += SIMD3(o.x * cos(t) - o.y * sin(t), o.x * sin(t) + o.y * cos(t), o.z)
            case "Rotate":
                guard let a = vec(op["angles"]), abs(a.x) < 1e-9, abs(a.y) < 1e-9 else { return nil }
                rotZ += a.z
            case "Scale":
                guard let f = vec(op["factor"]), simd_length(f - SIMD3(1, 1, 1)) < 1e-9 else { return nil }
            case "SpurGear":
                func num(_ k: String) -> Double? { (op[k] as? NSNumber)?.doubleValue }
                guard let m = num("module"), let z = num("teeth"), let fw = num("face_width") else { return nil }
                return PlacedSpurGear(gearNodeId: id, module: m, teeth: Int(z),
                                      pressureAngle: num("pressure_angle_deg") ?? 20, faceWidth: fw,
                                      center: center, rotationZ: rotZ)
            default:
                return nil
            }
            guard let child = (op["child"] as? NSNumber)?.intValue else { return nil }
            id = child
        }
        return nil
    }

    /// The part to mesh with: the selected part, when it's a spur gear.
    var selectedSpurGear: PlacedSpurGear? {
        (selectedPartIndex ?? picking.selectionPart).flatMap { placedSpurGear(part: $0) }
    }

    /// Add a spur gear as one undo step. With `meshWith`, the new gear takes
    /// that gear's module and pressure angle, sits at the standard centre
    /// distance along +X from it, at the same height, phased tooth-into-gap.
    /// Otherwise it goes at the origin — or, when a part is selected, just
    /// past that part's +X side.
    @discardableResult
    func addSpurGear(_ p: SpurGearParams, meshWith mate: PlacedSpurGear? = nil) -> Bool {
        var p = p
        if let mate { p.module = mate.module; p.pressureAngle = mate.pressureAngle }
        if let why = p.problem { engineError = why; return false }

        var center = SIMD3<Double>(0, 0, 0)
        var rotZ = 0.0
        if let mate {
            let a = SpurGearMath.centerDistance(module: p.module, z1: mate.teeth, z2: p.teeth)
            center = mate.center + SIMD3(a, 0, 0)
            rotZ = SpurGearMath.meshRotation(z1: mate.teeth, rot1: mate.rotationZ, z2: p.teeth, direction: 0)
        } else if let i = selectedPartIndex ?? picking.selectionPart, i < docPartMeshes.count {
            let b = docPartMeshes[i]
            center = SIMD3(Double(b.hi.x) + p.tipDiameter / 2 + 5, Double(b.lo.y + b.hi.y) / 2, Double(b.lo.z))
        }

        guard promoteSandboxToDocument(blank: true) else {
            engineError = engineError ?? "Couldn't start a document."
            return false
        }
        let name = mate != nil ? "Mesh Spur Gear" : "Spur Gear"
        let ok = editIR(name) { doc in
            var nodes = doc["nodes"] as? [String: Any] ?? [:]
            let gear = (nodes.keys.compactMap { Int($0) }.max() ?? 0) + 1
            let rot = gear + 1, move = gear + 2
            let label = "Spur Gear \(p.teeth)T m\(SpurGearMath.fmt(p.module))"
            nodes["\(gear)"] = ["id": gear, "name": label,
                                "op": ["type": "SpurGear", "module": p.module, "teeth": p.teeth,
                                       "pressure_angle_deg": p.pressureAngle, "face_width": p.faceWidth,
                                       "bore_diameter": p.bore, "backlash": 0.0] as [String: Any]]
            nodes["\(rot)"] = ["id": rot, "name": "Rotate",
                               "op": ["type": "Rotate", "child": gear,
                                      "angles": ["x": 0.0, "y": 0.0, "z": rotZ]] as [String: Any]]
            nodes["\(move)"] = ["id": move, "name": label,
                                "op": ["type": "Translate", "child": rot,
                                       "offset": ["x": center.x, "y": center.y, "z": center.z]] as [String: Any]]
            doc["nodes"] = nodes
            var roots = doc["roots"] as? [[String: Any]] ?? []
            roots.append(["root": move, "material": "default"])
            doc["roots"] = roots
        }
        guard ok else { return false }
        if let failed = newEvaluationErrors(), !failed.isEmpty {
            undo()
            clearRedo()
            engineError = "The gear couldn't be built, so it was undone:\n\(failed)"
            return false
        }
        selectedFeatureID = featureNodes.last?.id
        return true
    }
}

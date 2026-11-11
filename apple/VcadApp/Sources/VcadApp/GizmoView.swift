import RealityKit
import simd
#if canImport(AppKit)
import AppKit
#endif

// The transform gizmo and direct-manipulation handles.
//
// Design: hairline, screen-space ink (Shaders/GizmoLines.metal) instead of
// lit tubes. Each axis is a thin line that starts clear of the hub with a
// small solid tip; plane handles are tiny outlined squares; rotation is a
// quarter arc per axis, not three full rings. Nothing competes with the
// model. Hover brightens and thickens the one handle under the pointer and
// quietly dims the rest; while a handle is held, only it and a faint guide
// along its axis remain. The whole root is rescaled per frame
// (`gizmoScreenScale`) so it is the same size on screen at any zoom.

@MainActor
enum GizmoStyle {
    // Widths are device pixels (retina: 2 px = 1 pt).
    static let lineW: Float = 3.0
    static let lineHover: Float = 5.0
    static let arcW: Float = 2.5
    static let arcHover: Float = 4.5
    static let dimAlpha: CGFloat = 0.32
    static let handleInk = NSColor(srgbRed: 1.0, green: 0.62, blue: 0.12, alpha: 1)   // brand orange

    static var viewportHeightPx: Float {
        let v = ReleaseWindowController.shared.arView
        return Float((v?.bounds.height ?? 900) * (v?.window?.backingScaleFactor ?? 2))
    }

    /// Screen-space line material in `color`, or nil without the shader lib.
    static func line(_ color: NSColor, alpha: CGFloat, widthPx: Float) -> CustomMaterial? {
        guard var m = SceneAssets.gizmoLineMaterial(viewportHeightPx: viewportHeightPx, widthPx: widthPx)
        else { return nil }
        m.baseColor = .init(tint: color.withAlphaComponent(1))
        m.blending = .transparent(opacity: .init(floatLiteral: Float(alpha)))
        return m
    }
}

extension SceneAssets {
    static func gizmoLineMaterial(viewportHeightPx: Float, widthPx: Float) -> CustomMaterial? {
        guard let url = Bundle.main.url(forResource: "vcad", withExtension: "metallib"),
              let device = MTLCreateSystemDefaultDevice(),
              let lib = try? device.makeLibrary(URL: url) else { return nil }
        let geo = CustomMaterial.GeometryModifier(named: "gizmoLineGeometry", in: lib)
        let surf = CustomMaterial.SurfaceShader(named: "gizmoLineSurface", in: lib)
        guard var m = try? CustomMaterial(surfaceShader: surf, geometryModifier: geo, lightingModel: .unlit)
        else { return nil }
        let pxPerUnit = 2 * tan(Float(60) / 2 * .pi / 180) / max(viewportHeightPx, 1)
        m.custom.value = SIMD4(pxPerUnit, widthPx, 0, 0.004)
        return m
    }
}

/// Polyline (pairs of points) drawn as constant-width screen lines, with a
/// thin-cylinder fallback when the shader library isn't bundled.
@MainActor
private func inkLines(_ segs: [SIMD3<Float>], color: NSColor, alpha: CGFloat, widthPx: Float,
                      fallbackRadius: Float) -> Entity {
    let e = Entity()
    if let mat = GizmoStyle.line(color, alpha: alpha, widthPx: widthPx),
       let r = EdgeOverlay.screenLineResource(segments: segs, name: "gizmoInk") {
        e.addChild(ModelEntity(mesh: r, materials: [mat]))
        return e
    }
    let mat = UnlitMaterial(color: color.withAlphaComponent(alpha))
    var i = 0
    while i + 1 < segs.count {
        let a = segs[i], b = segs[i + 1]
        let d = b - a, l = simd_length(d)
        if l > 1e-6 {
            let c = ModelEntity(mesh: .generateCylinder(height: l, radius: fallbackRadius * widthPx / 2),
                                materials: [mat])
            c.position = (a + b) / 2
            c.orientation = simd_quatf(from: SIMD3(0, 1, 0), to: d / l)
            e.addChild(c)
        }
        i += 2
    }
    return e
}

@MainActor
private func grab(_ name: String, at p: SIMD3<Float>, size: SIMD3<Float>, orient: simd_quatf = simd_quatf()) -> Entity {
    let hit = ModelEntity()
    hit.name = name
    hit.position = p
    hit.orientation = orient
    hit.components.set(CollisionComponent(shapes: [.generateBox(size: size)]))
    hit.components.set(InputTargetComponent())
    return hit
}

/// The gizmo root (kernel coords, parented under `centering`): translate
/// axes, plane squares, rotation arcs — or, with a planar face picked, a
/// single press-pull arrow — plus the selected feature's handle.
@MainActor
func buildGizmo(model: EditorModel) -> Entity {
    let root = Entity(); root.name = "gizmoRoot"
    let s = gizmoScreenScale(model: model)
    let len = model.gizmoArmLength()
    let hov = model.hoveredGizmoHandle
    let active = model.manip.activeName
    let px = len * 0.004          // fallback-cylinder radius per px of width
    func dim(_ name: String) -> CGFloat {
        if let active { return active == name ? 1 : 0 }
        if let hov { return hov == name ? 1 : GizmoStyle.dimAlpha }
        return 1
    }

    // Press-pull: one arrow off the face, in brand orange.
    if let (_, f) = model.pullFace {
        root.position = f.centerF
        root.scale = SIMD3(repeating: s)
        let n = simd_normalize(f.normalF)
        let on = hov == "pull" || active == "pull"
        let ink = on ? GizmoInk.brighten(GizmoStyle.handleInk) : GizmoStyle.handleInk
        let a0 = len * 0.08, a1 = len * 0.78
        root.addChild(inkLines([n * a0, n * a1], color: ink, alpha: 1,
                               widthPx: on ? GizmoStyle.lineHover : GizmoStyle.lineW, fallbackRadius: px))
        let head = ModelEntity(mesh: .generateCone(height: len * 0.16, radius: len * (on ? 0.055 : 0.045)),
                               materials: [UnlitMaterial(color: ink)])
        head.orientation = simd_quatf(from: SIMD3(0, 1, 0), to: n)
        head.position = n * (a1 + len * 0.08)
        root.addChild(head)
        let dot = ModelEntity(mesh: .generateSphere(radius: len * 0.03), materials: [UnlitMaterial(color: ink)])
        root.addChild(dot)
        if active == "pull" {
            root.addChild(inkLines([n * -len * 4, n * len * 4], color: ink, alpha: 0.28, widthPx: 1, fallbackRadius: px))
        }
        root.addChild(grab("pull", at: n * (len * 0.5), size: SIMD3(len * 0.18, len, len * 0.18),
                           orient: simd_quatf(from: SIMD3(0, 1, 0), to: n)))
        return drawOnTop(root)
    }

    guard let c = model.gizmoCenterKernel() else { return root }
    root.position = c
    root.scale = SIMD3(repeating: s)

    // Translate axes.
    let axes: [(name: String, dir: SIMD3<Float>, color: NSColor)] = [
        ("gizmoX", SIMD3(1, 0, 0), GizmoInk.x),
        ("gizmoY", SIMD3(0, 1, 0), GizmoInk.y),
        ("gizmoZ", SIMD3(0, 0, 1), GizmoInk.z),
    ]
    let a0 = len * 0.2, a1 = len * 0.88, headLen = len * 0.15
    for a in axes {
        let alpha = dim(a.name)
        guard alpha > 0 else { continue }
        let on = hov == a.name || active == a.name
        let ink = on ? GizmoInk.brighten(a.color) : a.color
        root.addChild(inkLines([a.dir * a0, a.dir * a1], color: ink, alpha: alpha,
                               widthPx: on ? GizmoStyle.lineHover : GizmoStyle.lineW, fallbackRadius: px))
        let head = ModelEntity(mesh: .generateCone(height: headLen, radius: len * (on ? 0.05 : 0.04)),
                               materials: [UnlitMaterial(color: ink.withAlphaComponent(alpha))])
        head.orientation = simd_quatf(from: SIMD3(0, 1, 0), to: a.dir)
        head.position = a.dir * (a1 + headLen / 2)
        root.addChild(head)
        if active == a.name {
            root.addChild(inkLines([a.dir * -len * 6, a.dir * len * 6], color: a.color, alpha: 0.3,
                                   widthPx: 1, fallbackRadius: px))
        }
        root.addChild(grab(a.name, at: a.dir * (len * 0.6), size: SIMD3(len * 0.16, len * 0.85, len * 0.16),
                           orient: simd_quatf(from: SIMD3(0, 1, 0), to: a.dir)))
    }

    // Plane squares: outline + whisper fill, coloured by the plane normal.
    let pOff = len * 0.3, pSize = len * 0.13
    let planes: [(name: String, a: SIMD3<Float>, b: SIMD3<Float>, color: NSColor)] = [
        ("planeXY", SIMD3(1, 0, 0), SIMD3(0, 1, 0), GizmoInk.z),
        ("planeYZ", SIMD3(0, 1, 0), SIMD3(0, 0, 1), GizmoInk.x),
        ("planeXZ", SIMD3(1, 0, 0), SIMD3(0, 0, 1), GizmoInk.y),
    ]
    for p in planes {
        let alpha = dim(p.name)
        guard alpha > 0 else { continue }
        let on = hov == p.name || active == p.name
        let ink = on ? GizmoInk.brighten(p.color) : p.color
        let o = (p.a + p.b) * pOff
        let h = pSize / 2
        let q = [o - p.a * h - p.b * h, o + p.a * h - p.b * h, o + p.a * h + p.b * h, o - p.a * h + p.b * h]
        root.addChild(inkLines([q[0], q[1], q[1], q[2], q[2], q[3], q[3], q[0]], color: ink, alpha: alpha,
                               widthPx: on ? 2.2 : 1.25, fallbackRadius: px))
        let n = simd_normalize(simd_cross(p.a, p.b))
        let rot = simd_quatf(from: SIMD3(0, 0, 1), to: n)
        var fill = UnlitMaterial(color: ink)
        fill.blending = .transparent(opacity: .init(floatLiteral: Float((on ? 0.38 : 0.12) * alpha)))
        let sq = ModelEntity(mesh: .generatePlane(width: pSize, height: pSize), materials: [fill])
        sq.orientation = rot; sq.position = o
        root.addChild(sq)
        let back = sq.clone(recursive: false)
        back.orientation = rot * simd_quatf(angle: .pi, axis: SIMD3(1, 0, 0))
        root.addChild(back)
        root.addChild(grab(p.name, at: o, size: SIMD3(pSize * 1.3, pSize * 1.3, pSize * 0.4), orient: rot))
    }

    // Rotation: a quarter arc per axis, between the other two axes, out past
    // the plane squares — enough to grab, never a cage around the part.
    let ringR = len * 0.62
    let rings: [(name: String, axis: SIMD3<Float>, color: NSColor)] = [
        ("rotX", SIMD3(1, 0, 0), GizmoInk.x),
        ("rotY", SIMD3(0, 1, 0), GizmoInk.y),
        ("rotZ", SIMD3(0, 0, 1), GizmoInk.z),
    ]
    for r in rings {
        let alpha = dim(r.name)
        guard alpha > 0 else { continue }
        let on = hov == r.name || active == r.name
        let ink = on ? GizmoInk.brighten(r.color) : r.color
        let (u1, u2) = EditorModel.ringBasis(r.axis)
        // While spinning, the whole circle shows where the part can go.
        let full = active == r.name
        let lo: Float = full ? 0 : 0.12, hi: Float = full ? 2 * .pi : .pi / 2 - 0.12
        let n = full ? 64 : 18
        var segs: [SIMD3<Float>] = []
        for i in 0..<n {
            let t0 = lo + (hi - lo) * Float(i) / Float(n)
            let t1 = lo + (hi - lo) * Float(i + 1) / Float(n)
            segs.append((u1 * cos(t0) + u2 * sin(t0)) * ringR)
            segs.append((u1 * cos(t1) + u2 * sin(t1)) * ringR)
        }
        root.addChild(inkLines(segs, color: ink, alpha: alpha,
                               widthPx: on ? GizmoStyle.arcHover : GizmoStyle.arcW, fallbackRadius: px))
        if !full {
            var i = 0
            while i < segs.count {
                let a = segs[i], b = segs[i + 1], d = b - a, l = simd_length(d)
                root.addChild(grab(r.name, at: (a + b) / 2, size: SIMD3(l * 1.1, len * 0.09, len * 0.09),
                                   orient: simd_quatf(from: SIMD3(1, 0, 0), to: d / l)))
                i += 4
            }
        }
    }

    // Hub.
    if active == nil {
        let hub = ModelEntity(mesh: .generateSphere(radius: len * 0.035),
                              materials: [UnlitMaterial(color: NSColor(white: 0.96, alpha: 1))])
        root.addChild(hub)
    }

    // Feature handles (extrude depth, fillet/chamfer size, shell wall): an
    // orange knob on a short stem at the driving geometry.
    if active == nil || active?.hasPrefix("fh:") == true {
        for h in model.featureHandles() {
            let local = (h.point - c) / s
            let on = hov == h.name || active == h.name
            let ink = on ? GizmoInk.brighten(GizmoStyle.handleInk) : GizmoStyle.handleInk
            let stem = len * 0.22
            root.addChild(inkLines([local, local + h.dir * stem], color: ink, alpha: 1,
                                   widthPx: on ? 2.5 : 1.5, fallbackRadius: px))
            let knob = ModelEntity(mesh: .generateSphere(radius: len * (on ? 0.05 : 0.04)),
                                   materials: [UnlitMaterial(color: ink)])
            knob.position = local + h.dir * stem
            root.addChild(knob)
            root.addChild(grab(h.name, at: local + h.dir * stem, size: SIMD3(repeating: len * 0.14)))
        }
    }
    return drawOnTop(root)
}

/// The gizmo sits at the part's centre, inside the solid: draw it over
/// everything (no depth test) so it is never buried in the geometry.
@MainActor
private func drawOnTop(_ root: Entity) -> Entity {
    func visit(_ e: Entity) {
        if let m = e as? ModelEntity, var model = m.model {
            model.materials = model.materials.map { mat -> any RealityKit.Material in
                if var u = mat as? UnlitMaterial { u.readsDepth = false; return u }
                if var c = mat as? CustomMaterial { c.readsDepth = false; return c }
                return mat
            }
            m.model = model
        }
        e.children.forEach(visit)
    }
    visit(root)
    return root
}

extension ReleaseWindowController {
    /// Re-hang the gizmo now (hover, grab, release, face pick).
    @MainActor
    func rebuildGizmo(model: EditorModel) {
        guard let centering = centeringEntity else { return }
        centering.findEntity(named: "gizmoRoot")?.removeFromParent()
        guard model.showsGizmo else { return }
        centering.addChild(buildGizmo(model: model))
    }
}

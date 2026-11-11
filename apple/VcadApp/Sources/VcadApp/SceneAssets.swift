import RealityKit
import CoreGraphics
import Metal
#if canImport(AppKit)
import AppKit
#endif

// Procedurally drawn scene assets shared by every viewport: the studio and
// zebra environments (image-based lighting), the zebra chrome, the reference
// grid and the contact shadow. No bundled HDRs or textures.

@MainActor
enum SceneAssets {
    /// A dark studio environment drawn procedurally, used for image-based
    /// reflections on the geometry. A soft ceiling, a horizon band, and three
    /// softbox highlights at varied azimuths give metals real reflection
    /// structure as the camera orbits.
    static let studioEnvironment: EnvironmentResource? = makeStudioEnvironment()
    static let zebraEnvironment: EnvironmentResource? = makeZebraEnvironment()

    /// Mirror-chrome for zebra analysis: near-zero roughness so the striped
    /// environment reflects as sharp bands.
    static let zebraChrome: PhysicallyBasedMaterial = {
        var m = PhysicallyBasedMaterial()
        m.baseColor = .init(tint: .white)
        m.metallic = 1.0
        m.roughness = 0.02
        return m
    }()

    /// Near-black unlit material for the feature-edge overlay — reads as ink
    /// lines over the shaded surfaces, the classic CAD look.
    static let edgeMaterial = UnlitMaterial(color: NSColor(white: 0.02, alpha: 1.0))

    /// The compiled viewport shaders (`Shaders/*.metal` → vcad.metallib,
    /// built by bundle.sh). nil in `swift test`/dev runs without it.
    private static let shaderLibrary: MTLLibrary? = {
        guard let url = Bundle.main.url(forResource: "vcad", withExtension: "metallib"),
              let device = MTLCreateSystemDefaultDevice() else { return nil }
        return try? device.makeLibrary(URL: url)
    }()

    /// Screen-space, antialiased ink lines (constant pixel width, never
    /// z-fighting the surface). nil when the shader library is missing —
    /// callers fall back to `edgeMaterial` ribbons.
    static func edgeLineMaterial(viewportHeightPx: Float, widthPx: Float = 1.5) -> CustomMaterial? {
        guard let lib = shaderLibrary else { return nil }
        let geo = CustomMaterial.GeometryModifier(named: "edgeLineGeometry", in: lib)
        let surf = CustomMaterial.SurfaceShader(named: "edgeLineSurface", in: lib)
        guard var m = try? CustomMaterial(surfaceShader: surf, geometryModifier: geo, lightingModel: .unlit)
        else { return nil }
        let pxPerUnit = 2 * tan(Float(60) / 2 * .pi / 180) / max(viewportHeightPx, 1)  // PerspectiveCamera FOV
        m.custom.value = SIMD4(pxPerUnit, widthPx, 0.02, 0.0015)
        return m
    }

    /// Coloured screen-space lines for selection/hover edge highlights:
    /// thicker than the ink and pulled further toward the camera so they sit
    /// on top of the ink lines they trace. nil without the shader library.
    static func highlightLineMaterial(viewportHeightPx: Float, widthPx: Float,
                                      color: NSColor) -> CustomMaterial? {
        guard let lib = shaderLibrary else { return nil }
        let geo = CustomMaterial.GeometryModifier(named: "edgeLineGeometry", in: lib)
        let surf = CustomMaterial.SurfaceShader(named: "edgeLineTintSurface", in: lib)
        guard var m = try? CustomMaterial(surfaceShader: surf, geometryModifier: geo, lightingModel: .unlit)
        else { return nil }
        let pxPerUnit = 2 * tan(Float(60) / 2 * .pi / 180) / max(viewportHeightPx, 1)
        m.custom.value = SIMD4(pxPerUnit, widthPx, 0, 0.003)
        m.baseColor = .init(tint: color)
        return m
    }

    /// Equirect of bold horizontal bands: the classic zebra-analysis light box.
    /// Reflected off a chromed surface, stripe kinks expose G1/G2 breaks.
    /// Viewport-only material softening, Fusion/Shapr3D style: dielectrics get
    /// a lower specular level and a roughness floor so the key light leaves a
    /// broad sheen rather than a hard, clipped hotspot. Metals keep their
    /// roughness (their look *is* the reflection) but still get the floor
    /// halved so mirror-polished parts don't spark.
    static func studioSoften(_ m: inout PhysicallyBasedMaterial) {
        let metal = m.metallic.scale
        let floor: Float = metal > 0.5 ? 0.22 : 0.42
        if m.roughness.scale < floor { m.roughness = .init(floatLiteral: floor) }
        m.specular = .init(floatLiteral: metal > 0.5 ? 0.5 : 0.3)
    }

    private static func makeZebraEnvironment() -> EnvironmentResource? {
        let w = 2048, h = 1024
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8,
                                  bytesPerRow: 0, space: cs,
                                  bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return nil }
        ctx.setFillColor(CGColor(gray: 0.03, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
        let bands = 32
        let bandH = CGFloat(h) / CGFloat(bands)
        ctx.setFillColor(CGColor(gray: 0.98, alpha: 1))
        for i in stride(from: 0, to: bands, by: 2) {
            ctx.fill(CGRect(x: 0, y: CGFloat(i) * bandH, width: CGFloat(w), height: bandH))
        }
        guard let img = ctx.makeImage() else { return nil }
        return try? EnvironmentResource(equirectangular: img)
    }

    private static func makeStudioEnvironment() -> EnvironmentResource? {
        let w = 2048, h = 1024
        let cs = CGColorSpaceCreateDeviceRGB()
        guard let ctx = CGContext(data: nil, width: w, height: h, bitsPerComponent: 8,
                                  bytesPerRow: 0, space: cs,
                                  bitmapInfo: CGImageAlphaInfo.noneSkipLast.rawValue) else { return nil }
        let fw = CGFloat(w), fh = CGFloat(h)
        let base = CGGradient(colorsSpace: cs, colors: [
            CGColor(red: 0.30, green: 0.31, blue: 0.34, alpha: 1),
            CGColor(red: 0.21, green: 0.22, blue: 0.25, alpha: 1),
            CGColor(red: 0.13, green: 0.135, blue: 0.15, alpha: 1),
            CGColor(red: 0.07, green: 0.07, blue: 0.08, alpha: 1),
            CGColor(red: 0.04, green: 0.04, blue: 0.045, alpha: 1),
        ] as CFArray, locations: [0, 0.34, 0.52, 0.74, 1])!
        ctx.drawLinearGradient(base, start: CGPoint(x: 0, y: fh), end: CGPoint(x: 0, y: 0), options: [])

        ctx.setBlendMode(.plusLighter)
        let horizon = CGGradient(colorsSpace: cs, colors: [
            CGColor(red: 0.16, green: 0.18, blue: 0.22, alpha: 0),
            CGColor(red: 0.16, green: 0.18, blue: 0.22, alpha: 0.7),
            CGColor(red: 0.16, green: 0.18, blue: 0.22, alpha: 0),
        ] as CFArray, locations: [0, 0.5, 1])!
        ctx.drawLinearGradient(horizon,
            start: CGPoint(x: 0, y: fh * 0.40), end: CGPoint(x: 0, y: fh * 0.56), options: [])

        func softbox(_ cx: CGFloat, _ cy: CGFloat, _ rad: CGFloat, _ c: CGColor) {
            let g = CGGradient(colorsSpace: cs, colors: [c, c.copy(alpha: 0)!] as CFArray, locations: [0, 1])!
            ctx.drawRadialGradient(g, startCenter: CGPoint(x: cx, y: cy), startRadius: 0,
                                   endCenter: CGPoint(x: cx, y: cy), endRadius: rad, options: [])
        }
        softbox(fw * 0.20, fh * 0.95, fh * 0.30, CGColor(red: 0.20, green: 0.23, blue: 0.28, alpha: 1))
        softbox(fw * 0.56, fh * 0.97, fh * 0.22, CGColor(red: 0.22, green: 0.20, blue: 0.17, alpha: 1))
        softbox(fw * 0.83, fh * 0.93, fh * 0.24, CGColor(red: 0.15, green: 0.18, blue: 0.22, alpha: 1))
        guard let img = ctx.makeImage() else { return nil }
        return try? EnvironmentResource(equirectangular: img)
    }

    // MARK: reference grid + contact shadow

    /// The ground grid's minor step (mm) for a part of the given size.
    static func gridStepMM(forPartSize size: Float) -> Float {
        GridSpacing.ground(extentMM: size).minor
    }

    /// One major cell of grid as a repeating coverage tile: hairline minor
    /// lines, a slightly heavier major line on the tile edge. Sampled with
    /// wrap addressing, so a quad of any size draws crisp lines from a 640 px
    /// texture — no per-line entities, no giant atlas — and mipmaps thin the
    /// lines out toward the background at distance.
    private static var gridTiles: [Bool: MaterialParameters.Texture] = [:]
    static let gridTilePixelsPerMinor = 192
    static func gridTile(strong: Bool) -> MaterialParameters.Texture? {
        if let t = gridTiles[strong] { return t }
        let px = gridTilePixelsPerMinor
        let sz = px * GridSpacing.majorEvery
        let boost: Float = strong ? 0.85 : 1
        let minorA: Float = 0.16 * boost, majorA: Float = 0.34 * boost
        // Coverage of the nearest line at each texel centre; the tile edge
        // (texel 0, wrapping) is the major line.
        var line = [Float](repeating: 0, count: sz)
        for p in 0..<sz {
            let t = Float(p) + 0.5
            let i = Int((t / Float(px)).rounded())
            let d = abs(t - Float(i * px))
            let major = i % GridSpacing.majorEvery == 0
            line[p] = major ? majorA * max(0, min(1, 1.0 - d)) : minorA * max(0, min(1, 0.75 - d))
        }
        var bytes = [UInt8](repeating: 0, count: sz * sz)
        for y in 0..<sz {
            for x in 0..<sz {
                bytes[y * sz + x] = UInt8((max(line[x], line[y]) * 255).rounded())
            }
        }
        guard let provider = CGDataProvider(data: Data(bytes) as CFData),
              let img = CGImage(width: sz, height: sz, bitsPerComponent: 8, bitsPerPixel: 8,
                                bytesPerRow: sz, space: CGColorSpaceCreateDeviceGray(),
                                bitmapInfo: CGBitmapInfo(rawValue: CGImageAlphaInfo.none.rawValue),
                                provider: provider, decode: nil, shouldInterpolate: true,
                                intent: .defaultIntent),
              let tex = try? TextureResource(image: img, options: .init(semantic: .raw,
                                                                         mipmapsMode: .allocateAndGenerateAll))
        else { return nil }
        let sd = MTLSamplerDescriptor()
        sd.sAddressMode = .repeat; sd.tAddressMode = .repeat
        sd.minFilter = .linear; sd.magFilter = .linear; sd.mipFilter = .linear
        sd.maxAnisotropy = 8
        let param = MaterialParameters.Texture(tex, sampler: .init(sd))
        gridTiles[strong] = param
        return param
    }

    /// Opacity of the fade ring at radius fraction `r` (0 centre … 1 rim):
    /// fully drawn through the middle, smoothly to nothing at the rim, so
    /// there is never a hard horizon.
    static func gridFade(_ r: Float) -> Float {
        let u = max(0, min(1, (r - 0.28) / 0.72))
        return 1 - u * u * (3 - 2 * u)
    }

    /// Radii (fractions of the half-side) of the fade rings, centre disc first.
    private static let gridRingRadii: [Float] = [0, 0.28] + (1...16).map { 0.28 + 0.72 * Float($0) / 16 }

    /// A grid in its local XZ plane (normal +Y), centred on a major crossing:
    /// a disc + fade rings carrying the repeating tile (one material per ring,
    /// stepping its opacity down), and optional red X / green Y axis strips
    /// that fade the same way. `scale` maps millimetres to entity units.
    static func gridEntity(name: String, layout: GridSpacing.Layout, scale: Float,
                           dark: Bool, axes: Bool, strong: Bool = false) -> Entity? {
        guard let tile = gridTile(strong: strong), scale > 0 else { return nil }
        let half = layout.sideMM * scale / 2
        let majorW = layout.major * scale
        let segs = 96
        var pos: [SIMD3<Float>] = [], uv: [SIMD2<Float>] = [], idx: [UInt32] = [], faceMat: [UInt32] = []
        func vertex(_ r: Float, _ k: Int) -> UInt32 {
            let a = Float(k) / Float(segs) * 2 * .pi
            let p = SIMD3<Float>(cos(a) * r * half, 0, sin(a) * r * half)
            pos.append(p); uv.append(SIMD2(p.x, p.z) / majorW)
            return UInt32(pos.count - 1)
        }
        func tri(_ a: UInt32, _ b: UInt32, _ c: UInt32, _ m: Int) {
            idx += [a, c, b, a, b, c]   // both faces
            faceMat += [UInt32(m), UInt32(m)]
        }
        let ink = NSColor(white: dark ? 1 : 0, alpha: 1)
        var materials: [UnlitMaterial] = []
        let radii = gridRingRadii
        for ring in 0..<(radii.count - 1) {
            let r0 = radii[ring], r1 = radii[ring + 1]
            var m = UnlitMaterial()
            m.color = .init(tint: ink)
            m.blending = .transparent(opacity: .init(scale: gridFade((r0 + r1) / 2), texture: tile))
            materials.append(m)
            for k in 0..<segs {
                if r0 == 0 {
                    pos.append(.zero); uv.append(.zero)
                    let c = UInt32(pos.count - 1)
                    tri(c, vertex(r1, k), vertex(r1, k + 1), ring)
                } else {
                    let a = vertex(r0, k), b = vertex(r0, k + 1), c = vertex(r1, k), d = vertex(r1, k + 1)
                    tri(a, c, d, ring); tri(a, d, b, ring)
                }
            }
        }
        var md = MeshDescriptor(name: name)
        md.positions = MeshBuffers.Positions(pos)
        md.normals = MeshBuffers.Normals(Array(repeating: SIMD3<Float>(0, 1, 0), count: pos.count))
        md.textureCoordinates = MeshBuffers.TextureCoordinates(uv)
        md.primitives = .triangles(idx)
        md.materials = .perFace(faceMat)
        guard let mesh = try? MeshResource.generate(from: [md]) else { return nil }
        let grid = ModelEntity(mesh: mesh, materials: materials)
        grid.name = name
        if axes, let strips = axisStrips(half: half, width: layout.minor * scale * 0.025, dark: dark) {
            strips.position.y = layout.minor * scale * 0.002
            grid.addChild(strips)
        }
        return grid
    }

    /// Red X (along local x) and green Y (along local z) axis strips through
    /// the centre, segmented so each piece fades with the grid rings.
    private static func axisStrips(half: Float, width w: Float, dark: Bool) -> Entity? {
        let n = 24
        var pos: [SIMD3<Float>] = [], idx: [UInt32] = [], faceMat: [UInt32] = []
        var materials: [UnlitMaterial] = []
        let colors = [NSColor(srgbRed: 0.86, green: 0.24, blue: 0.22, alpha: 1),
                      NSColor(srgbRed: 0.22, green: 0.66, blue: 0.30, alpha: 1)]
        let peak: Float = dark ? 0.8 : 0.7
        for (axis, color) in colors.enumerated() {
            for s in 0..<n {
                let t0 = -1 + 2 * Float(s) / Float(n), t1 = -1 + 2 * Float(s + 1) / Float(n)
                var m = UnlitMaterial()
                m.color = .init(tint: color)
                m.blending = .transparent(opacity: .init(floatLiteral: peak * gridFade(abs(t0 + t1) / 2)))
                materials.append(m)
                func p(_ t: Float, _ o: Float) -> SIMD3<Float> {
                    axis == 0 ? [t * half, 0, o] : [o, 0, t * half]
                }
                let b = UInt32(pos.count)
                pos += [p(t0, -w / 2), p(t1, -w / 2), p(t1, w / 2), p(t0, w / 2)]
                idx += [b, b + 2, b + 1, b, b + 3, b + 2, b, b + 1, b + 2, b, b + 2, b + 3]
                let mi = UInt32(materials.count - 1)
                faceMat += [mi, mi, mi, mi]
            }
        }
        var md = MeshDescriptor(name: "axes")
        md.positions = MeshBuffers.Positions(pos)
        md.normals = MeshBuffers.Normals(Array(repeating: SIMD3<Float>(0, 1, 0), count: pos.count))
        md.primitives = .triangles(idx)
        md.materials = .perFace(faceMat)
        guard let mesh = try? MeshResource.generate(from: [md]) else { return nil }
        let e = ModelEntity(mesh: mesh, materials: materials)
        e.name = "gridAxes"
        return e
    }

    /// The live sketch-plane grid: on the sketch frame at the snap step
    /// (coarsened by decades when the snap is too fine to read), with the
    /// sketch axes through its origin, fading at the edges. Kernel
    /// coordinates — parent it under the centering entity, like the ink.
    static func sketchGridEntity(frame: SketchFrame, snapMM: Double, extentMM: Float,
                                 dark: Bool) -> Entity? {
        let layout = GridSpacing.sketch(snapMM: Float(snapMM), extentMM: extentMM)
        guard let e = gridEntity(name: "sketchGrid", layout: layout, scale: 1, dark: dark,
                                 axes: true, strong: true) else { return nil }
        let x = simd_normalize(SIMD3<Float>(frame.xDir)), n = simd_normalize(SIMD3<Float>(frame.normal))
        e.orientation = simd_quatf(simd_float3x3(columns: (x, n, simd_cross(x, n))))
        // A hair off the plane so a sketch on a face doesn't z-fight the face.
        e.position = SIMD3<Float>(frame.origin) + n * max(extentMM, 1) * 0.0003
        return e
    }

    /// The grounding grid for a windowed viewport: a quiet reference grid at
    /// the part's lowest point (the shadow is `contactShadowEntity`). The grid
    /// sits on the world origin (so its coloured lines are the real X/Y axes)
    /// whenever the part is near it; otherwise it centres under the part,
    /// snapped to a major line, without axes. `dark` picks the line ink.
    @MainActor
    static func groundingEntities(partSizeMM: SIMD3<Float>, sceneSize: Float,
                                  sceneScale: Float, dark: Bool,
                                  sceneCenter: SIMD3<Float> = .zero,
                                  viewSpanMM: Float? = nil) -> [Entity] {
        var out: [Entity] = []
        let floorY = Self.floorY(partSizeMM: partSizeMM, sceneScale: sceneScale)
        let layout = GridSpacing.ground(extentMM: sceneSize, viewSpanMM: viewSpanMM)
        let center2 = SIMD2(sceneCenter.x, sceneCenter.y)
        let placement = GridSpacing.placement(partCenterMM: center2, partExtentMM: sceneSize, layout: layout)
        if layout.minor * sceneScale > 1e-6,
           let grid = gridEntity(name: "grid", layout: layout, scale: sceneScale, dark: dark,
                                 axes: placement.axes) {
            // Kernel (x, y) → display (x, -z) about the centred part.
            let d = (placement.centerMM - center2) * sceneScale
            grid.position = [d.x, floorY + 0.0004, -d.y]
            out.append(grid)
        }
        return out
    }

    /// Display Y of the floor: the part's lowest point, a hair below so the
    /// grid never z-fights a bottom face. (It used to sit 0.004 display units
    /// down — together with the shadow slab, enough to read as a part
    /// hovering over its floor.)
    static func floorY(partSizeMM: SIMD3<Float>, sceneScale: Float) -> Float {
        -(partSizeMM.z * 0.5 * sceneScale) - 0.001
    }

    /// The grounded contact shadow (see `ContactShadow`): the part's real
    /// top-down footprint baked into an alpha map, laid on the floor under
    /// the footprint in the same display frame as the part.
    ///
    /// `lo`/`hi` are the shadow-casting bounds in kernel mm and `sceneCenter`
    /// the kernel point the viewport centres on (the anchor origin), so the
    /// quad lands under the part wherever it sits. Kernel (x, y, z) maps to
    /// display (x, z, -y), scaled by `sceneScale`.
    @MainActor
    static func contactShadowEntity(triangles: [SIMD3<Float>], lo: SIMD3<Float>, hi: SIMD3<Float>,
                                    sceneCenter: SIMD3<Float>, sceneScale: Float,
                                    floorY: Float, dark: Bool) -> Entity? {
        guard lo.x <= hi.x, let img = ContactShadow.bake(triangles: triangles, lo: lo, hi: hi),
              let tex = try? TextureResource(image: img, options: .init(semantic: .raw)) else { return nil }
        let plane = ContactShadow.plane(lo: lo, hi: hi)
        var m = UnlitMaterial(color: .black)
        // Opacity comes from the baked map, per texel. (The old blob used a
        // scalar opacity over a colour texture, which drew the quad as one
        // flat slab hiding the grid lines in a hard square around the part.)
        m.blending = .transparent(opacity: .init(scale: dark ? 1.0 : 0.8, texture: .init(tex)))
        // A decal on the floor: never occlude the grid lines under it, and
        // never lose a depth test against them.
        m.writesDepth = false
        let w = max(1e-4, plane.size.x * sceneScale), d = max(1e-4, plane.size.y * sceneScale)
        // An explicit quad rather than `generatePlane`, so the UV orientation
        // is ours: u runs with kernel +X, v with kernel +Y (display −Z), and
        // v = 1 is the image's top row — the baked map's kernel max Y.
        var desc = MeshDescriptor(name: "contactShadow")
        desc.positions = MeshBuffers.Positions([[-w / 2, 0, d / 2], [w / 2, 0, d / 2],
                                                [w / 2, 0, -d / 2], [-w / 2, 0, -d / 2]])
        desc.normals = MeshBuffers.Normals([SIMD3<Float>](repeating: [0, 1, 0], count: 4))
        desc.textureCoordinates = MeshBuffers.TextureCoordinates([[0, 0], [1, 0], [1, 1], [0, 1]])
        desc.primitives = .triangles([0, 1, 2, 0, 2, 3])
        guard let mesh = try? MeshResource.generate(from: [desc]) else { return nil }
        let e = ModelEntity(mesh: mesh, materials: [m])
        e.name = "contactShadow"
        let off = (plane.center - SIMD2(sceneCenter.x, sceneCenter.y)) * sceneScale
        e.position = [off.x, floorY + 0.0008, -off.y]
        return e
    }
}

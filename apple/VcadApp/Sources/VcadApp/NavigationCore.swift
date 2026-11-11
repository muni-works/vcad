import Foundation
import simd

// The pure half of vcad's navigation layer: command ranking and recents,
// orientation targets and the Compass hit-test, camera tweening, saved views,
// the one-gesture section plane, and the Halo's radial geometry. No views,
// no model: everything here is a function of its inputs, so it is all tested
// (NavigationCoreTests). The surfaces live in NavigationViews.swift.

// MARK: - Find: ranking + recents

enum CommandRanker {
    /// Fuzzy score of `query` against `title` (both compared case-insensitively),
    /// or nil when the query is not a subsequence. Contiguous beats scattered,
    /// a prefix beats a substring, word starts beat mid-word letters, and shorter
    /// titles win ties so "Top" outranks "Stop Measuring" for "top".
    static func score(_ title: String, _ query: String) -> Int? {
        let s = title.lowercased()
        let q = query.lowercased().trimmingCharacters(in: .whitespaces)
        guard !q.isEmpty else { return 0 }
        if let r = s.range(of: q) {
            let atWord = r.lowerBound == s.startIndex || !s[s.index(before: r.lowerBound)].isLetter
            return 1000 - s.count + (r.lowerBound == s.startIndex ? 500 : 0) + (atWord ? 200 : 0)
        }
        var si = s.startIndex, score = 0, run = 0
        for ch in q where ch != " " {
            guard let i = s[si...].firstIndex(of: ch) else { return nil }
            if i == s.startIndex || !s[s.index(before: i)].isLetter { score += 10 }
            run = i == si ? run + 1 : 0
            score += 1 + run * 2
            si = s.index(after: i)
        }
        return score
    }

    /// Order `items` for a query. Empty query: recents first (most recent on
    /// top), then everything else in its given order. Otherwise: matches by
    /// score, recency breaking near-ties (a recent command gets a small boost).
    static func rank<T>(_ items: [T], query: String, recents: [String],
                        id: (T) -> String, title: (T) -> String) -> [T] {
        let recency = Dictionary(recents.enumerated().map { ($1, $0) }, uniquingKeysWith: { a, _ in a })
        let q = query.trimmingCharacters(in: .whitespaces)
        if q.isEmpty {
            let recent = items.filter { recency[id($0)] != nil }
                .sorted { recency[id($0)]! < recency[id($1)]! }
            return recent + items.filter { recency[id($0)] == nil }
        }
        return items.enumerated().compactMap { i, item -> (T, Int, Int)? in
            guard var s = score(title(item), q) else { return nil }
            if let r = recency[id(item)] { s += max(0, 40 - r * 5) }
            return (item, s, i)
        }
        .sorted { $0.1 != $1.1 ? $0.1 > $1.1 : $0.2 < $1.2 }
        .map(\.0)
    }
}

/// Most-recently-run command ids, newest first, capped.
struct RecentCommands: Equatable {
    static let defaultsKey = "vcad.find.recents"
    static let limit = 8
    private(set) var ids: [String] = []

    init(ids: [String] = []) { self.ids = Array(ids.prefix(Self.limit)) }

    mutating func record(_ id: String) {
        ids.removeAll { $0 == id }
        ids.insert(id, at: 0)
        if ids.count > Self.limit { ids.removeLast(ids.count - Self.limit) }
    }

    static func load(_ d: UserDefaults = .standard) -> RecentCommands {
        RecentCommands(ids: d.stringArray(forKey: defaultsKey) ?? [])
    }
    func save(_ d: UserDefaults = .standard) { d.set(ids, forKey: Self.defaultsKey) }
}

// MARK: - Orientation targets

/// One of the 26 directions the Compass can turn the camera to: 6 faces,
/// 12 edges, 8 corners. `dir` is where the camera sits, in the kernel's Z-up
/// frame, each component −1, 0 or 1.
struct ViewTarget: Hashable, Identifiable {
    let dir: SIMD3<Int>
    var id: String { "\(dir.x),\(dir.y),\(dir.z)" }

    enum Kind { case face, edge, corner }
    var kind: Kind {
        switch [dir.x, dir.y, dir.z].filter({ $0 != 0 }).count {
        case 1: .face
        case 2: .edge
        default: .corner
        }
    }

    static let all: [ViewTarget] = {
        var out: [ViewTarget] = []
        for x in -1...1 { for y in -1...1 { for z in -1...1 where x != 0 || y != 0 || z != 0 {
            out.append(ViewTarget(dir: SIMD3(x, y, z)))
        } } }
        return out
    }()

    static let front = ViewTarget(dir: SIMD3(0, -1, 0))
    static let back = ViewTarget(dir: SIMD3(0, 1, 0))
    static let right = ViewTarget(dir: SIMD3(1, 0, 0))
    static let left = ViewTarget(dir: SIMD3(-1, 0, 0))
    static let top = ViewTarget(dir: SIMD3(0, 0, 1))
    static let bottom = ViewTarget(dir: SIMD3(0, 0, -1))

    /// Face name for a face target, compound name otherwise ("Top Front Right").
    var label: String {
        var parts: [String] = []
        if dir.z == 1 { parts.append("Top") } else if dir.z == -1 { parts.append("Bottom") }
        if dir.y == -1 { parts.append("Front") } else if dir.y == 1 { parts.append("Back") }
        if dir.x == 1 { parts.append("Right") } else if dir.x == -1 { parts.append("Left") }
        return parts.joined(separator: " ")
    }

    /// Camera elevation is clamped short of the poles (the orbit's up vector
    /// is world Z; looking straight down would make it degenerate).
    static let maxElevation: Float = 1.45

    /// The orbit angles that put the camera on `dir` (Editor's convention:
    /// azimuth 0 = looking from the front, π/2 = from the right).
    var azimuth: Float {
        dir.x == 0 && dir.y == 0 ? 0 : atan2(Float(dir.x), Float(-dir.y))
    }
    var elevation: Float {
        let d = SIMD3<Float>(Float(dir.x), Float(dir.y), Float(dir.z))
        return min(Self.maxElevation, max(-Self.maxElevation, asin(d.z / simd_length(d))))
    }

    /// The face target nearest the current view — the face you are looking at.
    static func facing(azimuth: Float, elevation: Float) -> ViewTarget {
        let c = cameraDirection(azimuth: azimuth, elevation: elevation)
        let faces = all.filter { $0.kind == .face }
        return faces.max { simd_dot($0.unit, c) < simd_dot($1.unit, c) }!
    }

    var opposite: ViewTarget { ViewTarget(dir: SIMD3(-dir.x, -dir.y, -dir.z)) }
    var unit: SIMD3<Float> { simd_normalize(SIMD3(Float(dir.x), Float(dir.y), Float(dir.z))) }

    /// Unit vector from the target toward the camera, kernel frame.
    static func cameraDirection(azimuth: Float, elevation: Float) -> SIMD3<Float> {
        SIMD3(cos(elevation) * sin(azimuth), -cos(elevation) * cos(azimuth), sin(elevation))
    }
}

// MARK: - Compass projection + hit-test

/// The Compass cube seen from the current camera, projected orthographically
/// into a square widget. Points are in widget coordinates (y down), with the
/// cube's centre at the origin and its corners at `scale` from it along each
/// axis before projection.
struct CompassProjection {
    let azimuth: Float
    let elevation: Float
    var scale: Float = 1

    var camera: SIMD3<Float> { ViewTarget.cameraDirection(azimuth: azimuth, elevation: elevation) }
    private var basis: (right: SIMD3<Float>, up: SIMD3<Float>) {
        let f = -camera
        var r = simd_cross(f, SIMD3(0, 0, 1))
        if simd_length(r) < 1e-4 { r = SIMD3(1, 0, 0) }
        r = simd_normalize(r)
        return (r, simd_cross(r, f))
    }

    func project(_ p: SIMD3<Float>) -> SIMD2<Float> {
        let b = basis
        return SIMD2(simd_dot(p, b.right), -simd_dot(p, b.up)) * scale
    }

    struct Face {
        let normal: SIMD3<Int>
        /// Corners p0 (−u −v), p1 (+u −v), p2 (+u +v), p3 (−u +v), projected.
        let corners: [SIMD2<Float>]
        let u: SIMD3<Int>, v: SIMD3<Int>
        /// How squarely the face points at the camera (0 edge-on … 1 head-on).
        let facing: Float
        var target: ViewTarget { ViewTarget(dir: normal) }
    }

    /// The faces turned toward the camera, farthest first (paint order).
    var visibleFaces: [Face] {
        let c = camera
        var out: [Face] = []
        for axis in 0..<3 { for sign in [-1, 1] {
            var n = SIMD3<Int>(0, 0, 0); n[axis] = sign
            let facing = simd_dot(SIMD3<Float>(n), c)
            guard facing > 1e-3 else { continue }
            var u = SIMD3<Int>(0, 0, 0); u[(axis + 1) % 3] = 1
            var v = SIMD3<Int>(0, 0, 0); v[(axis + 2) % 3] = 1
            let nf = SIMD3<Float>(n), uf = SIMD3<Float>(u), vf = SIMD3<Float>(v)
            let corners = [nf - uf - vf, nf + uf - vf, nf + uf + vf, nf - uf + vf].map(project)
            out.append(Face(normal: n, corners: corners, u: u, v: v, facing: facing))
        } }
        return out.sorted { $0.facing < $1.facing }
    }

    /// Width of the edge/corner band on each face, as a fraction of the face.
    static let band: Float = 0.24

    /// Which of the 26 targets the widget point `p` lands on, or nil off-cube.
    func target(at p: SIMD2<Float>) -> ViewTarget? {
        for f in visibleFaces.reversed() {        // nearest first
            let o = f.corners[0], eu = f.corners[1] - o, ev = f.corners[3] - o
            let det = eu.x * ev.y - eu.y * ev.x
            guard abs(det) > 1e-6 else { continue }
            let d = p - o
            let s = (d.x * ev.y - d.y * ev.x) / det
            let t = (eu.x * d.y - eu.y * d.x) / det
            guard (0...1).contains(s), (0...1).contains(t) else { continue }
            func comp(_ x: Float) -> Int { x < Self.band ? -1 : x > 1 - Self.band ? 1 : 0 }
            return ViewTarget(dir: f.normal &+ f.u &* comp(s) &+ f.v &* comp(t))
        }
        return nil
    }

    /// The outline of a target's region on screen, for the hover highlight:
    /// the sub-quads of every visible face the target touches.
    func highlight(_ target: ViewTarget) -> [[SIMD2<Float>]] {
        visibleFaces.compactMap { f in
            // Along the face normal the target must agree with it.
            let nAxis = f.normal.x != 0 ? 0 : f.normal.y != 0 ? 1 : 2
            guard target.dir[nAxis] == f.normal[nAxis] else { return nil }
            let uAxis = (nAxis + 1) % 3, vAxis = (nAxis + 2) % 3
            func span(_ c: Int) -> (Float, Float) {
                c < 0 ? (0, Self.band) : c > 0 ? (1 - Self.band, 1) : (Self.band, 1 - Self.band)
            }
            let (s0, s1) = span(target.dir[uAxis]), (t0, t1) = span(target.dir[vAxis])
            let o = f.corners[0], eu = f.corners[1] - o, ev = f.corners[3] - o
            return [(s0, t0), (s1, t0), (s1, t1), (s0, t1)].map { o + eu * $0.0 + ev * $0.1 }
        }
    }
}

// MARK: - Camera tween

/// A camera pose, as the orbit camera stores it.
struct CameraPose: Equatable, Codable {
    var azimuth: Float
    var elevation: Float
    var distance: Float
    var pan: SIMD3<Float>
}

/// Interpolates between two poses along the shortest azimuth arc with an
/// ease-in-out curve, so a click on the Compass glides rather than snaps.
struct CameraTween {
    let from: CameraPose
    let to: CameraPose
    var duration: Double = 0.42

    init(from: CameraPose, to: CameraPose, duration: Double = 0.42) {
        self.from = from
        var t = to
        t.azimuth = Self.nearestAngle(to.azimuth, near: from.azimuth)
        self.to = t
        self.duration = duration
    }

    /// `angle` shifted by whole turns to lie within π of `ref`.
    static func nearestAngle(_ angle: Float, near ref: Float) -> Float {
        let twoPi = 2 * Float.pi
        var a = angle
        while a - ref > .pi { a -= twoPi }
        while a - ref < -.pi { a += twoPi }
        return a
    }

    /// Smoothstep-style ease (cubic in-out), 0…1 → 0…1.
    static func ease(_ x: Double) -> Double {
        let t = min(1, max(0, x))
        return t < 0.5 ? 4 * t * t * t : 1 - pow(-2 * t + 2, 3) / 2
    }

    func pose(at elapsed: Double) -> CameraPose {
        let k = Float(Self.ease(duration <= 0 ? 1 : elapsed / duration))
        func mix(_ a: Float, _ b: Float) -> Float { a + (b - a) * k }
        return CameraPose(azimuth: mix(from.azimuth, to.azimuth),
                          elevation: mix(from.elevation, to.elevation),
                          distance: mix(from.distance, to.distance),
                          pan: from.pan + (to.pan - from.pan) * k)
    }
    func finished(at elapsed: Double) -> Bool { elapsed >= duration }
}

// MARK: - Saved views

struct SavedView: Codable, Equatable, Identifiable {
    var name: String
    var pose: CameraPose
    var id: String { name }
}

/// Named camera views for one document, persisted in user defaults under a
/// key derived from the document's path (untitled documents share one list).
struct SavedViewList: Codable, Equatable {
    private(set) var views: [SavedView] = []

    /// Save, replacing an existing view of the same name (case-insensitive)
    /// in place. Blank names become "View N".
    @discardableResult
    mutating func save(_ name: String, pose: CameraPose) -> SavedView {
        let trimmed = name.trimmingCharacters(in: .whitespacesAndNewlines)
        let n = trimmed.isEmpty ? nextDefaultName : trimmed
        let v = SavedView(name: n, pose: pose)
        if let i = views.firstIndex(where: { $0.name.caseInsensitiveCompare(n) == .orderedSame }) {
            views[i] = v
        } else {
            views.append(v)
        }
        return v
    }

    mutating func remove(_ name: String) { views.removeAll { $0.name.caseInsensitiveCompare(name) == .orderedSame } }

    var nextDefaultName: String {
        var i = views.count + 1
        while views.contains(where: { $0.name == "View \(i)" }) { i += 1 }
        return "View \(i)"
    }

    static func key(for documentPath: String?) -> String {
        "vcad.savedViews." + (documentPath ?? "untitled")
    }
    static func load(for documentPath: String?, _ d: UserDefaults = .standard) -> SavedViewList {
        guard let data = d.data(forKey: key(for: documentPath)),
              let list = try? JSONDecoder().decode(SavedViewList.self, from: data) else { return .init() }
        return list
    }
    func save(for documentPath: String?, _ d: UserDefaults = .standard) {
        if let data = try? JSONEncoder().encode(self) { d.set(data, forKey: Self.key(for: documentPath)) }
    }
}

// MARK: - One-gesture section

extension SectionPlane {
    /// The section a single keystroke should make. With a planar face picked
    /// whose normal is axis-aligned, cut on that face's plane; otherwise cut
    /// through `center` on the axis that faces the camera most squarely. The
    /// half toward the camera is the one removed, so you look into the cut.
    /// All vectors in the kernel frame (mm); `toCamera` need not be unit.
    static func smart(toCamera: SIMD3<Float>, center: SIMD3<Float>,
                      faceNormal: SIMD3<Float>? = nil, facePoint: SIMD3<Float>? = nil) -> SectionPlane {
        func dominant(_ v: SIMD3<Float>) -> Int {
            let a = simd_abs(v)
            return a.x >= a.y && a.x >= a.z ? 0 : a.y >= a.z ? 1 : 2
        }
        let axes: [OpAxis] = [.x, .y, .z]
        if let n = faceNormal, let p = facePoint, simd_length(n) > 0 {
            let u = simd_normalize(n), i = dominant(u)
            if abs(u[i]) > 0.95 {
                // Remove the side the face's outward normal points into, so
                // the cut sits flush with the face and you see behind it.
                return SectionPlane(axis: axes[i], offset: Double(p[i]), flip: u[i] < 0)
            }
        }
        let i = dominant(toCamera)
        return SectionPlane(axis: axes[i], offset: Double(center[i]), flip: toCamera[i] < 0)
    }
}

// MARK: - Halo radial geometry

enum RadialLayout {
    /// Angle (radians, 0 = north, clockwise, screen space) of slot `i` of `n`.
    static func angle(slot i: Int, of n: Int) -> Double {
        2 * .pi * Double(i) / Double(max(n, 1))
    }

    /// Centre of slot `i` at `radius` from the origin, screen coords (y down).
    /// `squash` < 1 flattens the ring vertically: labels are wide and short,
    /// so an ellipse keeps the diagonal pills clear of north and south.
    static func position(slot i: Int, of n: Int, radius: Double, squash: Double = 1) -> CGPoint {
        let a = angle(slot: i, of: n)
        return CGPoint(x: sin(a) * radius, y: -cos(a) * radius * squash)
    }

    /// The slot a pointer offset selects, or nil inside the dead zone.
    static func slot(for offset: CGSize, count n: Int, deadZone: Double = 18) -> Int? {
        let dx = Double(offset.width), dy = Double(offset.height)
        guard n > 0, dx * dx + dy * dy >= deadZone * deadZone else { return nil }
        var a = atan2(dx, -dy)                      // 0 = north, clockwise
        if a < 0 { a += 2 * .pi }
        let sector = 2 * .pi / Double(n)
        return Int((a + sector / 2) / sector) % n
    }
}

/// What the Halo's six context slots offer, decided from the selection.
enum HaloContext: Equatable {
    case nothing, part, face, edge, sketch

    /// Command ids for slots 0…7, clockwise from north. North is always Find
    /// and south always Frame, so the two you reach for blind never move.
    var slots: [String] {
        let ctx: [String]
        switch self {
        case .nothing: ctx = ["sketch", "section", "measure", "add.cube", "views", "showAll"]
        case .part:    ctx = ["op.shell", "section", "hide", "isolate", "combine", "measure"]
        case .face:    ctx = ["sketch", "op.hole", "sectionHere", "op.shell", "measure", "hide"]
        case .edge:    ctx = ["op.fillet", "op.chamfer", "measure", "section", "clearPick", "hide"]
        case .sketch:  ctx = ["sketch.finish", "views", "section", "measure", "undo", "redo"]
        }
        // Clockwise: N, NE, E, SE, S, SW, W, NW.
        return ["find", ctx[0], ctx[1], ctx[2], "frame", ctx[3], ctx[4], ctx[5]]
    }
}

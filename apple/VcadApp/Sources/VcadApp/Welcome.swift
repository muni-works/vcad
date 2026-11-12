#if os(macOS)
import AppKit
#endif
import Foundation
import SwiftUI

// First run: a welcome window with sample projects bundled in the app and a
// short "your first part in loon" tutorial. Samples live in
// apple/VcadApp/Samples (bundle.sh copies them to Contents/Resources/samples)
// and are described by samples.json there.

/// One bundled sample.
struct SampleProject: Decodable, Identifiable, Equatable, Sendable {
    let id: String
    let title: String
    let subtitle: String
    let file: String
    let thumbnail: String
    var tutorial: Bool? = nil
    /// Files or folders (relative to the samples directory) the document
    /// reads by relative path — mesh imports — copied beside it on open.
    var assets: [String]? = nil
    /// Gallery category, for grouping ("Mechanical", "Electronics", …).
    var category: String? = nil
    /// What the sample teaches ("sketch → extrude → fillet"): set on the
    /// Welcome window's "Start from an example" cards.
    var lesson: String? = nil
}

enum SampleLibrary {
    /// Where the samples are: `$VCAD_SAMPLES`, the app bundle's
    /// `Resources/samples`, or `apple/VcadApp/Samples` in a checkout above the
    /// running binary (a dev build).
    static func directory(environment: [String: String] = ProcessInfo.processInfo.environment,
                          bundle: Bundle = .main) -> URL? {
        let fm = FileManager.default
        var candidates: [URL] = []
        if let env = environment["VCAD_SAMPLES"], !env.isEmpty { candidates.append(URL(fileURLWithPath: env)) }
        if let res = bundle.resourceURL { candidates.append(res.appendingPathComponent("samples")) }
        var dir = bundle.bundleURL
        for _ in 0..<8 {
            dir = dir.deletingLastPathComponent()
            candidates.append(dir.appendingPathComponent("apple/VcadApp/Samples"))
            candidates.append(dir.appendingPathComponent("Samples"))
        }
        return candidates.first { fm.fileExists(atPath: $0.appendingPathComponent("samples.json").path) }
    }

    /// The samples listed in `dir/samples.json` whose files exist.
    static func load(from dir: URL) -> [SampleProject] {
        guard let data = try? Data(contentsOf: dir.appendingPathComponent("samples.json")),
              let all = try? JSONDecoder().decode([SampleProject].self, from: data) else { return [] }
        let fm = FileManager.default
        return all.filter { fm.fileExists(atPath: dir.appendingPathComponent($0.file).path) }
    }

    /// Where opened samples are copied, so they can be edited (the app
    /// bundle is read-only and signed): `~/Documents/vcad/Samples`.
    static var workspace: URL {
        FileManager.default.homeDirectoryForCurrentUser
            .appendingPathComponent("Documents/vcad/Samples", isDirectory: true)
    }

    /// Copy a sample into `workspace` (once; an edited copy is never
    /// overwritten) and return the copy.
    static func materialize(_ sample: SampleProject, from dir: URL, into workspace: URL = workspace) throws -> URL {
        let fm = FileManager.default
        try fm.createDirectory(at: workspace, withIntermediateDirectories: true)
        let dest = workspace.appendingPathComponent(sample.file)
        if !fm.fileExists(atPath: dest.path) {
            // Read through any symlink (the checkout links repo examples).
            try fm.createDirectory(at: dest.deletingLastPathComponent(), withIntermediateDirectories: true)
            let data = try Data(contentsOf: dir.appendingPathComponent(sample.file).resolvingSymlinksInPath())
            try data.write(to: dest, options: .atomic)
            // The baked mesh bundle (bake-sample-meshes.sh), so the first open
            // is a cache hit rather than a cold solve (Stirling: 38 s → <0.1 s).
            func meshBundle(_ u: URL) -> URL { u.deletingPathExtension().appendingPathExtension("vcadmesh") }
            let bundle = meshBundle(dir.appendingPathComponent(sample.file))
            if fm.fileExists(atPath: bundle.path) {
                try? fm.copyItem(at: bundle, to: meshBundle(dest))
            }
        }
        // Assets a sample reads by relative path (mesh imports), copied once
        // to the same relative place beside the workspace copy.
        for asset in sample.assets ?? [] {
            let target = workspace.appendingPathComponent(asset)
            guard !fm.fileExists(atPath: target.path) else { continue }
            try fm.createDirectory(at: target.deletingLastPathComponent(), withIntermediateDirectories: true)
            try fm.copyItem(at: dir.appendingPathComponent(asset).resolvingSymlinksInPath(), to: target)
        }
        return dest
    }
}


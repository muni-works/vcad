import Foundation

/// Where each IR operation (`CsgOp`) is reachable by mouse. Paired with
/// `OpCoverageTests`, which fails if a non-hidden variant has no entry or an
/// entry's palette command stops existing.
enum OpCoverage {
    /// Command-palette id reaching the op, with where else it lives.
    static let paths: [String: (palette: String, place: String)] = [
        "Cube": ("add.cube", "Design dock: Primitives"),
        "Cylinder": ("add.cylinder", "Design dock: Primitives"),
        "Sphere": ("add.sphere", "Design dock: Primitives"),
        "Cone": ("add.cone", "Design dock: Primitives"),
        "Torus": ("add.torus", "Design dock: Primitives"),
        "Wedge": ("add.wedge", "Design dock: Primitives"),
        "Prism": ("add.prism", "Design dock: Primitives"),
        "SpurGear": ("op.spurGear", "All tools: Spur Gear (Mesh with selected gear)"),
        "Union": ("bool.union", "Design dock"),
        "Difference": ("bool.difference", "Design dock"),
        "Intersection": ("bool.intersection", "Design dock"),
        "Translate": ("op.move", "All tools; inspector Position"),
        "Rotate": ("op.rotate", "All tools; inspector Rotation"),
        "Scale": ("op.scale", "All tools; inspector Scale"),
        "Mirror": ("op.mirror", "All tools"),
        "Sketch2D": ("sketch", "Design dock"),
        "Extrude": ("sketch.extrude", "Sketch finish bar"),
        "Revolve": ("sketch.revolve", "Sketch finish bar"),
        "Sweep": ("sketch.sweep", "Sketch finish bar"),
        "Loft": ("sketch.loft", "Sketch finish bar"),
        "LinearPattern": ("op.linearPattern", "All tools"),
        "CircularPattern": ("op.circularPattern", "Design dock"),
        "SweepRotate": ("op.sweepRotate", "All tools"),
        "Shell": ("op.shell", "Design dock"),
        "Fillet": ("op.fillet", "Design dock"),
        "Chamfer": ("op.chamfer", "Design dock"),
        "EdgeBlend": ("op.blend", "All tools; picked edges on Fillet and Chamfer"),
        "Text2D": ("op.text", "All tools"),
        "Hull": ("op.hull", "All tools; Hull from Points"),
        "SheetMetalBaseFlangeRect": ("tool.fab.sheet", "Fab: Sheet Metal, Author"),
        "SheetMetalBaseFlangePolygon": ("tool.fab.sheet", "Fab: Sheet Metal, Author, From outline"),
        "SheetMetalEdgeFlange": ("tool.fab.sheet", "Fab: Sheet Metal, Author, Edges"),
        "SheetMetalJog": ("tool.fab.sheet", "Fab: Sheet Metal, Author, Edges"),
        "SheetMetalHem": ("tool.fab.sheet", "Fab: Sheet Metal, Author, Edges"),
        "SheetMetalBendRelief": ("tool.fab.sheet", "Fab: Sheet Metal, Author, Bend relief"),
    ]

    /// `#[tool(hidden)]` ops deliberately left without a modeling command.
    static let hidden: [String: String] = [
        "Empty": "An internal placeholder: it has no geometry to author.",
        "SketchOnFace": "Created by sketching on a named face; it has no parameters of its own.",
        "ImportedMesh": "Created by File, Open on a mesh; it has no parameters.",
        "StepImport": "Created by File, Open on a STEP file.",
        "MeshImport": "Created by importing a URDF; a path reference until it resolves.",
        "PcbBoard": "Authored in the Electronics workspace, not as a CAD op.",
        "EmbroideryPattern": "Created by the embroidery tooling; no CAD authoring surface.",
        "PartInstance": "Library parts resolve through the agent and part library; no browser in the app yet.",
    ]
}

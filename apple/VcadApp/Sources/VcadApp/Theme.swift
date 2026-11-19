import SwiftUI

// The one design system. Every floating surface, radius, spacing constant,
// section eyebrow, key/value row and selection highlight in the app comes from
// here — so a panel in Manufacture cannot drift from a panel in Design, and a
// light-mode or wallpaper backdrop never meets a hard-coded dark-mode alpha.
//
// Type is semantic (`.body`, `.callout`, `.subheadline`, `.caption`) rather
// than point sizes: on macOS those resolve to 13 / 12 / 11 / 10 pt, track the
// user's text-size setting, and stay legible under accessibility scaling.

enum Theme {
    enum Radius {
        /// Floating panels and the workspace header.
        static let panel: CGFloat = 16
        /// Buttons, scrub fields, keycaps, badges.
        static let control: CGFloat = 6
        /// List rows (feature tree, operations, parts).
        static let row: CGFloat = 6
    }
    enum Space {
        static let xs: CGFloat = 4
        static let s: CGFloat = 8
        static let m: CGFloat = 12
        static let l: CGFloat = 16
    }
    enum Width {
        #if os(visionOS)
        // visionOS type runs ~1.3× the Mac's (body 17 pt vs 13) and its
        // controls carry 44 pt hit targets, so the same panels at Mac widths
        // truncated names and clipped units. Same layout, wider rails.
        static let navigator: CGFloat = 250
        static let inspector: CGFloat = 270
        static let toolInspector: CGFloat = 380
        static let analysisWide: CGFloat = 600
        #elseif os(iOS)
        // iPhone: every rail panel stacks above the dock at the phone's width
        // (402 pt on a 17 Pro, minus the 16 pt gutters).
        static let navigator: CGFloat = 368
        static let inspector: CGFloat = 368
        static let toolInspector: CGFloat = 368
        static let analysisWide: CGFloat = 368
        #else
        /// The left rail: model navigator, CNC job outline, circuit navigator.
        static let navigator: CGFloat = 170
        /// The right rail: object inspector, machine panel, circuit inspector.
        static let inspector: CGFloat = 170
        /// Tool panels with real forms (Fab: sheet metal, slicer, CAM job;
        /// analysis; the CNC machine panel). Their rows (pop-ups, tool tables,
        /// export buttons) overflowed and clipped at 170 pt.
        static let toolInspector: CGFloat = 280
        /// Analysis → Fields: section image + charts, legible at 1080p.
        static let analysisWide: CGFloat = 470
        #endif
        /// Manufacture's compact job outline.
        #if os(visionOS)
        static let cncOutline: CGFloat = 260
        #else
        static let cncOutline: CGFloat = 192
        #endif
    }
    enum Height {
        static let header: CGFloat = 52
    }
}

// MARK: - Surfaces

extension EnvironmentValues {
    /// True while the chrome floats straight over the desktop (released mode)
    /// rather than over the window's own viewport. Glass there refracts other
    /// apps' windows, the wallpaper and the model itself into an illegible
    /// smear, so surfaces add a solid backing under their content.
    @Entry var chromeOverDesktop = false
}

extension View {
    /// The solid backing under glass content while chrome floats over the
    /// desktop: the glass keeps its edge and depth, the panel stays readable.
    func desktopBacking<S: InsettableShape>(_ on: Bool, in shape: S) -> some View {
        #if os(macOS)
        background(on ? AnyShapeStyle(Color(nsColor: .windowBackgroundColor).opacity(0.94)) : AnyShapeStyle(.clear),
                   in: shape)
        #else
        // Mac only: the iPhone's material cards made `.secondary` labels
        // vanish over RealityKit, and visionOS has its own system glass.
        self
        #endif
    }
}

/// The floating panel surface: Liquid Glass where the OS has it, a regular
/// material with a hairline and a soft shadow everywhere else. The shadow
/// weight follows the appearance so a panel over a light desktop does not
/// carry a night-time shadow.
private struct PanelSurface<S: InsettableShape>: ViewModifier {
    let shape: S
    @Environment(\.colorScheme) private var scheme

    @Environment(\.accessibilityReduceTransparency) private var reduceTransparency
    @Environment(\.chromeOverDesktop) private var overDesktop

    func body(content: Content) -> some View {
        #if os(macOS)
        if reduceTransparency {
            content.background(Color(nsColor: .windowBackgroundColor), in: shape)
                .overlay(shape.strokeBorder(.separator, lineWidth: 1))
        } else if #available(macOS 26.0, *) {
            content.desktopBacking(overDesktop, in: shape).glassEffect(.regular, in: shape)
        } else {
            fallback(content)
        }
        #elseif os(visionOS)
        // The visionOS system glass, so the Mac's panels read as native windows.
        content.glassBackgroundEffect(in: shape)
        #else
        // iPhone: the Mac's glass. A material card over the RealityKit view
        // drew every `.secondary` label (row keys, eyebrows) invisible.
        if !reduceTransparency, #available(iOS 26.0, *) {
            content.glassEffect(.regular, in: shape)
        } else {
            fallback(content)
        }
        #endif
    }

    private func fallback(_ content: Content) -> some View {
        content
            .background(.regularMaterial, in: shape)
            .overlay(shape.strokeBorder(.separator, lineWidth: 1))
            .shadow(color: .black.opacity(scheme == .dark ? 0.32 : 0.14), radius: 14, y: 5)
    }
}

extension View {
    /// A floating panel over the viewport.
    func panelSurface() -> some View {
        modifier(PanelSurface(shape: RoundedRectangle(cornerRadius: Theme.Radius.panel, style: .continuous)))
    }
    /// A floating pill (transports, hint bars, status chips).
    func pillSurface() -> some View {
        modifier(PanelSurface(shape: Capsule(style: .continuous)))
    }
    /// The background of a list row: accent selection, a quiet hover, or nothing.
    func selectableRow(selected: Bool, hovered: Bool = false) -> some View {
        background(
            selected ? AnyShapeStyle(Color.primary.opacity(0.12)) : hovered ? AnyShapeStyle(.quaternary) : AnyShapeStyle(.clear),
            in: RoundedRectangle(cornerRadius: Theme.Radius.row, style: .continuous))
    }
    /// The inset well behind an editable value (scrub field, rename field).
    func controlWell(active: Bool = false) -> some View {
        background(.quaternary.opacity(active ? 1 : 0.6),
                   in: RoundedRectangle(cornerRadius: Theme.Radius.control, style: .continuous))
        .overlay(RoundedRectangle(cornerRadius: Theme.Radius.control, style: .continuous)
            .strokeBorder(active ? AnyShapeStyle(Color.accentColor.opacity(0.7)) : AnyShapeStyle(.separator),
                          lineWidth: active ? 1 : 0.5))
    }
}

// MARK: - Panel chrome

/// The title row of a floating panel: an optional symbol, the title, and the
/// one close affordance every panel shares.
struct PanelHeader: View {
    let title: String
    var systemImage: String? = nil
    var onClose: (() -> Void)? = nil

    var body: some View {
        HStack(spacing: Theme.Space.s) {
            if let systemImage {
                Image(systemName: systemImage).foregroundStyle(.secondary)
            }
            Text(title).font(.headline).lineLimit(1).truncationMode(.middle)
            Spacer(minLength: 0)
            if let onClose {
                Button(action: onClose) {
                    Image(systemName: "xmark.circle.fill").foregroundStyle(.tertiary)
                }
                .buttonStyle(.borderless)
                .help("Close \(title)")
                .accessibilityLabel("Close \(title)")
            }
        }
    }
}

/// A section eyebrow: small caps, tracked, secondary.
struct Eyebrow: View {
    let text: String
    init(_ text: String) { self.text = text }
    var body: some View {
        Text(text.uppercased())
            .font(.caption2.weight(.semibold))
            .tracking(0.6)
            .foregroundStyle(.secondary)
            .accessibilityAddTraits(.isHeader)
    }
}

/// A read-only key/value row. The value is monospaced-digit so columns of
/// numbers line up; the key is secondary so the value reads first.
struct KeyValueRow: View {
    let key: String
    let value: String
    init(_ key: String, _ value: String) { self.key = key; self.value = value }
    var body: some View {
        // Side by side when the value fits on one line; otherwise the value
        // goes under its key. In the 170 pt inspector "508.4 × 262.0 ×
        // 415.3 mm" used to wrap mid-number, ragged against the key.
        ViewThatFits(in: .horizontal) {
            HStack(alignment: .firstTextBaseline) {
                Text(key).foregroundStyle(.secondary)
                Spacer(minLength: Theme.Space.s)
                Text(value).monospacedDigit().lineLimit(1).fixedSize()
                    .textSelection(.enabled)
            }
            VStack(alignment: .leading, spacing: 2) {
                Text(key).foregroundStyle(.secondary)
                Text(value).monospacedDigit().lineLimit(1).minimumScaleFactor(0.75)
                    .textSelection(.enabled)
            }
            .frame(maxWidth: .infinity, alignment: .leading)
        }
        .font(.callout)
        .accessibilityElement(children: .combine)
    }
}

/// The empty state every panel uses.
struct EmptyPanelState: View {
    let title: String
    let systemImage: String
    var detail: String? = nil
    var body: some View {
        VStack(spacing: Theme.Space.xs) {
            Image(systemName: systemImage).font(.title2).foregroundStyle(.tertiary)
            Text(title).font(.callout).foregroundStyle(.secondary)
            if let detail {
                Text(detail).font(.caption).foregroundStyle(.tertiary)
                    .multilineTextAlignment(.center)
            }
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, Theme.Space.l)
    }
}

/// A count with its noun, pluralised: "1 feature", "3 features".
func counted(_ n: Int, _ noun: String, plural: String? = nil) -> String {
    "\(n) \(n == 1 ? noun : (plural ?? noun + "s"))"
}

// MARK: - Preferences

/// User preferences, stored in UserDefaults under stable keys so the Settings
/// window (`@AppStorage`) and the model code (`Prefs.*`) read the same values.
enum Prefs {
    static let soundsKey = "vcad.prefs.sounds"
    static let hapticsKey = "vcad.prefs.haptics"
    static let gridKey = "vcad.prefs.grid"
    static let sketchGridKey = "vcad.prefs.sketchGrid"
    static let triadKey = "vcad.prefs.triad"
    static let presentationKey = "vcad.prefs.presentation"
    static let labsKey = "vcad.prefs.labs"

    private static var defaults: UserDefaults { .standard }
    private static func bool(_ key: String, default value: Bool) -> Bool {
        defaults.object(forKey: key) == nil ? value : defaults.bool(forKey: key)
    }

    /// Play the solve / verdict chimes.
    static var playsSounds: Bool { bool(soundsKey, default: true) }
    /// Haptic detents while scrubbing values.
    static var haptics: Bool { bool(hapticsKey, default: true) }
    /// Draw the reference grid and contact shadow in the windowed viewport.
    static var showsGrid: Bool { bool(gridKey, default: true) }
    /// Draw the grid on the active sketch plane while sketching — independent
    /// of the ground grid, which hides while a sketch is open.
    static var showsSketchGrid: Bool { bool(sketchGridKey, default: true) }
    /// Draw the orientation triad in the viewport corner.
    static var showsTriad: Bool { bool(triadKey, default: true) }
    /// Open new instances in a window rather than released over the desktop.
    static var opensInWindow: Bool { bool(presentationKey, default: false) }
    /// Labs: the Electronics and Manufacture (CNC Studio) workspaces and
    /// simulation. Off by default; v1 ships Design + Agent.
    static var labs: Bool { bool(labsKey, default: false) }
}

/// What the app remembers about its layout between launches: the active
/// workspace, which panels are open, and the window presentation.
struct LayoutMemory: Codable, Equatable {
    static let key = "vcad.layout"
    var workspace = "design"
    var showsTree = true
    var showsInspector = true
    var cncLeft = true
    var cncRight = true
    var cncBottom = false
    var measurementsShown = false
    var windowed = false

    static func load() -> LayoutMemory {
        guard let data = UserDefaults.standard.data(forKey: key),
              let m = try? JSONDecoder().decode(LayoutMemory.self, from: data) else { return LayoutMemory() }
        return m
    }
    func save() {
        if let data = try? JSONEncoder().encode(self) {
            UserDefaults.standard.set(data, forKey: Self.key)
        }
    }
}

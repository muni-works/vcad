import SwiftUI

// visionOS defaults for the controls inside the Mac's panels, applied at the
// ornament / window roots so the shared views stay untouched. The system
// DisclosureGroup is a full-width platter button here, which indents its
// label off the panel's text column; the panels want the Mac's chevron row.

struct PanelDisclosureStyle: DisclosureGroupStyle {
    func makeBody(configuration: Configuration) -> some View {
        VStack(alignment: .leading, spacing: 0) {
            Button {
                withAnimation(Motion.snappy) { configuration.isExpanded.toggle() }
            } label: {
                HStack(spacing: Theme.Space.s) {
                    configuration.label
                    Spacer(minLength: 0)
                    Image(systemName: "chevron.right")
                        .font(.caption.weight(.semibold)).foregroundStyle(.secondary)
                        .rotationEffect(.degrees(configuration.isExpanded ? 90 : 0))
                }
                .contentShape(Rectangle())
            }
            .buttonStyle(.plain)
            if configuration.isExpanded { configuration.content }
        }
    }
}

extension View {
    /// The Mac panel control styles, for chrome hosted in visionOS ornaments
    /// and windows.
    func macPanelControls() -> some View {
        disclosureGroupStyle(PanelDisclosureStyle())
    }
}

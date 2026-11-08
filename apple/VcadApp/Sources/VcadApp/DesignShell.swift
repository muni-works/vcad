import SwiftUI

// The Design workspace's panels: the model navigator (left rail), the
// modelling tools strip, the breadcrumb, and the viewport tools. All of them
// share the one surface, one width, and one header from Theme.swift.

/// The left rail: the document's feature tree.
struct DesignModelNavigator: View {
    @Bindable var model: EditorModel
    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.m) {
            PanelHeader(title: "Model")
            Divider()
            ScrollViewReader { proxy in
                ScrollView {
                    FeatureTreeView(model: model, embedded: true)
                }.frame(maxHeight: 330).fixedSize(horizontal: false, vertical: true)
                    .onChange(of: model.selectedFeatureID) { _, id in
                        if let id { withAnimation(Motion.snappy) { proxy.scrollTo(id, anchor: .center) } }
                    }
            }
            Divider()
            HStack {
                Text(counted(model.usesDocumentTree ? model.featureNodes.count : model.features.count, "feature"))
                Spacer(minLength: 0)
                if model.hasHiddenParts {
                    Button("Show All") { model.showAllParts() }
                        .buttonStyle(.plain).foregroundStyle(Color.accentColor)
                        .help("Show all hidden parts")
                }
            }
            .font(.caption).foregroundStyle(.secondary)
        }.padding(Theme.Space.m).frame(width: Theme.Width.navigator).panelSurface()
    }
}


/// Document › selected feature, floating at the top of the viewport.
struct DesignBreadcrumb: View {
    @Bindable var model: EditorModel
    var body: some View {
        HStack(spacing: Theme.Space.s) {
            Text(model.source.label).lineLimit(1).truncationMode(.middle)
            if let title = selectedTitle {
                Image(systemName: "chevron.right").font(.caption2)
                Text(title).foregroundStyle(.primary).lineLimit(1)
            }
        }.font(.caption).foregroundStyle(.secondary)
            .padding(.horizontal, Theme.Space.m).padding(.vertical, 7).pillSurface()
            .frame(maxWidth: 260).allowsHitTesting(false)
            .accessibilityElement(children: .combine)
    }
    private var selectedTitle: String? {
        model.selectedFeatureNode?.name ?? model.features.first { $0.id == model.selectedFeatureID }?.name
    }
}

/// Camera and display controls, bottom-right of the viewport.
struct DesignViewportTools: View {
    @Bindable var model: EditorModel
    var body: some View {
        HStack(spacing: Theme.Space.m) {
            Menu("View") {
                Button("Isometric") { model.animateCamera(to: .isometric) }
                Button("Front") { model.animateCamera(to: .front) }
                Button("Right") { model.animateCamera(to: .right) }
                Button("Top") { model.animateCamera(to: .top) }
                Divider()
                Button("Frame All") { model.resetCamera(animated: true) }
                Button("Frame Selection") { ReleaseWindowController.shared.frameSelection() }
                    .disabled(!model.hasSelection)
                Divider()
                Toggle("Zebra Analysis", isOn: $model.zebraMode)
                Button("Show All Parts") { model.showAllParts() }.disabled(!model.hasHiddenParts)
            }.menuStyle(.borderlessButton).fixedSize()
            Button { model.resetCamera(animated: true) } label: { Image(systemName: "viewfinder") }
                .buttonStyle(.borderless).help("Frame all (⌘0)").accessibilityLabel("Frame all")
            Text("mm").font(.caption).foregroundStyle(.secondary).help("Units: millimetres")
        }.padding(10).pillSurface()
    }
}

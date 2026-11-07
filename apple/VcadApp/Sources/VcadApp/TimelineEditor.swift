#if canImport(AppKit)
import AppKit
#endif
import AVFoundation
import SwiftUI
import UniformTypeIdentifiers

// Keyframe editor for the document timeline: one lane per driven joint,
// draggable key diamonds, a scrubbable playhead, key-pose / delete / easing,
// duration, and Export Video (the native twin of the web canvas recorder:
// frames are the viewport, stepped through the timeline at its fps).

struct TimelineEditor: View {
    @Bindable var model: EditorModel
    @State private var drag: (jointId: String, from: Double, to: Double)?

    private var ui: AssemblyUIState { model.assembly }
    private var duration: Double { model.timeline?.durationS ?? AsmEdit.defaultDuration }
    private static let laneHeight: CGFloat = 18
    private static let labelWidth: CGFloat = 120

    /// Driven joints (revolute / prismatic) — the ones a key can drive.
    private var lanes: [(id: String, name: String)] {
        model.asmJoints.compactMap { j in
            guard let id = j["id"] as? String,
                  JointKindChoice(rawValue: ((j["kind"] as? [String: Any])?["type"] as? String) ?? "")?.hasLimits == true
            else { return nil }
            return (id, j["name"] as? String ?? id)
        }
    }

    var body: some View {
        VStack(alignment: .leading, spacing: Theme.Space.s) {
            transport
            ScrollView(.vertical) {
                VStack(alignment: .leading, spacing: 2) {
                    ForEach(lanes, id: \.id) { lane in laneRow(lane) }
                }
            }
            .frame(maxHeight: 180)
            keyInspector
        }
        .padding(Theme.Space.m)
        .frame(width: 640)
        .panelSurface()
    }

    // MARK: transport

    private var transport: some View {
        HStack(spacing: Theme.Space.s) {
            Button { model.togglePlayback() } label: {
                Image(systemName: model.isPlaying ? "pause.fill" : "play.fill")
            }
            .buttonStyle(.borderless).disabled(model.timeline == nil)
            Text(String(format: "%.2f s", model.playbackTime)).font(.caption.monospacedDigit()).frame(width: 56)
            Button { model.keyPose() } label: { Label("Key Pose", systemImage: "diamond.fill") }
                .help("Key the dragged joints (or every driven joint) at the playhead")
            Spacer()
            Text("Length").font(.caption).foregroundStyle(.secondary)
            TextField("", value: Binding(get: { duration }, set: { model.setTimelineDuration(max(0.1, $0)) }),
                      format: .number.precision(.fractionLength(2)))
                .frame(width: 52).textFieldStyle(.roundedBorder)
            Text("s").font(.caption).foregroundStyle(.secondary)
            if let p = ui.exportProgress {
                ProgressView(value: p).frame(width: 80)
            } else {
                Button { exportVideo() } label: { Label("Export Video", systemImage: "film") }
                    .disabled(model.timeline == nil)
            }
            Button { ui.showsTimeline = false } label: { Image(systemName: "xmark.circle.fill") }
                .buttonStyle(.borderless).foregroundStyle(.tertiary).help("Close timeline")
        }
        .controlSize(.small)
    }

    // MARK: lanes

    private func laneRow(_ lane: (id: String, name: String)) -> some View {
        HStack(spacing: 4) {
            Text(lane.name).font(.caption).lineLimit(1).truncationMode(.middle)
                .frame(width: Self.labelWidth, alignment: .leading)
                .foregroundStyle(ui.selectedJointId == lane.id ? Color.accentColor : .primary)
                .onTapGesture { ui.selectedJointId = lane.id }
            GeometryReader { geo in
                let w = geo.size.width
                ZStack(alignment: .leading) {
                    Rectangle().fill(Color.secondary.opacity(0.10))
                        .contentShape(Rectangle())
                        .gesture(DragGesture(minimumDistance: 0).onChanged { g in
                            model.setPlaybackTime(Double(g.location.x / w) * duration)
                        })
                        .onTapGesture(count: 2) { model.keyJoint(lane.id) }
                    ForEach(Array(model.keys(forJoint: lane.id).enumerated()), id: \.offset) { _, k in
                        keyDiamond(jointId: lane.id, key: k, width: w)
                    }
                    Rectangle().fill(Color.red.opacity(0.8)).frame(width: 1.5)
                        .offset(x: CGFloat(model.playbackTime / duration) * w)
                        .allowsHitTesting(false)
                }
            }
            .frame(height: Self.laneHeight)
        }
    }

    private func keyDiamond(jointId: String, key: DocAnimKey, width w: CGFloat) -> some View {
        let isDragging = drag?.jointId == jointId && abs((drag?.from ?? -1) - key.t) < AsmEdit.keyEpsilon
        let t = isDragging ? drag!.to : key.t
        let selected = ui.selectedKey.map { $0.jointId == jointId && abs($0.t - key.t) < AsmEdit.keyEpsilon } ?? false
        return Image(systemName: "diamond.fill")
            .font(.system(size: 10))
            .foregroundStyle(selected ? Color.accentColor : Color.orange)
            .offset(x: CGFloat(t / duration) * w - 5)
            .help(String(format: "%.2f s → %.2f", key.t, key.value))
            .onTapGesture {
                ui.selectedKey = (jointId, key.t)
                model.setPlaybackTime(key.t)
            }
            .gesture(DragGesture(minimumDistance: 2).onChanged { g in
                let to = min(max(Double(g.location.x / w) * duration, 0), duration)
                drag = (jointId, key.t, snap(to))
            }.onEnded { _ in
                if let d = drag { model.moveKey(jointId: d.jointId, from: d.from, to: d.to) }
                drag = nil
            })
            .contextMenu {
                Button("Delete Key") { model.deleteKey(jointId: jointId, t: key.t) }
            }
    }

    /// Snap to the frame grid.
    private func snap(_ t: Double) -> Double {
        let fps = model.timeline?.fps ?? 24
        return (t * fps).rounded() / fps
    }

    // MARK: selected key

    @ViewBuilder private var keyInspector: some View {
        if let sel = ui.selectedKey,
           let key = model.keys(forJoint: sel.jointId).first(where: { abs($0.t - sel.t) < AsmEdit.keyEpsilon }) {
            HStack(spacing: Theme.Space.s) {
                Image(systemName: "diamond.fill").foregroundStyle(Color.accentColor)
                Text(AsmEdit.jointSpec(model.asmJoints.first { $0["id"] as? String == sel.jointId } ?? [:]).name ?? sel.jointId)
                    .font(.caption)
                Text(String(format: "@ %.2f s", key.t)).font(.caption.monospacedDigit()).foregroundStyle(.secondary)
                TextField("Value", value: Binding(get: { key.value },
                                                  set: { model.setKeyValue(jointId: sel.jointId, t: key.t, value: $0, ease: key.ease) }),
                          format: .number)
                    .frame(width: 70).textFieldStyle(.roundedBorder)
                Picker("Ease", selection: Binding(get: { key.ease },
                                                  set: { model.setKeyEase(jointId: sel.jointId, t: key.t, ease: $0) })) {
                    Text("Linear").tag("linear"); Text("Ease").tag("ease-in-out"); Text("Step").tag("step")
                }
                .frame(width: 130)
                Spacer()
                Button(role: .destructive) { model.deleteKey(jointId: sel.jointId, t: key.t) } label: {
                    Image(systemName: "trash")
                }
            }
            .controlSize(.small)
        } else {
            Text("Drag a joint in the Assembly panel, then Key Pose. Double-click a lane to key one joint.")
                .font(.caption).foregroundStyle(.secondary)
        }
    }

    // MARK: video export

    private func exportVideo() {
        let panel = NSSavePanel()
        panel.allowedContentTypes = [.mpeg4Movie]
        panel.nameFieldStringValue = "\(model.documentName).mp4"
        guard panel.runModal() == .OK, let url = panel.url else { return }
        Task { @MainActor in
            do {
                try await TimelineVideoExporter.export(model: model, to: url)
            } catch {
                model.assembly.message = "Video export failed: \(error.localizedDescription)"
            }
            model.assembly.exportProgress = nil
        }
    }
}

/// Steps the timeline at its fps, snapshots the viewport per frame, and
/// encodes H.264 into an .mp4.
enum TimelineVideoExporter {
    struct Failure: LocalizedError { let errorDescription: String? }

    @MainActor
    static func export(model: EditorModel, to url: URL) async throws {
        guard let tl = model.timeline else { throw Failure(errorDescription: "no timeline") }
        model.pausePlayback()
        let fps = max(1, min(60, tl.fps))
        let frames = max(1, Int((tl.durationS * fps).rounded()) + 1)
        try? FileManager.default.removeItem(at: url)

        var writer: AVAssetWriter?
        var input: AVAssetWriterInput?
        var adaptor: AVAssetWriterInputPixelBufferAdaptor?
        model.assembly.exportProgress = 0
        for i in 0..<frames {
            model.setPlaybackTime(Double(i) / fps)
            try await Task.sleep(nanoseconds: 40_000_000)      // let the viewport present the pose
            guard let cg = await frame() else { throw Failure(errorDescription: "viewport snapshot unavailable") }
            if writer == nil {
                let w = cg.width & ~1, h = cg.height & ~1
                let wr = try AVAssetWriter(outputURL: url, fileType: .mp4)
                let inp = AVAssetWriterInput(mediaType: .video, outputSettings: [
                    AVVideoCodecKey: AVVideoCodecType.h264, AVVideoWidthKey: w, AVVideoHeightKey: h,
                ])
                inp.expectsMediaDataInRealTime = false
                let ad = AVAssetWriterInputPixelBufferAdaptor(assetWriterInput: inp, sourcePixelBufferAttributes: [
                    kCVPixelBufferPixelFormatTypeKey as String: kCVPixelFormatType_32BGRA,
                    kCVPixelBufferWidthKey as String: w, kCVPixelBufferHeightKey as String: h,
                ])
                wr.add(inp)
                guard wr.startWriting() else { throw wr.error ?? Failure(errorDescription: "writer failed") }
                wr.startSession(atSourceTime: .zero)
                writer = wr; input = inp; adaptor = ad
            }
            guard let ad = adaptor, let inp = input, let pool = ad.pixelBufferPool else { continue }
            while !inp.isReadyForMoreMediaData { try await Task.sleep(nanoseconds: 5_000_000) }
            var pb: CVPixelBuffer?
            CVPixelBufferPoolCreatePixelBuffer(nil, pool, &pb)
            guard let pb else { continue }
            draw(cg, into: pb)
            ad.append(pb, withPresentationTime: CMTime(value: CMTimeValue(i), timescale: CMTimeScale(fps)))
            model.assembly.exportProgress = Double(i + 1) / Double(frames)
        }
        input?.markAsFinished()
        await writer?.finishWriting()
        if let e = writer?.error { throw e }
    }

    @MainActor
    private static func frame() async -> CGImage? {
        guard let ar = ReleaseWindowController.shared.arView else { return nil }
        return await withCheckedContinuation { cont in
            ar.snapshot(saveToHDR: false) { image in
                cont.resume(returning: image?.cgImage(forProposedRect: nil, context: nil, hints: nil))
            }
        }
    }

    private static func draw(_ cg: CGImage, into pb: CVPixelBuffer) {
        CVPixelBufferLockBaseAddress(pb, [])
        defer { CVPixelBufferUnlockBaseAddress(pb, []) }
        let w = CVPixelBufferGetWidth(pb), h = CVPixelBufferGetHeight(pb)
        guard let ctx = CGContext(data: CVPixelBufferGetBaseAddress(pb), width: w, height: h, bitsPerComponent: 8,
                                  bytesPerRow: CVPixelBufferGetBytesPerRow(pb), space: CGColorSpaceCreateDeviceRGB(),
                                  bitmapInfo: CGImageAlphaInfo.premultipliedFirst.rawValue
                                      | CGBitmapInfo.byteOrder32Little.rawValue) else { return }
        // The released viewport is transparent; give the video a solid ground.
        ctx.setFillColor(CGColor(gray: 0.13, alpha: 1))
        ctx.fill(CGRect(x: 0, y: 0, width: w, height: h))
        ctx.draw(cg, in: CGRect(x: 0, y: 0, width: w, height: h))
    }
}

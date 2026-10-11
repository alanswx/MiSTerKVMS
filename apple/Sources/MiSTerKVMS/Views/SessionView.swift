import AppKit
import KVMSCore
import SwiftUI

struct SessionView: View {
    @EnvironmentObject private var controller: SessionController
    let profile: HostProfile
    let onEdit: () -> Void
    @State private var showCaptureHint = false

    var body: some View {
        ZStack {
            CaptureSurface(controller: controller)

            if controller.videoStats?.state != "connected" {
                VideoPlaceholder(profile: profile)
                    .allowsHitTesting(false)
            }

            VStack {
                if showCaptureHint {
                    CaptureHint(mode: controller.captureMode).allowsHitTesting(false)
                }
                Spacer()
                HStack {
                    // Clickable (pin button); the empty space around it still
                    // passes clicks through to the video to start capture.
                    if controller.statsVisible || controller.statsPinned {
                        StatsOverlay().transition(.opacity)
                    }
                    Spacer()
                }
            }
            .padding(12)
            .animation(.easeInOut(duration: 0.3), value: controller.statsVisible)
        }
        .background(.black)
        .safeAreaInset(edge: .bottom, spacing: 0) { StatusBar() }
        .navigationTitle(profile.name)
        .navigationSubtitle(controller.coreRunning.isEmpty ? profile.misterHost : controller.coreRunning)
        .toolbar {
            ToolbarItemGroup {
                Button {
                    controller.openMisterMenu()
                } label: {
                    Label("MiSTer Menu (F12)", systemImage: "menucard")
                }
                .help("Sends F12, which opens the MiSTer menu (OSD). You can also press F12 while capturing.")
                .disabled(!controller.isConnected)

                if controller.capturing {
                    Button { controller.stopCapture() } label: {
                        Label("Release", systemImage: "cursorarrow.slash")
                    }
                    .help("Release keyboard and mouse (Right ⌘ + Right ⌥ + Esc)")
                } else {
                    Button { controller.startCapture() } label: {
                        Label("Capture", systemImage: "cursorarrow.rays")
                    }
                    .help("Send keyboard and mouse to MiSTer (or click the screen)")
                    .disabled(!controller.isConnected)
                }

                Button(action: onEdit) { Label("Edit Host", systemImage: "slider.horizontal.3") }
            }
            ToolbarItemGroup {
                Picker("Scale", selection: Binding(
                    get: { controller.display.scaleMode },
                    set: { mode in setDisplay { $0.scaleMode = mode } }
                )) {
                    Text("Fit").tag(ScaleMode.fit)
                    Text("Integer").tag(ScaleMode.integer)
                    Text("1:1").tag(ScaleMode.native)
                }
                .pickerStyle(.segmented)
                .help("Fit: fill the window · Integer: sharpest whole-number scale · 1:1: one MiSTer pixel per point")

                Toggle(isOn: Binding(
                    get: { controller.display.smooth },
                    set: { smooth in setDisplay { $0.smooth = smooth } }
                )) { Label("Smooth", systemImage: "wand.and.rays") }
                .help("Smooth scaling (off = sharp pixels)")

                Toggle(isOn: Binding(
                    get: { controller.display.predictiveCursor },
                    set: { on in setDisplay { $0.predictiveCursor = on } }
                )) { Label("Predictive Cursor", systemImage: "cursorarrow.motionlines") }
                .help("Draw the pointer immediately while the MiSTer's own cursor catches up (experimental)")

                Toggle(isOn: Binding(
                    get: { controller.display.autoCrop },
                    set: { crop in setDisplay { $0.autoCrop = crop } }
                )) { Label("Auto-crop", systemImage: "crop") }
                .help("Hide MiSTer's black borders")

                ControlGroup {
                    Button { controller.zoom(by: 1 / 1.25) } label: { Label("Zoom Out", systemImage: "minus.magnifyingglass") }
                    Button { controller.resetZoom() } label: {
                        Text("\(Int((controller.display.zoom * 100).rounded()))%").monospacedDigit()
                    }
                    .help("Reset zoom (⌘0)")
                    Button { controller.zoom(by: 1.25) } label: { Label("Zoom In", systemImage: "plus.magnifyingglass") }
                }

                Button { controller.toggleRecording() } label: {
                    Label(controller.isRecording ? "Stop Recording" : "Record",
                          systemImage: controller.isRecording ? "stop.circle.fill" : "record.circle")
                }
                .foregroundStyle(controller.isRecording ? .red : .primary)
                .help(controller.isRecording ? "Stop recording (⌘⇧R)" : "Record video and audio to Movies/MiSTerKVMS (⌘⇧R)")
                .disabled(!controller.isRecording && controller.videoStats?.state != "connected")

                Toggle(isOn: $controller.muted) {
                    Label("Mute", systemImage: controller.muted ? "speaker.slash" : "speaker.wave.2")
                }
            }
        }
        .onChange(of: controller.capturing) { _, capturing in
            guard capturing else { showCaptureHint = false; return }
            withAnimation { showCaptureHint = true }
            DispatchQueue.main.asyncAfter(deadline: .now() + 4) {
                withAnimation { showCaptureHint = false }
            }
        }
    }
}

extension SessionView {
    /// Apply a display change (the core remembers scale, smoothing,
    /// auto-crop and the predictive cursor for this host).
    private func setDisplay(_ change: (inout DisplaySettings) -> Void) {
        controller.updateDisplay(change)
    }
}

private struct VideoPlaceholder: View {
    @EnvironmentObject private var controller: SessionController
    let profile: HostProfile

    var body: some View {
        VStack(spacing: 8) {
            Image(systemName: "display").font(.system(size: 48)).foregroundStyle(.secondary)
            if profile.ndiSource.isEmpty {
                Text("No NDI source set").font(.headline)
                Text("Edit the host and choose the OBS NDI source for this MiSTer.")
                    .font(.caption).foregroundStyle(.secondary)
            } else if let error = controller.videoError {
                Text("Video unavailable").font(.headline)
                Text(error).font(.caption).foregroundStyle(.secondary)
                    .multilineTextAlignment(.center).frame(maxWidth: 420)
            } else {
                Text(profile.ndiSource).font(.headline)
                Text(stateText).font(.caption).foregroundStyle(.secondary)
            }
            if controller.isConnected && !controller.capturing {
                Text("Click here to capture keyboard and mouse")
                    .font(.callout).padding(.top, 12)
            }
        }
        .foregroundStyle(.white)
    }

    private var stateText: String {
        switch controller.videoStats?.state {
        case nil, "searching": "Looking for the NDI source…"
        case "no video": "Connected, waiting for video"
        case "not found":
            if let seen = controller.videoStats?.availableSources, !seen.isEmpty {
                "Not found on the network. Visible sources: \(seen.joined(separator: ", ")). Edit the host to choose one."
            } else {
                "Not found, and no NDI sources are visible. Is OBS running with its NDI output on?"
            }
        case let other?: other
        }
    }
}

private struct CaptureHint: View {
    @EnvironmentObject private var controller: SessionController
    let mode: InputCapture.Mode?

    var body: some View {
        VStack(spacing: 4) {
            Text("Keyboard and mouse go to MiSTer")
                .fontWeight(.semibold)
            Text("Press **Right ⌘ + Right ⌥ + Esc** to release · **F12** opens the MiSTer menu")
            if controller.profile?.rawMouse == true && !controller.rawMouseActive {
                Text("Raw mouse input needs Input Monitoring permission: using macOS acceleration.")
                    .foregroundStyle(.yellow)
            }
            if mode == .appOnly {
                Text("Accessibility not allowed: macOS keeps ⌘Tab, ⌘Space and similar shortcuts.")
                    .foregroundStyle(.yellow)
            }
        }
        .font(.callout)
        .padding(.horizontal, 16).padding(.vertical, 10)
        .background(.black.opacity(0.75), in: RoundedRectangle(cornerRadius: 10))
        .foregroundStyle(.white)
        .transition(.move(edge: .top).combined(with: .opacity))
    }
}

private struct StatsOverlay: View {
    @EnvironmentObject private var controller: SessionController

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text("INPUT").fontWeight(.bold)
                Spacer()
                Button {
                    controller.statsPinned.toggle()
                } label: {
                    Image(systemName: controller.statsPinned ? "pin.fill" : "pin")
                }
                .buttonStyle(.plain)
                .help(controller.statsPinned ? "Unpin: hide automatically (⌘⇧I)" : "Pin: always show (⌘⇧I)")
            }
            if let s = controller.stats {
                Text("link   \(s.connected ? "up" : "down")")
                Text("msg/s  \(s.messagesPerSec, format: .number.precision(.fractionLength(0)))")
                Text("sent   \(s.messagesSent)")
                Text("queue  \(s.queued)")
            }
            Text("VIDEO").fontWeight(.bold).padding(.top, 4)
            if let v = controller.videoStats {
                Text("state  \(v.state)")
                Text("src    \(v.sourceWidth)×\(v.sourceHeight)")
                Text("crop   \(v.cropWidth)×\(v.cropHeight)")
                Text("fps    \(v.receivedFps, format: .number.precision(.fractionLength(1))) in / \(v.renderedFps, format: .number.precision(.fractionLength(1))) drawn")
                Text("drop   \(v.droppedFrames) ndi / \(v.skippedFrames) skipped")
                Text("age    \(ageText(v.frameAgeMs))")
                Text("audio  \(v.audioBufferMs, format: .number.precision(.fractionLength(0))) ms buf · \(v.audioUnderruns) underruns")
                if controller.display.predictiveCursor {
                    Text("cursor \(v.cursorLocked ? "tracked" : "searching") · err \(v.cursorErrorPx, format: .number.precision(.fractionLength(0))) px")
                    Text("model  lag \(v.cursorLagMs, format: .number.precision(.fractionLength(0))) ms · ×\(v.cursorGain, format: .number.precision(.fractionLength(2))) · \(v.cursorFitSamples) fits/min")
                }
            } else {
                Text("off")
            }
        }
        .font(.system(size: 11, design: .monospaced))
        .padding(8)
        .frame(minWidth: 230, alignment: .leading)
        .background(.black.opacity(0.6), in: RoundedRectangle(cornerRadius: 6))
        .foregroundStyle(.green)
    }
}

/// NDI frame age needs the OBS PC and Mac clocks to agree; a value over a
/// second almost always means they don't.
private func ageText(_ ms: Double) -> String {
    if ms < 0 { return "n/a" }
    if ms > 1000 { return String(format: "clocks differ (~%.1f s)", ms / 1000) }
    return String(format: "≈%.0f ms OBS→screen", ms)
}

private struct StatusBar: View {
    @EnvironmentObject private var controller: SessionController

    var body: some View {
        HStack(spacing: 12) {
            Circle().fill(color).frame(width: 8, height: 8)
            Text(text).lineLimit(1)
            if let since = controller.recordingSince {
                HStack(spacing: 4) {
                    Circle().fill(.red).frame(width: 8, height: 8)
                    Text(timerInterval: since...Date.distantFuture, countsDown: false)
                        .monospacedDigit()
                }
                .foregroundStyle(.red)
            } else if let error = controller.recordingError {
                Text("Recording failed: \(error)").foregroundStyle(.red).lineLimit(1)
            } else if let last = controller.lastRecording {
                Button("Saved \(last.lastPathComponent) · Show in Finder") { controller.showLastRecording() }
                    .buttonStyle(.link)
                    .lineLimit(1)
            }
            Spacer()
            if !controller.gameRunning.isEmpty {
                Text(controller.gameRunning).lineLimit(1).foregroundStyle(.secondary)
            }
            Text("F12 = MiSTer menu").foregroundStyle(.secondary)
            Text(controller.capturing ? "Capturing — Right ⌘ + Right ⌥ + Esc to release" : "Not capturing")
                .foregroundStyle(controller.capturing ? .primary : .secondary)
        }
        .font(.caption)
        .padding(.horizontal, 12).padding(.vertical, 6)
        .background(.bar)
    }

    private var color: Color {
        switch controller.connection {
        case .connected: .green
        case .connecting: .yellow
        case .disconnected: .red
        }
    }

    private var text: String {
        switch controller.connection {
        case .connected:
            controller.coreRunning.isEmpty ? "Connected · MiSTer menu" : "Connected · \(controller.coreRunning)"
        case .connecting: "Connecting…"
        case .disconnected(let reason): "Disconnected: \(reason)"
        }
    }
}

/// Hosts the video (a Metal layer added by the core) and anchors input capture.
struct CaptureSurface: NSViewRepresentable {
    let controller: SessionController

    func makeNSView(context: Context) -> SurfaceView {
        let view = SurfaceView()
        view.controller = controller
        return view
    }

    func updateNSView(_ view: SurfaceView, context: Context) {
        view.controller = controller
    }

    final class SurfaceView: NSView {
        weak var controller: SessionController?

        override init(frame: NSRect) {
            super.init(frame: frame)
            wantsLayer = true
            layer?.backgroundColor = NSColor.black.cgColor
        }

        required init?(coder: NSCoder) { fatalError("init(coder:) is not used") }

        override var acceptsFirstResponder: Bool { true }
        override func acceptsFirstMouse(for event: NSEvent?) -> Bool { true }

        override func viewDidMoveToWindow() {
            super.viewDidMoveToWindow()
            if window != nil {
                controller?.attach(view: self)
            } else {
                // Leaving the window (no host selected): stop drawing into it
                // before it can go away.
                controller?.detach(view: self)
            }
        }

        override func layout() {
            super.layout()
            controller?.viewResized(self)
        }

        override func viewDidChangeBackingProperties() {
            super.viewDidChangeBackingProperties()
            controller?.viewResized(self)
        }

        override func mouseDown(with event: NSEvent) {
            window?.makeFirstResponder(self)
            controller?.startCapture()
        }

        // Only reached when not capturing (capture swallows these).
        override func magnify(with event: NSEvent) {
            controller?.zoom(by: 1 + event.magnification)
        }

        override func scrollWheel(with event: NSEvent) {
            guard let controller, controller.display.zoom > 1 else { return }
            let w = max(bounds.width, 1) * controller.display.zoom
            let h = max(bounds.height, 1) * controller.display.zoom
            controller.updateDisplay {
                $0.panX += event.scrollingDeltaX / w
                $0.panY += event.scrollingDeltaY / h
            }
        }
    }
}

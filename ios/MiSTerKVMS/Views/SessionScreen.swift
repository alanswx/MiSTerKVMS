import SwiftUI

/// One MiSTer, full screen (P13): the video as a touchpad, the keyboard and
/// function bar, the remote, the control banner. Each can be hidden from
/// the controls menu (the corner button, or a three-finger tap), P14.
struct SessionScreen: View {
    @EnvironmentObject private var model: AppModel
    @AppStorage(Pref.keyboard) private var showKeyboard = true
    @AppStorage(Pref.functionBar) private var showFunctionBar = true
    @AppStorage(Pref.remote) private var showRemote = true
    @AppStorage(Pref.banner) private var showBanner = true
    @AppStorage(Pref.stats) private var showStats = false
    @State private var menuOpen = false

    var body: some View {
        ZStack {
            Color.black.ignoresSafeArea()

            TouchpadVideo(model: model, onMenu: { menuOpen.toggle() }, onSwitch: { model.step($0) })
                .ignoresSafeArea(.container)

            KeyboardHost(model: model, showKeyboard: showKeyboard && !menuOpen, showFunctionBar: showFunctionBar && !menuOpen)
                .frame(width: 0, height: 0)

            statusOverlay

            if showRemote { RemoteView() }

            VStack(spacing: 6) {
                HStack(alignment: .top) {
                    if showBanner, let banner = controlBanner { banner }
                    Spacer()
                    Button { menuOpen = true } label: {
                        Image(systemName: "slider.horizontal.3")
                            .font(.system(size: 17))
                            .frame(width: 40, height: 40)
                            .background(Circle().fill(.black.opacity(0.5)))
                    }
                    .tint(.white)
                    .accessibilityLabel("Controls")
                }
                if showStats { stats }
                Spacer()
            }
            .padding(.horizontal, 12)
            .padding(.top, 4)
        }
        .statusBarHidden()
        .persistentSystemOverlays(.hidden)
        .sheet(isPresented: $menuOpen) {
            ControlsMenu()
                .presentationDetents([.medium, .large])
                .presentationBackground(.ultraThinMaterial)
        }
    }

    /// Connecting, or why there's no picture.
    @ViewBuilder private var statusOverlay: some View {
        let text: String? = {
            switch model.state.connection {
            case .connecting: return "Connecting to \(model.profile?.name ?? "MiSTer")…"
            case .disconnected(let reason): return reason.isEmpty ? "Not connected" : reason
            case .connected:
                if let stream = model.state.stream, !stream.connected { return stream.error.isEmpty ? "Connecting to the capture server…" : stream.error }
                return model.state.videoError
            }
        }()
        if let text {
            Text(text)
                .font(.callout)
                .multilineTextAlignment(.center)
                .padding(12)
                .background(RoundedRectangle(cornerRadius: 10).fill(.black.opacity(0.7)))
                .padding(.horizontal, 40)
                .allowsHitTesting(false)
        }
    }

    /// "Watching — Name has control" with Take control (CAPTURE-SERVER.md §7).
    private var controlBanner: AnyView? {
        guard let stream = model.state.stream, stream.connected else { return nil }
        if model.someoneElseInControl {
            return AnyView(
                HStack(spacing: 8) {
                    Text("\(stream.holder) has control").font(.footnote)
                    Button("Take control") { model.takeControl() }
                        .font(.footnote.weight(.semibold))
                        .buttonStyle(.borderedProminent)
                        .controlSize(.small)
                }
                .padding(.horizontal, 10).padding(.vertical, 6)
                .background(Capsule().fill(.black.opacity(0.7)))
            )
        }
        if !stream.misterConnected {
            return AnyView(
                Text("The capture server can't reach the MiSTer")
                    .font(.footnote)
                    .padding(.horizontal, 10).padding(.vertical, 6)
                    .background(Capsule().fill(.black.opacity(0.7)))
            )
        }
        return nil
    }

    private var stats: some View {
        let v = model.state.videoStats
        let parts: [String] = [
            v.map { "\($0.sourceWidth)×\($0.sourceHeight)" },
            v.map { String(format: "%.0f fps", $0.receivedFps) },
            v.map { String(format: "%.0f ms", $0.frameAgeMs) },
            model.state.stream.map { String(format: "rtt %.0f ms", $0.roundTripMs) },
        ].compactMap { $0 }
        return HStack {
            Text(parts.joined(separator: "  ·  "))
                .font(.caption.monospacedDigit())
                .padding(.horizontal, 8).padding(.vertical, 4)
                .background(Capsule().fill(.black.opacity(0.6)))
            Spacer()
        }
        .allowsHitTesting(false)
    }
}

/// Show or hide each part of the session screen; back to the host list.
struct ControlsMenu: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @AppStorage(Pref.keyboard) private var showKeyboard = true
    @AppStorage(Pref.functionBar) private var showFunctionBar = true
    @AppStorage(Pref.remote) private var showRemote = true
    @AppStorage(Pref.banner) private var showBanner = true
    @AppStorage(Pref.stats) private var showStats = false
    @AppStorage(Pref.remoteOpacity) private var remoteOpacity = 0.6

    var body: some View {
        NavigationStack {
            Form {
                Section("Show") {
                    Toggle("Keyboard", isOn: $showKeyboard)
                    Toggle("Function keys", isOn: $showFunctionBar)
                    Toggle("Remote", isOn: $showRemote)
                    if showRemote {
                        HStack {
                            Text("See-through")
                            Slider(value: $remoteOpacity, in: 0.25...1)
                        }
                    }
                    Toggle("Control banner", isOn: $showBanner)
                    Toggle("Stats", isOn: $showStats)
                    Toggle("Sound", isOn: Binding(get: { !model.muted }, set: { model.muted = !$0 }))
                }
                Section {
                    Button("Reset zoom") { model.resetZoom() }
                    if model.someoneElseInControl {
                        Button("Take control") { model.takeControl(); dismiss() }
                    }
                } footer: {
                    Text("Three-finger tap: this menu, even with everything hidden. Three-finger swipe: next or previous MiSTer.")
                }
                Section {
                    Button("All MiSTers", role: .destructive) {
                        dismiss()
                        model.close()
                    }
                }
            }
            .navigationTitle(model.profile?.name ?? "MiSTer")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) { Button("Done") { dismiss() } }
            }
        }
    }
}

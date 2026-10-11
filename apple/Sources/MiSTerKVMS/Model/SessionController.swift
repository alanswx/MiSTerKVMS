import AppKit
import Foundation
import QuartzCore
import KVMSCore

/// UI-facing state for the one live MiSTer session. The session logic (input
/// and video sessions, stats overlay rule, saving learned values, display
/// settings per host) lives in the core's Controller; this mirrors its state
/// for SwiftUI and adds what's macOS-specific: input capture, the video view
/// and the AVFoundation recorder.
@MainActor
final class SessionController: ObservableObject {
    let core: Controller
    @Published private(set) var state: ControllerState
    @Published private(set) var captureMode: InputCapture.Mode?
    @Published private(set) var rawMouseActive = false
    /// When the current recording started (nil = not recording).
    @Published private(set) var recordingSince: Date?
    /// Told when the saved hosts change.
    weak var profiles: ProfileStore?

    let capture = InputCapture()
    private var recorder: Recorder?
    private(set) weak var captureView: NSView?

    init() {
        let observer = Observer()
        let core = Controller(observer: observer)
        self.core = core
        state = core.state()
        observer.controller = self
        capture.onEnd = { [weak self] in
            self?.captureMode = nil
            self?.core.setCapturing(capturing: false)
        }
    }

    // MARK: - State

    var profile: HostProfile? {
        guard let id = state.profileId else { return nil }
        return profiles?.profiles.first { $0.id == id }
    }
    var connection: ConnectionState { state.connection }
    var coreRunning: String { state.coreRunning }
    var gameRunning: String { state.gameRunning }
    var capturing: Bool { state.capturing }
    var stats: InputStats? { state.inputStats }
    var videoStats: VideoStats? { state.videoStats }
    var videoError: String? { state.videoError }
    var display: DisplaySettings { state.display }
    var statsVisible: Bool { state.statsVisible }
    var lastRecording: URL? { state.lastRecording.map { URL(fileURLWithPath: $0) } }
    var recordingError: String? { state.recordingError }

    var muted: Bool {
        get { state.muted }
        set { core.setMuted(muted: newValue) }
    }

    var statsPinned: Bool {
        get { state.statsPinned }
        set { core.setStatsPinned(pinned: newValue) }
    }

    var isConnected: Bool {
        if case .connected = state.connection { return true }
        return false
    }

    fileprivate func handle(_ event: ControllerEvent) {
        switch event {
        case .changed:
            refresh()
        case .profilesChanged:
            profiles?.reload()
            refresh()
        case .captureRequested:
            // D6: capture automatically once the connection is up.
            startCapture()
        case .releaseRequested:
            capture.stop()
        }
    }

    private func refresh() {
        let new = core.state()
        if new.recordingSeconds == nil {
            recordingSince = nil
        } else if recordingSince == nil, let seconds = new.recordingSeconds {
            recordingSince = Date().addingTimeInterval(-seconds)
        }
        state = new
    }

    // MARK: - Hosts

    func open(_ profile: HostProfile) {
        guard profile.id != state.profileId else { return }
        capture.stop()
        stopRecording()
        removeVideoLayers()
        core.open(id: profile.id)
        capture.session = core.inputSession()
        capture.rawMouse = profile.rawMouse
        refresh()
    }

    /// Save a new or edited host. An edited open host reconnects with the
    /// new settings.
    func save(_ profile: HostProfile) {
        let isOpen = profile.id == state.profileId
        if isOpen { close() }
        profiles?.upsert(profile)
        if isOpen { open(profile) }
    }

    func close() {
        capture.stop()
        stopRecording()
        core.close()
        capture.session = nil
        removeVideoLayers()
        refresh()
    }

    // MARK: - Video view

    /// Called by the video view once it's in a window (and again if it's recreated).
    func attach(view: NSView) {
        guard view !== captureView else { return }
        if captureView != nil { core.detachSurface() }
        removeVideoLayers()
        captureView = view
        let size = Self.pixelSize(of: view)
        core.attachSurface(
            surface: .appKit(nsView: UInt64(UInt(bitPattern: Unmanaged.passUnretained(view).toOpaque()))),
            widthPx: size.width, heightPx: size.height, pointsScale: size.scale)
    }

    /// The video view is leaving its window: the core must let go of it.
    func detach(view: NSView) {
        guard view === captureView else { return }
        capture.stop()
        core.detachSurface()
        removeVideoLayers()
        captureView = nil
    }

    func viewResized(_ view: NSView) {
        guard view === captureView else { return }
        let size = Self.pixelSize(of: view)
        core.resizeSurface(widthPx: size.width, heightPx: size.height, pointsScale: size.scale)
    }

    private static func pixelSize(of view: NSView) -> (width: UInt32, height: UInt32, scale: Float) {
        let scale = view.window?.backingScaleFactor ?? 2
        return (UInt32(max(1, view.bounds.width * scale)), UInt32(max(1, view.bounds.height * scale)), Float(scale))
    }

    /// The renderer adds a Metal sublayer to the view for each session and
    /// leaves it behind when the session ends. Remove it, or the previous
    /// MiSTer's last frame stays visible after switching hosts. (A layer that
    /// is still being drawn into just stops being shown.)
    private func removeVideoLayers() {
        captureView?.layer?.sublayers?
            .filter { $0 is CAMetalLayer }
            .forEach { $0.removeFromSuperlayer() }
    }

    // MARK: - Display

    func updateDisplay(_ change: (inout DisplaySettings) -> Void) {
        var display = state.display
        change(&display)
        core.setDisplay(settings: display)
    }

    func zoom(by factor: Double) { core.zoomBy(factor: factor) }
    func resetZoom() { core.resetZoom() }

    /// Show the overlay for a few seconds (unless pinned, when it's always on).
    func flashStats() { core.flashStats() }

    // MARK: - Recording

    var isRecording: Bool { state.recordingSeconds != nil }

    func toggleRecording() {
        isRecording ? stopRecording() : startRecording()
    }

    func startRecording() {
        guard !isRecording, let profile else { return }
        let recorder = Recorder(baseURL: Recorder.defaultURL(hostName: profile.name))
        recorder.onFinished = { [weak self] result in
            switch result {
            case .success(let url):
                self?.core.recordingFinished(path: url.path, error: nil)
            case .failure(let error):
                self?.core.recordingFinished(path: nil, error: error.localizedDescription)
            }
        }
        do {
            try core.startRecordingWithSink(sink: recorder)
            self.recorder = recorder
        } catch {
            core.recordingFinished(path: nil, error: "\(error)")
        }
    }

    func stopRecording() {
        guard let recorder else { return }
        core.stopRecording()
        recorder.stop()
        self.recorder = nil
    }

    func showLastRecording() {
        guard let lastRecording else { return }
        NSWorkspace.shared.activateFileViewerSelecting([lastRecording])
    }

    // MARK: - Input

    func startCapture() {
        guard isConnected, !capturing, let view = captureView else { return }
        capture.session = core.inputSession()
        capture.start(over: view)
        rawMouseActive = capture.rawActive
        captureMode = capture.mode
        core.setCapturing(capturing: capture.isActive)
    }

    func stopCapture() {
        capture.stop()
    }

    func openMisterMenu() {
        core.openMenu()
    }
}

/// Bridges core events (background threads) onto the main actor.
private final class Observer: ControllerObserver, @unchecked Sendable {
    weak var controller: SessionController?

    func onEvent(event: ControllerEvent) {
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated { self?.controller?.handle(event) }
        }
    }
}

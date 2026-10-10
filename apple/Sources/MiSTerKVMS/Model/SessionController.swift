import AppKit
import Foundation
import KVMSCore

/// UI-facing state for the one live MiSTer session.
@MainActor
final class SessionController: ObservableObject {
    @Published private(set) var profile: HostProfile?
    @Published private(set) var connection: ConnectionState = .disconnected(reason: "not connected")
    @Published private(set) var coreRunning = ""
    @Published private(set) var gameRunning = ""
    @Published private(set) var capturing = false
    @Published private(set) var captureMode: InputCapture.Mode?
    @Published private(set) var rawMouseActive = false
    /// When the current recording started (nil = not recording).
    @Published private(set) var recordingSince: Date?
    /// Last finished recording, for "Show in Finder".
    @Published private(set) var lastRecording: URL?
    @Published private(set) var recordingError: String?
    private var recorder: Recorder?
    @Published private(set) var stats: InputStats?
    /// Stats overlay: shown for a few seconds when something changes, or
    /// always when pinned.
    @Published private(set) var statsVisible = false
    @Published var statsPinned = UserDefaults.standard.bool(forKey: "statsPinned") {
        didSet {
            UserDefaults.standard.set(statsPinned, forKey: "statsPinned")
            if !statsPinned { flashStats() }
        }
    }
    @Published private(set) var videoStats: VideoStats?
    @Published private(set) var videoError: String?
    @Published private(set) var display = DisplaySettings(
        scaleMode: .fit, smooth: false, zoom: 1, panX: 0, panY: 0, autoCrop: true,
        predictiveCursor: true)
    /// Where learned per-host values (cursor model) are saved.
    weak var profiles: ProfileStore?
    @Published var muted = false {
        didSet { video?.setMuted(muted: muted) }
    }

    let capture = InputCapture()
    private var session: MisterSession?
    private var video: VideoSession?
    /// Start capture once the connection is up (D6: auto-capture on connect).
    private var captureOnConnect = false
    private var statsTimer: Timer?
    /// Saves what the predictive cursor learned once a minute.
    private var modelSaveTimer: Timer?
    private var statsHideAt = Date.distantPast
    private var statsSignature = ""
    private static let statsFlashSeconds: TimeInterval = 5
    /// Bumped per session so late callbacks from a closed session are ignored.
    private var generation = 0
    private(set) weak var captureView: NSView?

    /// Called by the video view once it exists (and again if it's recreated).
    func attach(view: NSView) {
        guard view !== captureView else { return }
        stopVideo()
        captureView = view
        startVideo()
    }

    init() {
        capture.onEnd = { [weak self] in
            self?.capturing = false
            self?.captureMode = nil
            self?.video?.setPointerCaptured(captured: false)
        }
    }

    func open(_ profile: HostProfile) {
        guard profile != self.profile else { return }
        close()
        self.profile = profile
        display = DisplaySettings(
            scaleMode: ScaleMode(profile.scaleMode), smooth: profile.smoothScaling,
            zoom: 1, panX: 0, panY: 0, autoCrop: profile.autoCrop,
            predictiveCursor: profile.predictiveCursor)
        startVideo()
        guard !profile.misterHost.isEmpty else {
            connection = .disconnected(reason: "no MiSTer address set")
            return
        }
        generation += 1
        let session = MisterSession(
            host: profile.misterHost, port: profile.misterPort,
            observer: Observer(controller: self, generation: generation)
        )
        session.setSettings(settings: InputSettings(
            mouseSensitivity: profile.mouseSensitivity,
            altClickIsMiddle: profile.altClickIsMiddle
        ))
        self.session = session
        capture.session = session
        capture.rawMouse = profile.rawMouse
        video?.linkInput(session: session)
        captureOnConnect = true
        ensureStatsTimer()
    }

    func close() {
        capture.stop()
        stopVideo()
        session?.disconnect()
        session = nil
        capture.session = nil
        statsTimer?.invalidate()
        statsTimer = nil
        profile = nil
        coreRunning = ""
        gameRunning = ""
        stats = nil
        connection = .disconnected(reason: "not connected")
    }

    // MARK: - Video

    private func startVideo() {
        guard video == nil, let profile, !profile.ndiSource.isEmpty, let view = captureView,
              view.window != nil else { return }
        let size = Self.pixelSize(of: view)
        do {
            let video = try VideoSession(
                ndiSource: profile.ndiSource, extraIp: profile.obsHost,
                nsView: UInt64(UInt(bitPattern: Unmanaged.passUnretained(view).toOpaque())),
                widthPx: size.width, heightPx: size.height, pointsScale: size.scale)
            video.setDisplay(settings: display)
            video.setMuted(muted: muted)
            if profile.cursorLagMs > 0 {
                // Gain 0 = mouse scaling changed since it was learned; start neutral.
                let gain = profile.cursorGain > 0 ? profile.cursorGain : 1
                video.setCursorModel(model: CursorModel(lagMs: profile.cursorLagMs, gain: gain))
            }
            if let session { video.linkInput(session: session) }
            modelSaveTimer = Timer.scheduledTimer(withTimeInterval: 60, repeats: true) { [weak self] _ in
                MainActor.assumeIsolated { self?.saveCursorModel() }
            }
            video.setPointerCaptured(captured: capturing)
            self.video = video
            videoError = nil
        } catch {
            videoError = "\(error)"
        }
        ensureStatsTimer()
    }

    private func ensureStatsTimer() {
        guard statsTimer == nil else { return }
        statsTimer = Timer.scheduledTimer(withTimeInterval: 0.5, repeats: true) { [weak self] _ in
            MainActor.assumeIsolated { self?.refreshStats() }
        }
    }

    private func refreshStats() {
        stats = session?.stats()
        videoStats = video?.stats()
        checkStatsSignature()
        if statsVisible && !statsPinned && Date() >= statsHideAt {
            statsVisible = false
        }
    }

    /// Show the overlay for a few seconds (unless pinned, when it's always on).
    func flashStats() {
        statsHideAt = Date().addingTimeInterval(Self.statsFlashSeconds)
        statsVisible = true
    }

    /// Re-show the overlay when anything worth noticing changes. Audio
    /// underruns are left out: they also count while no audio is playing.
    private func checkStatsSignature() {
        let connected: String = switch connection {
        case .connected: "up"
        case .connecting: "connecting"
        case .disconnected: "down"
        }
        var parts = [connected, coreRunning]
        if let v = videoStats {
            parts += [v.state, "\(v.sourceWidth)x\(v.sourceHeight)",
                      "\(v.cropWidth)x\(v.cropHeight)", "\(v.droppedFrames)"]
        }
        let signature = parts.joined(separator: "|")
        if signature != statsSignature {
            statsSignature = signature
            flashStats()
        }
    }

    // MARK: - Recording

    var isRecording: Bool { recordingSince != nil }

    func toggleRecording() {
        isRecording ? stopRecording() : startRecording()
    }

    func startRecording() {
        guard !isRecording, let video, let profile else { return }
        let recorder = Recorder(baseURL: Recorder.defaultURL(hostName: profile.name))
        recorder.onFinished = { [weak self] result in
            switch result {
            case .success(let url):
                self?.lastRecording = url
                self?.recordingError = nil
            case .failure(let error):
                self?.recordingError = error.localizedDescription
            }
        }
        video.setMediaSink(sink: recorder)
        self.recorder = recorder
        recordingSince = Date()
        recordingError = nil
    }

    func stopRecording() {
        guard let recorder else { return }
        video?.setMediaSink(sink: nil)
        recorder.stop()
        self.recorder = nil
        recordingSince = nil
    }

    func showLastRecording() {
        guard let lastRecording else { return }
        NSWorkspace.shared.activateFileViewerSelecting([lastRecording])
    }

    private func stopVideo() {
        stopRecording()
        modelSaveTimer?.invalidate()
        modelSaveTimer = nil
        saveCursorModel()
        video?.stop()
        video = nil
        videoStats = nil
    }

    /// Remember what the predictive cursor learned for next time.
    private func saveCursorModel() {
        guard let video, let id = profile?.id, let profiles,
              var saved = profiles.profiles.first(where: { $0.id == id }) else { return }
        // Nothing measured this session: keep what was saved before.
        guard (video.stats().cursorFitSamples) > 0 else { return }
        let model = video.cursorModel()
        saved.cursorLagMs = model.lagMs
        saved.cursorGain = model.gain
        profiles.upsert(saved)
    }

    func viewResized(_ view: NSView) {
        guard view === captureView else { return }
        if video == nil {
            startVideo() // the view may not have had a window at attach time
        }
        let size = Self.pixelSize(of: view)
        video?.resize(widthPx: size.width, heightPx: size.height, pointsScale: size.scale)
    }

    private static func pixelSize(of view: NSView) -> (width: UInt32, height: UInt32, scale: Float) {
        let scale = view.window?.backingScaleFactor ?? 2
        return (UInt32(max(1, view.bounds.width * scale)), UInt32(max(1, view.bounds.height * scale)), Float(scale))
    }

    func updateDisplay(_ change: (inout DisplaySettings) -> Void) {
        change(&display)
        display.zoom = min(max(display.zoom, 0.25), 16)
        display.panX = min(max(display.panX, -0.5), 0.5)
        display.panY = min(max(display.panY, -0.5), 0.5)
        if display.zoom <= 1 { display.panX = 0; display.panY = 0 }
        video?.setDisplay(settings: display)
    }

    func zoom(by factor: Double) { updateDisplay { $0.zoom *= factor } }
    func resetZoom() { updateDisplay { $0.zoom = 1 } }

    var isConnected: Bool {
        if case .connected = connection { return true }
        return false
    }

    func startCapture() {
        guard isConnected, !capturing, let view = captureView else { return }
        capture.start(over: view)
        capturing = capture.isActive
        rawMouseActive = capture.rawActive
        video?.setPointerCaptured(captured: capturing)
        captureMode = capture.mode
    }

    func stopCapture() {
        capture.stop()
    }

    func openMisterMenu() {
        session?.openMenu()
    }

    fileprivate func update(_ state: ConnectionState, generation: Int) {
        guard generation == self.generation, session != nil else { return }
        connection = state
        checkStatsSignature()
        switch state {
        case .connected:
            if captureOnConnect {
                captureOnConnect = false
                startCapture()
            }
        case .disconnected:
            capture.stop()
        case .connecting:
            break
        }
    }

    fileprivate func update(status key: String, value: String, generation: Int) {
        guard generation == self.generation, session != nil else { return }
        switch key {
        case "coreRunning": coreRunning = value
        case "gameRunning": gameRunning = value
        default: break
        }
    }
}

/// Bridges core callbacks (worker thread) onto the main actor.
private final class Observer: SessionObserver, @unchecked Sendable {
    weak var controller: SessionController?
    let generation: Int

    init(controller: SessionController, generation: Int) {
        self.controller = controller
        self.generation = generation
    }

    func onState(state: ConnectionState) {
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated { self?.controller?.update(state, generation: self?.generation ?? -1) }
        }
    }

    func onStatus(key: String, value: String) {
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated { self?.controller?.update(status: key, value: value, generation: self?.generation ?? -1) }
        }
    }
}

extension ScaleMode {
    init(_ name: String) {
        switch name {
        case "integer": self = .integer
        case "native": self = .native
        default: self = .fit
        }
    }

    var name: String {
        switch self {
        case .fit: "fit"
        case .integer: "integer"
        case .native: "native"
        }
    }
}

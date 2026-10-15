import AVFoundation
import UIKit

/// UI-facing state for the app: the saved hosts and the one open session.
/// The session logic lives in the core's Controller (as on the Mac); this
/// mirrors its state for SwiftUI and adds what's iOS-specific: the video
/// view, touch input, the on-screen modifiers and the app lifecycle.
@MainActor
final class AppModel: ObservableObject {
    let core: Controller
    @Published private(set) var state: ControllerState
    @Published private(set) var profiles: [HostProfile] = []
    /// Function bar modifiers (HID usages): latched ones apply to the next
    /// key only, locked ones until tapped again.
    @Published private(set) var latched: Set<UInt16> = []
    @Published private(set) var locked: Set<UInt16> = []

    private weak var videoView: UIView?
    /// The host that was open when the app went to the background.
    private var suspendedId: String?

    init() {
        let observer = Observer()
        core = Controller(observer: observer)
        state = core.state()
        observer.model = self
        profiles = core.profiles()
        // Other viewers see this name ("iPhone has control"). The core falls
        // back to "MiSTerKVMS" where it can't read a computer name, as on iOS.
        if core.viewerName() == "MiSTerKVMS" {
            core.setViewerName(name: UIDevice.current.name)
        }
        let audio = AVAudioSession.sharedInstance()
        try? audio.setCategory(.playback, mode: .default, options: [.mixWithOthers])
        try? audio.setActive(true)
    }

    fileprivate func handle(_ event: ControllerEvent) {
        switch event {
        case .changed:
            state = core.state()
        case .profilesChanged:
            profiles = core.profiles()
            state = core.state()
        case .captureRequested:
            // A touch screen is always "capturing" while connected (D6).
            if !someoneElseInControl { core.setCapturing(capturing: true) }
        case .releaseRequested:
            core.setCapturing(capturing: false)
        }
    }

    // MARK: - State

    var profile: HostProfile? {
        guard let id = state.profileId else { return nil }
        return profiles.first { $0.id == id }
    }

    var isConnected: Bool {
        if case .connected = state.connection { return true }
        return false
    }

    /// A capture server stream that someone else controls.
    var someoneElseInControl: Bool {
        guard let stream = state.stream else { return false }
        return !stream.haveControl && !stream.holder.isEmpty
    }

    var muted: Bool {
        get { state.muted }
        set { core.setMuted(muted: newValue) }
    }

    // MARK: - Hosts

    func save(_ profile: HostProfile) { core.saveProfile(profile: profile) }
    func delete(_ id: String) { core.deleteProfile(id: id) }

    func open(_ profile: HostProfile) {
        guard profile.id != state.profileId else { return }
        latched = []
        core.open(id: profile.id)
        state = core.state()
    }

    func close() {
        core.releaseAll()
        core.close()
        state = core.state()
    }

    /// The next (+1) or previous (-1) host, wrapping around.
    func step(_ offset: Int) {
        guard profiles.count > 1, let id = state.profileId,
              let i = profiles.firstIndex(where: { $0.id == id }) else { return }
        switchTo(profiles[(i + offset + profiles.count) % profiles.count])
    }

    /// Leave the open MiSTer (keys and buttons released) for another one.
    func switchTo(_ profile: HostProfile) {
        guard profile.id != state.profileId else { return }
        core.releaseAll()
        core.close()
        open(profile)
    }

    /// iOS suspends sockets in the background: let go of the MiSTer cleanly
    /// and reconnect when the app is back.
    func suspend() {
        guard let id = state.profileId else { return }
        suspendedId = id
        close()
    }

    func resume() {
        guard let id = suspendedId else { return }
        suspendedId = nil
        if let profile = profiles.first(where: { $0.id == id }) { open(profile) }
    }

    // MARK: - Video view

    func attach(view: UIView) {
        guard view !== videoView else { return }
        if videoView != nil { core.detachSurface() }
        videoView = view
        let size = Self.pixelSize(of: view)
        core.attachSurface(
            surface: .uiKit(uiView: UInt64(UInt(bitPattern: Unmanaged.passUnretained(view).toOpaque()))),
            widthPx: size.width, heightPx: size.height, pointsScale: size.scale)
        askForScreenSize(size)
    }

    func detach(view: UIView) {
        guard view === videoView else { return }
        core.detachSurface()
        videoView = nil
    }

    func resized(view: UIView) {
        guard view === videoView else { return }
        let size = Self.pixelSize(of: view)
        core.resizeSurface(widthPx: size.width, heightPx: size.height, pointsScale: size.scale)
        askForScreenSize(size)
    }

    /// No bigger picture than the phone shows: the server scales it down
    /// (1080p → 720p in portrait), a third of the data.
    private func askForScreenSize(_ size: (width: UInt32, height: UInt32, scale: Float)) {
        core.setStreamLimits(limits: StreamLimits(maxWidth: size.width, maxHeight: size.height, maxFps: nil, quality: nil))
    }

    private static func pixelSize(of view: UIView) -> (width: UInt32, height: UInt32, scale: Float) {
        let scale = view.window?.screen.scale ?? view.contentScaleFactor
        return (UInt32(max(1, view.bounds.width * scale)), UInt32(max(1, view.bounds.height * scale)), Float(scale))
    }

    // MARK: - Input

    /// Input from a viewer without control is dropped by the server; the
    /// first input on a free stream takes control (CAPTURE-SERVER.md §7).
    private func ready() -> Bool {
        guard isConnected, !someoneElseInControl else { return false }
        if !state.capturing { core.setCapturing(capturing: true) }
        return true
    }

    func takeControl() {
        core.takeControl()
        core.setCapturing(capturing: true)
    }

    func openMenu() {
        guard ready() else { return }
        core.openMenu()
    }

    /// Text from the system keyboard.
    func type(_ text: String) {
        guard ready() else { return }
        core.typeText(text: text, modifiers: takeModifiers())
    }

    /// A key from the function bar or the remote, with the modifiers held.
    func tap(_ usage: UInt16) {
        guard ready() else { return }
        let modifiers = takeModifiers()
        if modifiers.isEmpty {
            core.tapKey(hidUsage: usage)
        } else {
            core.pressCombo(hidUsages: modifiers + [usage])
        }
    }

    func move(dx: Double, dy: Double) {
        guard ready() else { return }
        core.mouseMove(dx: dx, dy: dy)
    }

    func button(_ button: MouseButton, down: Bool) {
        guard ready() else { return }
        core.mouseButton(button: button, down: down)
    }

    func click(_ button: MouseButton) {
        guard ready() else { return }
        core.mouseButton(button: button, down: true)
        DispatchQueue.main.asyncAfter(deadline: .now() + 0.04) { [core] in
            core.mouseButton(button: button, down: false)
        }
    }

    func zoom(by factor: Double) { core.zoomBy(factor: factor) }
    func resetZoom() { core.resetZoom() }
    /// Pan by a fraction of the picture.
    func pan(dx: Double, dy: Double) { core.panBy(dx: dx, dy: dy) }

    // MARK: - Function bar modifiers

    enum ModifierState { case off, latched, locked }

    func modifierState(_ usage: UInt16) -> ModifierState {
        locked.contains(usage) ? .locked : latched.contains(usage) ? .latched : .off
    }

    /// Tap cycles off → next key only → locked → off.
    func cycleModifier(_ usage: UInt16) {
        switch modifierState(usage) {
        case .off: latched.insert(usage)
        case .latched: latched.remove(usage); locked.insert(usage)
        case .locked: locked.remove(usage)
        }
    }

    private func takeModifiers() -> [UInt16] {
        let all = latched.union(locked).sorted()
        latched = []
        return all
    }
}

/// Bridges core events (background threads) onto the main actor.
private final class Observer: ControllerObserver, @unchecked Sendable {
    weak var model: AppModel?

    func onEvent(event: ControllerEvent) {
        DispatchQueue.main.async { [weak self] in
            MainActor.assumeIsolated { self?.model?.handle(event) }
        }
    }
}

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
        loadKeys()
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
            saveMovedKeys()
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

    func delete(_ id: String) {
        let server = core.deviceKeyServer(id: id)
        core.deleteProfile(id: id)
        profiles = core.profiles()
        if !profiles.contains(where: { $0.useInternet && core.deviceKeyServer(id: $0.id) == server }) {
            DeviceKeychain.delete(server)
        }
    }

    // MARK: - This iPhone's name

    /// Shown on the capture PC's list of paired devices and to other viewers
    /// ("Dani's iPhone has control"). iOS only tells apps "iPhone", so the
    /// person names it.
    var deviceName: String {
        get { core.viewerName() }
        set {
            let name = newValue.trimmingCharacters(in: .whitespaces)
            core.setViewerName(name: name.isEmpty ? UIDevice.current.name : name)
            objectWillChange.send()
        }
    }

    /// Still the generic name: ask for a real one before pairing.
    var deviceNameIsDefault: Bool { deviceName == UIDevice.current.name || deviceName == "iPhone" }

    // MARK: - Over the internet (docs/IPHONE.md §2–3)

    /// This phone holds a key for the MiSTer's capture PC.
    func isPaired(_ profile: HostProfile) -> Bool { core.hasDeviceKey(id: profile.id) }

    /// Use the MiSTer over the internet: reuses this phone's key for its
    /// capture PC, or pairs (on the home network) and keeps the new key in
    /// the Keychain. `again`: a new key, e.g. after the capture PC revoked
    /// the old one. Throws the server's own explanation, e.g. internet
    /// access turned off on the capture PC.
    func pairForInternet(_ profile: HostProfile, again: Bool = false) async throws {
        let core = self.core
        let id = profile.id
        let name = deviceName
        let result = try await Task.detached { try core.pairDevice(id: id, deviceName: name, again: again) }.value
        let have = DeviceKeychain.load(result.serverId)
        if !result.reused || have == nil {
            guard DeviceKeychain.save(.init(keyId: result.keyId, key: result.key), as: result.serverId) else {
                // Without the key saved, the next launch couldn't connect from outside.
                _ = core.stopInternet(id: id)
                profiles = core.profiles()
                throw ControllerError.Failed(message: "this iPhone couldn't save the key in its Keychain")
            }
        }
        profiles = core.profiles()
    }

    /// Stop connecting this MiSTer from the internet. The key goes too when
    /// no other MiSTer of the same capture PC uses it. (The capture PC's
    /// setup page can revoke it there.)
    func stopInternet(_ profile: HostProfile) {
        let server = core.deviceKeyServer(id: profile.id)
        if !core.stopInternet(id: profile.id) {
            DeviceKeychain.delete(server)
        }
        profiles = core.profiles()
    }

    /// Keys from the Keychain, one per capture PC (the core never stores them).
    private func loadKeys() {
        for profile in profiles where profile.useInternet {
            let server = core.deviceKeyServer(id: profile.id)
            // Builds before one key per capture PC filed it under the MiSTer.
            if DeviceKeychain.load(server) == nil, let old = DeviceKeychain.load(profile.id) {
                DeviceKeychain.save(old, as: server)
            }
            DeviceKeychain.delete(profile.id)
            if let entry = DeviceKeychain.load(server) {
                core.setDeviceKey(serverId: server, keyId: entry.keyId, key: entry.key)
            }
        }
    }

    /// A key kept by a capture PC's address moves to its id once the core
    /// learns the id: keep the Keychain in step.
    private func saveMovedKeys() {
        for profile in profiles where profile.useInternet {
            let server = core.deviceKeyServer(id: profile.id)
            if DeviceKeychain.load(server) == nil, let key = core.deviceKey(serverId: server) {
                DeviceKeychain.save(.init(keyId: key.keyId, key: key.key), as: server)
            }
        }
    }

    // MARK: - Predictive cursor

    /// For the route in use: the core keeps home's and the internet's apart.
    var predictiveCursor: Bool {
        get { state.display.predictiveCursor }
        set {
            var display = state.display
            display.predictiveCursor = newValue
            core.setDisplay(settings: display)
        }
    }

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
        core.click(button: button)
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

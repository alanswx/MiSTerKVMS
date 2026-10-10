import AppKit
import Carbon.HIToolbox
import ApplicationServices
import KVMSCore

/// Captures keyboard and mouse while active and forwards them to a MisterSession.
///
/// With Accessibility permission it uses a HID-level CGEventTap, so system
/// shortcuts (⌘Tab, ⌘Space, ⌘Q...) reach MiSTer instead of macOS. Without it,
/// it falls back to in-app event monitors: most keys work, but macOS keeps the
/// system shortcuts.
@MainActor
final class InputCapture {
    enum Mode {
        case eventTap
        case appOnly
    }

    var session: MisterSession?
    /// Called when capture ends for any reason (exit chord, focus loss, stop()).
    var onEnd: (() -> Void)?

    private(set) var mode: Mode?
    var isActive: Bool { mode != nil }

    /// Send the mouse's own movement (no macOS acceleration) when possible.
    var rawMouse = false
    private let raw = RawMouse()
    /// Raw mouse input was wanted and is running.
    private(set) var rawActive = false

    private var tap: CFMachPort?
    private var tapSource: CFRunLoopSource?
    private var monitors: [Any] = []
    private var resignObserver: NSObjectProtocol?

    static var hasAccessibility: Bool { AXIsProcessTrusted() }

    /// Shows the system prompt that adds the app to Accessibility settings.
    static func requestAccessibility() {
        let key = kAXTrustedCheckOptionPrompt.takeUnretainedValue() as String
        _ = AXIsProcessTrustedWithOptions([key: true] as CFDictionary)
    }

    func start(over view: NSView) {
        guard !isActive, session != nil, let window = view.window else { return }

        // Park the (hidden) cursor over the view and stop it moving, so raw
        // deltas still arrive but nothing outside the view gets clicked.
        let rectInWindow = view.convert(view.bounds, to: nil)
        let rectOnScreen = window.convertToScreen(rectInWindow)
        if let screen = window.screen {
            let mainHeight = NSScreen.screens.first?.frame.maxY ?? screen.frame.maxY
            CGWarpMouseCursorPosition(CGPoint(x: rectOnScreen.midX, y: mainHeight - rectOnScreen.midY))
        }
        CGAssociateMouseAndMouseCursorPosition(0)
        NSCursor.hide()

        if Self.hasAccessibility, installTap() {
            mode = .eventTap
        } else {
            window.acceptsMouseMovedEvents = true
            installMonitors()
            mode = .appOnly
        }

        if rawMouse {
            rawActive = raw.start { [weak self] dx, dy in self?.session?.mouseMove(dx: dx, dy: dy) }
        }

        resignObserver = NotificationCenter.default.addObserver(
            forName: NSApplication.didResignActiveNotification, object: nil, queue: .main
        ) { [weak self] _ in
            MainActor.assumeIsolated { self?.stop() }
        }
    }

    func stop() {
        guard isActive else { return }
        if let tap {
            CGEvent.tapEnable(tap: tap, enable: false)
            if let tapSource { CFRunLoopRemoveSource(CFRunLoopGetMain(), tapSource, .commonModes) }
            CFMachPortInvalidate(tap)
        }
        tap = nil
        tapSource = nil
        raw.stop()
        rawActive = false
        monitors.forEach(NSEvent.removeMonitor)
        monitors = []
        if let resignObserver { NotificationCenter.default.removeObserver(resignObserver) }
        resignObserver = nil

        CGAssociateMouseAndMouseCursorPosition(1)
        NSCursor.unhide()
        session?.releaseAll()
        mode = nil
        onEnd?()
    }

    // MARK: - Event sources

    private func installTap() -> Bool {
        let types: [CGEventType] = [
            .keyDown, .keyUp, .flagsChanged,
            .mouseMoved, .leftMouseDragged, .rightMouseDragged, .otherMouseDragged,
            .leftMouseDown, .leftMouseUp, .rightMouseDown, .rightMouseUp,
            .otherMouseDown, .otherMouseUp, .scrollWheel,
        ]
        let mask = types.reduce(CGEventMask(0)) { $0 | (1 << $1.rawValue) }
        let callback: CGEventTapCallBack = { _, type, event, refcon in
            let capture = Unmanaged<InputCapture>.fromOpaque(refcon!).takeUnretainedValue()
            return MainActor.assumeIsolated { capture.tapped(type: type, event: event) }
        }
        guard let tap = CGEvent.tapCreate(
            tap: .cghidEventTap, place: .headInsertEventTap, options: .defaultTap,
            eventsOfInterest: mask, callback: callback,
            userInfo: Unmanaged.passUnretained(self).toOpaque()
        ) else { return false }
        let source = CFMachPortCreateRunLoopSource(nil, tap, 0)
        CFRunLoopAddSource(CFRunLoopGetMain(), source, .commonModes)
        CGEvent.tapEnable(tap: tap, enable: true)
        self.tap = tap
        tapSource = source
        return true
    }

    private func tapped(type: CGEventType, event: CGEvent) -> Unmanaged<CGEvent>? {
        switch type {
        case .tapDisabledByTimeout, .tapDisabledByUserInput:
            if let tap { CGEvent.tapEnable(tap: tap, enable: true) }
            return Unmanaged.passUnretained(event)
        default:
            handle(type: type, event: event)
            return nil // swallow everything while captured
        }
    }

    private func installMonitors() {
        let mask: NSEvent.EventTypeMask = [
            .keyDown, .keyUp, .flagsChanged,
            .mouseMoved, .leftMouseDragged, .rightMouseDragged, .otherMouseDragged,
            .leftMouseDown, .leftMouseUp, .rightMouseDown, .rightMouseUp,
            .otherMouseDown, .otherMouseUp, .scrollWheel,
        ]
        let monitor = NSEvent.addLocalMonitorForEvents(matching: mask) { [weak self] event in
            guard let self, let cg = event.cgEvent else { return event }
            self.handle(type: cg.type, event: cg)
            return nil
        }
        if let monitor { monitors.append(monitor) }
    }

    // MARK: - Translation

    private func handle(type: CGEventType, event: CGEvent) {
        guard let session else { return }
        switch type {
        case .keyDown, .keyUp:
            if type == .keyDown, event.getIntegerValueField(.keyboardEventAutorepeat) != 0 {
                return // MiSTer generates its own repeat from the held key
            }
            let vk = Int(event.getIntegerValueField(.keyboardEventKeycode))
            guard let usage = MacKeymap.hidUsage(forVirtualKey: vk) else { return }
            if session.keyEvent(hidUsage: usage, down: type == .keyDown) == .exitCapture {
                // Leave the tap callback before tearing the tap down.
                DispatchQueue.main.async { [weak self] in self?.stop() }
            }

        case .flagsChanged:
            let vk = Int(event.getIntegerValueField(.keyboardEventKeycode))
            guard let usage = MacKeymap.hidUsage(forVirtualKey: vk) else { return }
            if let mask = MacKeymap.modifierMask(forVirtualKey: vk) {
                let down = event.flags.rawValue & mask != 0
                _ = session.keyEvent(hidUsage: usage, down: down)
            } else if vk == kVK_CapsLock {
                // macOS reports Caps Lock as a toggle, not press/release.
                session.tapKey(hidUsage: usage)
            }

        case .mouseMoved, .leftMouseDragged, .rightMouseDragged, .otherMouseDragged:
            // A mouse sending raw movement: this is macOS's accelerated copy
            // of it. (Trackpads have no raw movement, so they still use this.)
            if rawActive, Date().timeIntervalSince(raw.lastMovement) < 0.25 { return }
            let dx = event.getDoubleValueField(.mouseEventDeltaX)
            let dy = event.getDoubleValueField(.mouseEventDeltaY)
            if dx != 0 || dy != 0 { session.mouseMove(dx: dx, dy: dy) }

        case .leftMouseDown: session.mouseButton(button: .left, down: true)
        case .leftMouseUp: session.mouseButton(button: .left, down: false)
        case .rightMouseDown: session.mouseButton(button: .right, down: true)
        case .rightMouseUp: session.mouseButton(button: .right, down: false)
        case .otherMouseDown, .otherMouseUp:
            if event.getIntegerValueField(.mouseEventButtonNumber) == 2 {
                session.mouseButton(button: .middle, down: type == .otherMouseDown)
            }

        default:
            break // scroll wheel: mrext has no wheel command yet
        }
    }
}

import Foundation
import IOKit.hid

/// Reads mouse movement straight from HID devices, before macOS applies its
/// pointer acceleration, so the vintage OS's own acceleration is the only one.
///
/// Needs the Input Monitoring permission. Trackpads don't report raw relative
/// movement, so callers should fall back to regular events when nothing
/// arrives here.
@MainActor
final class RawMouse {
    private var manager: IOHIDManager?
    private var onMove: ((Double, Double) -> Void)?
    /// Last time a raw movement arrived (to decide whether to ignore the
    /// accelerated copy macOS also delivers).
    private(set) var lastMovement = Date.distantPast

    static var hasPermission: Bool {
        IOHIDCheckAccess(kIOHIDRequestTypeListenEvent) == kIOHIDAccessTypeGranted
    }

    /// Shows the system prompt / adds the app to Input Monitoring settings.
    static func requestPermission() {
        _ = IOHIDRequestAccess(kIOHIDRequestTypeListenEvent)
    }

    /// Returns false if raw input isn't available (usually: no permission).
    func start(onMove: @escaping (Double, Double) -> Void) -> Bool {
        stop()
        guard Self.hasPermission else { return false }
        let manager = IOHIDManagerCreate(kCFAllocatorDefault, IOOptionBits(kIOHIDOptionsTypeNone))
        let matching: [[String: Int]] = [
            [kIOHIDDeviceUsagePageKey: kHIDPage_GenericDesktop, kIOHIDDeviceUsageKey: kHIDUsage_GD_Mouse],
            [kIOHIDDeviceUsagePageKey: kHIDPage_GenericDesktop, kIOHIDDeviceUsageKey: kHIDUsage_GD_Pointer],
        ]
        IOHIDManagerSetDeviceMatchingMultiple(manager, matching as CFArray)
        let callback: IOHIDValueCallback = { context, _, _, value in
            let raw = Unmanaged<RawMouse>.fromOpaque(context!).takeUnretainedValue()
            let element = IOHIDValueGetElement(value)
            guard IOHIDElementGetUsagePage(element) == UInt32(kHIDPage_GenericDesktop) else { return }
            let delta = Double(IOHIDValueGetIntegerValue(value))
            guard delta != 0 else { return }
            MainActor.assumeIsolated {
                switch Int(IOHIDElementGetUsage(element)) {
                case kHIDUsage_GD_X: raw.moved(delta, 0)
                case kHIDUsage_GD_Y: raw.moved(0, delta)
                default: break
                }
            }
        }
        IOHIDManagerRegisterInputValueCallback(manager, callback, Unmanaged.passUnretained(self).toOpaque())
        IOHIDManagerScheduleWithRunLoop(manager, CFRunLoopGetMain(), CFRunLoopMode.commonModes.rawValue)
        guard IOHIDManagerOpen(manager, IOOptionBits(kIOHIDOptionsTypeNone)) == kIOReturnSuccess else {
            IOHIDManagerUnscheduleFromRunLoop(manager, CFRunLoopGetMain(), CFRunLoopMode.commonModes.rawValue)
            return false
        }
        self.manager = manager
        self.onMove = onMove
        return true
    }

    func stop() {
        guard let manager else { return }
        IOHIDManagerUnscheduleFromRunLoop(manager, CFRunLoopGetMain(), CFRunLoopMode.commonModes.rawValue)
        IOHIDManagerClose(manager, IOOptionBits(kIOHIDOptionsTypeNone))
        self.manager = nil
        onMove = nil
    }

    private func moved(_ dx: Double, _ dy: Double) {
        lastMovement = Date()
        onMove?(dx, dy)
    }
}

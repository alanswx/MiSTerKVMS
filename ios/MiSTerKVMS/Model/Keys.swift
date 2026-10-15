/// USB HID keyboard usages (page 7) for the on-screen keys.
enum HID {
    static let enter: UInt16 = 0x28
    static let escape: UInt16 = 0x29
    static let backspace: UInt16 = 0x2A
    static let tab: UInt16 = 0x2B
    static let f1: UInt16 = 0x3A
    static let f12: UInt16 = 0x45
    static let delete: UInt16 = 0x4C
    static let right: UInt16 = 0x4F
    static let left: UInt16 = 0x50
    static let down: UInt16 = 0x51
    static let up: UInt16 = 0x52
    static let leftCtrl: UInt16 = 0xE0
    static let leftShift: UInt16 = 0xE1
    static let leftAlt: UInt16 = 0xE2
    static let leftGUI: UInt16 = 0xE3

    /// F1…F12.
    static let functionKeys: [(String, UInt16)] = (0..<12).map { ("F\($0 + 1)", f1 + UInt16($0)) }
}

/// What's shown on the session screen (P14), remembered for the app.
enum Pref {
    static let keyboard = "show.keyboard"
    static let functionBar = "show.functionBar"
    static let dpad = "show.dpad"
    static let f12Button = "show.f12"
    static let backspaceButton = "show.backspace"
    static let banner = "show.banner"
    static let stats = "show.stats"
    /// How opaque the quick buttons are (0.25–1).
    static let buttonOpacity = "quick.opacity"
}

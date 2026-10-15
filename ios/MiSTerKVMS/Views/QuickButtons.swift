import SwiftUI

/// The quick buttons (P11): arrows around OK (Enter), F12 (the MiSTer menu)
/// and Backspace. Each piece is shown or hidden on its own from the controls
/// menu, and in Arrange mode each can be dragged anywhere on the screen; the
/// places are remembered as fractions of the space above the keyboard.
struct QuickButtons: View {
    @Binding var arranging: Bool
    @AppStorage(Pref.dpad) private var showDpad = true
    @AppStorage(Pref.f12Button) private var showF12 = true
    @AppStorage(Pref.backspaceButton) private var showBackspace = true
    @AppStorage(Pref.buttonOpacity) private var opacity = 0.6

    var body: some View {
        ZStack {
            if arranging {
                // Swallows touches, so arranging never clicks on the MiSTer.
                Color.black.opacity(0.35).contentShape(Rectangle())
            }
            if showDpad {
                Floating(id: "dpad", size: CGSize(width: DPad.size, height: DPad.size),
                         initial: CGPoint(x: 0.95, y: 0.7), arranging: arranging, opacity: opacity) { DPad() }
            }
            if showF12 {
                Floating(id: "f12", size: SmallButton.size, initial: CGPoint(x: 0.04, y: 1),
                         arranging: arranging, opacity: opacity) { SmallButton(title: "F12", usage: HID.f12) }
            }
            if showBackspace {
                Floating(id: "backspace", size: SmallButton.size, initial: CGPoint(x: 0.96, y: 1),
                         arranging: arranging, opacity: opacity) {
                    SmallButton(systemImage: "delete.left", usage: HID.backspace, label: "Backspace")
                }
            }
        }
    }

    /// Forget where the buttons were moved to.
    static func resetPositions() {
        for id in ["dpad", "f12", "backspace"] {
            UserDefaults.standard.removeObject(forKey: Floating<EmptyView>.key(id, "x"))
            UserDefaults.standard.removeObject(forKey: Floating<EmptyView>.key(id, "y"))
        }
    }
}

/// One piece placed at a remembered spot; draggable in Arrange mode only, so
/// pressing a button never moves it.
private struct Floating<Content: View>: View {
    let size: CGSize
    let arranging: Bool
    let opacity: Double
    let content: Content
    @AppStorage private var x: Double
    @AppStorage private var y: Double
    @GestureState private var drag: CGSize = .zero

    static func key(_ id: String, _ axis: String) -> String { "quick.\(id).\(axis)" }

    init(id: String, size: CGSize, initial: CGPoint, arranging: Bool, opacity: Double,
         @ViewBuilder content: () -> Content) {
        self.size = size
        self.arranging = arranging
        self.opacity = opacity
        self.content = content()
        _x = AppStorage(wrappedValue: initial.x, Self.key(id, "x"))
        _y = AppStorage(wrappedValue: initial.y, Self.key(id, "y"))
    }

    var body: some View {
        GeometryReader { geo in
            let free = CGSize(width: max(0, geo.size.width - size.width), height: max(0, geo.size.height - size.height))
            let origin = CGPoint(x: min(free.width, max(0, free.width * x + drag.width)),
                                 y: min(free.height, max(0, free.height * y + drag.height)))
            content
                .allowsHitTesting(!arranging)
                .frame(width: size.width, height: size.height)
                .opacity(arranging ? 1 : opacity)
                .overlay {
                    if arranging {
                        RoundedRectangle(cornerRadius: 14)
                            .strokeBorder(Color.accentColor, style: StrokeStyle(lineWidth: 2, dash: [6, 4]))
                            .padding(-6)
                    }
                }
                .contentShape(Rectangle())
                .position(x: origin.x + size.width / 2, y: origin.y + size.height / 2)
                .gesture(
                    DragGesture(minimumDistance: 0)
                        .updating($drag) { value, state, _ in state = value.translation }
                        .onEnded { value in
                            if free.width > 0 { x = min(1, max(0, (free.width * x + value.translation.width) / free.width)) }
                            if free.height > 0 { y = min(1, max(0, (free.height * y + value.translation.height) / free.height)) }
                        },
                    // Only while arranging; otherwise the buttons get every touch.
                    including: arranging ? .all : .subviews
                )
        }
    }
}

/// Arrows around OK (Enter).
private struct DPad: View {
    static let size: CGFloat = 168
    @EnvironmentObject private var model: AppModel

    var body: some View {
        ZStack {
            Circle().fill(.black.opacity(0.7)).overlay(Circle().stroke(.white.opacity(0.4)))
            arrow("chevron.up", HID.up, "Up").offset(y: -Self.size / 3)
            arrow("chevron.down", HID.down, "Down").offset(y: Self.size / 3)
            arrow("chevron.left", HID.left, "Left").offset(x: -Self.size / 3)
            arrow("chevron.right", HID.right, "Right").offset(x: Self.size / 3)
            Button { model.tap(HID.enter) } label: {
                Text("OK").font(.system(size: 17, weight: .semibold))
                    .frame(width: 58, height: 58)
                    .background(Circle().fill(.white.opacity(0.2)))
            }
        }
        .tint(.white)
    }

    private func arrow(_ symbol: String, _ usage: UInt16, _ label: String) -> some View {
        Button { model.tap(usage) } label: {
            Image(systemName: symbol).font(.system(size: 22, weight: .semibold))
                .frame(width: 52, height: 52).contentShape(Rectangle())
        }
        .accessibilityLabel(label)
    }
}

/// A single round-ended key: F12 or Backspace.
private struct SmallButton: View {
    static let size = CGSize(width: 56, height: 44)
    @EnvironmentObject private var model: AppModel
    var title: String? = nil
    var systemImage: String? = nil
    let usage: UInt16
    var label: String? = nil

    var body: some View {
        Button {
            usage == HID.f12 ? model.openMenu() : model.tap(usage)
        } label: {
            Group {
                if let title { Text(title).font(.system(size: 15, weight: .semibold)) }
                if let systemImage { Image(systemName: systemImage).font(.system(size: 17)) }
            }
            .frame(width: Self.size.width, height: Self.size.height)
            .background(Capsule().fill(.black.opacity(0.7)))
            .overlay(Capsule().stroke(.white.opacity(0.4)))
        }
        .tint(.white)
        .accessibilityLabel(label ?? title ?? "")
    }
}

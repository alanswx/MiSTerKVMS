import SwiftUI

/// A floating TV-style remote (P11): arrows around OK (Enter), F12 (the
/// MiSTer menu) and Backspace. Translucent, draggable, and collapses to a
/// small round button.
struct RemoteView: View {
    @EnvironmentObject private var model: AppModel
    @AppStorage(Pref.remoteCollapsed) private var collapsed = false
    @AppStorage(Pref.remoteOpacity) private var opacity = 0.6
    /// Position as a fraction of the space it can move in.
    @AppStorage(Pref.remoteX) private var x = 0.85
    @AppStorage(Pref.remoteY) private var y = 0.75
    @GestureState private var drag: CGSize = .zero

    private let pad: CGFloat = 168

    var body: some View {
        GeometryReader { geo in
            let size = collapsed ? CGSize(width: 48, height: 48) : CGSize(width: pad, height: pad + 56)
            let free = CGSize(width: max(0, geo.size.width - size.width), height: max(0, geo.size.height - size.height))
            let origin = CGPoint(x: free.width * x + drag.width, y: free.height * y + drag.height)
            content
                .frame(width: size.width, height: size.height)
                .opacity(opacity)
                .position(x: origin.x + size.width / 2, y: origin.y + size.height / 2)
                .gesture(
                    DragGesture(minimumDistance: 8)
                        .updating($drag) { value, state, _ in state = value.translation }
                        .onEnded { value in
                            if free.width > 0 { x = min(1, max(0, (free.width * x + value.translation.width) / free.width)) }
                            if free.height > 0 { y = min(1, max(0, (free.height * y + value.translation.height) / free.height)) }
                        }
                )
        }
    }

    @ViewBuilder private var content: some View {
        if collapsed {
            Button { collapsed = false } label: {
                Image(systemName: "dpad").font(.system(size: 22))
                    .frame(width: 48, height: 48)
                    .background(Circle().fill(.black.opacity(0.7)))
                    .overlay(Circle().stroke(.white.opacity(0.4)))
            }
            .tint(.white)
        } else {
            VStack(spacing: 8) {
                ZStack {
                    Circle().fill(.black.opacity(0.7)).overlay(Circle().stroke(.white.opacity(0.4)))
                    arrow("chevron.up", HID.up).offset(y: -pad / 3)
                    arrow("chevron.down", HID.down).offset(y: pad / 3)
                    arrow("chevron.left", HID.left).offset(x: -pad / 3)
                    arrow("chevron.right", HID.right).offset(x: pad / 3)
                    Button { model.tap(HID.enter) } label: {
                        Text("OK").font(.system(size: 17, weight: .semibold))
                            .frame(width: 58, height: 58)
                            .background(Circle().fill(.white.opacity(0.2)))
                    }
                }
                .frame(width: pad, height: pad)
                HStack(spacing: 8) {
                    small(systemImage: "chevron.down.circle", label: "Hide") { collapsed = true }
                    small(title: "F12") { model.openMenu() }
                    small(systemImage: "delete.left") { model.tap(HID.backspace) }
                }
            }
            .tint(.white)
        }
    }

    private func arrow(_ symbol: String, _ usage: UInt16) -> some View {
        Button { model.tap(usage) } label: {
            Image(systemName: symbol).font(.system(size: 22, weight: .semibold)).frame(width: 52, height: 52).contentShape(Rectangle())
        }
    }

    private func small(title: String? = nil, systemImage: String? = nil, label: String? = nil, action: @escaping () -> Void) -> some View {
        Button(action: action) {
            Group {
                if let title { Text(title).font(.system(size: 15, weight: .semibold)) }
                if let systemImage { Image(systemName: systemImage).font(.system(size: 17)) }
            }
            .frame(width: 48, height: 40)
            .background(Capsule().fill(.black.opacity(0.7)))
            .overlay(Capsule().stroke(.white.opacity(0.4)))
        }
        .accessibilityLabel(label ?? title ?? "")
    }
}

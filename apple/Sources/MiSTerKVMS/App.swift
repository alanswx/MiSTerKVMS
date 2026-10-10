import AppKit
import SwiftUI

@main
struct MiSTerKVMSApp: App {
    @NSApplicationDelegateAdaptor private var appDelegate: AppDelegate
    @StateObject private var profiles = ProfileStore()
    @StateObject private var controller = SessionController()

    var body: some Scene {
        WindowGroup("MiSTerKVMS") {
            ContentView()
                .environmentObject(profiles)
                .environmentObject(controller)
                .frame(minWidth: 900, minHeight: 600)
        }
        .commands {
            CommandMenu("Session") {
                Button("Capture Input") { controller.startCapture() }
                    .keyboardShortcut("k", modifiers: [.command, .shift])
                    .disabled(!controller.isConnected)
                Button("Send MiSTer Menu (F12)") { controller.openMisterMenu() }
                    .disabled(!controller.isConnected)
                Divider()
                Button("Zoom In") { controller.zoom(by: 1.25) }
                    .keyboardShortcut("+", modifiers: .command)
                Button("Zoom Out") { controller.zoom(by: 1 / 1.25) }
                    .keyboardShortcut("-", modifiers: .command)
                Button("Actual Size") { controller.resetZoom() }
                    .keyboardShortcut("0", modifiers: .command)
                Toggle("Mute", isOn: $controller.muted)
                    .keyboardShortcut("m", modifiers: [.command, .shift])
                Divider()
                Button("Show Stats") { controller.flashStats() }
                    .keyboardShortcut("i", modifiers: .command)
                Toggle("Pin Stats Overlay", isOn: $controller.statsPinned)
                    .keyboardShortcut("i", modifiers: [.command, .shift])
            }
        }
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationDidFinishLaunching(_ notification: Notification) {
        // Built as a plain SwiftPM executable: make sure it behaves like a
        // regular foreground app with a Dock icon and menu bar.
        NSApp.setActivationPolicy(.regular)
        NSApp.activate(ignoringOtherApps: true)
    }

    func applicationShouldTerminateAfterLastWindowClosed(_ sender: NSApplication) -> Bool {
        true
    }
}

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
            CommandGroup(replacing: .appInfo) { AboutCommand() }
            CommandMenu("Session") {
                Button("Capture Input") { controller.startCapture() }
                    .keyboardShortcut("k", modifiers: [.command, .shift])
                    .disabled(!controller.isConnected)
                Button("Send MiSTer Menu (F12)") { controller.openMisterMenu() }
                    .disabled(!controller.isConnected)
                Button(controller.isRecording ? "Stop Recording" : "Start Recording") { controller.toggleRecording() }
                    .keyboardShortcut("r", modifiers: [.command, .shift])
                    .disabled(!controller.isRecording && controller.videoStats?.state != "connected")
                Button("Show Last Recording in Finder") { controller.showLastRecording() }
                    .disabled(controller.lastRecording == nil)
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

        Window("About MiSTerKVMS", id: AboutView.windowID) {
            AboutView()
        }
        .windowResizability(.contentSize)
        .windowStyle(.hiddenTitleBar)
    }
}

final class AppDelegate: NSObject, NSApplicationDelegate {
    func applicationWillFinishLaunching(_ notification: Notification) {
        Self.redirectLogsToFile()
    }

    /// When launched from Finder, stderr goes nowhere. Send it (the core's
    /// warnings and errors) to ~/Library/Logs/MiSTerKVMS.log so problems can
    /// be diagnosed. Left alone when run from a terminal.
    static func redirectLogsToFile() {
        guard isatty(STDERR_FILENO) == 0 else { return }
        let logs = FileManager.default.urls(for: .libraryDirectory, in: .userDomainMask)[0]
            .appendingPathComponent("Logs", isDirectory: true)
        try? FileManager.default.createDirectory(at: logs, withIntermediateDirectories: true)
        let url = logs.appendingPathComponent("MiSTerKVMS.log")
        // Keep the file from growing forever: start fresh past 5 MB.
        if let size = try? FileManager.default.attributesOfItem(atPath: url.path)[.size] as? Int, size > 5_000_000 {
            try? FileManager.default.removeItem(at: url)
        }
        freopen(url.path, "a", stderr)
        setvbuf(stderr, nil, _IOLBF, 0)
        let version = Bundle.main.infoDictionary?["CFBundleShortVersionString"] as? String ?? "dev"
        FileHandle.standardError.write("--- MiSTerKVMS \(version) started \(Date())\n".data(using: .utf8)!)
    }

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

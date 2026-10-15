import AppKit
import SwiftUI

struct AboutView: View {
    private var version: String {
        let info = Bundle.main.infoDictionary
        let short = info?["CFBundleShortVersionString"] as? String ?? "dev"
        let build = info?["CFBundleVersion"] as? String ?? "0"
        return "Version \(short) (\(build))"
    }

    var body: some View {
        VStack(spacing: 14) {
            Image(nsImage: NSApp.applicationIconImage)
                .resizable()
                .frame(width: 128, height: 128)
            VStack(spacing: 4) {
                Text("MiSTerKVMS").font(.title).fontWeight(.semibold)
                Text(version).font(.callout).foregroundStyle(.secondary)
                Text("Keyboard, mouse, video and sound for your MiSTer FPGAs, over the network.")
                    .font(.callout)
                    .multilineTextAlignment(.center)
                    .padding(.top, 4)
            }

            Divider().padding(.vertical, 4)

            VStack(alignment: .leading, spacing: 10) {
                Credit(title: "MiSTer-Kun mascot") {
                    Text("Original design by HeWhoisRed, a gift to the MiSTer community. Remastered artwork by ")
                        + Text("[baxysquare/mister_kun](https://github.com/baxysquare/mister_kun)") + Text(".")
                }
                Credit(title: "MiSTer FPGA") {
                    Text("[MiSTer FPGA project](https://github.com/MiSTer-devel/Main_MiSTer/wiki) by Sorgelig and contributors.")
                }
                Credit(title: "Keyboard & mouse") {
                    Text("[mrext Remote](https://github.com/wizzomafizzo/mrext) by wizzo and contributors.")
                }
                Credit(title: "Built with") {
                    Text("Rust: wgpu, cpal, tungstenite, UniFFI. Swift: SwiftUI and AppKit.")
                }
            }
            .font(.callout)
            .frame(maxWidth: .infinity, alignment: .leading)

            Divider().padding(.vertical, 4)

            HStack(spacing: 4) {
                Text("MIT License ·")
                Link("Source on GitHub", destination: URL(string: "https://github.com/danifunker/MiSTerKVMS")!)
            }
            .font(.caption)
            .foregroundStyle(.secondary)
        }
        .padding(24)
        .frame(width: 420)
    }
}

private struct Credit<Content: View>: View {
    let title: String
    @ViewBuilder let content: Content

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title).fontWeight(.semibold)
            content.foregroundStyle(.secondary).fixedSize(horizontal: false, vertical: true)
        }
    }
}

/// "About MiSTerKVMS" in the app menu, opening our window instead of the
/// standard panel so the credits fit.
struct AboutCommand: View {
    @Environment(\.openWindow) private var openWindow

    var body: some View {
        Button("About MiSTerKVMS") { openWindow(id: AboutView.windowID) }
    }
}

extension AboutView {
    static let windowID = "about"
}

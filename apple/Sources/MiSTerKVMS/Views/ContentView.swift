import SwiftUI

struct ContentView: View {
    @EnvironmentObject private var profiles: ProfileStore
    @EnvironmentObject private var controller: SessionController
    @State private var selection: HostProfile.ID?
    @State private var editing: HostProfile?

    var body: some View {
        NavigationSplitView {
            List(selection: $selection) {
                ForEach(profiles.profiles) { profile in
                    HostRow(profile: profile, isLive: controller.profile?.id == profile.id)
                        .tag(profile.id)
                        .contextMenu {
                            Button("Edit…") { editing = profile }
                            Button("Delete", role: .destructive) { delete(profile.id) }
                        }
                }
            }
            .navigationSplitViewColumnWidth(min: 200, ideal: 240)
            .toolbar {
                ToolbarItem {
                    Button { editing = HostProfile() } label: { Label("Add Host", systemImage: "plus") }
                }
            }
            .overlay {
                if profiles.profiles.isEmpty {
                    ContentUnavailableView {
                        Label("No Hosts", systemImage: "display.2")
                    } description: {
                        Text("Add a MiSTer and the NDI source that carries its video.")
                    } actions: {
                        Button("Add Host…") { editing = HostProfile() }
                    }
                }
            }
        } detail: {
            if let profile = controller.profile {
                SessionView(profile: profile, onEdit: { editing = profile })
            } else {
                ContentUnavailableView("Select a Host", systemImage: "sidebar.left",
                                       description: Text("Choose a MiSTer in the sidebar to connect."))
            }
        }
        .sheet(item: $editing) { profile in
            ProfileEditor(profile: profile) { saved in
                profiles.upsert(saved)
                if controller.profile?.id == saved.id {
                    controller.close()
                    controller.open(saved)
                }
            }
        }
        .onChange(of: selection) { _, id in
            if let id, let profile = profiles.profiles.first(where: { $0.id == id }) {
                controller.open(profile)
            } else {
                controller.close()
            }
        }
        .safeAreaInset(edge: .top) { AccessibilityBanner() }
        .onAppear { controller.profiles = profiles }
    }

    private func delete(_ id: HostProfile.ID) {
        if controller.profile?.id == id { controller.close() }
        if selection == id { selection = nil }
        profiles.delete(id)
    }
}

private struct HostRow: View {
    let profile: HostProfile
    let isLive: Bool

    var body: some View {
        VStack(alignment: .leading, spacing: 2) {
            HStack {
                Text(profile.name).fontWeight(isLive ? .semibold : .regular)
                if isLive {
                    Image(systemName: "dot.radiowaves.left.and.right").foregroundStyle(.green)
                }
            }
            Text(profile.misterHost.isEmpty ? "No address" : profile.misterHost)
                .font(.caption).foregroundStyle(.secondary)
        }
        .padding(.vertical, 2)
    }
}

/// Explains why Accessibility is needed (D5) until it's granted.
private struct AccessibilityBanner: View {
    @State private var trusted = InputCapture.hasAccessibility
    private let timer = Timer.publish(every: 2, on: .main, in: .common).autoconnect()

    var body: some View {
        Group {
            if !trusted {
                HStack(spacing: 12) {
                    Image(systemName: "keyboard.badge.exclamationmark").font(.title2)
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Allow Accessibility to send every key to MiSTer").fontWeight(.semibold)
                        Text("Without it, macOS keeps shortcuts like ⌘Tab, ⌘Space and ⌘Q for itself. MiSTerKVMS only intercepts input while you're in capture mode.")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Open Settings…") { InputCapture.requestAccessibility() }
                }
                .padding(10)
                .background(.yellow.opacity(0.15))
            }
        }
        .onReceive(timer) { _ in trusted = InputCapture.hasAccessibility }
    }
}

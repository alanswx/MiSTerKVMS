import SwiftUI

/// The MiSTers (one per capture server stream). Tapping one opens it full
/// screen; one at a time (P13).
struct HostListView: View {
    @EnvironmentObject private var model: AppModel
    @State private var adding = false
    @State private var settingsFor: HostProfile?

    var body: some View {
        List {
            ForEach(model.profiles) { profile in
                Button {
                    model.open(profile)
                } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text(profile.name).font(.headline)
                            Text(profile.subtitle).font(.subheadline).foregroundStyle(.secondary)
                        }
                        Spacer()
                        if profile.useInternet {
                            Image(systemName: "globe").foregroundStyle(.secondary).accessibilityLabel("Also over the internet")
                        }
                    }
                }
                .tint(.primary)
                .swipeActions {
                    Button(role: .destructive) { model.delete(profile.id) } label: { Label("Delete", systemImage: "trash") }
                    Button { settingsFor = profile } label: { Label("Settings", systemImage: "gearshape") }
                }
                .contextMenu {
                    Button { settingsFor = profile } label: { Label("Settings", systemImage: "gearshape") }
                    Button(role: .destructive) { model.delete(profile.id) } label: { Label("Delete", systemImage: "trash") }
                }
            }
        }
        .overlay {
            if model.profiles.isEmpty {
                ContentUnavailableView {
                    Label("No MiSTers yet", systemImage: "display")
                } description: {
                    Text("Run MiSTerKVMS Capture on the PC with the capture card, then add its stream here while you're on the same Wi-Fi.")
                } actions: {
                    Button("Add a MiSTer") { adding = true }.buttonStyle(.borderedProminent)
                }
            }
        }
        .navigationTitle("MiSTerKVMS")
        .toolbar {
            Button { adding = true } label: { Image(systemName: "plus") }
        }
        .sheet(isPresented: $adding) { AddHostView() }
        .sheet(item: $settingsFor) { profile in HostSettingsView(hostId: profile.id) }
    }
}

extension HostProfile: Identifiable {}

extension HostProfile {
    /// The stream's name, from its address (`mkvm://pc/name`).
    var streamName: String {
        var path = Substring(stream)
        if let scheme = path.range(of: "://") { path = path[scheme.upperBound...] }
        guard let slash = path.firstIndex(of: "/") else { return "" }
        let name = path[path.index(after: slash)...].trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        return name.removingPercentEncoding ?? name
    }

    var subtitle: String {
        if !streamName.isEmpty { return "Stream \(streamName)" }
        if !misterHost.isEmpty { return misterHost }
        return "No stream chosen"
    }
}

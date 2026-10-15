import SwiftUI

/// The MiSTers (one per capture server stream). Tapping one opens it full
/// screen; one at a time (P13).
struct HostListView: View {
    @EnvironmentObject private var model: AppModel
    @State private var adding = false
    @State private var renaming: HostProfile?

    var body: some View {
        List {
            ForEach(model.profiles) { profile in
                Button {
                    model.open(profile)
                } label: {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(profile.name).font(.headline)
                        Text(profile.subtitle).font(.subheadline).foregroundStyle(.secondary)
                    }
                }
                .tint(.primary)
                .swipeActions {
                    Button(role: .destructive) { model.delete(profile.id) } label: { Label("Delete", systemImage: "trash") }
                    Button { renaming = profile } label: { Label("Rename", systemImage: "pencil") }
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
        .alert("Rename", isPresented: Binding(get: { renaming != nil }, set: { if !$0 { renaming = nil } })) {
            RenameField(profile: renaming) { name in
                if var p = renaming, !name.isEmpty { p.name = name; model.save(p) }
                renaming = nil
            }
        }
    }
}

private struct RenameField: View {
    let profile: HostProfile?
    let done: (String) -> Void
    @State private var name = ""

    var body: some View {
        TextField("Name", text: $name).onAppear { name = profile?.name ?? "" }
        Button("Save") { done(name.trimmingCharacters(in: .whitespaces)) }
        Button("Cancel", role: .cancel) { done("") }
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

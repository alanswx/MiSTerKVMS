import SwiftUI

/// Add a MiSTer: a stream found on the network, or one typed by address
/// (`host[:port]/stream`) where discovery doesn't reach.
struct AddHostView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @StateObject private var browser = StreamBrowser()
    @State private var address = ""
    @State private var name = ""
    @State private var resolving: String?
    @State private var error: String?

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    if browser.found.isEmpty {
                        HStack {
                            ProgressView()
                            Text("Looking for capture servers…").foregroundStyle(.secondary)
                        }
                    }
                    ForEach(browser.found) { stream in
                        Button {
                            // Set before the task starts, so a second tap can't add it twice.
                            resolving = stream.id
                            Task { await add(stream) }
                        } label: {
                            HStack {
                                VStack(alignment: .leading, spacing: 2) {
                                    Text(stream.name)
                                    Text(stream.mister.isEmpty ? stream.server : "\(stream.mister) on \(stream.server)")
                                        .font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                if resolving == stream.id {
                                    ProgressView()
                                } else if isSaved(stream) {
                                    Label("Added", systemImage: "checkmark").labelStyle(.iconOnly)
                                        .foregroundStyle(.secondary)
                                }
                            }
                        }
                        .tint(.primary)
                        .disabled(resolving != nil || isSaved(stream))
                    }
                } header: {
                    Text("On this network")
                }

                Section {
                    TextField("capture-pc/MiSTer-SS1", text: $address)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .keyboardType(.URL)
                    TextField("Name (optional)", text: $name)
                    Button("Add") { addTyped() }
                        .disabled(address.trimmingCharacters(in: .whitespaces).isEmpty)
                } header: {
                    Text("By address")
                } footer: {
                    Text("The capture server's address and the stream name, as on its setup page. Add the port if it isn't 5970: pc:6000/MiSTer-SS1.")
                }
            }
            .navigationTitle("Add a MiSTer")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            }
            .alert("Can't add it", isPresented: Binding(get: { error != nil }, set: { if !$0 { error = nil } })) {
                Button("OK", role: .cancel) {}
            } message: {
                Text(error ?? "")
            }
        }
        .onAppear { browser.start() }
        .onDisappear { browser.stop() }
    }

    /// Already in the list (same stream name on the same capture server).
    private func isSaved(_ stream: StreamBrowser.Found) -> Bool {
        model.profiles.contains { $0.streamName == stream.name && (stream.mister.isEmpty || $0.misterHost == stream.mister) }
    }

    private func add(_ stream: StreamBrowser.Found) async {
        defer { resolving = nil }
        guard let address = await StreamBrowser.address(of: stream) else {
            error = "\(stream.name) didn't answer. Is the phone on the same network as \(stream.server)?"
            return
        }
        save(name: stream.name, address: address, mister: stream.mister)
    }

    private func addTyped() {
        var text = address.trimmingCharacters(in: .whitespaces)
        if !text.contains("://") { text = "mkvm://" + text }
        let profile = makeProfile(address: text)
        guard !profile.streamName.isEmpty else {
            error = "Add the stream's name after the address: pc/MiSTer-SS1."
            return
        }
        let given = name.trimmingCharacters(in: .whitespaces)
        save(name: given.isEmpty ? profile.streamName : given, address: text, mister: "")
    }

    private func makeProfile(address: String) -> HostProfile {
        var profile = newHostProfile()
        profile.source = "capture"
        profile.stream = address
        return profile
    }

    private func save(name: String, address: String, mister: String) {
        // The same address twice is one MiSTer: keep the saved one.
        guard !model.profiles.contains(where: { $0.stream.caseInsensitiveCompare(address) == .orderedSame }) else {
            dismiss()
            return
        }
        var profile = makeProfile(address: address)
        profile.name = name
        profile.misterHost = mister
        model.save(profile)
        dismiss()
    }
}

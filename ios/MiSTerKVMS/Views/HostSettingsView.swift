import SwiftUI

/// One MiSTer's settings: its name, and using it over the internet
/// (pairing with its capture server, the address from outside).
struct HostSettingsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let hostId: String
    @State private var name = ""
    @State private var internetAddress = ""
    @State private var pairing = false
    @State private var error: String?

    private var profile: HostProfile? { model.profiles.first { $0.id == hostId } }

    var body: some View {
        NavigationStack {
            Form {
                Section("Name") {
                    TextField("Name", text: $name)
                }
                if let profile {
                    Section {
                        Toggle(isOn: Binding(get: { profile.useInternet || pairing }, set: { on in toggleInternet(profile, on) })) {
                            Text("Use over the internet")
                        }
                        .disabled(pairing)
                        if pairing {
                            HStack {
                                ProgressView()
                                Text("Pairing with the capture PC…").foregroundStyle(.secondary)
                            }
                        } else if profile.useInternet {
                            Label(model.isPaired(profile) ? "Paired: encrypted with this iPhone's own key" : "Not paired on this iPhone",
                                  systemImage: model.isPaired(profile) ? "lock.fill" : "exclamationmark.triangle")
                                .font(.footnote)
                                .foregroundStyle(model.isPaired(profile) ? Color.secondary : Color.orange)
                            TextField("myhome.duckdns.org:5970", text: $internetAddress)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                                .keyboardType(.URL)
                        }
                        if let error {
                            Text(error).font(.footnote).foregroundStyle(.red)
                        }
                    } header: {
                        Text("Over the internet")
                    } footer: {
                        Text(profile.useInternet
                             ? "Away from home the app connects to this address (your home's public address or dynamic-DNS name, and the port forwarded to the capture PC). At home it connects directly."
                             : "Pairs this iPhone with the capture PC while you're on the same Wi-Fi, so it can connect from anywhere, encrypted. First turn on “Allow connections from the internet” on the capture PC's setup page.")
                    }
                }
            }
            .navigationTitle(profile?.name ?? "MiSTer")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) { Button("Done") { saveAndClose() }.disabled(pairing) }
            }
            .onAppear {
                name = profile?.name ?? ""
                internetAddress = profile?.internetAddress ?? ""
            }
            .onChange(of: profile?.internetAddress) { _, address in
                // Pairing brings the address the capture PC knows.
                if let address, internetAddress.isEmpty { internetAddress = address }
            }
        }
    }

    private func toggleInternet(_ profile: HostProfile, _ on: Bool) {
        error = nil
        if on {
            pairing = true
            Task {
                do {
                    try await model.pairForInternet(profile)
                } catch {
                    self.error = "Couldn't pair: \(describe(error))"
                }
                pairing = false
            }
        } else {
            model.stopInternet(profile)
        }
    }

    private func saveAndClose() {
        if var p = profile {
            let trimmedName = name.trimmingCharacters(in: .whitespaces)
            if !trimmedName.isEmpty { p.name = trimmedName }
            p.internetAddress = internetAddress.trimmingCharacters(in: .whitespaces)
            model.save(p)
        }
        dismiss()
    }

    private func describe(_ error: Error) -> String {
        if case let ControllerError.Failed(message) = error { return message }
        return error.localizedDescription
    }
}

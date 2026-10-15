import SwiftUI

/// One MiSTer's settings: its name, and using it over the internet (this
/// iPhone's key for its capture PC, the address from outside).
struct HostSettingsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    let hostId: String
    @State private var name = ""
    @State private var internetAddress = ""
    @State private var pairing = false
    @State private var error: String?
    @State private var askingName = false
    @State private var pendingAgain = false

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
                                Text("Setting up with the capture PC…").foregroundStyle(.secondary)
                            }
                        } else if profile.useInternet {
                            Label(model.isPaired(profile) ? "Paired: encrypted with this iPhone's key for this capture PC" : "Not paired on this iPhone",
                                  systemImage: model.isPaired(profile) ? "lock.fill" : "exclamationmark.triangle")
                                .font(.footnote)
                                .foregroundStyle(model.isPaired(profile) ? Color.secondary : Color.orange)
                            TextField("myhome.duckdns.org:5970", text: $internetAddress)
                                .textInputAutocapitalization(.never)
                                .autocorrectionDisabled()
                                .keyboardType(.URL)
                            Button("Pair again") { startPairing(profile, again: true) }
                        }
                        if let error {
                            Text(error).font(.footnote).foregroundStyle(.red)
                        }
                    } header: {
                        Text("Over the internet")
                    } footer: {
                        Text(profile.useInternet
                             ? "Away from home the app connects to this address (your home's public address or dynamic-DNS name, and the port forwarded to the capture PC). At home it connects directly. Pair again if the capture PC revoked this iPhone."
                             : "Lets this iPhone connect from anywhere, encrypted. It pairs with the capture PC while you're on the same Wi-Fi (once per capture PC: its other MiSTers use the same key). First turn on “Allow connections from the internet” on the capture PC's setup page.")
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
            .nameThisIPhone(isPresented: $askingName) {
                if let profile { startPairing(profile, again: pendingAgain) }
            }
        }
    }

    private func toggleInternet(_ profile: HostProfile, _ on: Bool) {
        error = nil
        if on {
            startPairing(profile, again: false)
        } else {
            model.stopInternet(profile)
        }
    }

    private func startPairing(_ profile: HostProfile, again: Bool) {
        error = nil
        // The capture PC lists this iPhone by its name: ask for a real one first.
        if model.deviceNameIsDefault && !askingName {
            pendingAgain = again
            askingName = true
            return
        }
        pairing = true
        Task {
            do {
                try await model.pairForInternet(profile, again: again)
            } catch {
                self.error = "Couldn't set it up: \(describe(error))"
            }
            pairing = false
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
}

func describe(_ error: Error) -> String {
    if case let ControllerError.Failed(message) = error { return message }
    return error.localizedDescription
}

extension View {
    /// Asks for this iPhone's name (shown on the capture PC and to other
    /// viewers), then runs `then`.
    func nameThisIPhone(isPresented: Binding<Bool>, then: @escaping () -> Void) -> some View {
        modifier(NameThisIPhone(isPresented: isPresented, then: then))
    }
}

private struct NameThisIPhone: ViewModifier {
    @EnvironmentObject private var model: AppModel
    @Binding var isPresented: Bool
    let then: () -> Void
    @State private var name = ""

    func body(content: Content) -> some View {
        content.alert("Name this iPhone", isPresented: $isPresented) {
            TextField("Dani's iPhone", text: $name)
            Button("Continue") {
                model.deviceName = name
                // Leave the alert before carrying on (it may present again).
                DispatchQueue.main.async { then() }
            }
            .disabled(name.trimmingCharacters(in: .whitespaces).isEmpty)
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("The capture PC lists paired phones by name, and other viewers see it when you have control.")
        }
    }
}

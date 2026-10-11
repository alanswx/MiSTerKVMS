import KVMSCore
import SwiftUI

struct ProfileEditor: View {
    @Environment(\.dismiss) private var dismiss
    @State var profile: HostProfile
    let onSave: (HostProfile) -> Void
    private let original: HostProfile
    @State private var found: [String] = []
    @State private var searching = false
    @State private var findError: String?
    @State private var debounce: Task<Void, Never>?
    /// Ignores results from an older search that finishes after a newer one.
    @State private var searchID = 0

    init(profile: HostProfile, onSave: @escaping (HostProfile) -> Void) {
        _profile = State(initialValue: profile)
        self.onSave = onSave
        original = profile
    }

    /// Look for NDI sources (on the network and on the OBS PC, if given).
    /// NDI discovery blocks for a couple of seconds, so it runs off the main thread.
    private func findSources() {
        searchID += 1
        let id = searchID
        let hosts = profile.obsHost
        searching = true
        findError = nil
        Task.detached {
            let result = Result { try ndiFindSources(obsHosts: hosts, waitMs: 2000) }
            await MainActor.run {
                guard id == searchID else { return }
                searching = false
                switch result {
                case .success(let names):
                    found = names
                    if profile.ndiSource.isEmpty, names.count == 1 {
                        profile.ndiSource = names[0]
                    }
                case .failure(let error):
                    found = []
                    findError = "\(error)"
                }
            }
        }
    }

    var body: some View {
        Form {
            Section("Host") {
                TextField("Name", text: $profile.name)
            }
            Section("MiSTer (keyboard & mouse via mrext Remote)") {
                TextField("MiSTer IP or hostname", text: $profile.misterHost, prompt: Text("192.168.1.50"))
                TextField("mrext port", value: $profile.misterPort, format: .number.grouping(.never))
            }
            Section {
                TextField("OBS PC", text: $profile.obsHost, prompt: Text("IP or name, e.g. obs-pc.local (optional)"))
                    .onSubmit(findSources)
                LabeledContent("NDI source") {
                    HStack(spacing: 6) {
                        TextField("NDI source", text: $profile.ndiSource, prompt: Text("Choose or type"))
                            .labelsHidden()
                        Menu {
                            if found.isEmpty {
                                Text(searching ? "Searching…" : "No sources found")
                            }
                            ForEach(found, id: \.self) { name in
                                Button(name) { profile.ndiSource = name }
                            }
                        } label: {
                            Image(systemName: "chevron.up.chevron.down")
                        }
                        .menuStyle(.borderlessButton)
                        .menuIndicator(.hidden)
                        .fixedSize()
                        .help("Sources found on the network and on the OBS PC above")
                        Button(action: findSources) {
                            if searching {
                                ProgressView().controlSize(.small)
                            } else {
                                Text("Find")
                            }
                        }
                        .disabled(searching)
                    }
                }
                if let findError {
                    Text(findError).font(.caption).foregroundStyle(.red)
                } else if !searching, !found.isEmpty {
                    Text("Found: \(found.joined(separator: ", "))")
                        .font(.caption).foregroundStyle(.secondary)
                }
            } header: {
                Text("Video & audio (OBS via NDI)")
            } footer: {
                HStack(spacing: 4) {
                    Text("Enter the OBS PC to list its sources directly; otherwise sources found on the local network are listed. Video uses NDI®.")
                    Link("ndi.video", destination: URL(string: "https://ndi.video")!)
                }
                .font(.caption)
            }
            Section("Mouse") {
                LabeledContent("Sensitivity") {
                    HStack {
                        Slider(value: $profile.mouseSensitivity, in: 0.1...4, step: 0.05)
                        Text(profile.mouseSensitivity, format: .number.precision(.fractionLength(2)))
                            .monospacedDigit().frame(width: 40)
                    }
                }
                Toggle("⌥-click sends middle click", isOn: $profile.altClickIsMiddle)
                Toggle(isOn: $profile.rawMouse) {
                    Text("Raw mouse input")
                    Text("Sends your mouse's own movement without macOS acceleration, so only the vintage OS accelerates it, like a mouse plugged straight in. Trackpads keep using macOS movement.")
                }
                if profile.rawMouse && !RawMouse.hasPermission {
                    LabeledContent {
                        Button("Allow…") { RawMouse.requestPermission() }
                    } label: {
                        Text("Needs Input Monitoring permission")
                        Text("macOS asks once; the setting is in Privacy & Security → Input Monitoring.")
                    }
                }
            }
        }
        .formStyle(.grouped)
        .frame(width: 500)
        .onAppear(perform: findSources)
        .onChange(of: profile.obsHost) { _, _ in
            // Search again once typing pauses.
            debounce?.cancel()
            debounce = Task {
                try? await Task.sleep(for: .milliseconds(800))
                if !Task.isCancelled { findSources() }
            }
        }
        .toolbar {
            ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
            ToolbarItem(placement: .confirmationAction) {
                Button("Save") {
                    profile.misterHost = profile.misterHost.trimmingCharacters(in: .whitespaces)
                    // Mouse scaling changed: the predictive cursor must relearn it.
                    if profile.rawMouse != original.rawMouse || profile.mouseSensitivity != original.mouseSensitivity {
                        profile.cursorGain = 0
                    }
                    onSave(profile)
                    dismiss()
                }
                .disabled(profile.name.isEmpty)
            }
        }
    }
}

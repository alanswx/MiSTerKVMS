import SwiftUI

/// Settings for this iPhone (not one MiSTer): its name.
struct AppSettingsView: View {
    @EnvironmentObject private var model: AppModel
    @Environment(\.dismiss) private var dismiss
    @State private var name = ""

    var body: some View {
        NavigationStack {
            Form {
                Section {
                    TextField("Dani's iPhone", text: $name)
                        .submitLabel(.done)
                } header: {
                    Text("This iPhone's name")
                } footer: {
                    Text("Capture PCs list this iPhone by this name under their paired devices, and other viewers see it when you have control (“\(name.isEmpty ? "iPhone" : name) has control”). A change shows on the capture PC the next time this iPhone connects.")
                }
            }
            .navigationTitle("This iPhone")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) { Button("Cancel") { dismiss() } }
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") {
                        model.deviceName = name
                        dismiss()
                    }
                }
            }
            .onAppear { name = model.deviceNameIsDefault ? "" : model.deviceName }
        }
    }
}

import SwiftUI

/// MiSTerKVMS ▸ Settings: what's not per host.
struct SettingsView: View {
    let controller: SessionController
    @State private var viewerName = ""

    var body: some View {
        Form {
            TextField("Viewer name", text: $viewerName)
                .onSubmit { controller.viewerName = viewerName }
            Text("Other people watching a MiSTerKVMS Capture stream see this name, for example when you have control.")
                .font(.caption).foregroundStyle(.secondary)
        }
        .formStyle(.grouped)
        .frame(width: 420)
        .onAppear { viewerName = controller.viewerName }
        .onDisappear { controller.viewerName = viewerName }
    }
}

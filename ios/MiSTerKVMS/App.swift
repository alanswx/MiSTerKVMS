import SwiftUI

@main
struct MiSTerKVMSApp: App {
    @StateObject private var model = AppModel()
    @Environment(\.scenePhase) private var scenePhase

    var body: some Scene {
        WindowGroup {
            RootView()
                .environmentObject(model)
                .preferredColorScheme(.dark)
        }
        .onChange(of: scenePhase) { _, phase in
            switch phase {
            case .background: model.suspend()
            case .active: model.resume()
            default: break
            }
        }
    }
}

struct RootView: View {
    @EnvironmentObject private var model: AppModel

    var body: some View {
        NavigationStack {
            HostListView()
        }
        .fullScreenCover(isPresented: Binding(
            get: { model.state.profileId != nil },
            set: { if !$0 { model.close() } }
        )) {
            SessionScreen()
        }
    }
}

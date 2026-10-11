import Foundation
import KVMSCore

// HostProfile is the core's record (core/src/profiles.rs); hosts are saved by
// the core in ~/Library/Application Support/MiSTerKVMS/profiles.json.
extension HostProfile: Identifiable {}

extension HostProfile {
    /// A new host with default settings and a fresh id.
    static func make() -> HostProfile { newHostProfile() }
}

/// The saved hosts, as the core has them. Changes go through the core, which
/// reports back with a `profilesChanged` event (see SessionController).
@MainActor
final class ProfileStore: ObservableObject {
    @Published private(set) var profiles: [HostProfile] = []
    private let core: Controller

    init(core: Controller) {
        self.core = core
        importLegacyDefaults()
        reload()
    }

    func reload() {
        profiles = core.profiles()
    }

    /// Add or replace (by id). If it's the open host, the core reconnects.
    func upsert(_ profile: HostProfile) {
        core.saveProfile(profile: profile)
    }

    func delete(_ id: HostProfile.ID) {
        core.deleteProfile(id: id)
    }

    /// Versions before the shared settings file kept hosts in UserDefaults.
    /// Move them over once; the old data is left in place as a backup.
    private func importLegacyDefaults() {
        guard !core.legacyImported() else { return }
        let defaults = UserDefaults.standard
        if let data = defaults.data(forKey: "hostProfiles"), let json = String(data: data, encoding: .utf8) {
            do {
                let count = try core.importProfiles(json: json)
                FileHandle.standardError.write("profiles: imported \(count) from UserDefaults\n".data(using: .utf8)!)
            } catch {
                FileHandle.standardError.write("profiles: UserDefaults import failed: \(error)\n".data(using: .utf8)!)
            }
        }
        if defaults.bool(forKey: "statsPinned") {
            core.setStatsPinned(pinned: true)
        }
        core.markLegacyImported()
    }
}

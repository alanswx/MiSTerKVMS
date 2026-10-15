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

extension HostProfile {
    /// Where video comes from: "capture" (a capture server stream), "local"
    /// (a card on this Mac). Any other `source` (empty, or the removed "ndi")
    /// is inferred from which field is filled, as the core does.
    var sourceKind: String {
        switch source {
        case "capture", "local": source
        default:
            if !stream.trimmingCharacters(in: .whitespaces).isEmpty { "capture" }
            else if !localVideo.isEmpty { "local" }
            else { "capture" }
        }
    }

    /// The stream's name, from its address (`mkvm://pc/name`).
    var streamName: String {
        var path = Substring(stream)
        if let scheme = path.range(of: "://") { path = path[scheme.upperBound...] }
        guard let slash = path.firstIndex(of: "/") else { return "" }
        let name = path[path.index(after: slash)...].trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        return name.removingPercentEncoding ?? name
    }

    /// The host list's second line.
    var subtitle: String {
        switch sourceKind {
        case "local": "Capture card on this Mac"
        default:
            if !streamName.isEmpty { "Stream \(streamName)" }
            else if !misterHost.isEmpty { misterHost }
            else { "No stream chosen" }
        }
    }
}

extension ControllerState {
    /// A capture server stream that someone else controls.
    var someoneElseInControl: Bool {
        guard let stream else { return false }
        return !stream.haveControl && !stream.holder.isEmpty
    }
}

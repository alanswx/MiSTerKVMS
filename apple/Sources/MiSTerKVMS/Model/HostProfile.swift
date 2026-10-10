import Foundation

struct HostProfile: Identifiable, Codable, Hashable {
    var id = UUID()
    var name = "New MiSTer"
    /// MiSTer running mrext Remote.
    var misterHost = ""
    var misterPort: UInt16 = 8182
    /// OBS PC sending NDI (optional: NDI discovery usually finds it by name).
    var obsHost = ""
    /// NDI source name as discovered, e.g. "OBSPC (MiSTer-1)".
    var ndiSource = ""
    var mouseSensitivity = 1.0
    /// Alt/Option + click → middle click.
    var altClickIsMiddle = true
    /// Send the mouse's raw movement, so only the vintage OS accelerates it.
    var rawMouse = false
    /// "fit", "integer" or "native" (see ScaleMode in the core).
    var scaleMode = "fit"
    var smoothScaling = false
    var autoCrop = true
    /// Draw a local cursor ahead of the guest's lagging one.
    var predictiveCursor = true
    /// What the predictive cursor learned last time (0 = not yet).
    var cursorLagMs = 0.0
    var cursorGain = 0.0

    init() {}

    // Decode field by field so profiles saved by older versions keep working
    // when fields are added.
    init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        let d = HostProfile()
        id = try c.decodeIfPresent(UUID.self, forKey: .id) ?? d.id
        name = try c.decodeIfPresent(String.self, forKey: .name) ?? d.name
        misterHost = try c.decodeIfPresent(String.self, forKey: .misterHost) ?? d.misterHost
        misterPort = try c.decodeIfPresent(UInt16.self, forKey: .misterPort) ?? d.misterPort
        obsHost = try c.decodeIfPresent(String.self, forKey: .obsHost) ?? d.obsHost
        ndiSource = try c.decodeIfPresent(String.self, forKey: .ndiSource) ?? d.ndiSource
        mouseSensitivity = try c.decodeIfPresent(Double.self, forKey: .mouseSensitivity) ?? d.mouseSensitivity
        altClickIsMiddle = try c.decodeIfPresent(Bool.self, forKey: .altClickIsMiddle) ?? d.altClickIsMiddle
        rawMouse = try c.decodeIfPresent(Bool.self, forKey: .rawMouse) ?? d.rawMouse
        scaleMode = try c.decodeIfPresent(String.self, forKey: .scaleMode) ?? d.scaleMode
        smoothScaling = try c.decodeIfPresent(Bool.self, forKey: .smoothScaling) ?? d.smoothScaling
        autoCrop = try c.decodeIfPresent(Bool.self, forKey: .autoCrop) ?? d.autoCrop
        predictiveCursor = try c.decodeIfPresent(Bool.self, forKey: .predictiveCursor) ?? d.predictiveCursor
        cursorLagMs = try c.decodeIfPresent(Double.self, forKey: .cursorLagMs) ?? d.cursorLagMs
        cursorGain = try c.decodeIfPresent(Double.self, forKey: .cursorGain) ?? d.cursorGain
    }
}

@MainActor
final class ProfileStore: ObservableObject {
    @Published var profiles: [HostProfile] {
        didSet { save() }
    }

    private static let key = "hostProfiles"

    init() {
        if let data = UserDefaults.standard.data(forKey: Self.key),
           let saved = try? JSONDecoder().decode([HostProfile].self, from: data) {
            profiles = saved
        } else {
            profiles = []
        }
    }

    func upsert(_ profile: HostProfile) {
        if let i = profiles.firstIndex(where: { $0.id == profile.id }) {
            profiles[i] = profile
        } else {
            profiles.append(profile)
        }
    }

    func delete(_ id: HostProfile.ID) {
        profiles.removeAll { $0.id == id }
    }

    private func save() {
        if let data = try? JSONEncoder().encode(profiles) {
            UserDefaults.standard.set(data, forKey: Self.key)
        }
    }
}

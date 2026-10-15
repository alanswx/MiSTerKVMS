import Foundation
import Security

/// Device keys for MiSTers paired to connect from the internet
/// (docs/IPHONE.md §2), one per host, in the Keychain: this device only
/// (never synced or backed up to another phone), readable once the phone
/// has been unlocked after a restart. The core gets them at launch; they
/// never go into its settings file.
enum DeviceKeychain {
    private static let service = "com.danifunker.misterkvms.device-key"

    struct Entry: Codable {
        let keyId: String
        let key: String
    }

    private static func query(host: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: host]
    }

    @discardableResult
    static func save(_ entry: Entry, host: String) -> Bool {
        guard let data = try? JSONEncoder().encode(entry) else { return false }
        delete(host: host)
        var add = query(host: host)
        add[kSecValueData as String] = data
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        return SecItemAdd(add as CFDictionary, nil) == errSecSuccess
    }

    static func load(host: String) -> Entry? {
        var q = query(host: host)
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        guard SecItemCopyMatching(q as CFDictionary, &result) == errSecSuccess, let data = result as? Data else { return nil }
        return try? JSONDecoder().decode(Entry.self, from: data)
    }

    static func delete(host: String) {
        SecItemDelete(query(host: host) as CFDictionary)
    }
}

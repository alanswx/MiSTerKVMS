import Foundation
import Security

/// Device keys for connecting from the internet (docs/IPHONE.md §2), one
/// per capture PC (filed under the name `Controller.deviceKeyServer` gives:
/// the PC's id), in the Keychain: this device only
/// (never synced or backed up to another phone), readable once the phone
/// has been unlocked after a restart. The core gets them at launch; they
/// never go into its settings file.
enum DeviceKeychain {
    private static let service = "com.danifunker.misterkvms.device-key"

    struct Entry: Codable {
        let keyId: String
        let key: String
    }

    private static func query(_ account: String) -> [String: Any] {
        [kSecClass as String: kSecClassGenericPassword,
         kSecAttrService as String: service,
         kSecAttrAccount as String: account]
    }

    @discardableResult
    static func save(_ entry: Entry, as account: String) -> Bool {
        guard let data = try? JSONEncoder().encode(entry) else { return false }
        delete(account)
        var add = query(account)
        add[kSecValueData as String] = data
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        return SecItemAdd(add as CFDictionary, nil) == errSecSuccess
    }

    static func load(_ account: String) -> Entry? {
        var q = query(account)
        q[kSecReturnData as String] = true
        q[kSecMatchLimit as String] = kSecMatchLimitOne
        var result: AnyObject?
        guard SecItemCopyMatching(q as CFDictionary, &result) == errSecSuccess, let data = result as? Data else { return nil }
        return try? JSONDecoder().decode(Entry.self, from: data)
    }

    static func delete(_ account: String) {
        SecItemDelete(query(account) as CFDictionary)
    }
}

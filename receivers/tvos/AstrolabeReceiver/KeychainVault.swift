import Foundation
import Security

struct KeychainVault {
    private let service = "com.nixiesoftware.astrolabe.receiver"
    private let account = "credential-v1"
    private let encoder = JSONEncoder()
    private let decoder = JSONDecoder()

    func load() throws -> ReceiverCredential? {
        var result: CFTypeRef?
        let status = SecItemCopyMatching([
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecReturnData: true,
            kSecMatchLimit: kSecMatchLimitOne,
        ] as CFDictionary, &result)
        if status == errSecItemNotFound { return nil }
        guard status == errSecSuccess, let data = result as? Data else {
            throw DisplayProtocolV1.refusal("protected_storage", "Keychain read failed (\(status))")
        }
        return try decoder.decode(ReceiverCredential.self, from: data)
    }

    func save(_ credential: ReceiverCredential) throws {
        let data = try encoder.encode(credential)
        guard data.count <= 16 * 1024 else { throw DisplayProtocolV1.refusal("bound_exceeded", "credential") }
        let identity = [
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
        ] as CFDictionary
        let updated = SecItemUpdate(identity, [kSecValueData: data] as CFDictionary)
        if updated == errSecSuccess { return }
        guard updated == errSecItemNotFound else {
            throw DisplayProtocolV1.refusal("protected_storage", "Keychain update failed (\(updated))")
        }
        let added = SecItemAdd([
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
            kSecValueData: data,
            kSecAttrAccessible: kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly,
        ] as CFDictionary, nil)
        guard added == errSecSuccess else {
            throw DisplayProtocolV1.refusal("protected_storage", "Keychain add failed (\(added))")
        }
    }

    func clear() throws {
        let status = SecItemDelete([
            kSecClass: kSecClassGenericPassword,
            kSecAttrService: service,
            kSecAttrAccount: account,
        ] as CFDictionary)
        guard status == errSecSuccess || status == errSecItemNotFound else {
            throw DisplayProtocolV1.refusal("protected_storage", "Keychain delete failed (\(status))")
        }
    }
}

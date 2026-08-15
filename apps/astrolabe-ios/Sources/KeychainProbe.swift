import Foundation
import Security

/// The P0 evidence that platform-protected storage works: generate a secret,
/// keep it in the Keychain, read it back, and say which step failed if one
/// did. Failure is a named state, never a silently regenerated secret — the
/// same rule the design applies to device identity.
enum KeychainProbe {
    enum Outcome: Equatable {
        case roundTripped
        case writeFailed(OSStatus)
        case readFailed(OSStatus)
        case mismatch
    }

    private static let service = "com.nixiesoftware.astrolabe.spike"
    private static let account = "p0-probe-secret"

    static func run() -> Outcome {
        var secret = [UInt8](repeating: 0, count: 32)
        _ = SecRandomCopyBytes(kSecRandomDefault, secret.count, &secret)
        let data = Data(secret)

        let base: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]

        SecItemDelete(base as CFDictionary)

        var add = base
        add[kSecValueData as String] = data
        add[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlockThisDeviceOnly
        let wrote = SecItemAdd(add as CFDictionary, nil)
        guard wrote == errSecSuccess else { return .writeFailed(wrote) }

        var query = base
        query[kSecReturnData as String] = true
        query[kSecMatchLimit as String] = kSecMatchLimitOne
        var out: CFTypeRef?
        let read = SecItemCopyMatching(query as CFDictionary, &out)
        guard read == errSecSuccess, let got = out as? Data else {
            return .readFailed(read)
        }
        return got == data ? .roundTripped : .mismatch
    }
}

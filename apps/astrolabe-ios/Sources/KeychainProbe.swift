import Foundation
import Security

/// The P0 evidence that platform-protected storage works: generate a secret,
/// keep it in the Keychain, read it back, and say which step failed if one
/// did. Failure is a named state, never a silently regenerated secret — the
/// same rule the design applies to device identity.
enum KeychainProbe {
    enum Outcome: Equatable {
        case roundTripped
        case entropyFailed(OSStatus)
        case writeFailed(OSStatus)
        case readFailed(OSStatus)
        case mismatch
    }

    private static let service = "com.nixiesoftware.astrolabe.keychain-probe"
    private static let account = "probe-secret"

    /// One probe per process. The answer cannot change while the app runs,
    /// and re-proving it on every render of the row that shows it was a
    /// Keychain write per frame.
    static let outcome: Outcome = run()

    private static func run() -> Outcome {
        var secret = [UInt8](repeating: 0, count: 32)
        let minted = SecRandomCopyBytes(kSecRandomDefault, secret.count, &secret)
        guard minted == errSecSuccess else { return .entropyFailed(minted) }
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

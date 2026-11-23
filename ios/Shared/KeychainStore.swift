import Foundation
import Security

public struct FleetCredentials: Sendable, Hashable {
    public var serverURL: String
    public var token: String

    public init(serverURL: String, token: String) {
        self.serverURL = serverURL
        self.token = token
    }

    public var isConfigured: Bool {
        !serverURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            && !token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}

public final class KeychainStore: Sendable {
    public static let shared = KeychainStore()

    private let service: String
    private let tokenAccount = "api-bearer-token"
    private let serverDefaultsKey = "fleet.api.serverURL"
    private let appGroup: String?

    public init(
        service: String = "fleet.companion.credentials",
        appGroup: String? = Bundle.main.object(forInfoDictionaryKey: "FleetAppGroup") as? String
    ) {
        self.service = service
        if let appGroup, !appGroup.isEmpty, !appGroup.contains("$(") {
            self.appGroup = appGroup
        } else {
            self.appGroup = nil
        }
    }

    public static var defaultServerURL: String {
        if let raw = Bundle.main.object(forInfoDictionaryKey: "FleetDefaultServer") as? String,
           !raw.isEmpty,
           !raw.contains("$("),
           !raw.contains("example.ts.net") {
            return raw
        }
        return "https://hub.example.ts.net:8444"
    }

    public func loadCredentials() -> FleetCredentials {
        let server = loadServerURL() ?? Self.defaultServerURL
        let token = loadToken() ?? ""
        return FleetCredentials(serverURL: server, token: token)
    }

    public func saveCredentials(serverURL: String, token: String) throws {
        let cleanServer = serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let cleanToken = token.trimmingCharacters(in: .whitespacesAndNewlines)
        saveServerURL(cleanServer)
        try saveToken(cleanToken)
    }

    public func clearToken() {
        deleteToken(useAccessGroup: true)
        deleteToken(useAccessGroup: false)
    }

    private func defaults() -> [UserDefaults] {
        var stores: [UserDefaults] = []
        if let appGroup,
           FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup) != nil,
           let groupDefaults = UserDefaults(suiteName: appGroup) {
            stores.append(groupDefaults)
        }
        stores.append(.standard)
        return stores
    }

    private func loadServerURL() -> String? {
        for store in defaults() {
            if let val = store.string(forKey: serverDefaultsKey), !val.isEmpty {
                return val
            }
        }
        return nil
    }

    private func saveServerURL(_ value: String) {
        for store in defaults() {
            store.set(value, forKey: serverDefaultsKey)
        }
    }

    private func loadToken() -> String? {
        if let appGroup, let val = readKeychainToken(accessGroup: appGroup) {
            return val
        }
        return readKeychainToken(accessGroup: nil)
    }

    private func saveToken(_ token: String) throws {
        guard let data = token.data(using: .utf8) else { return }
        if let appGroup {
            let status = writeKeychainToken(data: data, accessGroup: appGroup)
            if status == errSecSuccess {
                _ = writeKeychainToken(data: data, accessGroup: nil)
                return
            }
            if status != errSecMissingEntitlement {
                throw KeychainError.osStatus(status)
            }
        }
        let status = writeKeychainToken(data: data, accessGroup: nil)
        if status != errSecSuccess {
            throw KeychainError.osStatus(status)
        }
    }

    private func readKeychainToken(accessGroup: String?) -> String? {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: tokenAccount,
            kSecReturnData as String: true,
            kSecMatchLimit as String: kSecMatchLimitOne,
        ]
        if let accessGroup {
            query[kSecAttrAccessGroup as String] = accessGroup
        }
        var result: AnyObject?
        let status = SecItemCopyMatching(query as CFDictionary, &result)
        guard status == errSecSuccess, let data = result as? Data else {
            return nil
        }
        return String(data: data, encoding: .utf8)
    }

    private func writeKeychainToken(data: Data, accessGroup: String?) -> OSStatus {
        var matchQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: tokenAccount,
        ]
        if let accessGroup {
            matchQuery[kSecAttrAccessGroup as String] = accessGroup
        }
        let updateAttrs: [String: Any] = [
            kSecValueData as String: data,
            kSecAttrAccessible as String: kSecAttrAccessibleAfterFirstUnlock,
        ]
        let updateStatus = SecItemUpdate(matchQuery as CFDictionary, updateAttrs as CFDictionary)
        if updateStatus == errSecSuccess {
            return errSecSuccess
        }
        if updateStatus != errSecItemNotFound {
            return updateStatus
        }
        var addQuery = matchQuery
        addQuery[kSecValueData as String] = data
        addQuery[kSecAttrAccessible as String] = kSecAttrAccessibleAfterFirstUnlock
        return SecItemAdd(addQuery as CFDictionary, nil)
    }

    private func deleteToken(useAccessGroup: Bool) {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: tokenAccount,
        ]
        if useAccessGroup, let appGroup {
            query[kSecAttrAccessGroup as String] = appGroup
        }
        SecItemDelete(query as CFDictionary)
    }
}

public enum KeychainError: LocalizedError, Sendable {
    case osStatus(OSStatus)

    public var errorDescription: String? {
        switch self {
        case .osStatus(let code):
            return "Keychain error (\(code))"
        }
    }
}

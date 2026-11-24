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

public struct MissionControlCredentials: Sendable, Hashable {
    public var primaryURL: String
    public var fallbackURL: String
    public var token: String

    public init(primaryURL: String, fallbackURL: String = "", token: String) {
        self.primaryURL = primaryURL
        self.fallbackURL = fallbackURL
        self.token = token
    }

    public var isConfigured: Bool {
        let hasHost = !primaryURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
            || !fallbackURL.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
        return hasHost && !token.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty
    }
}

public final class KeychainStore: Sendable {
    public static let shared = KeychainStore()

    private let service: String
    private let tokenAccount = "api-bearer-token"
    private let mcTokenAccount = "mc-api-bearer-token"
    private let serverDefaultsKey = "fleet.api.serverURL"
    private let mcPrimaryDefaultsKey = "mc.api.primaryURL"
    private let mcFallbackDefaultsKey = "mc.api.fallbackURL"
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
        infoURL(forKey: "FleetDefaultServer") ?? "https://hub.example.ts.net:8444"
    }

    public static var defaultMCPrimaryURL: String {
        infoURL(forKey: "MCPrimaryServer") ?? ""
    }

    public static var defaultMCFallbackURL: String {
        infoURL(forKey: "MCFallbackServer") ?? ""
    }

    private static func infoURL(forKey key: String) -> String? {
        guard let raw = Bundle.main.object(forInfoDictionaryKey: key) as? String,
              !raw.isEmpty,
              !raw.contains("$("),
              !raw.contains("example.ts.net") else {
            return nil
        }
        return raw.trimmingCharacters(in: .whitespacesAndNewlines).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
    }

    public func loadCredentials() -> FleetCredentials {
        let server = loadString(forKey: serverDefaultsKey) ?? Self.defaultServerURL
        let token = loadToken(account: tokenAccount) ?? ""
        return FleetCredentials(serverURL: server, token: token)
    }

    public func saveCredentials(serverURL: String, token: String) throws {
        let cleanServer = serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let cleanToken = token.trimmingCharacters(in: .whitespacesAndNewlines)
        saveString(cleanServer, forKey: serverDefaultsKey)
        try saveToken(cleanToken, account: tokenAccount)
    }

    public func clearToken() {
        deleteToken(account: tokenAccount, useAccessGroup: true)
        deleteToken(account: tokenAccount, useAccessGroup: false)
    }

    public func loadMCCredentials() -> MissionControlCredentials {
        let primary = loadString(forKey: mcPrimaryDefaultsKey) ?? Self.defaultMCPrimaryURL
        let fallback = loadString(forKey: mcFallbackDefaultsKey) ?? Self.defaultMCFallbackURL
        let token = loadToken(account: mcTokenAccount) ?? ""
        return MissionControlCredentials(primaryURL: primary, fallbackURL: fallback, token: token)
    }

    public func saveMCCredentials(primaryURL: String, fallbackURL: String, token: String) throws {
        let cleanPrimary = primaryURL.trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let cleanFallback = fallbackURL.trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let cleanToken = token.trimmingCharacters(in: .whitespacesAndNewlines)
        saveString(cleanPrimary, forKey: mcPrimaryDefaultsKey)
        saveString(cleanFallback, forKey: mcFallbackDefaultsKey)
        try saveToken(cleanToken, account: mcTokenAccount)
    }

    public func clearMCToken() {
        deleteToken(account: mcTokenAccount, useAccessGroup: true)
        deleteToken(account: mcTokenAccount, useAccessGroup: false)
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

    private func loadString(forKey key: String) -> String? {
        for store in defaults() {
            if let val = store.string(forKey: key), !val.isEmpty {
                return val
            }
        }
        return nil
    }

    private func saveString(_ value: String, forKey key: String) {
        for store in defaults() {
            store.set(value, forKey: key)
        }
    }

    private func loadToken(account: String) -> String? {
        if let appGroup, let val = readKeychainToken(account: account, accessGroup: appGroup) {
            return val
        }
        return readKeychainToken(account: account, accessGroup: nil)
    }

    private func saveToken(_ token: String, account: String) throws {
        guard let data = token.data(using: .utf8) else { return }
        if let appGroup {
            let status = writeKeychainToken(data: data, account: account, accessGroup: appGroup)
            if status == errSecSuccess {
                _ = writeKeychainToken(data: data, account: account, accessGroup: nil)
                return
            }
            if status != errSecMissingEntitlement {
                throw KeychainError.osStatus(status)
            }
        }
        let status = writeKeychainToken(data: data, account: account, accessGroup: nil)
        if status != errSecSuccess {
            throw KeychainError.osStatus(status)
        }
    }

    private func readKeychainToken(account: String, accessGroup: String?) -> String? {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
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

    private func writeKeychainToken(data: Data, account: String, accessGroup: String?) -> OSStatus {
        var matchQuery: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
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

    private func deleteToken(account: String, useAccessGroup: Bool) {
        var query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
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

import Foundation
import Security

public struct FleetCredentials: Codable, Sendable, Hashable {
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

public struct MissionControlCredentials: Codable, Sendable, Hashable {
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
    private let fleetPairAccount = "fleet-paired-credentials"
    private let mcPairAccount = "mc-paired-credentials"
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
        if let data = loadKeychainData(account: fleetPairAccount) {
            return (try? JSONDecoder().decode(FleetCredentials.self, from: data))
                ?? FleetCredentials(serverURL: "", token: "")
        }
        let server = loadString(forKey: serverDefaultsKey) ?? Self.defaultServerURL
        let token = loadToken(account: tokenAccount) ?? ""
        return FleetCredentials(serverURL: server, token: token)
    }

    public func saveCredentials(serverURL: String, token: String) throws {
        let pair = FleetCredentials(
            serverURL: try PairingLink.validatedServerURL(serverURL),
            token: token.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        try saveKeychainData(JSONEncoder().encode(pair), account: fleetPairAccount)
        deleteToken(account: tokenAccount)
    }

    public func clearToken() {
        let pair = loadCredentials()
        deleteToken(account: tokenAccount)
        saveString(pair.serverURL, forKey: serverDefaultsKey)
        deleteToken(account: fleetPairAccount)
    }

    public func loadMCCredentials() -> MissionControlCredentials {
        if let data = loadKeychainData(account: mcPairAccount) {
            return (try? JSONDecoder().decode(MissionControlCredentials.self, from: data))
                ?? MissionControlCredentials(primaryURL: "", token: "")
        }
        let primary = loadString(forKey: mcPrimaryDefaultsKey) ?? Self.defaultMCPrimaryURL
        let fallback = loadString(forKey: mcFallbackDefaultsKey) ?? Self.defaultMCFallbackURL
        let token = loadToken(account: mcTokenAccount) ?? ""
        return MissionControlCredentials(primaryURL: primary, fallbackURL: fallback, token: token)
    }

    public func saveMCCredentials(primaryURL: String, fallbackURL: String, token: String) throws {
        let primary = primaryURL.trimmingCharacters(in: .whitespacesAndNewlines)
        let fallback = fallbackURL.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !primary.isEmpty || !fallback.isEmpty else { throw PairingLinkError.invalidServerURL }
        let pair = MissionControlCredentials(
            primaryURL: primary.isEmpty ? "" : try PairingLink.validatedServerURL(primary),
            fallbackURL: fallback.isEmpty ? "" : try PairingLink.validatedServerURL(fallback),
            token: token.trimmingCharacters(in: .whitespacesAndNewlines)
        )
        try saveKeychainData(JSONEncoder().encode(pair), account: mcPairAccount)
        deleteToken(account: mcTokenAccount)
    }

    public func clearMCToken() {
        let pair = loadMCCredentials()
        deleteToken(account: mcTokenAccount)
        saveString(pair.primaryURL, forKey: mcPrimaryDefaultsKey)
        saveString(pair.fallbackURL, forKey: mcFallbackDefaultsKey)
        deleteToken(account: mcPairAccount)
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
            if let val = store.string(forKey: key) {
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
        loadKeychainData(account: account).flatMap { String(data: $0, encoding: .utf8) }
    }

    private func loadKeychainData(account: String) -> Data? {
        if let appGroup, let val = readKeychainData(account: account, accessGroup: appGroup) {
            return val
        }
        return readKeychainData(account: account, accessGroup: nil)
    }

    private func saveKeychainData(_ data: Data, account: String) throws {
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

    private func readKeychainData(account: String, accessGroup: String?) -> Data? {
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
        return data
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

    private func deleteToken(account: String) {
        let query: [String: Any] = [
            kSecClass as String: kSecClassGenericPassword,
            kSecAttrService as String: service,
            kSecAttrAccount as String: account,
        ]
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

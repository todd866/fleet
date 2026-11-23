import Foundation

public struct PairingLink: Sendable, Equatable {
    public let serverURL: String
    public let token: String

    public init(serverURL: String, token: String) {
        self.serverURL = serverURL
        self.token = token
    }

    public static func parse(_ url: URL) throws -> PairingLink {
        guard let scheme = url.scheme?.lowercased(), scheme == "fleet" else {
            throw PairingLinkError.invalidScheme
        }
        let host = url.host?.lowercased() ?? ""
        let path = url.path.lowercased()
        guard host == "pair" || path == "/pair" else {
            throw PairingLinkError.invalidAction
        }
        guard let components = URLComponents(url: url, resolvingAgainstBaseURL: false),
              let items = components.queryItems else {
            throw PairingLinkError.missingParameters
        }
        let rawServer = items.first(where: { $0.name == "url" || $0.name == "server" })?.value?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let rawToken = items.first(where: { $0.name == "token" || $0.name == "t" })?.value?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

        guard let parsedServer = URL(string: rawServer),
              let serverScheme = parsedServer.scheme?.lowercased(),
              let serverHost = parsedServer.host?.lowercased(),
              !serverHost.isEmpty else {
            throw PairingLinkError.invalidServerURL
        }
        let isLocalHTTP = serverScheme == "http" && (serverHost == "127.0.0.1" || serverHost == "localhost")
        guard serverScheme == "https" || isLocalHTTP else {
            throw PairingLinkError.insecureServerURL
        }
        guard rawToken.count >= 16 else {
            throw PairingLinkError.invalidToken
        }

        let normalized = rawServer.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        return PairingLink(serverURL: normalized, token: rawToken)
    }

    public static func parse(_ string: String) throws -> PairingLink {
        let trimmed = string.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed) else {
            throw PairingLinkError.invalidURL
        }
        return try parse(url)
    }
}

public enum PairingLinkError: LocalizedError, Sendable, Equatable {
    case invalidURL
    case invalidScheme
    case invalidAction
    case missingParameters
    case invalidServerURL
    case insecureServerURL
    case invalidToken

    public var errorDescription: String? {
        switch self {
        case .invalidURL:
            return "Malformed pairing URL."
        case .invalidScheme:
            return "Expected fleet:// URL scheme."
        case .invalidAction:
            return "Expected fleet://pair link."
        case .missingParameters:
            return "Pairing link is missing server url or token."
        case .invalidServerURL:
            return "Server URL in pairing link is invalid."
        case .insecureServerURL:
            return "Hub URL must use HTTPS (except localhost testing)."
        case .invalidToken:
            return "Pairing token must be at least 16 characters."
        }
    }
}

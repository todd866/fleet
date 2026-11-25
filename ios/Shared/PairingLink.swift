import Foundation

public struct PairingLink: Sendable, Equatable {
    public let serverURL: String
    public let token: String
    public let mcPrimaryURL: String?
    public let mcFallbackURL: String?
    public let mcToken: String?

    public var hasFleetCredentials: Bool {
        !serverURL.isEmpty && !token.isEmpty
    }

    public var hasMissionControlCredentials: Bool {
        !(mcPrimaryURL ?? "").isEmpty && !(mcToken ?? "").isEmpty
    }

    public init(
        serverURL: String,
        token: String,
        mcPrimaryURL: String? = nil,
        mcFallbackURL: String? = nil,
        mcToken: String? = nil
    ) {
        self.serverURL = serverURL
        self.token = token
        self.mcPrimaryURL = mcPrimaryURL
        self.mcFallbackURL = mcFallbackURL
        self.mcToken = mcToken
    }

    static func validatedServerURL(_ raw: String) throws -> String {
        let base = raw.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let parsed = URLComponents(string: base),
              parsed.user == nil, parsed.password == nil, parsed.query == nil, parsed.fragment == nil,
              let scheme = parsed.scheme?.lowercased(),
              let host = parsed.host?.lowercased(),
              !host.isEmpty else {
            throw PairingLinkError.invalidServerURL
        }
        let isLocalHTTP = scheme == "http" && (host == "127.0.0.1" || host == "localhost")
        guard scheme == "https" || isLocalHTTP else {
            throw PairingLinkError.insecureServerURL
        }
        return base.trimmingCharacters(in: CharacterSet(charactersIn: "/"))
    }

    public static func parse(_ url: URL) throws -> PairingLink {
        guard let scheme = url.scheme?.lowercased(), scheme == "fleet" || scheme == "agycontrol" else {
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

        if scheme == "agycontrol" {
            let rawPrimary = items.first(where: { $0.name == "url" || $0.name == "server" })?.value?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            let rawFallback = items.first(where: { $0.name == "fallback" })?.value?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            let rawMcTok = items.first(where: { $0.name == "token" || $0.name == "t" })?.value?
                .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
            let mcPrimary = try validatedServerURL(rawPrimary)
            let mcFallback = rawFallback.isEmpty ? nil : try validatedServerURL(rawFallback)
            guard rawMcTok.count >= 16 else {
                throw PairingLinkError.invalidToken
            }
            return PairingLink(
                serverURL: "",
                token: "",
                mcPrimaryURL: mcPrimary,
                mcFallbackURL: mcFallback,
                mcToken: rawMcTok
            )
        }

        let rawServer = items.first(where: { $0.name == "url" || $0.name == "server" })?.value?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let rawToken = items.first(where: { $0.name == "token" || $0.name == "t" })?.value?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

        let normalized = try validatedServerURL(rawServer)
        guard rawToken.count >= 16 else {
            throw PairingLinkError.invalidToken
        }

        let rawMcPrimary = items.first(where: { $0.name == "mcUrl" || $0.name == "mc_url" })?.value?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let rawMcFallback = items.first(where: { $0.name == "mcFallback" || $0.name == "mc_fallback" })?.value?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
        let rawMcToken = items.first(where: { $0.name == "mcToken" || $0.name == "mc_token" })?.value?
            .trimmingCharacters(in: .whitespacesAndNewlines) ?? ""

        let mcPrimary = rawMcPrimary.isEmpty ? nil : try validatedServerURL(rawMcPrimary)
        let mcFallback = rawMcFallback.isEmpty ? nil : try validatedServerURL(rawMcFallback)
        let mcTok: String?
        if rawMcToken.isEmpty {
            mcTok = nil
        } else {
            guard rawMcToken.count >= 16 else {
                throw PairingLinkError.invalidToken
            }
            mcTok = rawMcToken
        }

        return PairingLink(
            serverURL: normalized,
            token: rawToken,
            mcPrimaryURL: mcPrimary,
            mcFallbackURL: mcFallback,
            mcToken: mcTok
        )
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

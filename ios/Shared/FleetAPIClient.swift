import Foundation

public enum FleetAPIError: LocalizedError, Sendable, Equatable {
    case unconfigured
    case invalidURL(String)
    case unauthorized
    case httpStatus(Int, String)
    case transport(String)
    case decoding(String)

    public var errorDescription: String? {
        switch self {
        case .unconfigured:
            return "Configure a hub server URL and token in Controls or tap a pairing link."
        case .invalidURL(let raw):
            return "Invalid server URL: \(raw)"
        case .unauthorized:
            return "Bearer token rejected (HTTP 401). Update token in Controls."
        case .httpStatus(let code, let message):
            return "Server returned HTTP \(code): \(message)"
        case .transport(let message):
            return "Connection failed: \(message)"
        case .decoding(let message):
            return "Unexpected server response: \(message)"
        }
    }
}

public actor FleetAPIClient {
    private let session: URLSession
    private let credentialsProvider: @Sendable () -> FleetCredentials

    public init(
        session: URLSession? = nil,
        credentialsProvider: @escaping @Sendable () -> FleetCredentials = { KeychainStore.shared.loadCredentials() }
    ) {
        if let session {
            self.session = session
        } else {
            let cfg = URLSessionConfiguration.default
            cfg.timeoutIntervalForRequest = 12
            cfg.timeoutIntervalForResource = 25
            cfg.requestCachePolicy = .reloadIgnoringLocalCacheData
            self.session = URLSession(configuration: cfg)
        }
        self.credentialsProvider = credentialsProvider
    }

    public func testConnection(serverURL: String, token: String) async throws -> HealthResponse {
        try await request(
            path: "/v1/health",
            method: "GET",
            overrideCredentials: FleetCredentials(serverURL: serverURL, token: token)
        )
    }

    public func fetchHealth() async throws -> HealthResponse {
        try await request(path: "/v1/health")
    }

    public func fetchOverview() async throws -> FleetOverviewResponse {
        try await request(path: "/v1/overview")
    }

    public func fetchHosts() async throws -> HostsResponse {
        try await request(path: "/v1/hosts")
    }

    public func fetchMorning() async throws -> MorningResponse {
        try await request(path: "/v1/morning")
    }

    public func fetchCapacity() async throws -> CapacityResponse {
        try await request(path: "/v1/capacity")
    }

    public func fetchBackups() async throws -> BackupsResponse {
        try await request(path: "/v1/backups")
    }

    public func fetchSecurity() async throws -> SecurityResponse {
        try await request(path: "/v1/security")
    }

    public func fetchMail() async throws -> MailResponse {
        try await request(path: "/v1/mail")
    }

    public func sendMail(to: String, text: String) async throws -> ActionResultResponse {
        try await postAction(path: "/v1/actions/mail", payload: ["to": to, "text": text])
    }

    public func sendVoiceNote(to: String?, text: String) async throws -> ActionResultResponse {
        var payload: [String: String] = ["text": text]
        if let to, !to.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            payload["to"] = to.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return try await postAction(path: "/v1/actions/voice-note", payload: payload)
    }

    public func triggerMorningRefresh() async throws -> ActionResultResponse {
        try await postAction(path: "/v1/actions/morning-refresh", payload: [String: String]())
    }

    public func setProviderPaused(provider: String, paused: Bool) async throws -> ActionResultResponse {
        struct PauseBody: Encodable {
            let provider: String
            let paused: Bool
        }
        return try await postEncodable(path: "/v1/actions/provider-pause", body: PauseBody(provider: provider, paused: paused))
    }

    private func postAction<P: Encodable>(path: String, payload: P) async throws -> ActionResultResponse {
        try await postEncodable(path: path, body: payload)
    }

    private func postEncodable<Body: Encodable>(path: String, body: Body) async throws -> ActionResultResponse {
        let encoded = try JSONEncoder().encode(body)
        return try await request(path: path, method: "POST", body: encoded)
    }

    private func request<T: Decodable>(
        path: String,
        method: String = "GET",
        body: Data? = nil,
        overrideCredentials: FleetCredentials? = nil
    ) async throws -> T {
        let creds = overrideCredentials ?? credentialsProvider()
        let baseRaw = creds.serverURL.trimmingCharacters(in: .whitespacesAndNewlines)
            .trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let token = creds.token.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !baseRaw.isEmpty, !token.isEmpty else {
            throw FleetAPIError.unconfigured
        }
        guard let url = URL(string: baseRaw + path) else {
            throw FleetAPIError.invalidURL(baseRaw + path)
        }

        var req = URLRequest(url: url)
        req.httpMethod = method
        req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
        req.setValue("application/json", forHTTPHeaderField: "Accept")
        if let body {
            req.httpBody = body
            req.setValue("application/json", forHTTPHeaderField: "Content-Type")
        }

        let data: Data
        let response: URLResponse
        do {
            (data, response) = try await session.data(for: req)
        } catch {
            throw FleetAPIError.transport(error.localizedDescription)
        }

        guard let http = response as? HTTPURLResponse else {
            throw FleetAPIError.transport("Non-HTTP response")
        }

        if http.statusCode == 401 || http.statusCode == 403 {
            throw FleetAPIError.unauthorized
        }

        if !(200...299).contains(http.statusCode) {
            if let errResp = try? JSONDecoder().decode(ActionResultResponse.self, from: data),
               let msg = errResp.error, !msg.isEmpty {
                throw FleetAPIError.httpStatus(http.statusCode, msg)
            }
            let snippet = String(data: data.prefix(160), encoding: .utf8) ?? ""
            throw FleetAPIError.httpStatus(http.statusCode, snippet.isEmpty ? HTTPURLResponse.localizedString(forStatusCode: http.statusCode) : snippet)
        }

        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw FleetAPIError.decoding(error.localizedDescription)
        }
    }
}

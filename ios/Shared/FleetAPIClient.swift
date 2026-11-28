import Foundation

// API calls carry a hub bearer and may carry a command body. Both belong to the paired URL.
final class PairedHubTransport: NSObject, URLSessionTaskDelegate {
    static let shared = PairedHubTransport()

    func urlSession(
        _ session: URLSession, task: URLSessionTask,
        willPerformHTTPRedirection response: HTTPURLResponse, newRequest request: URLRequest,
        completionHandler: @escaping (URLRequest?) -> Void
    ) {
        completionHandler(nil)
    }
}

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

    func fetchServices() async throws -> ServiceDirectory {
        try await request(path: "/v1/services")
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

    public func fetchDriveBrief(
        lat: Double? = nil,
        lon: Double? = nil,
        eventTitle: String? = nil,
        eventLocation: String? = nil,
        eventStart: String? = nil,
        shuffle: Bool = false
    ) async throws -> DriveBriefResponse {
        var items: [URLQueryItem] = []
        if let lat, let lon {
            items.append(URLQueryItem(name: "lat", value: String(format: "%.6f", lat)))
            items.append(URLQueryItem(name: "lon", value: String(format: "%.6f", lon)))
        }
        if let eventTitle, !eventTitle.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            items.append(URLQueryItem(name: "event_title", value: eventTitle))
        }
        if let eventLocation, !eventLocation.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            items.append(URLQueryItem(name: "event_location", value: eventLocation))
        }
        if let eventStart, !eventStart.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            items.append(URLQueryItem(name: "event_start", value: eventStart))
        }
        if shuffle {
            items.append(URLQueryItem(name: "shuffle", value: "1"))
        }
        if items.isEmpty {
            return try await request(path: "/v1/drive/brief")
        }
        var comps = URLComponents()
        comps.queryItems = items
        let query = comps.percentEncodedQuery ?? ""
        return try await request(path: "/v1/drive/brief?\(query)")
    }

    public func setDrivePlan(
        destinationQuery: String? = nil,
        destinationName: String? = nil,
        placeID: String? = nil,
        stationID: String? = nil,
        musicMood: String? = nil,
        volumePct: Int? = nil,
        ttlHours: Int = 6
    ) async throws -> DriveBriefResponse {
        struct DrivePlanBody: Encodable {
            let destination_query: String?
            let destination_name: String?
            let place_id: String?
            let station_id: String?
            let music_mood: String?
            let volume_pct: Int?
            let ttl_hours: Int
        }
        struct DrivePlanEnvelope: Decodable {
            let ok: Bool
            let brief: DriveBriefResponse
        }
        let body = DrivePlanBody(
            destination_query: destinationQuery,
            destination_name: destinationName,
            place_id: placeID,
            station_id: stationID,
            music_mood: musicMood,
            volume_pct: volumePct,
            ttl_hours: ttlHours
        )
        let encoded = try JSONEncoder().encode(body)
        let env: DrivePlanEnvelope = try await request(path: "/v1/actions/drive-plan", method: "POST", body: encoded)
        return env.brief
    }

    public func clearDrivePlan() async throws -> DriveBriefResponse {
        struct ClearBody: Encodable { let clear: Bool }
        struct DrivePlanEnvelope: Decodable {
            let ok: Bool
            let brief: DriveBriefResponse
        }
        let encoded = try JSONEncoder().encode(ClearBody(clear: true))
        let env: DrivePlanEnvelope = try await request(path: "/v1/actions/drive-plan", method: "POST", body: encoded)
        return env.brief
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

    public func ackAllMail() async throws -> ActionResultResponse {
        struct AckAllBody: Encodable {
            let all: Bool
        }
        return try await postEncodable(path: "/v1/actions/mail-ack", body: AckAllBody(all: true))
    }

    public func ackMail(ids: [String]) async throws -> ActionResultResponse {
        struct AckIdsBody: Encodable {
            let ids: [String]
        }
        return try await postEncodable(path: "/v1/actions/mail-ack", body: AckIdsBody(ids: ids))
    }

    public func fetchJobLog(host: String, label: String, lines: Int = 60) async throws -> JobLogResponse {
        var comps = URLComponents()
        comps.queryItems = [
            URLQueryItem(name: "host", value: host),
            URLQueryItem(name: "label", value: label),
            URLQueryItem(name: "lines", value: String(lines)),
        ]
        let query = comps.percentEncodedQuery ?? ""
        return try await request(path: "/v1/job-log?\(query)")
    }

    public func kickstartJob(host: String, label: String) async throws -> ActionResultResponse {
        try await postAction(path: "/v1/actions/job-kickstart", payload: ["host": host, "label": label])
    }

    public func runHostAction(host: String, kind: String) async throws -> ActionResultResponse {
        try await postAction(path: "/v1/actions/host-action", payload: ["host": host, "kind": kind])
    }

    public func snooze(key: String, hours: Int, note: String? = nil) async throws -> ActionResultResponse {
        struct SnoozeBody: Encodable {
            let key: String
            let hours: Int
            let note: String?
        }
        return try await postEncodable(path: "/v1/actions/snooze", body: SnoozeBody(key: key, hours: hours, note: note))
    }

    public func unsnooze(key: String) async throws -> ActionResultResponse {
        struct UnsnoozeBody: Encodable {
            let key: String
            let clear: Bool
        }
        return try await postEncodable(path: "/v1/actions/snooze", body: UnsnoozeBody(key: key, clear: true))
    }

    public func dispatchFix(to: String, key: String? = nil, title: String? = nil, prompt: String) async throws -> ActionResultResponse {
        struct FixBody: Encodable {
            let to: String
            let key: String?
            let title: String?
            let prompt: String
        }
        return try await postEncodable(
            path: "/v1/actions/dispatch-fix",
            body: FixBody(to: to, key: key, title: title, prompt: prompt)
        )
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
        let base = try PairingLink.validatedServerURL(baseRaw)
        guard let url = URL(string: base + path) else {
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
            (data, response) = try await session.data(for: req, delegate: PairedHubTransport.shared)
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

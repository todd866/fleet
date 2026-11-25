import Foundation

public enum MissionControlAPIError: LocalizedError, Sendable {
    case notConfigured
    case invalidURL(String)
    case httpStatus(Int, String)
    case networkFailure(String)
    case unknownOutcome

    public var errorDescription: String? {
        switch self {
        case .unknownOutcome:
            return "Action delivery is unconfirmed. Check the thread before sending again."
        case .notConfigured:
            return "Mission Control hub is not configured yet. Set primary server and token in Controls."
        case .invalidURL(let url):
            return "Invalid Mission Control server URL: \(url)"
        case .httpStatus(let code, let detail):
            return detail.isEmpty ? "Mission Control error (\(code))" : detail
        case .networkFailure(let msg):
            return "Unable to reach Mission Control hubs: \(msg)"
        }
    }
}

public actor MissionControlAPIClient {
    private let fastSession: URLSession
    private let longSession: URLSession
    private let credentialsProvider: @Sendable () -> MissionControlCredentials
    public private(set) var activeHostURL: String = ""

    public init(
        fastSession: URLSession? = nil,
        longSession: URLSession? = nil,
        credentialsProvider: @escaping @Sendable () -> MissionControlCredentials = { KeychainStore.shared.loadMCCredentials() }
    ) {
        if let fastSession {
            self.fastSession = fastSession
        } else {
            let cfg = URLSessionConfiguration.default
            cfg.timeoutIntervalForRequest = 15
            cfg.timeoutIntervalForResource = 25
            self.fastSession = URLSession(configuration: cfg)
        }
        if let longSession {
            self.longSession = longSession
        } else {
            let cfg = URLSessionConfiguration.default
            cfg.timeoutIntervalForRequest = 75
            cfg.timeoutIntervalForResource = 90
            self.longSession = URLSession(configuration: cfg)
        }
        self.credentialsProvider = credentialsProvider
    }

    // MARK: - Core Failover Transport

    private func endpoints() throws -> (urls: [String], token: String) {
        let creds = credentialsProvider()
        let token = creds.token.trimmingCharacters(in: .whitespacesAndNewlines)
        var list: [String] = []
        let primary = creds.primaryURL.trimmingCharacters(in: .whitespacesAndNewlines).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        let fallback = creds.fallbackURL.trimmingCharacters(in: .whitespacesAndNewlines).trimmingCharacters(in: CharacterSet(charactersIn: "/"))
        if !primary.isEmpty, !primary.contains("example.ts.net") {
            list.append(try PairingLink.validatedServerURL(primary))
        }
        if !fallback.isEmpty, !fallback.contains("example.ts.net"), fallback != primary {
            list.append(try PairingLink.validatedServerURL(fallback))
        }
        guard !list.isEmpty, !token.isEmpty else {
            throw MissionControlAPIError.notConfigured
        }
        return (list, token)
    }

    private func request<T: Decodable>(
        path: String,
        method: String = "GET",
        bodyData: Data? = nil,
        longTimeout: Bool = false
    ) async throws -> T {
        let (urls, token) = try endpoints()
        var lastError: Error = MissionControlAPIError.notConfigured

        let preferred = urls.contains(activeHostURL) ? activeHostURL : urls[0]
        let candidates = method == "GET" ? [preferred] + urls.filter { $0 != preferred } : [preferred]
        for base in candidates {
            try Task.checkCancellation()
            guard let url = URL(string: "\(base)\(path)") else {
                lastError = MissionControlAPIError.invalidURL(base)
                continue
            }
            var req = URLRequest(url: url)
            req.httpMethod = method
            req.setValue("application/json", forHTTPHeaderField: "Accept")
            req.setValue("Bearer \(token)", forHTTPHeaderField: "Authorization")
            if let bodyData {
                req.setValue("application/json", forHTTPHeaderField: "Content-Type")
                req.httpBody = bodyData
            }

            do {
                let session = longTimeout ? longSession : fastSession
                let (data, response) = try await session.data(for: req, delegate: PairedHubTransport.shared)
                guard let http = response as? HTTPURLResponse else {
                    lastError = MissionControlAPIError.networkFailure("Invalid server response")
                    continue
                }
                if (200 ..< 300).contains(http.statusCode) {
                    activeHostURL = base
                    return try JSONDecoder().decode(T.self, from: data)
                }
                let detail = Self.extractErrorDetail(from: data)
                let apiErr = MissionControlAPIError.httpStatus(http.statusCode, detail)
                if http.statusCode >= 500 {
                    lastError = apiErr
                    continue
                }
                activeHostURL = base
                throw apiErr
            } catch let err as MissionControlAPIError {
                if case .httpStatus(let code, _) = err, code < 500 {
                    throw err
                }
                lastError = err
            } catch {
                lastError = error
            }
        }
        try Task.checkCancellation()
        if method != "GET" {
            if case MissionControlAPIError.invalidURL = lastError { throw lastError }
            throw MissionControlAPIError.unknownOutcome
        }
        if let apiErr = lastError as? MissionControlAPIError {
            throw apiErr
        }
        throw MissionControlAPIError.networkFailure(lastError.localizedDescription)
    }

    private static func extractErrorDetail(from data: Data) -> String {
        if let parsed = try? JSONSerialization.jsonObject(with: data) as? [String: Any],
           let err = parsed["error"] as? String, !err.isEmpty {
            return err
        }
        return String(data: data, encoding: .utf8)?.trimmingCharacters(in: .whitespacesAndNewlines) ?? ""
    }

    private func postJSON<T: Decodable>(_ path: String, payload: [String: Any], longTimeout: Bool = false) async throws -> T {
        let data = try JSONSerialization.data(withJSONObject: payload, options: [])
        return try await request(path: path, method: "POST", bodyData: data, longTimeout: longTimeout)
    }

    // MARK: - Board & Threads

    public func testConnection() async throws -> MCHealthResponse {
        try await request(path: "/healthz")
    }

    public func testConnection(primaryURL: String, fallbackURL: String, token: String) async throws -> MCHealthResponse {
        let probe = MissionControlAPIClient(
            fastSession: fastSession,
            longSession: longSession,
            credentialsProvider: {
                MissionControlCredentials(primaryURL: primaryURL, fallbackURL: fallbackURL, token: token)
            }
        )
        let res = try await probe.testConnection()
        let used = await probe.activeHostURL
        if !used.isEmpty {
            activeHostURL = used
        }
        return res
    }

    public func fetchBoard() async throws -> MCBoardResponse {
        try await request(path: "/v1/board")
    }

    public func fetchThreadDetail(id: String, host: String? = nil) async throws -> MCThreadDetailResponse {
        let encoded = id.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? id
        let query = host.flatMap { $0.isEmpty ? nil : "?host=\($0.addingPercentEncoding(withAllowedCharacters: .urlQueryAllowed) ?? $0)" } ?? ""
        return try await request(path: "/v1/threads/\(encoded)\(query)")
    }

    public func sendMessage(conversationId: String, text: String, host: String? = nil) async throws -> MCActionResponse {
        let encoded = conversationId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? conversationId
        var payload: [String: Any] = ["text": text]
        if let host, !host.isEmpty { payload["host"] = host }
        return try await postJSON("/v1/threads/\(encoded)/message", payload: payload)
    }

    public func focusThread(conversationId: String, host: String? = nil) async throws -> MCActionResponse {
        let encoded = conversationId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? conversationId
        var payload: [String: Any] = [:]
        if let host, !host.isEmpty { payload["host"] = host }
        return try await postJSON("/v1/threads/\(encoded)/focus", payload: payload)
    }

    public func stopThread(conversationId: String, host: String? = nil) async throws -> MCActionResponse {
        let encoded = conversationId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? conversationId
        var payload: [String: Any] = [:]
        if let host, !host.isEmpty { payload["host"] = host }
        return try await postJSON("/v1/threads/\(encoded)/stop", payload: payload)
    }

    public func startNewThread(text: String, projectName: String? = nil, host: String? = nil) async throws -> MCActionResponse {
        var payload: [String: Any] = ["text": text]
        if let projectName, !projectName.isEmpty { payload["projectName"] = projectName }
        if let host, !host.isEmpty { payload["host"] = host }
        return try await postJSON("/v1/threads/new", payload: payload)
    }

    public func refreshBrief(conversationId: String, host: String? = nil) async throws -> MCActionResponse {
        let encoded = conversationId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? conversationId
        var payload: [String: Any] = [:]
        if let host, !host.isEmpty { payload["host"] = host }
        return try await postJSON("/v1/threads/\(encoded)/brief", payload: payload, longTimeout: true)
    }

    public func setAutoWake(conversationId: String, enabled: Bool?, host: String? = nil) async throws -> MCActionResponse {
        let encoded = conversationId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? conversationId
        var payload: [String: Any] = [:]
        if let enabled { payload["enabled"] = enabled }
        if let host, !host.isEmpty { payload["host"] = host }
        return try await postJSON("/v1/threads/\(encoded)/auto-wake", payload: payload)
    }

    public func submitRelay(targetConversationId: String, message: String, fromLabel: String = "iPhone", host: String? = nil) async throws -> MCActionResponse {
        var payload: [String: Any] = [
            "targetConversationId": targetConversationId,
            "message": message,
            "fromLabel": fromLabel,
            "hop": 1,
        ]
        if let host, !host.isEmpty { payload["host"] = host }
        return try await postJSON("/v1/relay", payload: payload)
    }

    public func approveRelay(relayId: String, editedText: String? = nil, host: String? = nil) async throws -> MCActionResponse {
        let encoded = relayId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? relayId
        var payload: [String: Any] = [:]
        if let editedText, !editedText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            payload["text"] = editedText.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        if let host, !host.isEmpty { payload["host"] = host }
        return try await postJSON("/v1/relay/\(encoded)/approve", payload: payload)
    }

    public func dismissRelay(relayId: String, host: String? = nil) async throws -> MCActionResponse {
        let encoded = relayId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? relayId
        var payload: [String: Any] = [:]
        if let host, !host.isEmpty { payload["host"] = host }
        return try await postJSON("/v1/relay/\(encoded)/dismiss", payload: payload)
    }

    // MARK: - AI Commander

    public func fetchCommanderState() async throws -> CommanderStateResponse {
        try await request(path: "/v1/ai/state")
    }

    public func fetchCommanderDigest(refresh: Bool = false) async throws -> CommanderDigestResponse {
        try await request(path: refresh ? "/v1/ai/digest?refresh=1" : "/v1/ai/digest", longTimeout: true)
    }

    public func sendCommanderMessage(
        text: String,
        routingMode: CommanderRoutingMode? = nil,
        directDispatch: Bool? = nil
    ) async throws -> CommanderChatResponse {
        var payload: [String: Any] = ["text": text]
        if let routingMode { payload["routingMode"] = routingMode.rawValue }
        if let directDispatch { payload["directDispatch"] = directDispatch }
        return try await postJSON("/v1/ai/chat", payload: payload, longTimeout: true)
    }

    public func executeCommanderAction(actionId: String, editedText: String? = nil) async throws -> CommanderActionExecuteResponse {
        let encoded = actionId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? actionId
        var payload: [String: Any] = [:]
        if let editedText, !editedText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
            payload["editedText"] = editedText.trimmingCharacters(in: .whitespacesAndNewlines)
        }
        return try await postJSON("/v1/ai/actions/\(encoded)/execute", payload: payload, longTimeout: true)
    }

    public func dismissCommanderAction(actionId: String) async throws -> CommanderActionExecuteResponse {
        let encoded = actionId.addingPercentEncoding(withAllowedCharacters: .urlPathAllowed) ?? actionId
        return try await postJSON("/v1/ai/actions/\(encoded)/dismiss", payload: [:])
    }

    public func updateCommanderSettings(routingMode: CommanderRoutingMode? = nil, directDispatch: Bool? = nil) async throws -> CommanderStateResponse {
        var payload: [String: Any] = [:]
        if let routingMode { payload["routingMode"] = routingMode.rawValue }
        if let directDispatch { payload["directDispatch"] = directDispatch }
        return try await postJSON("/v1/ai/settings", payload: payload)
    }

    public func clearCommanderChat() async throws -> CommanderStateResponse {
        try await postJSON("/v1/ai/clear", payload: [:])
    }
}

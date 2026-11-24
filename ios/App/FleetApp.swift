import LocalAuthentication
import Observation
import SwiftUI

enum FleetTab: Hashable {
    case command
    case agents
    case fleet
    case inbox
    case controls
}

enum FleetSubtab: String, CaseIterable, Identifiable {
    case overview = "Overview"
    case board = "Board"
    case services = "Stack"
    var id: String { rawValue }
}

enum FleetTheme {
    static let bg = Color(uiColor: .systemGroupedBackground)
    static let card = Color(uiColor: .secondarySystemGroupedBackground)
    static let elevated = Color(uiColor: .tertiarySystemFill)
    static let border = Color(uiColor: .separator).opacity(0.45)

    private static func ink(_ light: Int, _ dark: Int) -> Color {
        Color(uiColor: UIColor { traits in
            let rgb = traits.userInterfaceStyle == .dark ? dark : light
            return UIColor(red: CGFloat(rgb >> 16) / 255,
                           green: CGFloat((rgb >> 8) & 255) / 255,
                           blue: CGFloat(rgb & 255) / 255, alpha: 1)
        })
    }

    static let pass = ink(0x176648, 0x73D4A6)
    static let warn = ink(0x925014, 0xF3B45C)
    static let fail = ink(0xAD3037, 0xFF8888)
    static let info = ink(0x226387, 0x77BFDF)
    static let steel = ink(0x50617B, 0xAAC1DB)
    static let secondaryText = ink(0x53565C, 0xB6B9BE)
    static let actionFill = Color(red: 0.09, green: 0.40, blue: 0.28)

    static func color(forSeverity severity: RowSeverity) -> Color {
        switch severity {
        case .fail: return fail
        case .warn: return warn
        case .pass: return pass
        case .other: return info
        }
    }

    static func color(forStatus raw: String) -> Color {
        color(forSeverity: RowSeverity(status: raw))
    }

    static func color(forThreadState state: MCThreadState) -> Color {
        switch state {
        case .attention: return warn
        case .working: return info
        case .bg: return steel
        case .unread: return pass
        case .idle, .unknown: return .secondary
        }
    }
}

struct StatusPill: View {
    let label: String
    let color: Color

    var body: some View {
        Text(label.uppercased())
            .font(.caption2.weight(.bold))
            .monospaced()
            .padding(.horizontal, 8)
            .padding(.vertical, 3)
            .background(color.opacity(0.18), in: Capsule())
            .overlay(Capsule().stroke(color.opacity(0.45), lineWidth: 0.8))
            .foregroundStyle(color)
    }
}

struct MissionCard<Content: View>: View {
    private let content: Content

    init(@ViewBuilder content: () -> Content) {
        self.content = content()
    }

    var body: some View {
        content
            .padding(14)
            .frame(maxWidth: .infinity, alignment: .leading)
            .background(FleetTheme.card, in: RoundedRectangle(cornerRadius: 16, style: .continuous))
            .overlay(
                RoundedRectangle(cornerRadius: 16, style: .continuous)
                    .stroke(FleetTheme.border, lineWidth: 1)
            )
    }
}

@Observable
@MainActor
final class FleetAppModel {
    var selectedTab: FleetTab = .command
    var fleetSubtab: FleetSubtab = .overview
    var boardFilter: FleetRowFilter = .issues
    var agentFilter: AgentsView.StateFilter = .all

    // Fleet telemetry state
    var overview: FleetOverviewResponse?
    var morning: MorningResponse?
    var backups: BackupsResponse?
    var security: SecurityResponse?
    var mail: MailResponse?

    // Mission Control + AI Commander state
    var mcBoard: MCBoardResponse?
    var commanderState: CommanderState?
    var mcPrimaryURL: String
    var mcFallbackURL: String
    var mcTokenInput: String
    var isMCConfigured: Bool
    var mcActiveHost: String = ""
    var mcBoardError: String?
    var mcBoardUpdatedAt: Date?
    private var mcGeneration: Int = 0
    @ObservationIgnored private var mcBoardRequest: Task<Void, Never>?
    var mcErrorMessage: String?
    var isSendingCommander = false
    var isRefreshingDigest = false

    // Shared Hub / Refresh state
    var serverURL: String
    var tokenInput: String
    var isConfigured: Bool
    var isRefreshing = false
    var lastRefreshDate: Date?
    var errorMessage: String?
    var actionStatus: String?

    private let client = FleetAPIClient()
    private let mcClient = MissionControlAPIClient()

    var isAnyConfigured: Bool {
        isConfigured || isMCConfigured
    }

    var pendingRelaysCount: Int {
        (mcBoard?.relays ?? []).filter { $0.status == "pending" }.count
    }

    var mcBadgeCount: Int {
        (mcBoard?.counts.attention ?? 0) + pendingRelaysCount
    }

    func openBoard(filter: FleetRowFilter) {
        boardFilter = filter
        fleetSubtab = .board
        selectedTab = .fleet
    }

    func openControls() {
        selectedTab = .controls
    }

    init() {
        let creds = KeychainStore.shared.loadCredentials()
        self.serverURL = creds.serverURL
        self.tokenInput = creds.token
        self.isConfigured = creds.isConfigured

        let mcCreds = KeychainStore.shared.loadMCCredentials()
        self.mcPrimaryURL = mcCreds.primaryURL
        self.mcFallbackURL = mcCreds.fallbackURL
        self.mcTokenInput = mcCreds.token
        self.isMCConfigured = mcCreds.isConfigured

        self.overview = FleetStore.shared.loadOverview()
        self.morning = FleetStore.shared.loadMorning()
    }

    func reloadCredentialState() {
        let creds = KeychainStore.shared.loadCredentials()
        serverURL = creds.serverURL
        tokenInput = creds.token
        isConfigured = creds.isConfigured

        let mcCreds = KeychainStore.shared.loadMCCredentials()
        if mcPrimaryURL != mcCreds.primaryURL || mcFallbackURL != mcCreds.fallbackURL || mcTokenInput != mcCreds.token {
            mcGeneration += 1
            mcBoardRequest?.cancel()
            mcBoardRequest = nil
            mcBoard = nil
            mcBoardError = nil
            mcBoardUpdatedAt = nil
            mcActiveHost = ""
            commanderState = nil
            mcErrorMessage = nil
        }
        mcPrimaryURL = mcCreds.primaryURL
        mcFallbackURL = mcCreds.fallbackURL
        mcTokenInput = mcCreds.token
        isMCConfigured = mcCreds.isConfigured
    }

    func refreshAll() async {
        reloadCredentialState()
        guard isAnyConfigured else {
            errorMessage = nil
            mcErrorMessage = nil
            return
        }
        isRefreshing = true
        defer { isRefreshing = false }

        async let fleetTask = refreshFleet()
        async let mcTask = refreshMissionControl()
        let (fleetFailures, mcFailures) = await (fleetTask, mcTask)

        if (!isConfigured || fleetFailures.count < 5) && (!isMCConfigured || mcFailures.count < 2) {
            self.lastRefreshDate = Date()
        }
        self.errorMessage = fleetFailures.isEmpty ? nil : fleetFailures.joined(separator: "\n")
        self.mcErrorMessage = mcFailures.isEmpty ? nil : mcFailures.joined(separator: "\n")
    }

    private func refreshFleet() async -> [String] {
        guard isConfigured else { return [] }
        let fleetClient = client
        var failures: [String] = []

        async let overviewRes = Self.fetchResult { try await fleetClient.fetchOverview() }
        async let morningRes = Self.fetchResult { try await fleetClient.fetchMorning() }
        async let backupsRes = Self.fetchResult { try await fleetClient.fetchBackups() }
        async let securityRes = Self.fetchResult { try await fleetClient.fetchSecurity() }
        async let mailRes = Self.fetchResult { try await fleetClient.fetchMail() }

        let (ov, mo, bk, sec, ml) = await (overviewRes, morningRes, backupsRes, securityRes, mailRes)
        switch ov {
        case .success(let val):
            self.overview = val
            FleetStore.shared.saveOverview(val)
        case .failure(let err):
            failures.append("Overview: \(err.localizedDescription)")
        }
        switch mo {
        case .success(let val):
            self.morning = val
            FleetStore.shared.saveMorning(val)
        case .failure(let err):
            failures.append("Board: \(err.localizedDescription)")
        }
        switch bk {
        case .success(let val):
            self.backups = val
        case .failure(let err):
            failures.append("Backups: \(err.localizedDescription)")
        }
        switch sec {
        case .success(let val):
            self.security = val
        case .failure(let err):
            failures.append("Security: \(err.localizedDescription)")
        }
        switch ml {
        case .success(let val):
            self.mail = val
        case .failure(let err):
            failures.append("Mail: \(err.localizedDescription)")
        }
        return failures
    }

    func refreshAgentBoard() async {
        guard isMCConfigured else { return }
        if let existing = mcBoardRequest {
            await existing.value
            return
        }
        let generation = mcGeneration
        let request = Task { @MainActor in
            defer { if generation == mcGeneration { mcBoardRequest = nil } }
            do {
                let board = try await mcClient.fetchBoard()
                let host = await mcClient.activeHostURL
                guard generation == mcGeneration, !Task.isCancelled else { return }
                mcBoard = board
                mcActiveHost = host
                mcBoardUpdatedAt = Date()
                mcBoardError = nil
            } catch {
                guard generation == mcGeneration, !Task.isCancelled else { return }
                mcBoardError = error.localizedDescription
            }
        }
        mcBoardRequest = request
        await request.value
    }

    private func refreshMissionControl() async -> [String] {
        guard isMCConfigured else { return [] }
        let generation = mcGeneration
        let mc = mcClient
        async let board: Void = refreshAgentBoard()
        let cmd = await Self.fetchResult { try await mc.fetchCommanderState() }
        await board
        guard generation == mcGeneration else { return [] }
        var failures = mcBoardError.map { ["Agents: " + $0] } ?? []
        switch cmd {
        case .success(let val):
            self.commanderState = val.state
        case .failure(let err):
            failures.append("Commander: " + err.localizedDescription)
        }
        return failures
    }

    private nonisolated static func fetchResult<T: Sendable>(
        _ op: @Sendable () async throws -> T
    ) async -> Result<T, Error> {
        do {
            return .success(try await op())
        } catch {
            return .failure(error)
        }
    }

    func applyPairingURL(_ url: URL) async {
        do {
            let parsed = try PairingLink.parse(url)
            if parsed.hasFleetCredentials {
                try KeychainStore.shared.saveCredentials(serverURL: parsed.serverURL, token: parsed.token)
            }
            if parsed.hasMissionControlCredentials,
               let mcPrimary = parsed.mcPrimaryURL,
               let mcToken = parsed.mcToken {
                try KeychainStore.shared.saveMCCredentials(
                    primaryURL: mcPrimary,
                    fallbackURL: parsed.mcFallbackURL ?? "",
                    token: mcToken
                )
            }
            reloadCredentialState()
            selectedTab = .command
            actionStatus = "Paired unified hubs."
            errorMessage = nil
            mcErrorMessage = nil
            await refreshAll()
        } catch {
            errorMessage = error.localizedDescription
            selectedTab = .controls
        }
    }

    // MARK: - Credentials & Testing

    func saveCredentials(serverURL: String, token: String) throws {
        try KeychainStore.shared.saveCredentials(serverURL: serverURL, token: token)
        reloadCredentialState()
    }

    func clearCredentials() {
        KeychainStore.shared.clearToken()
        reloadCredentialState()
    }

    func testConnection(serverURL: String, token: String) async throws -> HealthResponse {
        try await client.testConnection(serverURL: serverURL, token: token)
    }

    func saveMCCredentials(primaryURL: String, fallbackURL: String, token: String) throws {
        try KeychainStore.shared.saveMCCredentials(primaryURL: primaryURL, fallbackURL: fallbackURL, token: token)
        reloadCredentialState()
    }

    func clearMCCredentials() {
        KeychainStore.shared.clearMCToken()
        reloadCredentialState()
    }

    func testMCConnection(primaryURL: String, fallbackURL: String, token: String) async throws -> MCHealthResponse {
        let res = try await mcClient.testConnection(primaryURL: primaryURL, fallbackURL: fallbackURL, token: token)
        let active = await mcClient.activeHostURL
        if !active.isEmpty {
            mcActiveHost = active
        }
        return res
    }

    // MARK: - Fleet Actions

    func sendMail(to: String, text: String) async throws -> ActionResultResponse {
        let res = try await client.sendMail(to: to, text: text)
        await refreshAll()
        return res
    }

    func sendVoiceNote(to: String?, text: String) async throws -> ActionResultResponse {
        let res = try await client.sendVoiceNote(to: to, text: text)
        await refreshAll()
        return res
    }

    func markAllMailRead() async throws -> ActionResultResponse {
        let res = try await client.ackAllMail()
        await refreshAll()
        return res
    }

    func markMailRead(id: String) async throws -> ActionResultResponse {
        let res = try await client.ackMail(ids: [id])
        await refreshAll()
        return res
    }

    func triggerMorningRefresh() async throws -> ActionResultResponse {
        try await authenticateOwner(reason: "Confirm running the fleet morning audit refresh")
        return try await client.triggerMorningRefresh()
    }

    func setProviderPaused(provider: String, paused: Bool) async throws -> ActionResultResponse {
        try await authenticateOwner(reason: "Confirm \(paused ? "pausing" : "resuming") \(provider)")
        let res = try await client.setProviderPaused(provider: provider, paused: paused)
        await refreshAll()
        return res
    }

    func fetchJobLog(host: String, label: String) async throws -> JobLogResponse {
        try await client.fetchJobLog(host: host, label: label)
    }

    func kickstartJob(host: String, label: String) async throws -> ActionResultResponse {
        try await authenticateOwner(reason: "Confirm running \(shortJobName(label)) on \(host.uppercased())")
        let res = try await client.kickstartJob(host: host, label: label)
        await refreshAll()
        return res
    }

    func runHostAction(host: String, kind: String) async throws -> ActionResultResponse {
        try await authenticateOwner(reason: "Confirm running \(kind) on \(host.uppercased())")
        let res = try await client.runHostAction(host: host, kind: kind)
        await refreshAll()
        return res
    }

    func snoozeItem(key: String, hours: Int, note: String? = nil) async throws -> ActionResultResponse {
        let res = try await client.snooze(key: key, hours: hours, note: note)
        await refreshAll()
        return res
    }

    func unsnoozeItem(key: String) async throws -> ActionResultResponse {
        let res = try await client.unsnooze(key: key)
        await refreshAll()
        return res
    }

    func dispatchFix(to: String, key: String? = nil, title: String? = nil, prompt: String) async throws -> ActionResultResponse {
        let res = try await client.dispatchFix(to: to, key: key, title: title, prompt: prompt)
        await refreshAll()
        return res
    }

    // MARK: - Mission Control Agents & Relays

    func fetchThreadDetail(id: String, host: String? = nil) async throws -> MCConversation {
        let res = try await mcClient.fetchThreadDetail(id: id, host: host)
        let active = await mcClient.activeHostURL
        if !active.isEmpty { mcActiveHost = active }
        return res.detail
    }

    func sendAgentMessage(conversationId: String, text: String, host: String? = nil) async throws -> MCActionResponse {
        let res = try await mcClient.sendMessage(conversationId: conversationId, text: text, host: host)
        await refreshAll()
        return res
    }

    func focusAgentThread(conversationId: String, host: String? = nil) async throws -> MCActionResponse {
        try await mcClient.focusThread(conversationId: conversationId, host: host)
    }

    func stopAgentThread(conversationId: String, title: String, host: String? = nil) async throws -> MCActionResponse {
        try await authenticateOwner(reason: "Confirm stopping \(title)")
        let res = try await mcClient.stopThread(conversationId: conversationId, host: host)
        await refreshAll()
        return res
    }

    func startNewAgentThread(text: String, projectName: String? = nil, host: String? = nil) async throws -> MCActionResponse {
        let res = try await mcClient.startNewThread(text: text, projectName: projectName, host: host)
        await refreshAll()
        return res
    }

    func refreshAgentBrief(conversationId: String, host: String? = nil) async throws -> MCActionResponse {
        let res = try await mcClient.refreshBrief(conversationId: conversationId, host: host)
        await refreshAll()
        return res
    }

    func setAgentAutoWake(conversationId: String, enabled: Bool?, host: String? = nil) async throws -> MCActionResponse {
        let res = try await mcClient.setAutoWake(conversationId: conversationId, enabled: enabled, host: host)
        await refreshAll()
        return res
    }

    func approveRelay(relayId: String, editedText: String? = nil, host: String? = nil) async throws -> MCActionResponse {
        let res = try await mcClient.approveRelay(relayId: relayId, editedText: editedText, host: host)
        await refreshAll()
        return res
    }

    func dismissRelay(relayId: String, host: String? = nil) async throws -> MCActionResponse {
        let res = try await mcClient.dismissRelay(relayId: relayId, host: host)
        await refreshAll()
        return res
    }

    // MARK: - AI Commander

    func refreshCommanderDigest(force: Bool = true) async {
        guard isMCConfigured, !isRefreshingDigest else { return }
        isRefreshingDigest = true
        defer { isRefreshingDigest = false }
        do {
            let res = try await mcClient.fetchCommanderDigest(refresh: force)
            if let st = res.state {
                commanderState = st
            } else if let d = res.digest {
                var cur = commanderState ?? CommanderState()
                cur.digest = d
                commanderState = cur
            }
            mcErrorMessage = res.error
        } catch {
            mcErrorMessage = error.localizedDescription
        }
    }

    @discardableResult
    func sendCommanderMessage(_ text: String) async -> Bool {
        let clean = text.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !clean.isEmpty, isMCConfigured, !isSendingCommander else { return false }
        isSendingCommander = true
        defer { isSendingCommander = false }
        let prefs = commanderState?.preferences ?? CommanderPreferences()
        do {
            let res = try await mcClient.sendCommanderMessage(
                text: clean,
                routingMode: prefs.routingMode,
                directDispatch: prefs.directDispatch
            )
            if let st = res.state {
                commanderState = st
            }
            if let err = res.error, !err.isEmpty {
                mcErrorMessage = err
            } else if !res.ok {
                mcErrorMessage = "The hub did not confirm delivery."
            } else {
                mcErrorMessage = nil
            }
            let active = await mcClient.activeHostURL
            if !active.isEmpty { mcActiveHost = active }
            if let board = try? await mcClient.fetchBoard() {
                mcBoard = board
            }
            return res.ok && (res.error ?? "").isEmpty
        } catch {
            mcErrorMessage = error.localizedDescription
            return false
        }
    }

    func executeCommanderAction(_ action: CommanderAction, editedText: String? = nil) async throws -> CommanderActionExecuteResponse {
        let highRiskKinds: Set<String> = ["fleet_job_kickstart", "fleet_provider_pause", "fleet_morning_refresh", "stop_agent"]
        if action.risk == .high || highRiskKinds.contains(action.kind) {
            try await authenticateOwner(reason: "Confirm \(action.label)")
        }
        let res = try await mcClient.executeCommanderAction(actionId: action.id, editedText: editedText)
        if let st = res.state {
            commanderState = st
        }
        await refreshAll()
        return res
    }

    func dismissCommanderAction(_ action: CommanderAction) async throws -> CommanderActionExecuteResponse {
        let res = try await mcClient.dismissCommanderAction(actionId: action.id)
        if let st = res.state {
            commanderState = st
        }
        return res
    }

    func updateCommanderSettings(routingMode: CommanderRoutingMode? = nil, directDispatch: Bool? = nil) async {
        guard isMCConfigured else { return }
        do {
            let res = try await mcClient.updateCommanderSettings(routingMode: routingMode, directDispatch: directDispatch)
            commanderState = res.state
            mcErrorMessage = nil
        } catch {
            mcErrorMessage = error.localizedDescription
        }
    }

    func clearCommanderChat() async {
        guard isMCConfigured else { return }
        do {
            let res = try await mcClient.clearCommanderChat()
            commanderState = res.state
            mcErrorMessage = nil
        } catch {
            mcErrorMessage = error.localizedDescription
        }
    }

    func authenticateOwner(reason: String, context: LAContext = LAContext()) async throws {
        let ok = try await context.evaluatePolicy(.deviceOwnerAuthentication, localizedReason: reason)
        if !ok {
            throw AuthGateError.cancelled
        }
    }
}

enum AuthGateError: LocalizedError {
    case cancelled

    var errorDescription: String? {
        "Authentication cancelled."
    }
}

struct FleetContainerView: View {
    @Bindable var model: FleetAppModel

    var body: some View {
        switch model.fleetSubtab {
        case .overview: OverviewView(model: model)
        case .board: BoardView(model: model)
        case .services: ServicesView(model: model)
        }
    }
}

@main
struct FleetApp: App {
    @Environment(\.scenePhase) private var scenePhase
    @State private var model = FleetAppModel()

    var body: some Scene {
        WindowGroup {
            TabView(selection: $model.selectedTab) {
                CommanderView(model: model)
                    .tabItem {
                        Label("Command", systemImage: "sparkles")
                    }
                    .tag(FleetTab.command)

                AgentsView(model: model)
                    .tabItem {
                        Label("Agents", systemImage: "cpu")
                    }
                    .badge(model.mcBadgeCount)
                    .tag(FleetTab.agents)

                FleetContainerView(model: model)
                    .tabItem {
                        Label("Fleet", systemImage: "gauge.with.dots.needle.33percent")
                    }
                    .tag(FleetTab.fleet)

                InboxView(model: model)
                    .tabItem {
                        Label("Inbox", systemImage: "tray.full")
                    }
                    .tag(FleetTab.inbox)

                ControlsView(model: model)
                    .tabItem {
                        Label("Controls", systemImage: "slider.horizontal.3")
                    }
                    .tag(FleetTab.controls)
            }
            .tint(FleetTheme.pass)
            .task {
                if model.isAnyConfigured {
                    await model.refreshAll()
                    if model.commanderState?.digest == nil && model.isMCConfigured {
                        await model.refreshCommanderDigest(force: false)
                    }
                }
            }
            .task(id: scenePhase) {
                guard scenePhase == .active else { return }
                while !Task.isCancelled {
                    if model.isMCConfigured && (model.mcBoardUpdatedAt?.timeIntervalSinceNow ?? -15) <= -5 {
                        await model.refreshAgentBoard()
                    }
                    do { try await Task.sleep(for: .seconds(8)) }
                    catch { return }
                }
            }
            .onOpenURL { url in
                if url.scheme == "fleet", ["stack", "services"].contains(url.host ?? "") {
                    model.selectedTab = .fleet
                    model.fleetSubtab = .services
                    return
                }
                Task {
                    await model.applyPairingURL(url)
                }
            }
        }
    }
}

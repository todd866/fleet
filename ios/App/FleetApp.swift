import LocalAuthentication
import Observation
import SwiftUI

enum FleetTab: Hashable {
    case overview
    case board
    case inbox
    case controls
}

enum FleetTheme {
    static let bg = Color(red: 0.039, green: 0.051, blue: 0.071)
    static let card = Color(red: 0.075, green: 0.098, blue: 0.137)
    static let elevated = Color(red: 0.106, green: 0.137, blue: 0.192)
    static let border = Color.white.opacity(0.08)

    static let pass = Color(red: 0.133, green: 0.773, blue: 0.369)
    static let warn = Color(red: 0.961, green: 0.620, blue: 0.043)
    static let fail = Color(red: 0.937, green: 0.267, blue: 0.267)
    static let info = Color(red: 0.220, green: 0.741, blue: 0.973)

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
    var selectedTab: FleetTab = .overview
    var boardFilter: FleetRowFilter = .issues
    var overview: FleetOverviewResponse?
    var morning: MorningResponse?
    var backups: BackupsResponse?
    var security: SecurityResponse?
    var mail: MailResponse?

    var serverURL: String
    var tokenInput: String
    var isConfigured: Bool
    var isRefreshing = false
    var lastRefreshDate: Date?
    var errorMessage: String?
    var actionStatus: String?

    private let client = FleetAPIClient()

    func openBoard(filter: FleetRowFilter) {
        boardFilter = filter
        selectedTab = .board
    }

    func openControls() {
        selectedTab = .controls
    }

    init() {
        let creds = KeychainStore.shared.loadCredentials()
        self.serverURL = creds.serverURL
        self.tokenInput = creds.token
        self.isConfigured = creds.isConfigured
        self.overview = FleetStore.shared.loadOverview()
        self.morning = FleetStore.shared.loadMorning()
    }

    func reloadCredentialState() {
        let creds = KeychainStore.shared.loadCredentials()
        serverURL = creds.serverURL
        tokenInput = creds.token
        isConfigured = creds.isConfigured
    }

    func refreshAll() async {
        reloadCredentialState()
        guard isConfigured else {
            errorMessage = nil
            return
        }
        isRefreshing = true
        defer { isRefreshing = false }

        async let overviewRes = fetchResult { try await self.client.fetchOverview() }
        async let morningRes = fetchResult { try await self.client.fetchMorning() }
        async let backupsRes = fetchResult { try await self.client.fetchBackups() }
        async let securityRes = fetchResult { try await self.client.fetchSecurity() }
        async let mailRes = fetchResult { try await self.client.fetchMail() }

        let (ov, mo, bk, sec, ml) = await (overviewRes, morningRes, backupsRes, securityRes, mailRes)
        var failures: [String] = []

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

        if failures.count < 5 {
            self.lastRefreshDate = Date()
        }
        self.errorMessage = failures.isEmpty ? nil : failures.joined(separator: "\n")
    }

    private func fetchResult<T: Sendable>(_ op: () async throws -> T) async -> Result<T, Error> {
        do {
            return .success(try await op())
        } catch {
            return .failure(error)
        }
    }

    func applyPairingURL(_ url: URL) async {
        do {
            let parsed = try PairingLink.parse(url)
            try KeychainStore.shared.saveCredentials(serverURL: parsed.serverURL, token: parsed.token)
            reloadCredentialState()
            selectedTab = .overview
            actionStatus = "Paired with hub."
            errorMessage = nil
            await refreshAll()
        } catch {
            errorMessage = error.localizedDescription
            selectedTab = .controls
        }
    }

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

    private func authenticateOwner(reason: String) async throws {
        let context = LAContext()
        var authError: NSError?
        let policy: LAPolicy = context.canEvaluatePolicy(.deviceOwnerAuthentication, error: &authError)
            ? .deviceOwnerAuthentication
            : .deviceOwnerAuthenticationWithBiometrics
        guard context.canEvaluatePolicy(policy, error: &authError) else {
            return
        }
        let ok = try await context.evaluatePolicy(policy, localizedReason: reason)
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

@main
struct FleetApp: App {
    @State private var model = FleetAppModel()

    var body: some Scene {
        WindowGroup {
            TabView(selection: $model.selectedTab) {
                OverviewView(model: model)
                    .tabItem {
                        Label("Overview", systemImage: "gauge.with.dots.needle.33percent")
                    }
                    .tag(FleetTab.overview)

                BoardView(model: model)
                    .tabItem {
                        Label("Board", systemImage: "checklist")
                    }
                    .tag(FleetTab.board)

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
            .preferredColorScheme(.dark)
            .task {
                if model.isConfigured {
                    await model.refreshAll()
                }
            }
            .onOpenURL { url in
                Task {
                    await model.applyPairingURL(url)
                }
            }
        }
    }
}

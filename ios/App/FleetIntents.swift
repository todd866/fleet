import AppIntents
import Foundation

struct SendFleetNoteIntent: AppIntent {
    static let title: LocalizedStringResource = "Send Fleet Voice Note"
    static let description = IntentDescription("Records a quick voice or text note in Fleet's inbox and optionally dispatches it to a host.")

    @Parameter(title: "Note")
    var text: String

    @Parameter(title: "Target (optional)")
    var target: String?

    init() {}

    init(text: String, target: String? = nil) {
        self.text = text
        self.target = target
    }

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let client = FleetAPIClient()
        let res = try await client.sendVoiceNote(to: target, text: text)
        if res.ok {
            return .result(dialog: "Saved Fleet note.")
        }
        return .result(dialog: "Fleet returned: \(res.error ?? "unknown error")")
    }
}

struct CheckFleetStatusIntent: AppIntent {
    static let title: LocalizedStringResource = "Check Fleet Status"
    static let description = IntentDescription("Fetches the latest pass/warn/fail scoreboard and security state from your Fleet hub.")

    init() {}

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let client = FleetAPIClient()
        let overview = try await client.fetchOverview()
        FleetStore.shared.saveOverview(overview)
        let summary = "Fleet is \(overview.counts.pass) passing, \(overview.counts.warn) warning, \(overview.counts.fail) failing. Security is \(overview.security.status), \(overview.unreadMail) unread mail."
        return .result(dialog: IntentDialog(stringLiteral: summary))
    }
}

struct AskCommanderIntent: AppIntent {
    static let title: LocalizedStringResource = "Ask Fleet Command"
    static let description = IntentDescription("Send a question or directive to Fleet AI Commander across your connected Macs.")

    @Parameter(title: "Directive")
    var prompt: String

    init() {}

    init(prompt: String) {
        self.prompt = prompt
    }

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let client = MissionControlAPIClient()
        let res = try await client.sendCommanderMessage(text: prompt)
        let reply = stripTrailingEllipsis(res.assistantMessage?.text ?? res.error ?? "Command dispatched.")
        return .result(dialog: IntentDialog(stringLiteral: reply))
    }
}

struct FleetShortcuts: AppShortcutsProvider {
    static var appShortcuts: [AppShortcut] {
        AppShortcut(
            intent: CheckFleetStatusIntent(),
            phrases: [
                "Check \(.applicationName) status",
                "How is \(.applicationName)"
            ],
            shortTitle: "Fleet Status",
            systemImageName: "gauge.with.dots.needle.33percent"
        )
        AppShortcut(
            intent: SendFleetNoteIntent(),
            phrases: [
                "Send note to \(.applicationName)",
                "Log voice note in \(.applicationName)"
            ],
            shortTitle: "Fleet Note",
            systemImageName: "mic.badge.plus"
        )
        AppShortcut(
            intent: AskCommanderIntent(),
            phrases: [
                "Ask \(.applicationName) Command",
                "Command \(.applicationName)"
            ],
            shortTitle: "Ask Command",
            systemImageName: "sparkles"
        )
    }
}

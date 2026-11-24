import Foundation

// MARK: - JSON parameter value (Sendable)

public enum JSONParamValue: Codable, Sendable, Hashable {
    case string(String)
    case bool(Bool)
    case int(Int)
    case double(Double)
    case null

    public init(from decoder: Decoder) throws {
        let container = try decoder.singleValueContainer()
        if container.decodeNil() {
            self = .null
        } else if let b = try? container.decode(Bool.self) {
            self = .bool(b)
        } else if let i = try? container.decode(Int.self) {
            self = .int(i)
        } else if let d = try? container.decode(Double.self) {
            self = .double(d)
        } else if let s = try? container.decode(String.self) {
            self = .string(s)
        } else {
            self = .null
        }
    }

    public func encode(to encoder: Encoder) throws {
        var container = encoder.singleValueContainer()
        switch self {
        case .string(let s): try container.encode(s)
        case .bool(let b): try container.encode(b)
        case .int(let i): try container.encode(i)
        case .double(let d): try container.encode(d)
        case .null: try container.encodeNil()
        }
    }

    public var stringValue: String? {
        switch self {
        case .string(let s): return s
        case .bool(let b): return b ? "true" : "false"
        case .int(let i): return String(i)
        case .double(let d): return String(d)
        case .null: return nil
        }
    }
}

// MARK: - Mission Control Models

public enum MCThreadState: String, Codable, Sendable, CaseIterable {
    case attention
    case working
    case bg
    case unread
    case idle
    case unknown

    public init(from decoder: Decoder) throws {
        let raw = (try? decoder.singleValueContainer().decode(String.self)) ?? ""
        self = MCThreadState(rawValue: raw) ?? .unknown
    }

    public var displayLabel: String {
        switch self {
        case .attention: return "Wait"
        case .working: return "Work"
        case .bg: return "Bg"
        case .unread: return "Unread"
        case .idle: return "Idle"
        case .unknown: return "Other"
        }
    }

    public var sfSymbol: String {
        switch self {
        case .attention: return "exclamationmark.triangle.fill"
        case .working: return "arrow.trianglehead.2.clockwise"
        case .bg: return "gearshape.fill"
        case .unread: return "diamond.fill"
        case .idle: return "circle"
        case .unknown: return "questionmark.circle"
        }
    }
}

public struct MCBoardCounts: Codable, Sendable, Hashable {
    public var total: Int
    public var attention: Int
    public var working: Int
    public var bg: Int
    public var unread: Int
    public var idle: Int
    public var hidden: Int
    public var pendingRelays: Int?
}

public struct MCHostInfo: Codable, Sendable, Hashable, Identifiable {
    public var id: String { name }
    public var name: String
    public var isSelf: Bool?
    public var online: Bool
    public var macAccessibility: Bool?
    public var conversationCount: Int?
    public var error: String?
}

public struct MCProjectInfo: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var name: String
    public var host: String?
}

public struct MCNextAction: Codable, Sendable, Hashable, Identifiable {
    public var id: String { "\(label)|\(prompt)" }
    public var label: String
    public var prompt: String
}

public struct MCBriefing: Codable, Sendable, Hashable {
    public var headline: String
    public var brief: String
    public var currentWork: String
    public var priorCompleted: String
    public var needsUser: String
    public var phase: String?
    public var health: String?
    public var bullets: [String]?
    public var nextActions: [MCNextAction]
    public var updatedAt: Double?
    public var model: String?
}

public struct MCStepItem: Codable, Sendable, Hashable {
    public var title: String
    public var status: String
}

public struct MCStepsInfo: Codable, Sendable, Hashable {
    public var items: [MCStepItem]
    public var done: Int
    public var total: Int
    public var currentTitle: String?
}

public struct MCEvent: Codable, Sendable, Hashable, Identifiable {
    public var id: String { "\(createdAt)|\(kind)|\(text.prefix(24))" }
    public var kind: String
    public var status: String
    public var createdAt: String
    public var text: String
}

public struct MCConversation: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var shortId: String
    public var title: String
    public var host: String?
    public var projectId: String
    public var projectName: String
    public var stepCount: Int
    public var state: MCThreadState
    public var waitingReason: String
    public var unread: Bool
    public var stalled: Bool
    public var ageMs: Double
    public var lastModifiedMs: Double
    public var lastUserInputMs: Double
    public var autoWake: Bool?
    public var focus: String
    public var focusIsCurrentTurn: Bool?
    public var steps: MCStepsInfo?
    public var lastPrompt: String?
    public var lastReply: String
    public var lastReplyFull: String?
    public var briefing: MCBriefing?
    public var briefingStale: Bool?
    public var recentEvents: [MCEvent]?
    public var runningBgCount: Int?
    public var subagentCount: Int?
    public var model: String?
    public var pinned: Bool
    public var hidden: Bool
    public var slot: Int

    public var hostBadge: String {
        (host ?? "").trimmingCharacters(in: .whitespacesAndNewlines).uppercased()
    }

    public var headlineClean: String {
        let candidates = [waitingReason, briefing?.headline ?? "", focus, lastReply]
        for raw in candidates {
            let trimmed = raw.trimmingCharacters(in: .whitespacesAndNewlines)
            if !trimmed.isEmpty {
                return stripTrailingEllipsis(trimmed)
            }
        }
        return "Ready"
    }
}

public struct MCRelayItem: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var fromConversationId: String?
    public var fromLabel: String
    public var targetConversationId: String
    public var targetTitle: String
    public var targetHost: String
    public var message: String
    public var hop: Int
    public var status: String
    public var autoDelivered: Bool?
    public var reason: String
    public var createdAt: Double
    public var resolvedAt: Double?
    public var error: String?
}

public struct MCLastAction: Codable, Sendable, Hashable {
    public var ok: Bool
    public var conversationId: String?
    public var title: String?
    public var message: String?
    public var error: String?
    public var at: Double?
}

public struct MCBoardResponse: Codable, Sendable, Hashable {
    public var generatedAt: Double
    public var sortMode: String
    public var counts: MCBoardCounts
    public var conversations: [MCConversation]
    public var hosts: [MCHostInfo]?
    public var projects: [MCProjectInfo]?
    public var relays: [MCRelayItem]?
    public var host: String?
    public var macAccessibility: Bool?
    public var lastAction: MCLastAction?
}

public struct MCThreadDetailResponse: Codable, Sendable {
    public var detail: MCConversation
}

public struct MCActionResponse: Codable, Sendable {
    public var ok: Bool?
    public var title: String?
    public var message: String?
    public var error: String?
    public var errorCode: String?
    public var autoWake: Bool?
    public var relay: MCRelayItem?
}

public struct MCHealthResponse: Codable, Sendable {
    public var ok: Bool
    public var service: String?
    public var host: String?
}

// MARK: - AI Commander Models

public enum CommanderRoutingMode: String, Codable, Sendable, CaseIterable, Identifiable {
    case auto
    case fast
    case deep

    public var id: String { rawValue }

    public var displayLabel: String {
        switch self {
        case .auto: return "Auto"
        case .fast: return "Fast"
        case .deep: return "Deep"
        }
    }
}

public enum CommanderActionRisk: String, Codable, Sendable {
    case low
    case medium
    case high
}

public enum CommanderActionStatus: String, Codable, Sendable {
    case pending
    case executed
    case dismissed
    case failed
}

public struct CommanderPreferences: Codable, Sendable, Hashable {
    public var routingMode: CommanderRoutingMode
    public var directDispatch: Bool

    public init(routingMode: CommanderRoutingMode = .auto, directDispatch: Bool = false) {
        self.routingMode = routingMode
        self.directDispatch = directDispatch
    }
}

public struct CommanderDigest: Codable, Sendable, Hashable {
    public var status: String?
    public var headline: String
    public var items: [String]
    public var starterPrompts: [String]
    public var generatedAt: Double
}

public struct CommanderAction: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var kind: String
    public var label: String
    public var summary: String
    public var explicitlyRequested: Bool
    public var risk: CommanderActionRisk
    public var params: [String: JSONParamValue]
    public var status: CommanderActionStatus
    public var resultMessage: String?
    public var executedAt: Double?

    public var defaultEditableText: String? {
        params["text"]?.stringValue ?? params["message"]?.stringValue ?? params["note"]?.stringValue
    }

    public var isEditableKind: Bool {
        ["send_to_agent", "relay_to_agent", "start_new_agent", "approve_relay", "fleet_mail", "fleet_snooze"].contains(kind)
    }
}

public struct CommanderMessage: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var role: String
    public var text: String
    public var createdAt: Double
    public var model: String?
    public var modelTier: String?
    public var routingReason: String?
    public var inspectedCount: Int?
    public var actions: [CommanderAction]?
    public var followUps: [String]?
}

public struct CommanderState: Codable, Sendable, Hashable {
    public var version: Int
    public var preferences: CommanderPreferences
    public var digest: CommanderDigest?
    public var messages: [CommanderMessage]

    public init(
        version: Int = 1,
        preferences: CommanderPreferences = CommanderPreferences(),
        digest: CommanderDigest? = nil,
        messages: [CommanderMessage] = []
    ) {
        self.version = version
        self.preferences = preferences
        self.digest = digest
        self.messages = messages
    }
}

public struct CommanderStateResponse: Codable, Sendable {
    public var ok: Bool
    public var state: CommanderState
}

public struct CommanderDigestResponse: Codable, Sendable {
    public var ok: Bool
    public var digest: CommanderDigest?
    public var state: CommanderState?
    public var error: String?
}

public struct CommanderChatResponse: Codable, Sendable {
    public var ok: Bool
    public var userMessage: CommanderMessage?
    public var assistantMessage: CommanderMessage?
    public var state: CommanderState?
    public var error: String?
}

public struct CommanderActionExecuteResponse: Codable, Sendable {
    public var ok: Bool
    public var message: String?
    public var error: String?
    public var action: CommanderAction?
    public var state: CommanderState?
}

public func stripTrailingEllipsis(_ text: String) -> String {
    text
        .replacingOccurrences(of: "\u{2026}", with: "")
        .replacingOccurrences(of: "...", with: "")
        .trimmingCharacters(in: .whitespacesAndNewlines)
}

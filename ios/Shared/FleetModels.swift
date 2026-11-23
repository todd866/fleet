import Foundation

// MARK: - Flexible decoding helpers

private extension KeyedDecodingContainer {
    func decodeFlexibleString(forKey key: Key) -> String? {
        if let str = try? decodeIfPresent(String.self, forKey: key) {
            return str
        }
        if let intVal = try? decodeIfPresent(Int.self, forKey: key) {
            return String(intVal)
        }
        if let dblVal = try? decodeIfPresent(Double.self, forKey: key) {
            if dblVal.rounded() == dblVal {
                return String(Int(dblVal))
            }
            return String(dblVal)
        }
        if let boolVal = try? decodeIfPresent(Bool.self, forKey: key) {
            return boolVal ? "true" : "false"
        }
        return nil
    }

    func decodeFlexibleInt(forKey key: Key) -> Int? {
        if let intVal = try? decodeIfPresent(Int.self, forKey: key) {
            return intVal
        }
        if let dblVal = try? decodeIfPresent(Double.self, forKey: key) {
            return Int(dblVal)
        }
        if let str = try? decodeIfPresent(String.self, forKey: key) {
            return Int(str.trimmingCharacters(in: .whitespacesAndNewlines))
        }
        return nil
    }

    func decodeFlexibleDouble(forKey key: Key) -> Double? {
        if let dblVal = try? decodeIfPresent(Double.self, forKey: key) {
            return dblVal
        }
        if let intVal = try? decodeIfPresent(Int.self, forKey: key) {
            return Double(intVal)
        }
        if let str = try? decodeIfPresent(String.self, forKey: key) {
            return Double(str.trimmingCharacters(in: .whitespacesAndNewlines))
        }
        return nil
    }
}

// MARK: - Core summary types

public struct CountsSummary: Codable, Sendable, Hashable {
    public var pass: Int
    public var warn: Int
    public var fail: Int

    public init(pass: Int = 0, warn: Int = 0, fail: Int = 0) {
        self.pass = pass
        self.warn = warn
        self.fail = fail
    }

    public var total: Int { pass + warn + fail }
}

public struct CadenceInfo: Codable, Sendable, Hashable {
    public var text: String?
    public var next: Int?
    public var every: String?
}

public struct FailingJob: Codable, Sendable, Hashable, Identifiable {
    public var label: String
    public var loaded: Bool?
    public var exit: Int?
    public var when: String?
    public var state: String?

    public var id: String { "\(label)|\(state ?? "")|\(when ?? "")" }

    enum CodingKeys: String, CodingKey {
        case label, loaded, exit, when, state
    }

    public init(label: String, loaded: Bool? = nil, exit: Int? = nil, when: String? = nil, state: String? = nil) {
        self.label = label
        self.loaded = loaded
        self.exit = exit
        self.when = when
        self.state = state
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        label = (try? c.decode(String.self, forKey: .label)) ?? "unknown"
        loaded = try? c.decodeIfPresent(Bool.self, forKey: .loaded)
        exit = c.decodeFlexibleInt(forKey: .exit)
        when = c.decodeFlexibleString(forKey: .when)
        state = try? c.decodeIfPresent(String.self, forKey: .state)
    }
}

public struct SyncthingStatus: Codable, Sendable, Hashable {
    public var errors: Int?
    public var outOfSync: Int?
    public var down: Bool?

    enum CodingKeys: String, CodingKey {
        case errors
        case outOfSync = "out_of_sync"
        case down
    }
}

public struct GlancebarStatus: Codable, Sendable, Hashable {
    public var state: String?
    public var behind: Int?
}

public struct FreeSpaceGuardStatus: Codable, Sendable, Hashable {
    public var state: String?
}

public struct DriveInfo: Codable, Sendable, Hashable, Identifiable {
    public var name: String
    public var mounted: Bool?
    public var freeGB: Double?
    public var totalGB: Double?
    public var trend24hGB: Double?
    public var alert: String?

    public var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name, mounted, alert
        case freeGB = "free_gb"
        case totalGB = "total_gb"
        case trend24hGB = "trend_24h_gb"
    }

    public var usedFraction: Double {
        guard let total = totalGB, total > 0, let free = freeGB else { return 0 }
        return min(max((total - free) / total, 0), 1)
    }
}

public struct HostSample: Codable, Sendable, Hashable, Identifiable {
    public var t: Int
    public var chipC: Double?
    public var cpu: Double?
    public var diskFreeGB: Double?

    public var id: Int { t }

    enum CodingKeys: String, CodingKey {
        case t, cpu
        case chipC = "chip_c"
        case diskFreeGB = "disk_free_gb"
    }
}

public struct HostSummary: Codable, Sendable, Hashable, Identifiable {
    public var id: String
    public var collectedAt: Int?
    public var ok: Bool?
    public var ownerActive: Bool?
    public var chipC: Double?
    public var thermal: String?
    public var cpuPct: Double?
    public var memPressure: String?
    public var memUsedGB: Double?
    public var memTotalGB: Double?
    public var load1m: Double?
    public var battPct: Int?
    public var ac: Bool?
    public var diskFreeGB: Double?
    public var diskTotalGB: Double?
    public var diskTargetGB: Double?
    public var diskFloorGB: Double?
    public var syncthing: SyncthingStatus?
    public var glancebar: GlancebarStatus?
    public var freeSpaceGuard: FreeSpaceGuardStatus?
    public var unreadMail: Int?
    public var failingJobsCount: Int?
    public var failingJobs: [FailingJob]?
    public var failCount: Int?
    public var warnCount: Int?
    public var drives: [DriveInfo]?
    public var samples: [HostSample]?

    enum CodingKeys: String, CodingKey {
        case id, ok, thermal, ac, syncthing, glancebar, drives, samples
        case collectedAt = "collected_at"
        case ownerActive = "owner_active"
        case chipC = "chip_c"
        case cpuPct = "cpu_pct"
        case memPressure = "mem_pressure"
        case memUsedGB = "mem_used_gb"
        case memTotalGB = "mem_total_gb"
        case load1m = "load_1m"
        case battPct = "batt_pct"
        case diskFreeGB = "disk_free_gb"
        case diskTotalGB = "disk_total_gb"
        case diskTargetGB = "disk_target_gb"
        case diskFloorGB = "disk_floor_gb"
        case freeSpaceGuard = "free_space_guard"
        case unreadMail = "unread_mail"
        case failingJobsCount = "failing_jobs_count"
        case failingJobs = "failing_jobs"
        case failCount = "fail_count"
        case warnCount = "warn_count"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        id = (try? c.decode(String.self, forKey: .id)) ?? "unknown"
        collectedAt = c.decodeFlexibleInt(forKey: .collectedAt)
        ok = try? c.decodeIfPresent(Bool.self, forKey: .ok)
        ownerActive = try? c.decodeIfPresent(Bool.self, forKey: .ownerActive)
        chipC = c.decodeFlexibleDouble(forKey: .chipC)
        thermal = c.decodeFlexibleString(forKey: .thermal)
        cpuPct = c.decodeFlexibleDouble(forKey: .cpuPct)
        memPressure = c.decodeFlexibleString(forKey: .memPressure)
        memUsedGB = c.decodeFlexibleDouble(forKey: .memUsedGB)
        memTotalGB = c.decodeFlexibleDouble(forKey: .memTotalGB)
        load1m = c.decodeFlexibleDouble(forKey: .load1m)
        battPct = c.decodeFlexibleInt(forKey: .battPct)
        ac = try? c.decodeIfPresent(Bool.self, forKey: .ac)
        diskFreeGB = c.decodeFlexibleDouble(forKey: .diskFreeGB)
        diskTotalGB = c.decodeFlexibleDouble(forKey: .diskTotalGB)
        diskTargetGB = c.decodeFlexibleDouble(forKey: .diskTargetGB)
        diskFloorGB = c.decodeFlexibleDouble(forKey: .diskFloorGB)
        syncthing = try? c.decodeIfPresent(SyncthingStatus.self, forKey: .syncthing)
        glancebar = try? c.decodeIfPresent(GlancebarStatus.self, forKey: .glancebar)
        freeSpaceGuard = try? c.decodeIfPresent(FreeSpaceGuardStatus.self, forKey: .freeSpaceGuard)
        unreadMail = c.decodeFlexibleInt(forKey: .unreadMail)
        failingJobsCount = c.decodeFlexibleInt(forKey: .failingJobsCount)
        failingJobs = try? c.decodeIfPresent([FailingJob].self, forKey: .failingJobs)
        failCount = c.decodeFlexibleInt(forKey: .failCount)
        warnCount = c.decodeFlexibleInt(forKey: .warnCount)
        drives = try? c.decodeIfPresent([DriveInfo].self, forKey: .drives)
        samples = try? c.decodeIfPresent([HostSample].self, forKey: .samples)
    }
}

public struct HostsResponse: Codable, Sendable, Hashable {
    public var generatedAt: Int?
    public var hosts: [HostSummary]

    enum CodingKeys: String, CodingKey {
        case hosts
        case generatedAt = "generated_at"
    }
}

// MARK: - AI Capacity

public struct CapacityRoute: Codable, Sendable, Hashable {
    public var build: String?
    public var review: String?
}

public struct CapacityPool: Codable, Sendable, Hashable, Identifiable {
    public var engine: String
    public var pool: String
    public var remainingPct: Double?
    public var jobsLeft: Double?
    public var tokensLeft: Double?
    public var resetAt: String?
    public var stale: Bool?
    public var freshness: String?
    public var paused: Bool?
    public var unavailable: Bool?
    public var unavailableReason: String?

    public var id: String { "\(engine)-\(pool)" }

    enum CodingKeys: String, CodingKey {
        case engine, pool, stale, freshness, paused, unavailable, unavailableReason
        case remainingPct = "remaining_pct"
        case jobsLeft = "jobs_left"
        case tokensLeft = "tokens_left"
        case resetAt = "reset_at"
    }

    public init(
        engine: String,
        pool: String,
        remainingPct: Double? = nil,
        jobsLeft: Double? = nil,
        tokensLeft: Double? = nil,
        resetAt: String? = nil,
        stale: Bool? = nil,
        freshness: String? = nil,
        paused: Bool? = nil,
        unavailable: Bool? = nil,
        unavailableReason: String? = nil
    ) {
        self.engine = engine
        self.pool = pool
        self.remainingPct = remainingPct
        self.jobsLeft = jobsLeft
        self.tokensLeft = tokensLeft
        self.resetAt = resetAt
        self.stale = stale
        self.freshness = freshness
        self.paused = paused
        self.unavailable = unavailable
        self.unavailableReason = unavailableReason
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        engine = (try? c.decode(String.self, forKey: .engine)) ?? "unknown"
        pool = (try? c.decode(String.self, forKey: .pool)) ?? "default"
        remainingPct = c.decodeFlexibleDouble(forKey: .remainingPct)
        jobsLeft = c.decodeFlexibleDouble(forKey: .jobsLeft)
        tokensLeft = c.decodeFlexibleDouble(forKey: .tokensLeft)
        resetAt = c.decodeFlexibleString(forKey: .resetAt)
        stale = try? c.decodeIfPresent(Bool.self, forKey: .stale)
        freshness = c.decodeFlexibleString(forKey: .freshness)
        paused = try? c.decodeIfPresent(Bool.self, forKey: .paused)
        unavailableReason = try? c.decodeIfPresent(String.self, forKey: .unavailableReason)
        if let boolVal = try? c.decodeIfPresent(Bool.self, forKey: .unavailable) {
            unavailable = boolVal
        } else if let strVal = try? c.decodeIfPresent(String.self, forKey: .unavailable), !strVal.isEmpty {
            unavailable = true
            unavailableReason = unavailableReason ?? strVal
        } else {
            unavailable = nil
        }
    }
}

public struct CapacityResponse: Codable, Sendable, Hashable {
    public var generatedAt: String?
    public var route: CapacityRoute?
    public var paused: [String]
    public var pools: [CapacityPool]

    enum CodingKeys: String, CodingKey {
        case route, paused, pools
        case generatedAt = "generated_at"
    }

    public init(generatedAt: String? = nil, route: CapacityRoute? = nil, paused: [String] = [], pools: [CapacityPool] = []) {
        self.generatedAt = generatedAt
        self.route = route
        self.paused = paused
        self.pools = pools
    }
}

// MARK: - Backups & Placement

private struct IDriveStorageDict: Decodable {
    let usedGB: Double?
    let totalGB: Double?

    enum CodingKeys: String, CodingKey {
        case usedGB = "used_gb"
        case totalGB = "total_gb"
    }
}

public struct IDriveSummary: Codable, Sendable, Hashable {
    public var checkedAt: Int?
    public var hostVM: String?
    public var container: String?
    public var running: Bool?
    public var appending: Bool?
    public var percent: Double?
    public var sentBytes: Int64?
    public var totalBytes: Int64?
    public var phase: String?
    public var lastSuccessEpoch: Int?
    public var lastSuccessStatus: String?
    public var storage: String?

    enum CodingKeys: String, CodingKey {
        case container, running, appending, percent, phase, storage
        case checkedAt = "checked_at"
        case hostVM = "host_vm"
        case sentBytes = "sent_bytes"
        case totalBytes = "total_bytes"
        case lastSuccessEpoch = "last_success_epoch"
        case lastSuccessStatus = "last_success_status"
    }

    public init(
        checkedAt: Int? = nil,
        hostVM: String? = nil,
        container: String? = nil,
        running: Bool? = nil,
        appending: Bool? = nil,
        percent: Double? = nil,
        sentBytes: Int64? = nil,
        totalBytes: Int64? = nil,
        phase: String? = nil,
        lastSuccessEpoch: Int? = nil,
        lastSuccessStatus: String? = nil,
        storage: String? = nil
    ) {
        self.checkedAt = checkedAt
        self.hostVM = hostVM
        self.container = container
        self.running = running
        self.appending = appending
        self.percent = percent
        self.sentBytes = sentBytes
        self.totalBytes = totalBytes
        self.phase = phase
        self.lastSuccessEpoch = lastSuccessEpoch
        self.lastSuccessStatus = lastSuccessStatus
        self.storage = storage
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        checkedAt = c.decodeFlexibleInt(forKey: .checkedAt)
        hostVM = c.decodeFlexibleString(forKey: .hostVM)
        container = c.decodeFlexibleString(forKey: .container)
        running = try? c.decodeIfPresent(Bool.self, forKey: .running)
        appending = try? c.decodeIfPresent(Bool.self, forKey: .appending)
        percent = c.decodeFlexibleDouble(forKey: .percent)
        if let sent = try? c.decodeIfPresent(Int64.self, forKey: .sentBytes) {
            sentBytes = sent
        } else if let dbl = c.decodeFlexibleDouble(forKey: .sentBytes) {
            sentBytes = Int64(dbl)
        } else {
            sentBytes = nil
        }
        if let total = try? c.decodeIfPresent(Int64.self, forKey: .totalBytes) {
            totalBytes = total
        } else if let dbl = c.decodeFlexibleDouble(forKey: .totalBytes) {
            totalBytes = Int64(dbl)
        } else {
            totalBytes = nil
        }
        phase = c.decodeFlexibleString(forKey: .phase)
        lastSuccessEpoch = c.decodeFlexibleInt(forKey: .lastSuccessEpoch)
        lastSuccessStatus = c.decodeFlexibleString(forKey: .lastSuccessStatus)
        if let str = try? c.decodeIfPresent(String.self, forKey: .storage) {
            storage = str
        } else if let dict = try? c.decodeIfPresent(IDriveStorageDict.self, forKey: .storage) {
            let usedTxt = FleetFormatter.formatGB(dict.usedGB)
            if let totalGB = dict.totalGB {
                storage = "\(usedTxt) / \(FleetFormatter.formatGB(totalGB))"
            } else {
                storage = usedTxt
            }
        } else {
            storage = nil
        }
    }
}

public struct GDriveSummary: Codable, Sendable, Hashable {
    public var total: Int?
    public var activeRuns: Int?
    public var okCount: Int?

    enum CodingKeys: String, CodingKey {
        case total
        case activeRuns = "active_runs"
        case okCount = "ok_count"
    }
}

public struct HomeMirrorHost: Codable, Sendable, Hashable {
    public var checkedAt: Int?
    public var state: String?
    public var plannedBytes: Int64?

    enum CodingKeys: String, CodingKey {
        case state
        case checkedAt = "checked_at"
        case plannedBytes = "planned_bytes"
    }
}

public struct OverviewBackups: Codable, Sendable, Hashable {
    public var idrive: IDriveSummary?
    public var gdrive: GDriveSummary?
    public var homeMirror: [String: HomeMirrorHost]?
    public var placementViolations: Int?

    enum CodingKeys: String, CodingKey {
        case idrive, gdrive
        case homeMirror = "home_mirror"
        case placementViolations = "placement_violations"
    }
}

public struct GDriveItem: Codable, Sendable, Hashable, Identifiable {
    public var name: String
    public var running: Bool?
    public var lastAttempt: Int?
    public var lastOK: Int?
    public var lastResult: String?

    public var id: String { name }

    enum CodingKeys: String, CodingKey {
        case name, running
        case lastAttempt = "last_attempt"
        case lastOK = "last_ok"
        case lastResult = "last_result"
    }
}

public struct GDriveDetail: Codable, Sendable, Hashable {
    public var total: Int?
    public var activeRuns: Int?
    public var okCount: Int?
    public var items: [GDriveItem]?

    enum CodingKeys: String, CodingKey {
        case total, items
        case activeRuns = "active_runs"
        case okCount = "ok_count"
    }
}

public struct PlacementViolation: Codable, Sendable, Hashable, Identifiable {
    public var host: String
    public var dataset: String
    public var code: String
    public var detail: String?

    public var id: String { "\(host)|\(dataset)|\(code)" }
}

public struct PlacementDetail: Codable, Sendable, Hashable {
    public var generated: Int?
    public var ok: Bool?
    public var violationsCount: Int?
    public var violations: [PlacementViolation]?

    enum CodingKeys: String, CodingKey {
        case generated, ok, violations
        case violationsCount = "violations_count"
    }
}

public struct BackupsResponse: Codable, Sendable, Hashable {
    public var idrive: IDriveSummary?
    public var gdrive: GDriveDetail?
    public var homeMirror: [String: HomeMirrorHost]?
    public var placement: PlacementDetail?

    enum CodingKeys: String, CodingKey {
        case idrive, gdrive, placement
        case homeMirror = "home_mirror"
    }
}

// MARK: - Morning Board & Security

public enum RowSeverity: Int, Comparable, Sendable, Hashable {
    case fail = 0
    case warn = 1
    case pass = 2
    case other = 3

    public static func < (lhs: RowSeverity, rhs: RowSeverity) -> Bool {
        lhs.rawValue < rhs.rawValue
    }

    public init(status: String) {
        switch status.trimmingCharacters(in: .whitespacesAndNewlines).lowercased() {
        case "fail", "error", "crit", "critical":
            self = .fail
        case "warn", "warning", "stale":
            self = .warn
        case "pass", "ok", "healthy":
            self = .pass
        default:
            self = .other
        }
    }
}

public struct MorningRow: Codable, Sendable, Hashable, Identifiable {
    public var key: String
    public var status: String
    public var symbol: String?
    public var area: String?
    public var name: String
    public var value: String?
    public var datum: String?
    public var detail: String?

    public var id: String { "\(key)|\(area ?? "")|\(name)|\(status)|\(datum ?? "")" }
    public var severity: RowSeverity { RowSeverity(status: status) }

    enum CodingKeys: String, CodingKey {
        case key, status, symbol, area, name, value, datum, detail
    }

    public init(
        key: String,
        status: String,
        symbol: String? = nil,
        area: String? = nil,
        name: String,
        value: String? = nil,
        datum: String? = nil,
        detail: String? = nil
    ) {
        self.key = key
        self.status = status
        self.symbol = symbol
        self.area = area
        self.name = name
        self.value = value
        self.datum = datum
        self.detail = detail
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        key = (try? c.decode(String.self, forKey: .key)) ?? ""
        status = (try? c.decode(String.self, forKey: .status)) ?? "unknown"
        symbol = try? c.decodeIfPresent(String.self, forKey: .symbol)
        area = try? c.decodeIfPresent(String.self, forKey: .area)
        name = (try? c.decode(String.self, forKey: .name)) ?? key
        value = c.decodeFlexibleString(forKey: .value)
        datum = c.decodeFlexibleString(forKey: .datum)
        detail = c.decodeFlexibleString(forKey: .detail)
    }
}

public struct SecuritySummaryBlock: Codable, Sendable, Hashable {
    public var status: String
    public var value: String?
    public var detail: String?
    public var generated: Int?
    public var rows: [MorningRow]?

    enum CodingKeys: String, CodingKey {
        case status, value, detail, generated, rows
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        status = (try? c.decode(String.self, forKey: .status)) ?? "unknown"
        value = c.decodeFlexibleString(forKey: .value)
        detail = c.decodeFlexibleString(forKey: .detail)
        generated = c.decodeFlexibleInt(forKey: .generated)
        rows = try? c.decodeIfPresent([MorningRow].self, forKey: .rows)
    }

    public init(status: String, value: String? = nil, detail: String? = nil, generated: Int? = nil, rows: [MorningRow]? = nil) {
        self.status = status
        self.value = value
        self.detail = detail
        self.generated = generated
        self.rows = rows
    }
}

public struct FleetOverviewResponse: Codable, Sendable, Hashable {
    public var generatedAt: Int
    public var date: String?
    public var counts: CountsSummary
    public var cadence: CadenceInfo?
    public var agyMorning: String?
    public var hosts: [HostSummary]
    public var capacity: CapacityResponse
    public var backups: OverviewBackups
    public var security: SecuritySummaryBlock
    public var unreadMail: Int
    public var activeClaims: Int

    enum CodingKeys: String, CodingKey {
        case date, counts, cadence, hosts, capacity, backups, security
        case generatedAt = "generated_at"
        case agyMorning = "agy_morning"
        case unreadMail = "unread_mail"
        case activeClaims = "active_claims"
    }
}

public struct MorningResponse: Codable, Sendable, Hashable {
    public var version: Int?
    public var generated: Int?
    public var date: String?
    public var ok: Bool?
    public var counts: CountsSummary
    public var cadence: CadenceInfo?
    public var agyMorning: String?
    public var rows: [MorningRow]

    enum CodingKeys: String, CodingKey {
        case version, generated, date, ok, counts, cadence, rows
        case agyMorning = "agy_morning"
    }
}

public struct AuditFinding: Codable, Sendable, Hashable, Identifiable {
    public var level: String
    public var rule: String
    public var text: String

    public var id: String { "\(level)|\(rule)|\(text)" }
}

private struct NamespaceAuditCountsDict: Decodable {
    let runs: Int?
    let securityRuns: Int?

    enum CodingKeys: String, CodingKey {
        case runs
        case securityRuns = "security_runs"
    }
}

public struct NamespaceAudit: Codable, Sendable, Hashable {
    public var file: String?
    public var status: String?
    public var runs: Int?
    public var securityRuns: Int?
    public var failCount: Int?
    public var warnCount: Int?
    public var topFindings: [AuditFinding]?

    enum CodingKeys: String, CodingKey {
        case file, status, counts, runs, securityRuns
        case failCount = "fail_count"
        case warnCount = "warn_count"
        case topFindings = "top_findings"
    }

    public init(
        file: String? = nil,
        status: String? = nil,
        runs: Int? = nil,
        securityRuns: Int? = nil,
        failCount: Int? = nil,
        warnCount: Int? = nil,
        topFindings: [AuditFinding]? = nil
    ) {
        self.file = file
        self.status = status
        self.runs = runs
        self.securityRuns = securityRuns
        self.failCount = failCount
        self.warnCount = warnCount
        self.topFindings = topFindings
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        file = c.decodeFlexibleString(forKey: .file)
        status = c.decodeFlexibleString(forKey: .status)
        if let directRuns = c.decodeFlexibleInt(forKey: .runs) {
            runs = directRuns
            securityRuns = c.decodeFlexibleInt(forKey: .securityRuns)
        } else if let dict = try? c.decodeIfPresent(NamespaceAuditCountsDict.self, forKey: .counts) {
            runs = dict.runs
            securityRuns = dict.securityRuns
        } else {
            runs = nil
            securityRuns = nil
        }
        failCount = c.decodeFlexibleInt(forKey: .failCount)
        warnCount = c.decodeFlexibleInt(forKey: .warnCount)
        topFindings = try? c.decodeIfPresent([AuditFinding].self, forKey: .topFindings)
    }

    public func encode(to encoder: Encoder) throws {
        var c = encoder.container(keyedBy: CodingKeys.self)
        try c.encodeIfPresent(file, forKey: .file)
        try c.encodeIfPresent(status, forKey: .status)
        try c.encodeIfPresent(runs, forKey: .runs)
        try c.encodeIfPresent(securityRuns, forKey: .securityRuns)
        try c.encodeIfPresent(failCount, forKey: .failCount)
        try c.encodeIfPresent(warnCount, forKey: .warnCount)
        try c.encodeIfPresent(topFindings, forKey: .topFindings)
    }
}

public struct SecurityNamespace: Codable, Sendable, Hashable {
    public var head: String?
    public var integrity: String?
    public var lastAt: String?
    public var links: Int?
    public var problems: [String]?
    public var silence: String?
    public var silentMin: Double?
    public var status: String?

    enum CodingKeys: String, CodingKey {
        case head, integrity, links, problems, silence, status
        case lastAt = "last_at"
        case silentMin = "silent_min"
    }
}

public struct SecurityResponse: Codable, Sendable, Hashable {
    public var status: String
    public var value: String?
    public var datum: String?
    public var detail: String?
    public var generated: Int?
    public var rows: [MorningRow]?
    public var namespaces: [String: SecurityNamespace]?
    public var namespaceAudits: [String: NamespaceAudit]?
    public var localTripwireOpen: Int?

    enum CodingKeys: String, CodingKey {
        case status, value, datum, detail, generated, rows, namespaces
        case namespaceAudits = "namespace_audits"
        case localTripwireOpen = "local_tripwire_open"
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        status = (try? c.decode(String.self, forKey: .status)) ?? "unknown"
        value = c.decodeFlexibleString(forKey: .value)
        datum = c.decodeFlexibleString(forKey: .datum)
        detail = c.decodeFlexibleString(forKey: .detail)
        generated = c.decodeFlexibleInt(forKey: .generated)
        rows = try? c.decodeIfPresent([MorningRow].self, forKey: .rows)
        namespaces = try? c.decodeIfPresent([String: SecurityNamespace].self, forKey: .namespaces)
        namespaceAudits = try? c.decodeIfPresent([String: NamespaceAudit].self, forKey: .namespaceAudits)
        localTripwireOpen = c.decodeFlexibleInt(forKey: .localTripwireOpen)
    }
}

// MARK: - Agent Mail & Cowork Claims

public struct MailMessage: Codable, Sendable, Hashable, Identifiable {
    public var id: String?
    public var at: String?
    public var from: String?
    public var project: String?
    public var to: String?
    public var text: String?
    public var body: String?

    public var stableID: String {
        id ?? "\(at ?? "")|\(from ?? "")|\(to ?? "")|\(displayText.prefix(24))"
    }

    public var displayText: String {
        if let text, !text.isEmpty { return text }
        if let body, !body.isEmpty { return body }
        return "(empty)"
    }
}

public struct CoworkClaim: Codable, Sendable, Hashable, Identifiable {
    public var work: String
    public var host: String
    public var provider: String?
    public var heartbeat: String?
    public var expires: String?
    public var note: String?

    public var id: String { "\(work)|\(host)|\(provider ?? "")" }

    enum CodingKeys: String, CodingKey {
        case work, host, provider, heartbeat, expires, note
    }

    public init(
        work: String,
        host: String,
        provider: String? = nil,
        heartbeat: String? = nil,
        expires: String? = nil,
        note: String? = nil
    ) {
        self.work = work
        self.host = host
        self.provider = provider
        self.heartbeat = heartbeat
        self.expires = expires
        self.note = note
    }

    public init(from decoder: Decoder) throws {
        let c = try decoder.container(keyedBy: CodingKeys.self)
        work = (try? c.decode(String.self, forKey: .work)) ?? "unknown"
        host = (try? c.decode(String.self, forKey: .host)) ?? "unknown"
        provider = c.decodeFlexibleString(forKey: .provider)
        heartbeat = c.decodeFlexibleString(forKey: .heartbeat)
        expires = c.decodeFlexibleString(forKey: .expires)
        note = c.decodeFlexibleString(forKey: .note)
    }
}

public struct MailResponse: Codable, Sendable, Hashable {
    public var messages: [MailMessage]
    public var unreadByHost: [String: Int]?
    public var claims: [CoworkClaim]

    enum CodingKeys: String, CodingKey {
        case messages, claims
        case unreadByHost = "unread_by_host"
    }
}

public struct HealthResponse: Codable, Sendable, Hashable {
    public var ok: Bool
    public var host: String?
    public var serverTime: Int?
    public var uptimeS: Double?
    public var apiVersion: Int?

    enum CodingKeys: String, CodingKey {
        case ok, host
        case serverTime = "server_time"
        case uptimeS = "uptime_s"
        case apiVersion = "api_version"
    }
}

public struct ActionResultResponse: Codable, Sendable, Hashable {
    public var ok: Bool
    public var error: String?
    public var to: String?
    public var id: String?
    public var recorded: Bool?
    public var mailId: String?
    public var started: Bool?
    public var provider: String?
    public var paused: Bool?
    public var list: [String]?

    enum CodingKeys: String, CodingKey {
        case ok, error, to, id, recorded, started, provider, paused, list
        case mailId = "mail_id"
    }
}

// MARK: - Board filtering and formatting utilities

public enum FleetRowFilter: String, CaseIterable, Identifiable, Sendable {
    case all = "All"
    case failing = "Fail"
    case warning = "Warn"
    case passing = "Pass"
    case security = "Security"
    case placement = "Placement"
    case jobs = "Jobs"
    case disks = "Disks"

    public var id: String { rawValue }
}

public func filterAndSortRows(
    _ rows: [MorningRow],
    filter: FleetRowFilter,
    searchText: String = ""
) -> [MorningRow] {
    let needle = searchText.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    let filtered = rows.filter { row in
        let areaLower = (row.area ?? "").lowercased()
        let keyLower = row.key.lowercased()
        switch filter {
        case .all:
            break
        case .failing:
            guard row.severity == .fail else { return false }
        case .warning:
            guard row.severity == .warn else { return false }
        case .passing:
            guard row.severity == .pass else { return false }
        case .security:
            guard areaLower.contains("sec") || keyLower.contains("sec") else { return false }
        case .placement:
            guard areaLower.contains("plac") || areaLower.contains("back") || keyLower.contains("plac") else { return false }
        case .jobs:
            guard areaLower.contains("job") || keyLower.contains("job") || keyLower.contains("launchd") else { return false }
        case .disks:
            guard areaLower.contains("disk") || areaLower.contains("drive") || keyLower.contains("disk") else { return false }
        }
        guard !needle.isEmpty else { return true }
        let haystack = [row.name, row.key, row.area, row.value, row.datum, row.detail]
            .compactMap { $0?.lowercased() }
            .joined(separator: " ")
        return haystack.contains(needle)
    }
    return filtered.sorted { lhs, rhs in
        if lhs.severity != rhs.severity {
            return lhs.severity < rhs.severity
        }
        if (lhs.area ?? "") != (rhs.area ?? "") {
            return (lhs.area ?? "") < (rhs.area ?? "")
        }
        return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
    }
}

public enum FleetFormatter {
    public static func relativeTime(fromEpoch epoch: Int?, now: Date = Date()) -> String {
        guard let epoch, epoch > 0 else { return "—" }
        let delta = Int(now.timeIntervalSince1970) - epoch
        return formatDelta(seconds: delta)
    }

    public static func parseISO(_ raw: String?) -> Date? {
        guard let raw, !raw.isEmpty else { return nil }
        let f1 = ISO8601DateFormatter()
        f1.formatOptions = [.withInternetDateTime, .withFractionalSeconds]
        if let d = f1.date(from: raw) { return d }
        let f2 = ISO8601DateFormatter()
        f2.formatOptions = [.withInternetDateTime]
        return f2.date(from: raw)
    }

    public static func parseTimestamp(_ raw: String?) -> Date? {
        guard let trimmed = raw?.trimmingCharacters(in: .whitespacesAndNewlines), !trimmed.isEmpty else {
            return nil
        }
        if let epoch = Double(trimmed), epoch > 1_000_000_000 {
            return Date(timeIntervalSince1970: epoch)
        }
        if let iso = parseISO(trimmed) {
            return iso
        }
        let localFmt = DateFormatter()
        localFmt.locale = Locale(identifier: "en_US_POSIX")
        localFmt.dateFormat = "yyyy-MM-dd HH:mm:ss"
        if let local = localFmt.date(from: trimmed) {
            return local
        }
        return nil
    }

    public static func relativeTime(fromISO iso: String?, now: Date = Date()) -> String {
        guard let date = parseISO(iso) else { return iso ?? "—" }
        return formatDelta(seconds: Int(now.timeIntervalSince(date)))
    }

    public static func relativeTime(fromTimestamp raw: String?, now: Date = Date()) -> String {
        guard let date = parseTimestamp(raw) else { return raw ?? "—" }
        return formatDelta(seconds: Int(now.timeIntervalSince(date)))
    }

    public static func resetCountdown(fromISO iso: String?, now: Date = Date()) -> String? {
        guard let target = parseISO(iso) else { return nil }
        let remaining = Int(target.timeIntervalSince(now))
        if remaining <= 0 { return "reset due" }
        let hours = remaining / 3600
        let minutes = (remaining % 3600) / 60
        if hours >= 24 {
            let days = hours / 24
            return "resets in \(days)d \(hours % 24)h"
        }
        if hours > 0 {
            return "resets in \(hours)h \(minutes)m"
        }
        return "resets in \(max(1, minutes))m"
    }

    public static func formatGB(_ gb: Double?) -> String {
        guard let gb else { return "—" }
        if gb >= 1000 {
            return String(format: "%.2f TB", gb / 1000.0)
        }
        return String(format: "%.0f GB", gb)
    }

    public static func formatBytes(_ bytes: Int64?) -> String {
        guard let bytes else { return "—" }
        let formatter = ByteCountFormatter()
        formatter.allowedUnits = [.useGB, .useTB, .useMB]
        formatter.countStyle = .decimal
        return formatter.string(fromByteCount: bytes)
    }

    private static func formatDelta(seconds: Int) -> String {
        if seconds < 0 {
            return "in \(-seconds / 60)m"
        }
        if seconds < 60 { return "just now" }
        let mins = seconds / 60
        if mins < 60 { return "\(mins)m ago" }
        let hours = mins / 60
        if hours < 48 { return "\(hours)h ago" }
        return "\(hours / 24)d ago"
    }
}

import Foundation

// MARK: - Job Label & Disk Threshold Helpers

public func shortJobName(_ label: String) -> String {
    let trimmed = label.trimmingCharacters(in: .whitespacesAndNewlines)
    let parts = trimmed.split(separator: ".", omittingEmptySubsequences: false)
    if parts.count >= 3 {
        let shortened = parts.dropFirst(2).joined(separator: ".")
        if !shortened.isEmpty {
            return shortened
        }
    }
    return trimmed
}

public enum DiskThresholdState: String, Sendable, Hashable {
    case critical
    case belowTarget
    case healthy
    case unknown
}

public struct DiskThresholdInfo: Sendable, Hashable {
    public let state: DiskThresholdState
    public let headline: String
    public let thresholdSummary: String
    public let usedFraction: Double
}

public func diskThresholdInfo(for host: HostSummary) -> DiskThresholdInfo {
    guard let free = host.diskFreeGB else {
        return DiskThresholdInfo(state: .unknown, headline: "—", thresholdSummary: "No disk reading", usedFraction: 0)
    }
    let headline = "\(FleetFormatter.formatGB(free)) free"
    let usedFraction: Double
    if let total = host.diskTotalGB, total > 0 {
        usedFraction = min(max((total - free) / total, 0), 1)
    } else {
        usedFraction = 0
    }

    if let floor = host.diskFloorGB, floor > 0, free < floor {
        let deficit = max(1, Int((floor - free).rounded()))
        return DiskThresholdInfo(
            state: .critical,
            headline: headline,
            thresholdSummary: "\(deficit) GB below \(FleetFormatter.formatGB(floor)) floor",
            usedFraction: usedFraction
        )
    }

    if let target = host.diskTargetGB, target > 0, free < target {
        let floorSuffix = host.diskFloorGB.map { " · floor \(FleetFormatter.formatGB($0))" } ?? ""
        return DiskThresholdInfo(
            state: .belowTarget,
            headline: headline,
            thresholdSummary: "Under \(FleetFormatter.formatGB(target)) target\(floorSuffix)",
            usedFraction: usedFraction
        )
    }

    if let target = host.diskTargetGB, target > 0 {
        return DiskThresholdInfo(
            state: .healthy,
            headline: headline,
            thresholdSummary: "Above \(FleetFormatter.formatGB(target)) target",
            usedFraction: usedFraction
        )
    }

    return DiskThresholdInfo(
        state: .healthy,
        headline: headline,
        thresholdSummary: host.diskTotalGB.map { "of \(FleetFormatter.formatGB($0))" } ?? "",
        usedFraction: usedFraction
    )
}

public struct HostVerdict: Sendable, Hashable {
    public let label: String
    public let severity: RowSeverity
}

public func hostHeadlineVerdict(for host: HostSummary) -> HostVerdict {
    let diskInfo = diskThresholdInfo(for: host)
    if diskInfo.state == .critical {
        return HostVerdict(label: "Low Disk", severity: .fail)
    }
    if host.ok == false {
        return HostVerdict(label: "Offline", severity: .fail)
    }
    let failJobs = host.failingJobsCount ?? (host.failingJobs?.count ?? 0)
    if failJobs > 0 {
        return HostVerdict(label: failJobs == 1 ? "1 Job Fail" : "\(failJobs) Jobs Fail", severity: .fail)
    }
    if (host.failCount ?? 0) > 0 {
        return HostVerdict(label: "\(host.failCount ?? 1) Fail", severity: .fail)
    }
    let thermalLower = (host.thermal ?? "").lowercased()
    if thermalLower == "heavy" || thermalLower == "critical" || (host.chipC ?? 0) >= 95 {
        return HostVerdict(label: "Hot", severity: .warn)
    }
    if diskInfo.state == .belowTarget {
        return HostVerdict(label: "Low Target", severity: .warn)
    }
    if (host.warnCount ?? 0) > 0 {
        return HostVerdict(label: "\(host.warnCount ?? 1) Warn", severity: .warn)
    }
    return HostVerdict(label: "Healthy", severity: .pass)
}

public func sortHostsByPriority(_ hosts: [HostSummary]) -> [HostSummary] {
    hosts.sorted { lhs, rhs in
        let lVerdict = hostHeadlineVerdict(for: lhs)
        let rVerdict = hostHeadlineVerdict(for: rhs)
        if lVerdict.severity != rVerdict.severity {
            return lVerdict.severity < rVerdict.severity
        }
        if (lhs.failCount ?? 0) != (rhs.failCount ?? 0) {
            return (lhs.failCount ?? 0) > (rhs.failCount ?? 0)
        }
        return lhs.id.localizedCaseInsensitiveCompare(rhs.id) == .orderedAscending
    }
}

// MARK: - Attention Clusters

public struct AttentionCluster: Identifiable, Sendable, Hashable {
    public let id: String
    public let title: String
    public let count: Int
    public let summary: String
    public let systemImage: String
    public let targetFilter: FleetRowFilter
    public let severity: RowSeverity
}

private func attentionBucket(for row: MorningRow) -> (id: String, title: String, icon: String, filter: FleetRowFilter, order: Int) {
    let area = (row.area ?? "").lowercased()
    let key = row.key.lowercased()
    if area.contains("disk") || area.contains("drive") || key.contains("disk") || key.contains("storage:") {
        return ("disk", "Disks Below Floor", "internaldrive.fill", .disks, 0)
    }
    if area.contains("sec") || key.contains("sec") {
        return ("security", "Security Audit", "shield.lefthalf.filled", .security, 1)
    }
    if area.contains("job") || key.contains("job") || key.contains("launchd") {
        return ("job", "Failing Jobs", "exclamationmark.arrow.circlepath", .jobs, 2)
    }
    if area.contains("place") || area.contains("plac") || key.contains("plac") {
        return ("placement", "Placement Gaps", "externaldrive.badge.exclamationmark", .placement, 3)
    }
    if area.contains("back") || area.contains("cloud") || key.contains("back") || key.contains("idrive") || key.contains("mirror") {
        return ("backup", "Backups", "icloud.slash", .backups, 4)
    }
    return ("other", "Other Issues", "exclamationmark.triangle", .issues, 5)
}

public func buildAttentionClusters(rows: [MorningRow]) -> [AttentionCluster] {
    let activeRows = rows.filter { !$0.isSnoozed }
    let fails = activeRows.filter { $0.severity == .fail }
    let sourceRows = fails.isEmpty ? activeRows.filter { $0.severity == .warn } : fails
    let targetSeverity: RowSeverity = fails.isEmpty ? .warn : .fail
    guard !sourceRows.isEmpty else { return [] }

    var grouped: [String: (meta: (id: String, title: String, icon: String, filter: FleetRowFilter, order: Int), items: [MorningRow])] = [:]
    for row in sourceRows {
        let meta = attentionBucket(for: row)
        if var existing = grouped[meta.id] {
            existing.items.append(row)
            grouped[meta.id] = existing
        } else {
            grouped[meta.id] = (meta, [row])
        }
    }

    return grouped.values
        .sorted { $0.meta.order < $1.meta.order }
        .map { bucket in
            let names = bucket.items.prefix(3).map { row -> String in
                if bucket.meta.id == "disk", let val = row.value, !val.isEmpty {
                    return "\(row.name) (\(val))"
                }
                return row.name
            }
            let tail = bucket.items.count > 3 ? " +\(bucket.items.count - 3) more" : ""
            let summary = names.joined(separator: " · ") + tail
            return AttentionCluster(
                id: bucket.meta.id,
                title: bucket.meta.title,
                count: bucket.items.count,
                summary: summary,
                systemImage: bucket.meta.icon,
                targetFilter: bucket.meta.filter,
                severity: targetSeverity
            )
        }
}

// MARK: - AI Capacity Partitioning

public struct CapacityEngineGroup: Identifiable, Sendable, Hashable {
    public let engine: String
    public let pools: [CapacityPool]

    public var id: String { engine }
}

public struct CapacityPartition: Sendable, Hashable {
    public let activeGroups: [CapacityEngineGroup]
    public let pausedPools: [CapacityPool]
}

public func displayPoolName(_ pool: CapacityPool) -> String {
    var raw = pool.pool
    let prefix = "\(pool.engine):"
    if raw.lowercased().hasPrefix(prefix.lowercased()) {
        raw = String(raw.dropFirst(prefix.count))
    }
    switch raw.lowercased() {
    case "5h": return "5-hour"
    case "week": return "Weekly"
    case "7d": return "7-day"
    case "claude-gpt-5h": return "Claude/GPT 5h"
    case "claude-gpt-week": return "Claude/GPT Week"
    case "api": return "API"
    case "cursor": return "Included"
    case "primary": return "Primary"
    default: return raw
    }
}

private func poolWindowOrder(_ pool: CapacityPool) -> Int {
    let p = pool.pool.lowercased()
    if p == "5h" { return 0 }
    if p == "week" || p == "7d" { return 1 }
    if p.contains("5h") { return 2 }
    if p.contains("week") { return 3 }
    if p == "api" { return 4 }
    return 5
}

public func partitionCapacityPools(_ pools: [CapacityPool]) -> CapacityPartition {
    var activeByEngine: [String: [CapacityPool]] = [:]
    var paused: [CapacityPool] = []

    for pool in pools {
        if pool.paused == true || pool.unavailable == true {
            paused.append(pool)
        } else {
            activeByEngine[pool.engine.lowercased(), default: []].append(pool)
        }
    }

    let groups = activeByEngine.map { engine, list -> CapacityEngineGroup in
        let sorted = list.sorted { lhs, rhs in
            let lo = poolWindowOrder(lhs)
            let ro = poolWindowOrder(rhs)
            if lo != ro { return lo < ro }
            return displayPoolName(lhs) < displayPoolName(rhs)
        }
        return CapacityEngineGroup(engine: engine, pools: sorted)
    }.sorted { lhs, rhs in
        let lMax = lhs.pools.compactMap(\.remainingPct).max() ?? -1
        let rMax = rhs.pools.compactMap(\.remainingPct).max() ?? -1
        if lMax != rMax { return lMax > rMax }
        return lhs.engine < rhs.engine
    }

    let sortedPaused = paused.sorted { lhs, rhs in
        if lhs.engine != rhs.engine { return lhs.engine < rhs.engine }
        return displayPoolName(lhs) < displayPoolName(rhs)
    }

    return CapacityPartition(activeGroups: groups, pausedPools: sortedPaused)
}

// MARK: - Morning Brief Parser

public struct BriefBullet: Identifiable, Sendable, Hashable {
    public let id: String
    public let lead: String?
    public let body: String
}

public struct BriefSection: Identifiable, Sendable, Hashable {
    public let id: String
    public let title: String
    public let bullets: [BriefBullet]
    public let paragraphs: [String]
}

private func stripMarkdownEmphasis(_ text: String) -> String {
    text.replacingOccurrences(of: "**", with: "")
        .replacingOccurrences(of: "`", with: "")
        .trimmingCharacters(in: .whitespacesAndNewlines)
}

private func isSectionHeadingLine(_ line: String) -> String? {
    if line.hasPrefix("#") {
        let stripped = line.drop(while: { $0 == "#" || $0 == " " })
        let clean = stripMarkdownEmphasis(String(stripped))
        return clean.isEmpty ? nil : clean
    }
    guard let dotRange = line.range(of: ". "),
          let num = Int(line[..<dotRange.lowerBound]),
          num >= 1 && num <= 20 else {
        return nil
    }
    let clean = stripMarkdownEmphasis(String(line[dotRange.upperBound...]))
    return clean.isEmpty ? nil : clean
}

public func parseMorningBrief(_ raw: String) -> [BriefSection] {
    let lines = raw.components(separatedBy: .newlines)
    var sections: [BriefSection] = []
    var currentTitle = "Summary"
    var currentBullets: [BriefBullet] = []
    var currentParagraphs: [String] = []
    var idx = 0

    func flush() {
        if !currentBullets.isEmpty || !currentParagraphs.isEmpty || !sections.isEmpty {
            if !currentBullets.isEmpty || !currentParagraphs.isEmpty {
                sections.append(
                    BriefSection(
                        id: "\(sections.count)|\(currentTitle)",
                        title: currentTitle,
                        bullets: currentBullets,
                        paragraphs: currentParagraphs
                    )
                )
            }
        }
        currentBullets = []
        currentParagraphs = []
    }

    for rawLine in lines {
        let line = rawLine.trimmingCharacters(in: .whitespacesAndNewlines)
        if line.isEmpty { continue }

        if let heading = isSectionHeadingLine(line) {
            flush()
            currentTitle = heading
            continue
        }

        if line.hasPrefix("- ") || line.hasPrefix("* ") {
            let content = stripMarkdownEmphasis(String(line.dropFirst(2)))
            idx += 1
            if let colonRange = content.range(of: ": ") {
                let lead = String(content[..<colonRange.lowerBound]).trimmingCharacters(in: .whitespacesAndNewlines)
                let rest = String(content[colonRange.upperBound...]).trimmingCharacters(in: .whitespacesAndNewlines)
                if !lead.isEmpty, lead.count <= 72, !rest.isEmpty {
                    currentBullets.append(BriefBullet(id: "b-\(idx)", lead: lead, body: rest))
                    continue
                }
            }
            currentBullets.append(BriefBullet(id: "b-\(idx)", lead: nil, body: content))
            continue
        }

        currentParagraphs.append(stripMarkdownEmphasis(line))
    }
    flush()
    return sections
}

// MARK: - Backups & Grouped Board Helpers

public func cleanMirrorState(_ raw: String?) -> (isSkipped: Bool, reason: String) {
    let trimmed = (raw ?? "—").trimmingCharacters(in: .whitespacesAndNewlines)
    let lower = trimmed.lowercased()
    if lower.hasPrefix("skipped:") {
        let rest = String(trimmed.dropFirst("skipped:".count)).trimmingCharacters(in: .whitespacesAndNewlines)
        return (true, rest.isEmpty ? "skipped" : rest)
    }
    return (false, trimmed.isEmpty ? "—" : trimmed)
}

public func gdriveBreakdown(detail: GDriveDetail?, summary: GDriveSummary?) -> String {
    if let items = detail?.items, !items.isEmpty {
        var okCount = 0
        var unmountedCount = 0
        var runningCount = 0
        var otherCount = 0
        for item in items {
            let res = (item.lastResult ?? "").lowercased()
            if res == "ok" {
                okCount += 1
            } else if res == "not mounted" {
                unmountedCount += 1
            } else if item.running == true {
                runningCount += 1
            } else {
                otherCount += 1
            }
        }
        var parts: [String] = []
        if okCount > 0 { parts.append("\(okCount) OK") }
        if runningCount > 0 { parts.append("\(runningCount) running") }
        if unmountedCount > 0 { parts.append("\(unmountedCount) unmounted") }
        if otherCount > 0 { parts.append("\(otherCount) pending") }
        if !parts.isEmpty {
            return parts.joined(separator: " · ")
        }
    }
    let ok = detail?.okCount ?? summary?.okCount ?? 0
    let total = detail?.total ?? summary?.total ?? 0
    let active = detail?.activeRuns ?? summary?.activeRuns ?? 0
    return "\(ok)/\(total) OK · \(active) active"
}

public struct BoardAreaGroup: Identifiable, Sendable, Hashable {
    public let id: String
    public let title: String
    public let order: Int
    public let worstSeverity: RowSeverity
    public let rows: [MorningRow]
}

private func boardAreaMeta(_ rawArea: String?) -> (id: String, title: String, order: Int) {
    let area = (rawArea ?? "other").lowercased()
    switch area {
    case "disk": return ("disk", "Disks & Free Space", 0)
    case "security": return ("security", "Security & Evidence", 1)
    case "job": return ("job", "Background Jobs", 2)
    case "place": return ("place", "Storage Placement", 3)
    case "backup", "cloud": return ("backup", "Backups & Cloud", 4)
    case "sync": return ("sync", "Syncthing", 5)
    case "thermal": return ("thermal", "Thermals", 6)
    case "capacity": return ("capacity", "AI Capacity", 7)
    case "project", "check": return ("project", "Projects & Checks", 8)
    case "kit", "update", "upstream", "agent": return ("infra", "Fleet Tools & Sync", 9)
    default: return ("other", "Other", 10)
    }
}

public func groupRowsByArea(_ rows: [MorningRow]) -> [BoardAreaGroup] {
    var grouped: [String: (meta: (id: String, title: String, order: Int), rows: [MorningRow])] = [:]
    for row in rows {
        let meta = boardAreaMeta(row.area)
        if var existing = grouped[meta.id] {
            existing.rows.append(row)
            grouped[meta.id] = existing
        } else {
            grouped[meta.id] = (meta, [row])
        }
    }

    return grouped.values.map { entry -> BoardAreaGroup in
        let sortedRows = entry.rows.sorted { lhs, rhs in
            if lhs.severity != rhs.severity { return lhs.severity < rhs.severity }
            return lhs.name.localizedCaseInsensitiveCompare(rhs.name) == .orderedAscending
        }
        let worst = sortedRows.first?.severity ?? .pass
        return BoardAreaGroup(
            id: entry.meta.id,
            title: entry.meta.title,
            order: entry.meta.order,
            worstSeverity: worst,
            rows: sortedRows
        )
    }.sorted { lhs, rhs in
        if lhs.worstSeverity != rhs.worstSeverity { return lhs.worstSeverity < rhs.worstSeverity }
        if lhs.order != rhs.order { return lhs.order < rhs.order }
        return lhs.title < rhs.title
    }
}

// MARK: - Interactive Action Extraction Helpers

public struct JobTarget: Sendable, Hashable {
    public let host: String
    public let label: String
}

public func extractJobTarget(from row: MorningRow) -> JobTarget? {
    let keyParts = row.key.split(separator: ":", omittingEmptySubsequences: false)
    guard keyParts.count >= 2, keyParts[0].lowercased() == "job" else {
        return nil
    }
    let host = String(keyParts[1]).trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    guard !host.isEmpty else { return nil }

    if let detail = row.detail {
        let first = detail.split(separator: ";").first.map {
            String($0).trimmingCharacters(in: .whitespacesAndNewlines)
        } ?? ""
        if first.contains("."), !first.contains(" "), first.count >= 3 {
            return JobTarget(host: host, label: first)
        }
    }
    if !row.name.contains(" "), row.name.contains("."), row.name.count >= 3 {
        return JobTarget(host: host, label: row.name)
    }
    return nil
}

public func extractHost(from row: MorningRow, knownHostIDs: [String] = []) -> String? {
    let parts = row.key.split(separator: ":", omittingEmptySubsequences: false).map {
        String($0).trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
    }
    guard parts.count >= 2 else { return nil }
    let knownSet = Set(knownHostIDs.map { $0.lowercased() })
    if !knownSet.isEmpty {
        return parts.dropFirst().first(where: { knownSet.contains($0) })
    }
    let hostScopedPrefixes: Set<String> = ["job", "disk", "storage", "thermal", "sync", "kit", "update"]
    if hostScopedPrefixes.contains(parts[0]) {
        return parts[1]
    }
    return nil
}

public struct HostActionRecommendation: Sendable, Hashable {
    public let host: String
    public let kind: String
    public let title: String
    public let systemImage: String
}

public func recommendedHostAction(
    for row: MorningRow,
    actionHosts: [String],
    knownHostIDs: [String] = []
) -> HostActionRecommendation? {
    let actionSet = Set(actionHosts.map { $0.lowercased() })
    guard !actionSet.isEmpty else { return nil }
    let key = row.key.lowercased()
    let area = (row.area ?? "").lowercased()

    if area == "disk" || key.hasPrefix("disk:") || key.hasPrefix("storage:") {
        if let host = extractHost(from: row, knownHostIDs: knownHostIDs), actionSet.contains(host) {
            return HostActionRecommendation(
                host: host,
                kind: "free-space-guard",
                title: "Run Free-Space Guard on \(host.uppercased())",
                systemImage: "wrench.and.screwdriver"
            )
        }
        return nil
    }

    if key.contains("idrive") {
        let host = extractHost(from: row, knownHostIDs: knownHostIDs) ?? actionHosts.first
        if let host, actionSet.contains(host.lowercased()) {
            return HostActionRecommendation(
                host: host.lowercased(),
                kind: "idrive-tick",
                title: "Tick IDrive Ingest on \(host.uppercased())",
                systemImage: "externaldrive.connected.to.line.below"
            )
        }
    }

    if area == "place" || key.hasPrefix("place:") {
        if let hub = actionHosts.first {
            return HostActionRecommendation(
                host: hub.lowercased(),
                kind: "placement-check",
                title: "Re-check Placement on \(hub.uppercased())",
                systemImage: "arrow.clockwise"
            )
        }
    }

    return nil
}

public func suggestedDispatchTarget(
    for row: MorningRow,
    hosts: [HostSummary],
    actionHosts: [String] = []
) -> String {
    let hostIDs = hosts.map(\.id)
    if let job = extractJobTarget(from: row) {
        return "\(job.host):fleet"
    }
    if let host = extractHost(from: row, knownHostIDs: hostIDs) {
        return "\(host):fleet"
    }
    if let hub = actionHosts.first ?? hosts.first?.id {
        return "\(hub):fleet"
    }
    return "hub:fleet"
}

public func suggestedFixPrompt(for row: MorningRow) -> String {
    var parts: [String] = ["Investigate and fix \(row.name) (\(row.key), status: \(row.status))."]
    if let val = row.value, !val.isEmpty {
        parts.append("Current value: \(val).")
    }
    if let detail = row.detail, !detail.isEmpty {
        parts.append("Detail: \(detail)")
    }
    return parts.joined(separator: " ")
}


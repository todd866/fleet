import Charts
import SwiftUI

struct OverviewView: View {
    @Bindable var model: FleetAppModel
    @State private var expandedHosts: Set<String> = []
    @State private var showFullBriefSheet = false
    @State private var showPausedPools = false
    @State private var selectedJob: JobPillSelection?

    var body: some View {
        NavigationStack {
            ScrollView {
                LazyVStack(spacing: 14) {
                    if !model.isConfigured {
                        unpairedBanner
                    }

                    if let err = model.errorMessage {
                        errorBanner(err)
                    }

                    if let overview = model.overview {
                        needsAttentionCard(overview)

                        if let digest = overview.agyMorning, !digest.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                            prioritiesCard(digest)
                        }

                        hostsSection(sortHostsByPriority(overview.hosts))
                        capacitySection(overview.capacity)
                        backupsCard(overview.backups)
                        securityPostureCard(overview)
                    } else if model.isConfigured {
                        MissionCard {
                            HStack(spacing: 12) {
                                ProgressView()
                                Text(model.isRefreshing ? "Contacting tailnet hub…" : "Pull down to fetch Fleet telemetry.")
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            }
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("Fleet")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        Task { await model.refreshAll() }
                    } label: {
                        if model.isRefreshing {
                            ProgressView()
                        } else {
                            Image(systemName: "arrow.clockwise")
                        }
                    }
                    .disabled(model.isRefreshing || !model.isConfigured)
                }
            }
            .refreshable {
                await model.refreshAll()
            }
            .sheet(isPresented: $showFullBriefSheet) {
                if let digest = model.overview?.agyMorning {
                    MorningBriefSheet(model: model, sections: parseMorningBrief(digest), dateLabel: model.overview?.date)
                }
            }
            .sheet(item: $selectedJob) { sel in
                JobActionSheet(model: model, selection: sel)
            }
        }
    }

    // MARK: - Unpaired / Error

    private var unpairedBanner: some View {
        MissionCard {
            VStack(alignment: .leading, spacing: 8) {
                Label("Not Paired with Hub", systemImage: "link.badge.plus")
                    .font(.headline)
                    .foregroundStyle(FleetTheme.warn)
                Text("Add your tailnet server URL and bearer token in Controls, or open a pairing link from Safari.")
                    .font(.subheadline)
                    .foregroundStyle(.secondary)
                Button("Open Controls") {
                    model.openControls()
                }
                .buttonStyle(.borderedProminent)
                .tint(FleetTheme.pass)
            }
        }
    }

    private func errorBanner(_ message: String) -> some View {
        MissionCard {
            HStack(alignment: .top, spacing: 10) {
                Image(systemName: "wifi.exclamationmark")
                    .foregroundStyle(FleetTheme.fail)
                VStack(alignment: .leading, spacing: 2) {
                    Text("Partial Sync Issue")
                        .font(.subheadline.weight(.semibold))
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    // MARK: - Tile 1: Needs Attention Hero

    private func needsAttentionCard(_ overview: FleetOverviewResponse) -> some View {
        let rows = model.morning?.rows ?? overview.security.rows ?? []
        let clusters = buildAttentionClusters(rows: rows)
        let headlineColor: Color = overview.counts.fail > 0 ? FleetTheme.fail : (overview.counts.warn > 0 ? FleetTheme.warn : FleetTheme.pass)
        let headlineTitle: String = {
            if overview.counts.fail > 0 {
                return "\(overview.counts.fail) \(overview.counts.fail == 1 ? "issue needs" : "issues need") attention"
            }
            if overview.counts.warn > 0 {
                return "\(overview.counts.warn) \(overview.counts.warn == 1 ? "warning" : "warnings") to watch"
            }
            return "All systems healthy"
        }()

        return MissionCard {
            VStack(alignment: .leading, spacing: 12) {
                HStack(alignment: .top) {
                    VStack(alignment: .leading, spacing: 3) {
                        HStack(spacing: 8) {
                            Circle()
                                .fill(headlineColor)
                                .frame(width: 10, height: 10)
                            Text(headlineTitle)
                                .font(.headline)
                        }
                        HStack(spacing: 6) {
                            Text("Updated \(FleetFormatter.relativeTime(fromEpoch: overview.generatedAt))")
                            if let cadenceText = overview.cadence?.text, !cadenceText.isEmpty {
                                Text("·")
                                Text(cadenceText)
                            } else if let nextEpoch = overview.cadence?.next {
                                Text("·")
                                Text("Next \(FleetFormatter.relativeTime(fromEpoch: nextEpoch))")
                            }
                            if let snoozed = overview.snoozedCount, snoozed > 0 {
                                Text("·")
                                Button("\(snoozed) snoozed") {
                                    model.openBoard(filter: .snoozed)
                                }
                                .foregroundStyle(FleetTheme.info)
                            }
                        }
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                    Spacer()
                }

                if !clusters.isEmpty {
                    VStack(spacing: 8) {
                        ForEach(clusters) { cluster in
                            Button {
                                model.openBoard(filter: cluster.targetFilter)
                            } label: {
                                HStack(spacing: 10) {
                                    Image(systemName: cluster.systemImage)
                                        .font(.subheadline)
                                        .foregroundStyle(FleetTheme.color(forSeverity: cluster.severity))
                                        .frame(width: 24)
                                    VStack(alignment: .leading, spacing: 2) {
                                        Text(cluster.title)
                                            .font(.subheadline.weight(.semibold))
                                            .foregroundStyle(.primary)
                                        Text(cluster.summary)
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                            .lineLimit(1)
                                    }
                                    Spacer()
                                    StatusPill(
                                        label: "\(cluster.count)",
                                        color: FleetTheme.color(forSeverity: cluster.severity)
                                    )
                                    Image(systemName: "chevron.right")
                                        .font(.caption2.weight(.semibold))
                                        .foregroundStyle(.tertiary)
                                }
                                .padding(.horizontal, 10)
                                .padding(.vertical, 8)
                                .background(FleetTheme.elevated, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }

                HStack(spacing: 8) {
                    Button {
                        model.openBoard(filter: .passing)
                    } label: {
                        ScoreTile(count: overview.counts.pass, label: "PASS", color: FleetTheme.pass)
                    }
                    .buttonStyle(.plain)

                    Button {
                        model.openBoard(filter: .warning)
                    } label: {
                        ScoreTile(count: overview.counts.warn, label: "WARN", color: FleetTheme.warn)
                    }
                    .buttonStyle(.plain)

                    Button {
                        model.openBoard(filter: .failing)
                    } label: {
                        ScoreTile(count: overview.counts.fail, label: "FAIL", color: FleetTheme.fail)
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    // MARK: - Tile 2: Morning Priorities

    private func prioritiesCard(_ digest: String) -> some View {
        let sections = parseMorningBrief(digest)
        let first = sections.first
        let previewBullets = Array((first?.bullets ?? []).prefix(3))

        return MissionCard {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Label(first?.title ?? "Morning Priorities", systemImage: "sparkles")
                        .font(.subheadline.weight(.semibold))
                    Spacer()
                    Button("Full Brief") {
                        showFullBriefSheet = true
                    }
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(FleetTheme.info)
                }

                if !previewBullets.isEmpty {
                    VStack(alignment: .leading, spacing: 8) {
                        ForEach(previewBullets) { bullet in
                            HStack(alignment: .top, spacing: 8) {
                                Circle()
                                    .fill(FleetTheme.info)
                                    .frame(width: 6, height: 6)
                                    .padding(.top, 6)
                                VStack(alignment: .leading, spacing: 2) {
                                    if let lead = bullet.lead {
                                        Text(lead)
                                            .font(.subheadline.weight(.semibold))
                                        Text(bullet.body)
                                            .font(.caption)
                                            .foregroundStyle(.secondary)
                                            .lineLimit(2)
                                    } else {
                                        Text(bullet.body)
                                            .font(.caption)
                                            .foregroundStyle(.primary)
                                            .lineLimit(3)
                                    }
                                }
                            }
                        }
                    }
                } else {
                    Text(first?.paragraphs.first ?? digest)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        .lineLimit(4)
                }
            }
        }
    }

    // MARK: - Tile 3: Machines

    private func hostsSection(_ hosts: [HostSummary]) -> some View {
        VStack(alignment: .leading, spacing: 10) {
            Text("MACHINES (\(hosts.count))")
                .font(.caption.weight(.bold))
                .foregroundStyle(.secondary)
                .padding(.horizontal, 4)

            ForEach(hosts) { host in
                HostTelemetryCard(
                    host: host,
                    isExpanded: expandedHosts.contains(host.id),
                    canRunActions: (model.overview?.actionHosts ?? []).contains(host.id.lowercased()),
                    onToggle: {
                        withAnimation(.snappy) {
                            if expandedHosts.contains(host.id) {
                                expandedHosts.remove(host.id)
                            } else {
                                expandedHosts.insert(host.id)
                            }
                        }
                    },
                    onSelectJob: { job in
                        selectedJob = JobPillSelection(hostID: host.id, job: job)
                    },
                    onRunHostAction: { kind in
                        try await model.runHostAction(host: host.id, kind: kind)
                    },
                    onOpenJobsBoard: {
                        model.openBoard(filter: .jobs)
                    }
                )
            }
        }
    }

    // MARK: - Tile 4: AI Capacity

    private func capacitySection(_ capacity: CapacityResponse) -> some View {
        let partition = partitionCapacityPools(capacity.pools)
        return MissionCard {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("AI CAPACITY")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.secondary)
                    Spacer()
                    if let build = capacity.route?.build, !build.isEmpty {
                        StatusPill(label: "build: \(build)", color: FleetTheme.info)
                    }
                    if let review = capacity.route?.review, !review.isEmpty, review != capacity.route?.build {
                        StatusPill(label: "rev: \(review)", color: FleetTheme.pass)
                    }
                }

                if partition.activeGroups.isEmpty && partition.pausedPools.isEmpty {
                    Text("No pool telemetry available.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(partition.activeGroups) { group in
                        VStack(alignment: .leading, spacing: 8) {
                            Text(group.engine.uppercased())
                                .font(.caption.weight(.bold))
                                .foregroundStyle(.secondary)
                            ForEach(group.pools) { pool in
                                CapacityPoolRow(pool: pool)
                            }
                        }
                        .padding(10)
                        .background(FleetTheme.elevated.opacity(0.65), in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                    }

                    if !partition.pausedPools.isEmpty {
                        DisclosureGroup(isExpanded: $showPausedPools) {
                            VStack(alignment: .leading, spacing: 8) {
                                ForEach(partition.pausedPools) { pool in
                                    CapacityPoolRow(pool: pool)
                                }
                                Button("Manage in Controls") {
                                    model.openControls()
                                }
                                .font(.caption.weight(.semibold))
                                .foregroundStyle(FleetTheme.info)
                                .padding(.top, 2)
                            }
                            .padding(.top, 8)
                        } label: {
                            HStack {
                                Image(systemName: "pause.circle")
                                    .foregroundStyle(FleetTheme.warn)
                                Text("Paused & unavailable (\(partition.pausedPools.count))")
                                    .font(.caption.weight(.semibold))
                                    .foregroundStyle(.secondary)
                            }
                        }
                        .tint(.secondary)
                    }
                }
            }
        }
    }

    // MARK: - Tile 5: Backups & Storage Safety

    private func backupsCard(_ backups: OverviewBackups) -> some View {
        let violations = backups.placementViolations ?? 0
        let gdriveSummaryText = gdriveBreakdown(detail: model.backups?.gdrive, summary: backups.gdrive)
        return MissionCard {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("BACKUPS & PLACEMENT")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.secondary)
                    Spacer()
                    Button {
                        model.openBoard(filter: .placement)
                    } label: {
                        HStack(spacing: 4) {
                            StatusPill(
                                label: violations == 0 ? "0 gaps" : "\(violations) gaps",
                                color: violations == 0 ? FleetTheme.pass : FleetTheme.fail
                            )
                            Image(systemName: "chevron.right")
                                .font(.caption2.weight(.semibold))
                                .foregroundStyle(.tertiary)
                        }
                    }
                    .buttonStyle(.plain)
                }

                if let idrive = backups.idrive {
                    VStack(alignment: .leading, spacing: 6) {
                        HStack {
                            Label("IDrive Offsite", systemImage: "externaldrive.connected.to.line.below")
                                .font(.subheadline.weight(.semibold))
                            Spacer()
                            let pillText: String = {
                                if let pct = idrive.percent, idrive.running == true {
                                    return "\(idrive.phase ?? "Running") \(String(format: "%.0f%%", pct))"
                                }
                                return idrive.phase ?? (idrive.running == true ? "Running" : "Idle")
                            }()
                            StatusPill(
                                label: pillText,
                                color: idrive.running == true ? FleetTheme.info : FleetTheme.pass
                            )
                        }
                        if let pct = idrive.percent {
                            ProgressView(value: min(max(pct / 100.0, 0), 1))
                                .tint(idrive.running == true ? FleetTheme.info : FleetTheme.pass)
                        }
                        HStack {
                            if let sent = idrive.sentBytes, let total = idrive.totalBytes, total > 0, idrive.running == true {
                                Text("\(FleetFormatter.formatBytes(sent)) / \(FleetFormatter.formatBytes(total))")
                            } else {
                                Text("Last OK: \(FleetFormatter.relativeTime(fromEpoch: idrive.lastSuccessEpoch))")
                            }
                            Spacer()
                            if let storage = idrive.storage, !storage.isEmpty {
                                Text(storage)
                            }
                        }
                        .font(.caption)
                        .foregroundStyle(.secondary)
                        if idrive.running == true, idrive.lastSuccessEpoch != nil {
                            Text("Last completed run: \(FleetFormatter.relativeTime(fromEpoch: idrive.lastSuccessEpoch)) (\(idrive.lastSuccessStatus ?? "ok"))")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }

                Divider().overlay(FleetTheme.border)

                VStack(alignment: .leading, spacing: 6) {
                    Text("HOME MIRRORS")
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(.secondary)
                    if let mirrors = backups.homeMirror, !mirrors.isEmpty {
                        ForEach(mirrors.keys.sorted(), id: \.self) { host in
                            if let item = mirrors[host] {
                                let cleaned = cleanMirrorState(item.state)
                                HStack(alignment: .center, spacing: 8) {
                                    Text(host.uppercased())
                                        .font(.caption.weight(.bold).monospaced())
                                        .frame(width: 28, alignment: .leading)
                                    if cleaned.isSkipped {
                                        StatusPill(label: "Skipped", color: FleetTheme.warn)
                                    }
                                    Text(cleaned.reason)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                        .lineLimit(1)
                                    Spacer()
                                    if let planned = item.plannedBytes, planned > 0 {
                                        Text(FleetFormatter.formatBytes(planned))
                                            .font(.caption2.monospacedDigit())
                                            .foregroundStyle(.tertiary)
                                    }
                                }
                            }
                        }
                    } else {
                        Text("No mirror reports yet.")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                Divider().overlay(FleetTheme.border)

                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Google Drive Sync")
                            .font(.subheadline.weight(.semibold))
                        Text(gdriveSummaryText)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("View Gaps") {
                        model.openBoard(filter: .backups)
                    }
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(FleetTheme.info)
                }
            }
        }
    }

    // MARK: - Tile 6: Security Posture

    private func securityPostureCard(_ overview: FleetOverviewResponse) -> some View {
        let sec = model.security
        let namespaces = sec?.namespaces ?? [:]
        let audits = sec?.namespaceAudits ?? [:]
        let tripwireOpen = sec?.localTripwireOpen ?? 0

        let liveStatus: String = {
            if !namespaces.isEmpty {
                let anyFail = namespaces.values.contains { ($0.status ?? "").lowercased() == "fail" }
                return anyFail ? "fail" : "pass"
            }
            return overview.security.status
        }()

        let chainSummary: String = {
            let parts = namespaces.keys.sorted().compactMap { ns -> String? in
                guard let info = namespaces[ns] else { return nil }
                let integrity = info.integrity ?? info.status ?? "ok"
                if let silent = info.silentMin {
                    return "\(ns.uppercased()) \(integrity) · \(String(format: "%.0f", silent))m"
                }
                return "\(ns.uppercased()) \(integrity)"
            }
            return parts.isEmpty ? (overview.security.detail ?? "Chain intact") : parts.joined(separator: "  ·  ")
        }()

        let auditFailTotal = audits.values.reduce(0) { $0 + ($1.failCount ?? 0) }
        let auditWarnTotal = audits.values.reduce(0) { $0 + ($1.warnCount ?? 0) }

        return MissionCard {
            VStack(alignment: .leading, spacing: 10) {
                Text("SECURITY POSTURE")
                    .font(.caption.weight(.bold))
                    .foregroundStyle(.secondary)

                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Live Evidence Chain")
                            .font(.subheadline.weight(.semibold))
                        Text(chainSummary)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    StatusPill(label: liveStatus, color: FleetTheme.color(forStatus: liveStatus))
                }

                Divider().overlay(FleetTheme.border)

                Button {
                    model.openBoard(filter: .security)
                } label: {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("Daily Audit & Tripwire")
                                .font(.subheadline.weight(.semibold))
                                .foregroundStyle(.primary)
                            Text("Audit: \(auditFailTotal) fail, \(auditWarnTotal) warn · \(tripwireOpen) local tripwire open")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                        StatusPill(
                            label: overview.security.status,
                            color: FleetTheme.color(forStatus: overview.security.status)
                        )
                        Image(systemName: "chevron.right")
                            .font(.caption2.weight(.semibold))
                            .foregroundStyle(.tertiary)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
            }
        }
    }
}

// MARK: - Subviews

private struct ScoreTile: View {
    let count: Int
    let label: String
    let color: Color

    var body: some View {
        HStack(spacing: 6) {
            Text("\(count)")
                .font(.headline.monospacedDigit())
                .foregroundStyle(color)
            Text(label)
                .font(.caption2.weight(.bold))
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 8)
        .background(FleetTheme.elevated, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
    }
}

private struct HostTelemetryCard: View {
    let host: HostSummary
    let isExpanded: Bool
    let canRunActions: Bool
    let onToggle: () -> Void
    let onSelectJob: (FailingJob) -> Void
    let onRunHostAction: (String) async throws -> ActionResultResponse
    let onOpenJobsBoard: () -> Void

    @State private var showUnmountedDrives = false
    @State private var runningActionKind: String?
    @State private var actionFeedback: String?

    private var verdict: HostVerdict { hostHeadlineVerdict(for: host) }
    private var diskInfo: DiskThresholdInfo { diskThresholdInfo(for: host) }

    private var diskBarColor: Color {
        switch diskInfo.state {
        case .critical: return FleetTheme.fail
        case .belowTarget: return FleetTheme.warn
        case .healthy, .unknown: return FleetTheme.pass
        }
    }

    private var thermalAlertText: String? {
        let t = (host.thermal ?? "").lowercased()
        if t == "heavy" || t == "critical" {
            return t
        }
        return nil
    }

    var body: some View {
        let jobs = host.failingJobs ?? []
        let failCount = host.failingJobsCount ?? jobs.count
        let syncErrs = (host.syncthing?.errors ?? 0) + (host.syncthing?.outOfSync ?? 0)

        return MissionCard {
            VStack(alignment: .leading, spacing: 10) {
                Button(action: onToggle) {
                    HStack(spacing: 8) {
                        Circle()
                            .fill(FleetTheme.color(forSeverity: verdict.severity))
                            .frame(width: 9, height: 9)
                        Text(host.id.uppercased())
                            .font(.headline.monospaced())
                        StatusPill(label: verdict.label, color: FleetTheme.color(forSeverity: verdict.severity))
                        if host.ownerActive == true {
                            StatusPill(label: "In Use", color: FleetTheme.info)
                        }
                        Spacer()
                        if let batt = host.battPct {
                            Label("\(batt)%", systemImage: host.ac == true ? "bolt.fill" : "battery.75")
                                .font(.caption2.monospacedDigit())
                                .foregroundStyle(.secondary)
                        }
                        Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.secondary)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)

                // Dedicated Disk Health block
                VStack(alignment: .leading, spacing: 4) {
                    HStack(alignment: .firstTextBaseline) {
                        Text(diskInfo.headline)
                            .font(.title3.weight(.bold).monospacedDigit())
                            .foregroundStyle(diskBarColor == FleetTheme.pass ? .primary : diskBarColor)
                        Spacer()
                        Text(diskInfo.thresholdSummary)
                            .font(.caption)
                            .foregroundStyle(diskInfo.state == .critical ? FleetTheme.fail : .secondary)
                    }
                    if diskInfo.usedFraction > 0 {
                        ProgressView(value: diskInfo.usedFraction)
                            .tint(diskBarColor)
                    }
                }

                // Compact 3-stat strip + snapshot age
                HStack(spacing: 12) {
                    HStack(spacing: 4) {
                        Text("Chip")
                            .foregroundStyle(.secondary)
                        Text(host.chipC.map { String(format: "%.0f°C", $0) } ?? "—")
                            .fontWeight(.semibold)
                            .monospacedDigit()
                        if let th = thermalAlertText {
                            StatusPill(label: th, color: FleetTheme.warn)
                        }
                    }
                    Spacer()
                    HStack(spacing: 4) {
                        Text("CPU")
                            .foregroundStyle(.secondary)
                        Text(host.cpuPct.map { String(format: "%.0f%%", $0) } ?? "—")
                            .fontWeight(.semibold)
                            .monospacedDigit()
                    }
                    Spacer()
                    Text(FleetFormatter.relativeTime(fromEpoch: host.collectedAt))
                        .foregroundStyle(.secondary)
                }
                .font(.caption)

                // Inline tappable failing jobs & sync alert without expanding
                if !jobs.isEmpty || syncErrs > 0 {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 6) {
                            ForEach(jobs.prefix(3)) { job in
                                Button {
                                    onSelectJob(job)
                                } label: {
                                    StatusPill(label: shortJobName(job.label), color: FleetTheme.fail)
                                }
                                .buttonStyle(.plain)
                            }
                            if failCount > 3 {
                                Button {
                                    onOpenJobsBoard()
                                } label: {
                                    StatusPill(label: "+\(failCount - 3) more", color: FleetTheme.fail)
                                }
                                .buttonStyle(.plain)
                            }
                            if syncErrs > 0 {
                                StatusPill(label: "Sync: \(syncErrs) pending", color: FleetTheme.warn)
                            }
                        }
                    }
                }

                if isExpanded {
                    Divider().overlay(FleetTheme.border)
                    expandedDetails
                }
            }
        }
    }

    @ViewBuilder
    private var expandedDetails: some View {
        let allDrives = host.drives ?? []
        let mountedDrives = allDrives.filter { $0.mounted != false }
        let unmountedDrives = allDrives.filter { $0.mounted == false }

        VStack(alignment: .leading, spacing: 10) {
            if canRunActions {
                HStack(spacing: 8) {
                    Button {
                        Task { await triggerAction("snapshot") }
                    } label: {
                        HStack(spacing: 5) {
                            if runningActionKind == "snapshot" {
                                ProgressView().controlSize(.mini)
                            } else {
                                Image(systemName: "arrow.clockwise")
                            }
                            Text("Refresh Snapshot")
                        }
                        .font(.caption.weight(.semibold))
                        .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.bordered)
                    .disabled(runningActionKind != nil)

                    Button {
                        Task { await triggerAction("free-space-guard") }
                    } label: {
                        HStack(spacing: 5) {
                            if runningActionKind == "free-space-guard" {
                                ProgressView().controlSize(.mini)
                            } else {
                                Image(systemName: "wrench.and.screwdriver")
                            }
                            Text("Trim Caches")
                        }
                        .font(.caption.weight(.semibold))
                        .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.bordered)
                    .disabled(runningActionKind != nil)
                }

                if let actionFeedback {
                    Text(actionFeedback)
                        .font(.caption2)
                        .foregroundStyle(FleetTheme.info)
                }
            }

            if let samples = host.samples?.filter({ $0.chipC != nil }), samples.count >= 2 {
                Text("THERMAL TREND")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(.secondary)
                Chart(samples) { sample in
                    LineMark(
                        x: .value("Time", Date(timeIntervalSince1970: TimeInterval(sample.t))),
                        y: .value("Temp", sample.chipC ?? 0)
                    )
                    .foregroundStyle(FleetTheme.info)
                    AreaMark(
                        x: .value("Time", Date(timeIntervalSince1970: TimeInterval(sample.t))),
                        y: .value("Temp", sample.chipC ?? 0)
                    )
                    .foregroundStyle(FleetTheme.info.opacity(0.15))
                }
                .chartXAxis(.hidden)
                .frame(height: 82)
            }

            HStack(spacing: 12) {
                if let memUsed = host.memUsedGB, let memTot = host.memTotalGB {
                    Text("RAM: \(String(format: "%.1f/%.0f GB", memUsed, memTot)) (\(host.memPressure ?? "—"))")
                }
                Spacer()
                if let gl = host.glancebar?.state {
                    Text("Glancebar: \(gl)")
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)

            if !mountedDrives.isEmpty {
                Text("MOUNTED DRIVES (\(mountedDrives.count))")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(.secondary)
                ForEach(mountedDrives) { drive in
                    HStack {
                        Text(drive.name)
                            .font(.caption.weight(.semibold))
                        Spacer()
                        if let trend = drive.trend24hGB, abs(trend) >= 0.5 {
                            Text(String(format: "%+.0f GB/24h", trend))
                                .font(.caption2.monospacedDigit())
                                .foregroundStyle(.tertiary)
                        }
                        if let totalGB = drive.totalGB {
                            Text("\(FleetFormatter.formatGB(drive.freeGB)) / \(FleetFormatter.formatGB(totalGB))")
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(.secondary)
                        } else {
                            Text("\(FleetFormatter.formatGB(drive.freeGB)) free")
                                .font(.caption.monospacedDigit())
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }

            if !unmountedDrives.isEmpty {
                DisclosureGroup(isExpanded: $showUnmountedDrives) {
                    VStack(alignment: .leading, spacing: 4) {
                        ForEach(unmountedDrives) { drive in
                            Text("• \(drive.name)")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .padding(.top, 4)
                } label: {
                    Text("Unmounted drives (\(unmountedDrives.count))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                .tint(.secondary)
            }

            if let jobs = host.failingJobs, !jobs.isEmpty {
                Text("FAILING LAUNCHD JOBS (TAP FOR LOGS & ACTIONS)")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(FleetTheme.fail)
                ForEach(jobs) { job in
                    Button {
                        onSelectJob(job)
                    } label: {
                        HStack {
                            VStack(alignment: .leading, spacing: 1) {
                                Text(shortJobName(job.label))
                                    .font(.caption.weight(.semibold))
                                    .foregroundStyle(.primary)
                                Text("\(job.label) · \(job.when ?? "scheduled") · exit \(job.exit.map(String.init) ?? "—")")
                                    .font(.caption2.monospaced())
                                    .foregroundStyle(.secondary)
                            }
                            Spacer()
                            Image(systemName: "chevron.right")
                                .font(.caption2)
                                .foregroundStyle(.tertiary)
                        }
                        .padding(.vertical, 2)
                        .contentShape(Rectangle())
                    }
                    .buttonStyle(.plain)
                }
            }
        }
    }

    private func triggerAction(_ kind: String) async {
        runningActionKind = kind
        defer { runningActionKind = nil }
        do {
            let res = try await onRunHostAction(kind)
            actionFeedback = res.detail ?? "Started \(kind)."
        } catch {
            actionFeedback = error.localizedDescription
        }
    }
}

private struct CapacityPoolRow: View {
    let pool: CapacityPool

    private var pct: Double {
        min(max(pool.remainingPct ?? 0, 0), 100)
    }

    private var barColor: Color {
        if pool.paused == true { return FleetTheme.warn }
        if pool.unavailable == true || pct < 10 { return FleetTheme.fail }
        if pct < 25 { return FleetTheme.warn }
        return FleetTheme.pass
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 4) {
            HStack {
                Text(displayPoolName(pool))
                    .font(.subheadline.weight(.medium))
                if pool.paused == true {
                    StatusPill(label: "paused", color: FleetTheme.warn)
                } else if pool.stale == true {
                    StatusPill(label: "stale", color: FleetTheme.warn)
                }
                Spacer()
                if let rem = pool.remainingPct {
                    Text(String(format: "%.0f%%", rem))
                        .font(.subheadline.weight(.bold).monospacedDigit())
                        .foregroundStyle(barColor)
                } else {
                    Text(pool.unavailable == true ? "unavailable" : "—")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }

            ProgressView(value: pct / 100.0)
                .tint(barColor)

            HStack {
                if let reason = pool.unavailableReason, !reason.isEmpty {
                    Text(reason)
                        .lineLimit(1)
                } else if let tokText = FleetFormatter.formatCompactTokens(pool.tokensLeft) {
                    if let jobs = pool.jobsLeft {
                        Text("\(tokText) · ~\(Int(jobs)) jobs")
                    } else {
                        Text(tokText)
                    }
                } else if let jobs = pool.jobsLeft {
                    Text("~\(Int(jobs)) jobs left")
                }
                Spacer()
                if let countdown = FleetFormatter.resetCountdown(fromISO: pool.resetAt) {
                    Text(countdown)
                } else if let fresh = pool.freshness, !fresh.isEmpty {
                    Text(fresh)
                }
            }
            .font(.caption2)
            .foregroundStyle(.secondary)
        }
    }
}

private struct MorningBriefSheet: View {
    @Bindable var model: FleetAppModel
    let sections: [BriefSection]
    let dateLabel: String?
    @Environment(\.dismiss) private var dismiss

    @State private var dispatchedIDs: Set<String> = []
    @State private var statusBanner: String?

    var body: some View {
        NavigationStack {
            List {
                if let statusBanner {
                    Section {
                        Text(statusBanner)
                            .font(.caption)
                            .foregroundStyle(FleetTheme.info)
                    }
                }

                ForEach(sections) { section in
                    Section(section.title) {
                        ForEach(section.bullets) { bullet in
                            VStack(alignment: .leading, spacing: 5) {
                                if let lead = bullet.lead {
                                    Text(lead)
                                        .font(.subheadline.weight(.semibold))
                                }
                                Text(bullet.body)
                                    .font(.callout)
                                    .foregroundStyle(bullet.lead != nil ? .secondary : .primary)

                                HStack {
                                    Spacer()
                                    if dispatchedIDs.contains(bullet.id) {
                                        Label("Dispatched", systemImage: "checkmark.circle.fill")
                                            .font(.caption2.weight(.semibold))
                                            .foregroundStyle(FleetTheme.pass)
                                    } else {
                                        Button("Ask Agent") {
                                            Task { await dispatchBullet(bullet) }
                                        }
                                        .font(.caption2.weight(.semibold))
                                        .foregroundStyle(FleetTheme.info)
                                    }
                                }
                            }
                            .padding(.vertical, 2)
                        }
                        ForEach(section.paragraphs, id: \.self) { para in
                            Text(para)
                                .font(.callout)
                        }
                    }
                }
            }
            .navigationTitle(dateLabel.map { "Brief · \($0)" } ?? "Morning Brief")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .preferredColorScheme(.dark)
    }

    private func dispatchBullet(_ bullet: BriefBullet) async {
        let host = model.overview?.actionHosts?.first ?? model.overview?.hosts.first?.id ?? "hub"
        let target = "\(host):fleet"
        let text = [bullet.lead, bullet.body].compactMap { $0 }.joined(separator: ": ")
        do {
            _ = try await model.dispatchFix(to: target, title: bullet.lead, prompt: text)
            dispatchedIDs.insert(bullet.id)
            statusBanner = "Dispatched priority item to \(target)."
        } catch {
            statusBanner = error.localizedDescription
        }
    }
}

import Charts
import SwiftUI

struct OverviewView: View {
    @Bindable var model: FleetAppModel
    @State private var expandedHosts: Set<String> = []
    @State private var showAgyDigest = false

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
                        scoreboardCard(overview)

                        if let digest = overview.agyMorning, !digest.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty {
                            digestCard(digest)
                        }

                        hostsSection(overview.hosts)
                        capacitySection(overview.capacity)
                        backupsCard(overview.backups)
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
        }
    }

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
                    model.selectedTab = .controls
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
                    Text("Sync Issue")
                        .font(.subheadline.weight(.semibold))
                    Text(message)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
        }
    }

    private func scoreboardCard(_ overview: FleetOverviewResponse) -> some View {
        MissionCard {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("MORNING AUDIT")
                            .font(.caption.weight(.bold))
                            .foregroundStyle(.secondary)
                        Text(overview.date ?? "Today")
                            .font(.headline)
                    }
                    Spacer()
                    StatusPill(
                        label: overview.security.status,
                        color: FleetTheme.color(forStatus: overview.security.status)
                    )
                }

                HStack(spacing: 10) {
                    ScoreTile(count: overview.counts.pass, label: "PASS", color: FleetTheme.pass)
                    ScoreTile(count: overview.counts.warn, label: "WARN", color: FleetTheme.warn)
                    ScoreTile(count: overview.counts.fail, label: "FAIL", color: FleetTheme.fail)
                }

                HStack {
                    Label("Generated \(FleetFormatter.relativeTime(fromEpoch: overview.generatedAt))", systemImage: "clock")
                    Spacer()
                    if let nextEpoch = overview.cadence?.next {
                        Text("Next \(FleetFormatter.relativeTime(fromEpoch: nextEpoch))")
                    } else if let cadenceText = overview.cadence?.text, !cadenceText.isEmpty {
                        Text(cadenceText)
                    }
                }
                .font(.caption)
                .foregroundStyle(.secondary)
            }
        }
    }

    private func digestCard(_ digest: String) -> some View {
        MissionCard {
            DisclosureGroup(isExpanded: $showAgyDigest) {
                Text(digest)
                    .font(.caption.monospaced())
                    .foregroundStyle(.secondary)
                    .padding(.top, 8)
                    .textSelection(.enabled)
            } label: {
                HStack {
                    Label("Morning Digest Summary", systemImage: "doc.text.magnifyingglass")
                        .font(.subheadline.weight(.semibold))
                    Spacer()
                    Text(showAgyDigest ? "Hide" : "Read")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            }
            .tint(.primary)
        }
    }

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
                    onToggle: {
                        withAnimation(.snappy) {
                            if expandedHosts.contains(host.id) {
                                expandedHosts.remove(host.id)
                            } else {
                                expandedHosts.insert(host.id)
                            }
                        }
                    }
                )
            }
        }
    }

    private func capacitySection(_ capacity: CapacityResponse) -> some View {
        MissionCard {
            VStack(alignment: .leading, spacing: 12) {
                HStack {
                    Text("AI CAPACITY")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.secondary)
                    Spacer()
                    if let build = capacity.route?.build, !build.isEmpty {
                        StatusPill(label: "build: \(build)", color: FleetTheme.info)
                    }
                }

                if capacity.pools.isEmpty {
                    Text("No pool telemetry available.")
                        .font(.subheadline)
                        .foregroundStyle(.secondary)
                } else {
                    ForEach(capacity.pools) { pool in
                        CapacityPoolRow(pool: pool)
                    }
                }
            }
        }
    }

    private func backupsCard(_ backups: OverviewBackups) -> some View {
        MissionCard {
            VStack(alignment: .leading, spacing: 10) {
                HStack {
                    Text("BACKUPS & PLACEMENT")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.secondary)
                    Spacer()
                    let violations = backups.placementViolations ?? 0
                    StatusPill(
                        label: violations == 0 ? "0 violations" : "\(violations) violations",
                        color: violations == 0 ? FleetTheme.pass : FleetTheme.warn
                    )
                }

                if let idrive = backups.idrive {
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text("IDrive")
                                .font(.subheadline.weight(.semibold))
                            Spacer()
                            Text(idrive.phase ?? (idrive.running == true ? "running" : "idle"))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        if let pct = idrive.percent {
                            ProgressView(value: min(max(pct / 100.0, 0), 1))
                                .tint(FleetTheme.pass)
                        }
                        HStack {
                            Text("Last success: \(FleetFormatter.relativeTime(fromEpoch: idrive.lastSuccessEpoch))")
                            Spacer()
                            if let storage = idrive.storage, !storage.isEmpty {
                                Text(storage)
                            }
                        }
                        .font(.caption)
                        .foregroundStyle(.secondary)
                    }
                }

                Divider().overlay(FleetTheme.border)

                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text("Google Drive")
                            .font(.subheadline.weight(.semibold))
                        Text("\(backups.gdrive?.okCount ?? 0)/\(backups.gdrive?.total ?? 0) OK · \(backups.gdrive?.activeRuns ?? 0) active")
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                    Spacer()
                    if let mirrors = backups.homeMirror, !mirrors.isEmpty {
                        VStack(alignment: .trailing, spacing: 2) {
                            Text("Home Mirrors")
                                .font(.subheadline.weight(.semibold))
                            Text(mirrors.map { "\($0.key.uppercased()): \($0.value.state ?? "—")" }.sorted().joined(separator: " · "))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
    }
}

private struct ScoreTile: View {
    let count: Int
    let label: String
    let color: Color

    var body: some View {
        VStack(spacing: 4) {
            Text("\(count)")
                .font(.system(.title, design: .rounded).weight(.bold))
                .monospacedDigit()
                .foregroundStyle(color)
            Text(label)
                .font(.caption2.weight(.bold))
                .foregroundStyle(.secondary)
        }
        .frame(maxWidth: .infinity)
        .padding(.vertical, 10)
        .background(FleetTheme.elevated, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
    }
}

private struct HostTelemetryCard: View {
    let host: HostSummary
    let isExpanded: Bool
    let onToggle: () -> Void

    private var statusColor: Color {
        if (host.failCount ?? 0) > 0 || host.ok == false { return FleetTheme.fail }
        if (host.warnCount ?? 0) > 0 || (host.failingJobsCount ?? 0) > 0 { return FleetTheme.warn }
        return FleetTheme.pass
    }

    var body: some View {
        MissionCard {
            VStack(alignment: .leading, spacing: 10) {
                Button(action: onToggle) {
                    HStack(spacing: 10) {
                        Circle()
                            .fill(statusColor)
                            .frame(width: 10, height: 10)
                        Text(host.id.uppercased())
                            .font(.headline.monospaced())
                        if host.ownerActive == true {
                            StatusPill(label: "active", color: FleetTheme.info)
                        }
                        Spacer()
                        Text(FleetFormatter.relativeTime(fromEpoch: host.collectedAt))
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        Image(systemName: isExpanded ? "chevron.up" : "chevron.down")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(.secondary)
                    }
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)

                HStack(spacing: 12) {
                    metricPill(title: "CHIP", value: host.chipC.map { String(format: "%.0f°C", $0) } ?? "—")
                    metricPill(title: "CPU", value: host.cpuPct.map { String(format: "%.0f%%", $0) } ?? "—")
                    metricPill(title: "FREE", value: FleetFormatter.formatGB(host.diskFreeGB))
                    metricPill(title: "JOBS", value: "\(host.failingJobsCount ?? 0) fail")
                }

                if let free = host.diskFreeGB, let total = host.diskTotalGB, total > 0 {
                    let freeFraction = min(max(free / total, 0), 1)
                    let belowFloor = (host.diskFloorGB ?? 0) > 0 && free < (host.diskFloorGB ?? 0)
                    ProgressView(value: 1.0 - freeFraction)
                        .tint(belowFloor ? FleetTheme.fail : FleetTheme.pass)
                }

                if isExpanded {
                    Divider().overlay(FleetTheme.border)
                    expandedDetails
                }
            }
        }
    }

    private func metricPill(title: String, value: String) -> some View {
        VStack(alignment: .leading, spacing: 2) {
            Text(title)
                .font(.caption2.weight(.bold))
                .foregroundStyle(.secondary)
            Text(value)
                .font(.subheadline.weight(.semibold).monospacedDigit())
        }
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    @ViewBuilder
    private var expandedDetails: some View {
        VStack(alignment: .leading, spacing: 10) {
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
                .frame(height: 86)
            }

            HStack(spacing: 12) {
                if let memUsed = host.memUsedGB, let memTot = host.memTotalGB {
                    Text("RAM: \(String(format: "%.1f/%.0f GB", memUsed, memTot)) (\(host.memPressure ?? "—"))")
                }
                Spacer()
                if let batt = host.battPct {
                    Text("Batt: \(batt)%\(host.ac == true ? " AC" : "")")
                }
            }
            .font(.caption)
            .foregroundStyle(.secondary)

            if let sync = host.syncthing {
                Text("Syncthing: \(sync.errors ?? 0) err · \(sync.outOfSync ?? 0) out-of-sync · Glancebar: \(host.glancebar?.state ?? "—")")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }

            if let drives = host.drives, !drives.isEmpty {
                Text("DRIVES")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(.secondary)
                ForEach(drives) { drive in
                    HStack {
                        Text(drive.name)
                            .font(.caption.weight(.semibold))
                        if drive.mounted == false {
                            StatusPill(label: "unmounted", color: FleetTheme.warn)
                        }
                        Spacer()
                        if let totalGB = drive.totalGB {
                            Text("\(FleetFormatter.formatGB(drive.freeGB)) free / \(FleetFormatter.formatGB(totalGB))")
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

            if let jobs = host.failingJobs, !jobs.isEmpty {
                Text("FAILING LAUNCHD JOBS")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(FleetTheme.fail)
                ForEach(jobs) { job in
                    Text("• \(job.label) (\(job.state ?? "—"), exit \(job.exit.map(String.init) ?? "—"))")
                        .font(.caption.monospaced())
                        .foregroundStyle(.secondary)
                }
            }
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
        if pool.unavailable == true || pct < 15 { return FleetTheme.fail }
        if pct < 35 { return FleetTheme.warn }
        return FleetTheme.pass
    }

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack {
                Text("\(pool.engine) · \(pool.pool)")
                    .font(.subheadline.weight(.semibold))
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

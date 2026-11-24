import SwiftUI
import UIKit

// MARK: - Live Job Log Box

struct JobLogBox: View {
    let log: JobLogResponse

    var body: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("State: \(log.state ?? "unknown")")
                    .font(.caption.weight(.semibold))
                if let exit = log.lastExit {
                    Text("· exit \(exit)")
                        .font(.caption.monospacedDigit())
                        .foregroundStyle(exit == "0" ? FleetTheme.pass : FleetTheme.warn)
                }
                Spacer()
                Button("Copy") {
                    let combined = [log.stderr, log.stdout]
                        .compactMap { $0 }
                        .filter { !$0.isEmpty }
                        .joined(separator: "\n---\n")
                    UIPasteboard.general.string = combined
                }
                .font(.caption.weight(.semibold))
                .foregroundStyle(FleetTheme.info)
            }

            if let err = log.error, !err.isEmpty {
                Text(err)
                    .font(.caption)
                    .foregroundStyle(FleetTheme.fail)
            }

            if let stderr = log.stderr, !stderr.isEmpty {
                VStack(alignment: .leading, spacing: 3) {
                    Text(log.stderrPath ?? "stderr")
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                    ScrollView(.horizontal, showsIndicators: true) {
                        Text(stderr)
                            .font(.caption2.monospaced())
                            .textSelection(.enabled)
                            .padding(8)
                    }
                    .frame(maxHeight: 200)
                    .background(FleetTheme.bg, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                }
            }

            if let stdout = log.stdout, !stdout.isEmpty {
                VStack(alignment: .leading, spacing: 3) {
                    Text(log.stdoutPath ?? "stdout")
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                    ScrollView(.horizontal, showsIndicators: true) {
                        Text(stdout)
                            .font(.caption2.monospaced())
                            .textSelection(.enabled)
                            .padding(8)
                    }
                    .frame(maxHeight: 160)
                    .background(FleetTheme.bg, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                }
            }

            if (log.stderr ?? "").isEmpty && (log.stdout ?? "").isEmpty && (log.error ?? "").isEmpty {
                Text("No log lines found on disk.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
        .padding(.vertical, 4)
    }
}

// MARK: - Board Row Action Sheet

struct RowActionSheet: View {
    @Bindable var model: FleetAppModel
    let row: MorningRow
    @Environment(\.dismiss) private var dismiss

    @State private var jobLog: JobLogResponse?
    @State private var isLoadingLog = false
    @State private var isRunningAction = false
    @State private var isDispatching = false
    @State private var isUpdatingSnooze = false
    @State private var dispatchTarget = ""
    @State private var dispatchPrompt = ""
    @State private var feedback: String?
    @State private var feedbackIsError = false

    private var actionHosts: [String] {
        model.overview?.actionHosts ?? []
    }

    private var hosts: [HostSummary] {
        model.overview?.hosts ?? []
    }

    private var jobTarget: JobTarget? {
        extractJobTarget(from: row)
    }

    private var hostAction: HostActionRecommendation? {
        recommendedHostAction(for: row, actionHosts: actionHosts, knownHostIDs: hosts.map(\.id))
    }

    private var targetChoices: [String] {
        let base = hosts.map { "\($0.id):fleet" }
        return base.isEmpty ? ["hub:fleet", "primary:fleet"] : base
    }

    var body: some View {
        NavigationStack {
            List {
                if let feedback {
                    Section {
                        Text(feedback)
                            .font(.subheadline)
                            .foregroundStyle(feedbackIsError ? FleetTheme.fail : FleetTheme.pass)
                    }
                }

                Section("Check") {
                    HStack {
                        Text(row.name)
                            .font(.headline)
                        Spacer()
                        if row.isSnoozed {
                            StatusPill(label: "Snoozed", color: FleetTheme.info)
                        } else {
                            StatusPill(label: row.status, color: FleetTheme.color(forSeverity: row.severity))
                        }
                    }
                    if let val = row.value, !val.isEmpty {
                        LabeledContent("Value", value: val)
                    }
                    LabeledContent("Key", value: row.key)
                    if let area = row.area {
                        LabeledContent("Area", value: area)
                    }
                    if let detail = row.detail, !detail.isEmpty {
                        Text(detail)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                }

                directActionsSection

                if let jobLog {
                    Section("Live Logs (\(jobLog.label ?? ""))") {
                        JobLogBox(log: jobLog)
                    }
                }

                askAgentSection
                snoozeSection
            }
            .navigationTitle("Take Action")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .onAppear {
                dispatchTarget = suggestedDispatchTarget(for: row, hosts: hosts, actionHosts: actionHosts)
                dispatchPrompt = suggestedFixPrompt(for: row)
            }
        }
    }

    @ViewBuilder
    private var directActionsSection: some View {
        if let job = jobTarget {
            let allowed = actionHosts.contains(job.host)
            Section {
                if allowed {
                    Button {
                        Task { await loadLogs(host: job.host, label: job.label) }
                    } label: {
                        HStack {
                            Label("View Live Logs", systemImage: "doc.text.magnifyingglass")
                            Spacer()
                            if isLoadingLog { ProgressView().controlSize(.small) }
                        }
                    }
                    .disabled(isLoadingLog)

                    Button {
                        Task { await restartJob(host: job.host, label: job.label) }
                    } label: {
                        HStack {
                            Label("Restart / Run Job Now", systemImage: "arrow.clockwise.circle.fill")
                            Spacer()
                            if isRunningAction {
                                ProgressView().controlSize(.small)
                            } else {
                                Image(systemName: "faceid").foregroundStyle(.secondary)
                            }
                        }
                    }
                    .disabled(isRunningAction)
                } else {
                    Text("Direct job controls are restricted for host \(job.host.uppercased()). Use Ask Agent or Snooze below.")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
            } header: {
                Text("Direct Job Control (\(job.host.uppercased()))")
            } footer: {
                Text(job.label)
                    .font(.caption2.monospaced())
            }
        } else if let rec = hostAction {
            Section("Direct Maintenance") {
                Button {
                    Task { await runMaintenance(host: rec.host, kind: rec.kind) }
                } label: {
                    HStack {
                        Label(rec.title, systemImage: rec.systemImage)
                        Spacer()
                        if isRunningAction {
                            ProgressView().controlSize(.small)
                        } else {
                            Image(systemName: "faceid").foregroundStyle(.secondary)
                        }
                    }
                }
                .disabled(isRunningAction)
            }
        }
    }

    private var askAgentSection: some View {
        Section("Ask Agent to Fix") {
            TextField("Target (host:project)", text: $dispatchTarget)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .font(.subheadline.monospaced())

            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(targetChoices, id: \.self) { chip in
                        Button(chip) {
                            dispatchTarget = chip
                        }
                        .font(.caption.monospaced())
                        .padding(.horizontal, 10)
                        .padding(.vertical, 4)
                        .background(FleetTheme.elevated, in: Capsule())
                    }
                }
            }

            TextField("Fix instructions", text: $dispatchPrompt, axis: .vertical)
                .lineLimit(3...6)
                .font(.callout)

            if let errLog = jobLog?.stderr, !errLog.isEmpty {
                Button("Append Last Log Lines") {
                    let tail = errLog.components(separatedBy: .newlines).suffix(8).joined(separator: "\n")
                    if !dispatchPrompt.contains(tail) {
                        dispatchPrompt += "\n\nRecent stderr:\n\(tail)"
                    }
                }
                .font(.caption)
                .foregroundStyle(FleetTheme.info)
            }

            Button {
                Task { await sendFix() }
            } label: {
                HStack {
                    if isDispatching { ProgressView().controlSize(.small) }
                    Label("Dispatch Fix to Agent", systemImage: "paperplane.fill")
                        .fontWeight(.semibold)
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(FleetTheme.pass)
            .disabled(isDispatching || dispatchTarget.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || dispatchPrompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        }
    }

    private var snoozeSection: some View {
        Section {
            if row.isSnoozed {
                if let until = row.snoozedUntil {
                    Text("Snoozed until \(FleetFormatter.relativeTime(fromTimestamp: until)) (\(until))")
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }
                Button("Unsnooze Item", role: .destructive) {
                    Task { await setSnooze(clear: true, hours: 0) }
                }
                .disabled(isUpdatingSnooze)
            } else {
                HStack(spacing: 12) {
                    Button("Snooze 24h") {
                        Task { await setSnooze(clear: false, hours: 24) }
                    }
                    .buttonStyle(.bordered)
                    .frame(maxWidth: .infinity)

                    Button("Snooze 7d") {
                        Task { await setSnooze(clear: false, hours: 168) }
                    }
                    .buttonStyle(.bordered)
                    .frame(maxWidth: .infinity)
                }
                .disabled(isUpdatingSnooze)
            }
        } header: {
            Text("Snooze Alert")
        } footer: {
            Text("Snoozing hides known items from active issue counts without changing underlying monitors.")
        }
    }

    private func loadLogs(host: String, label: String) async {
        isLoadingLog = true
        defer { isLoadingLog = false }
        do {
            jobLog = try await model.fetchJobLog(host: host, label: label)
            feedbackIsError = false
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }

    private func restartJob(host: String, label: String) async {
        isRunningAction = true
        defer { isRunningAction = false }
        do {
            let res = try await model.kickstartJob(host: host, label: label)
            feedback = res.detail ?? "Kickstarted \(label)."
            feedbackIsError = !res.ok
            jobLog = try? await model.fetchJobLog(host: host, label: label)
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }

    private func runMaintenance(host: String, kind: String) async {
        isRunningAction = true
        defer { isRunningAction = false }
        do {
            let res = try await model.runHostAction(host: host, kind: kind)
            feedback = res.detail ?? "Started \(kind) on \(host.uppercased())."
            feedbackIsError = !res.ok
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }

    private func sendFix() async {
        isDispatching = true
        defer { isDispatching = false }
        do {
            let res = try await model.dispatchFix(to: dispatchTarget, key: row.key, title: row.name, prompt: dispatchPrompt)
            feedback = res.detail ?? "Dispatched to \(dispatchTarget)."
            feedbackIsError = false
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }

    private func setSnooze(clear: Bool, hours: Int) async {
        isUpdatingSnooze = true
        defer { isUpdatingSnooze = false }
        do {
            if clear {
                _ = try await model.unsnoozeItem(key: row.key)
                feedback = "Unsnoozed item."
            } else {
                _ = try await model.snoozeItem(key: row.key, hours: hours, note: "Snoozed from iPhone")
                feedback = "Snoozed for \(hours)h."
            }
            feedbackIsError = false
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }
}

// MARK: - Job Pill Action Sheet (from Overview Mac Cards)

struct JobPillSelection: Identifiable, Hashable {
    let hostID: String
    let job: FailingJob
    var id: String { "\(hostID)|\(job.label)" }
}

struct JobActionSheet: View {
    @Bindable var model: FleetAppModel
    let selection: JobPillSelection
    @Environment(\.dismiss) private var dismiss

    @State private var jobLog: JobLogResponse?
    @State private var isLoadingLog = false
    @State private var isRunningAction = false
    @State private var isDispatching = false
    @State private var isUpdatingSnooze = false
    @State private var dispatchTarget = ""
    @State private var dispatchPrompt = ""
    @State private var feedback: String?
    @State private var feedbackIsError = false

    private var hostLower: String { selection.hostID.lowercased() }
    private var allowedHost: Bool {
        (model.overview?.actionHosts ?? []).contains(hostLower)
    }

    private var matchingRow: MorningRow? {
        (model.morning?.rows ?? []).first { row in
            guard let target = extractJobTarget(from: row) else { return false }
            return target.host == hostLower && target.label == selection.job.label
        }
    }

    private var snoozeKey: String {
        matchingRow?.key ?? "job:\(hostLower):\(shortJobName(selection.job.label))"
    }

    var body: some View {
        NavigationStack {
            List {
                if let feedback {
                    Section {
                        Text(feedback)
                            .font(.subheadline)
                            .foregroundStyle(feedbackIsError ? FleetTheme.fail : FleetTheme.pass)
                    }
                }

                Section("Failing Job") {
                    LabeledContent("Host", value: selection.hostID.uppercased())
                    LabeledContent("Label", value: selection.job.label)
                    LabeledContent("State", value: selection.job.state ?? "unknown")
                    if let exit = selection.job.exit {
                        LabeledContent("Last Exit", value: "\(exit)")
                    }
                    if let when = selection.job.when {
                        LabeledContent("Schedule", value: when)
                    }
                }

                if allowedHost {
                    Section("Direct Job Control") {
                        Button {
                            Task { await loadLogs() }
                        } label: {
                            HStack {
                                Label("Refresh Logs", systemImage: "arrow.triangle.2.circlepath")
                                Spacer()
                                if isLoadingLog { ProgressView().controlSize(.small) }
                            }
                        }
                        .disabled(isLoadingLog)

                        Button {
                            Task { await restartJob() }
                        } label: {
                            HStack {
                                Label("Restart / Run Job Now", systemImage: "arrow.clockwise.circle.fill")
                                Spacer()
                                if isRunningAction {
                                    ProgressView().controlSize(.small)
                                } else {
                                    Image(systemName: "faceid").foregroundStyle(.secondary)
                                }
                            }
                        }
                        .disabled(isRunningAction)
                    }
                }

                if let jobLog {
                    Section("Live Logs") {
                        JobLogBox(log: jobLog)
                    }
                }

                Section("Ask Agent to Fix") {
                    TextField("Target (host:project)", text: $dispatchTarget)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.subheadline.monospaced())

                    TextField("Fix instructions", text: $dispatchPrompt, axis: .vertical)
                        .lineLimit(3...6)
                        .font(.callout)

                    if let errLog = jobLog?.stderr, !errLog.isEmpty {
                        Button("Append Last Log Lines") {
                            let tail = errLog.components(separatedBy: .newlines).suffix(8).joined(separator: "\n")
                            if !dispatchPrompt.contains(tail) {
                                dispatchPrompt += "\n\nRecent stderr:\n\(tail)"
                            }
                        }
                        .font(.caption)
                        .foregroundStyle(FleetTheme.info)
                    }

                    Button {
                        Task { await sendFix() }
                    } label: {
                        HStack {
                            if isDispatching { ProgressView().controlSize(.small) }
                            Label("Dispatch Fix to Agent", systemImage: "paperplane.fill")
                                .fontWeight(.semibold)
                        }
                        .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(FleetTheme.pass)
                    .disabled(isDispatching || dispatchTarget.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || dispatchPrompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }

                Section("Snooze") {
                    if matchingRow?.isSnoozed == true {
                        Button("Unsnooze Item", role: .destructive) {
                            Task { await setSnooze(clear: true, hours: 0) }
                        }
                        .disabled(isUpdatingSnooze)
                    } else {
                        HStack(spacing: 12) {
                            Button("Snooze 24h") {
                                Task { await setSnooze(clear: false, hours: 24) }
                            }
                            .buttonStyle(.bordered)
                            .frame(maxWidth: .infinity)

                            Button("Snooze 7d") {
                                Task { await setSnooze(clear: false, hours: 168) }
                            }
                            .buttonStyle(.bordered)
                            .frame(maxWidth: .infinity)
                        }
                        .disabled(isUpdatingSnooze)
                    }
                }
            }
            .navigationTitle(shortJobName(selection.job.label))
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .onAppear {
                dispatchTarget = "\(hostLower):fleet"
                let exitStr = selection.job.exit.map { "exit \($0)" } ?? "failing"
                dispatchPrompt = "Fix failing launchd job \(selection.job.label) on \(selection.hostID.uppercased()) (\(exitStr), schedule: \(selection.job.when ?? "unknown"))."
            }
            .task {
                if allowedHost {
                    await loadLogs()
                }
            }
        }
    }

    private func loadLogs() async {
        isLoadingLog = true
        defer { isLoadingLog = false }
        do {
            jobLog = try await model.fetchJobLog(host: hostLower, label: selection.job.label)
            feedbackIsError = false
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }

    private func restartJob() async {
        isRunningAction = true
        defer { isRunningAction = false }
        do {
            let res = try await model.kickstartJob(host: hostLower, label: selection.job.label)
            feedback = res.detail ?? "Kickstarted \(selection.job.label)."
            feedbackIsError = !res.ok
            jobLog = try? await model.fetchJobLog(host: hostLower, label: selection.job.label)
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }

    private func sendFix() async {
        isDispatching = true
        defer { isDispatching = false }
        do {
            let res = try await model.dispatchFix(
                to: dispatchTarget,
                key: snoozeKey,
                title: shortJobName(selection.job.label),
                prompt: dispatchPrompt
            )
            feedback = res.detail ?? "Dispatched to \(dispatchTarget)."
            feedbackIsError = false
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }

    private func setSnooze(clear: Bool, hours: Int) async {
        isUpdatingSnooze = true
        defer { isUpdatingSnooze = false }
        do {
            if clear {
                _ = try await model.unsnoozeItem(key: snoozeKey)
                feedback = "Unsnoozed job."
            } else {
                _ = try await model.snoozeItem(key: snoozeKey, hours: hours, note: "Snoozed from Overview")
                feedback = "Snoozed for \(hours)h."
            }
            feedbackIsError = false
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }
}

// MARK: - Placement Gap Action Sheet

struct PlacementActionSheet: View {
    @Bindable var model: FleetAppModel
    let violation: PlacementViolation
    @Environment(\.dismiss) private var dismiss

    @State private var isRunningAction = false
    @State private var isDispatching = false
    @State private var isUpdatingSnooze = false
    @State private var dispatchTarget = ""
    @State private var dispatchPrompt = ""
    @State private var feedback: String?
    @State private var feedbackIsError = false

    private var hubHost: String? {
        let allowed = model.overview?.actionHosts ?? []
        let target = violation.host.lowercased()
        if allowed.contains(where: { $0.lowercased() == target }) {
            return target
        }
        return allowed.first
    }

    private var snoozeKey: String {
        let matched = (model.morning?.rows ?? []).first { row in
            (row.area ?? "").lowercased() == "place" && row.name.localizedCaseInsensitiveContains(violation.dataset)
        }
        return matched?.key ?? "place:\(violation.dataset)"
    }

    var body: some View {
        NavigationStack {
            List {
                if let feedback {
                    Section {
                        Text(feedback)
                            .font(.subheadline)
                            .foregroundStyle(feedbackIsError ? FleetTheme.fail : FleetTheme.pass)
                    }
                }

                Section("Placement Gap") {
                    LabeledContent("Host", value: violation.host.uppercased())
                    LabeledContent("Dataset", value: violation.dataset)
                    LabeledContent("Code", value: violation.code)
                    if let detail = violation.detail, !detail.isEmpty {
                        Text(detail)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                    }
                }

                if let hubHost {
                    Section("Direct Action") {
                        Button {
                            Task { await runPlacementCheck(hubHost) }
                        } label: {
                            HStack {
                                Label("Re-check Placement on \(hubHost.uppercased())", systemImage: "arrow.clockwise")
                                Spacer()
                                if isRunningAction {
                                    ProgressView().controlSize(.small)
                                } else {
                                    Image(systemName: "faceid").foregroundStyle(.secondary)
                                }
                            }
                        }
                        .disabled(isRunningAction)
                    }
                }

                Section("Ask Agent to Fix") {
                    TextField("Target (host:project)", text: $dispatchTarget)
                        .textInputAutocapitalization(.never)
                        .autocorrectionDisabled()
                        .font(.subheadline.monospaced())

                    TextField("Fix instructions", text: $dispatchPrompt, axis: .vertical)
                        .lineLimit(3...6)
                        .font(.callout)

                    Button {
                        Task { await sendFix() }
                    } label: {
                        HStack {
                            if isDispatching { ProgressView().controlSize(.small) }
                            Label("Dispatch Fix to Agent", systemImage: "paperplane.fill")
                                .fontWeight(.semibold)
                        }
                        .frame(maxWidth: .infinity)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(FleetTheme.pass)
                    .disabled(isDispatching || dispatchTarget.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || dispatchPrompt.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }

                Section("Snooze") {
                    HStack(spacing: 12) {
                        Button("Snooze 24h") {
                            Task { await setSnooze(hours: 24) }
                        }
                        .buttonStyle(.bordered)
                        .frame(maxWidth: .infinity)

                        Button("Snooze 7d") {
                            Task { await setSnooze(hours: 168) }
                        }
                        .buttonStyle(.bordered)
                        .frame(maxWidth: .infinity)
                    }
                    .disabled(isUpdatingSnooze)
                }
            }
            .navigationTitle(violation.dataset)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
            .onAppear {
                let targetHost = (hubHost ?? violation.host).lowercased()
                dispatchTarget = "\(targetHost):fleet"
                dispatchPrompt = "Resolve placement violation on \(violation.host.uppercased()) for dataset '\(violation.dataset)' (code: \(violation.code), detail: \(violation.detail ?? "—"))."
            }
        }
    }

    private func runPlacementCheck(_ host: String) async {
        isRunningAction = true
        defer { isRunningAction = false }
        do {
            let res = try await model.runHostAction(host: host, kind: "placement-check")
            feedback = res.detail ?? "Started placement check."
            feedbackIsError = !res.ok
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }

    private func sendFix() async {
        isDispatching = true
        defer { isDispatching = false }
        do {
            let res = try await model.dispatchFix(
                to: dispatchTarget,
                key: snoozeKey,
                title: violation.dataset,
                prompt: dispatchPrompt
            )
            feedback = res.detail ?? "Dispatched to \(dispatchTarget)."
            feedbackIsError = false
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }

    private func setSnooze(hours: Int) async {
        isUpdatingSnooze = true
        defer { isUpdatingSnooze = false }
        do {
            _ = try await model.snoozeItem(key: snoozeKey, hours: hours, note: violation.dataset)
            feedback = "Snoozed for \(hours)h."
            feedbackIsError = false
        } catch {
            feedback = error.localizedDescription
            feedbackIsError = true
        }
    }
}

import SwiftUI

struct AgentDetailView: View {
    @Bindable var model: FleetAppModel
    let conversation: MCConversation

    @State private var detail: MCConversation?
    @State private var promptDraft = ""
    @State private var isLoading = false
    @State private var isActing = false
    @State private var statusBanner: String?
    @State private var deliveryIssue: String?
    @State private var showingDeliveryIssue = false

    private var active: MCConversation {
        detail ?? conversation
    }

    var body: some View {
        VStack(spacing: 0) {
            ScrollView {
                LazyVStack(alignment: .leading, spacing: 14) {
                    if let statusBanner {
                        MissionCard {
                            Text(stripTrailingEllipsis(statusBanner))
                                .font(.caption)
                                .foregroundStyle(FleetTheme.info)
                        }
                    }

                    heroCard
                    iconActionBar

                    let needsText = !active.waitingReason.isEmpty ? active.waitingReason : (active.briefing?.needsUser ?? "")
                    if !needsText.isEmpty {
                        MissionCard {
                            VStack(alignment: .leading, spacing: 6) {
                                Label("Needs Your Call", systemImage: "exclamationmark.triangle.fill")
                                    .font(.caption.weight(.bold))
                                    .foregroundStyle(FleetTheme.warn)
                                Text(stripTrailingEllipsis(needsText))
                                    .font(.subheadline)
                                    .foregroundStyle(.primary)
                                    .fixedSize(horizontal: false, vertical: true)
                            }
                        }
                    }

                    briefingCard
                    nextActionsSection
                    checklistSection
                    eventsSection
                }
                .padding(.horizontal, 16)
                .padding(.vertical, 12)
            }

            composerBar
        }
        .background(FleetTheme.bg.ignoresSafeArea())
        .navigationTitle("Thread")
        .navigationBarTitleDisplayMode(.inline)
        .toolbar(.hidden, for: .tabBar)
        .task(id: model.mcBoard?.conversations.first { $0.id == conversation.id && $0.host == conversation.host }?.lastModifiedMs) {
            await loadDetail()
        }
    }

    private var heroCard: some View {
        let color = FleetTheme.color(forThreadState: active.state)
        return MissionCard {
            HStack(spacing: 12) {
                progressRing(color: color)

                VStack(alignment: .leading, spacing: 4) {
                    Text(stripTrailingEllipsis(active.title))
                        .font(.headline)
                        .foregroundStyle(.primary)
                        .fixedSize(horizontal: false, vertical: true)

                    HStack(spacing: 6) {
                        StatusPill(label: active.state.displayLabel, color: color)
                        Text(active.projectName)
                            .font(.caption)
                            .foregroundStyle(.secondary)
                        if !active.hostBadge.isEmpty {
                            Text(active.hostBadge)
                                .font(.caption2.monospaced().weight(.semibold))
                                .padding(.horizontal, 5)
                                .padding(.vertical, 2)
                                .background(FleetTheme.elevated, in: RoundedRectangle(cornerRadius: 4, style: .continuous))
                        }
                    }
                }
                Spacer(minLength: 0)
            }
        }
    }

    @ViewBuilder
    private func progressRing(color: Color) -> some View {
        if let steps = active.steps, steps.total > 0 {
            let pct = CGFloat(steps.done) / CGFloat(steps.total)
            ZStack {
                Circle()
                    .stroke(FleetTheme.elevated, lineWidth: 4)
                Circle()
                    .trim(from: 0, to: pct)
                    .stroke(steps.done == steps.total ? FleetTheme.pass : color, style: StrokeStyle(lineWidth: 4, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                Text("\(steps.done)/\(steps.total)")
                    .font(.caption2.monospacedDigit().weight(.bold))
            }
            .frame(width: 44, height: 44)
        } else {
            ZStack {
                Circle()
                    .stroke(color.opacity(0.35), lineWidth: 3)
                Image(systemName: active.state.sfSymbol)
                    .foregroundStyle(color)
            }
            .frame(width: 44, height: 44)
        }
    }

    private var iconActionBar: some View {
        HStack(spacing: 10) {
            Button {
                Task { await runFocus() }
            } label: {
                Label("Focus", systemImage: "arrow.up.right.square")
                    .font(.caption.weight(.semibold))
                    .padding(.horizontal, 12)
                    .padding(.vertical, 8)
                    .background(FleetTheme.elevated, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
            .buttonStyle(.plain)
            .disabled(isActing)

            Button {
                Task { await runRefreshBrief() }
            } label: {
                Image(systemName: "arrow.clockwise")
                    .padding(8)
                    .background(FleetTheme.elevated, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
            .buttonStyle(.plain)
            .disabled(isActing)

            Button {
                Task { await toggleWake() }
            } label: {
                let isOn = active.autoWake == true
                Image(systemName: isOn ? "bolt.fill" : "bolt")
                    .foregroundStyle(isOn ? FleetTheme.pass : .secondary)
                    .padding(8)
                    .background(isOn ? FleetTheme.pass.opacity(0.18) : FleetTheme.elevated, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
            }
            .buttonStyle(.plain)
            .disabled(isActing)

            if active.state == .working {
                Button {
                    Task { await runStop() }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "faceid")
                        Image(systemName: "stop.fill")
                    }
                    .foregroundStyle(FleetTheme.fail)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .background(FleetTheme.fail.opacity(0.16), in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                }
                .buttonStyle(.plain)
                .disabled(isActing)
            }

            Spacer()
            if isLoading {
                ProgressView().controlSize(.small)
            }
        }
    }

    @ViewBuilder
    private var briefingCard: some View {
        let br = active.briefing
        let summary = stripTrailingEllipsis(br?.brief ?? br?.headline ?? active.lastReply)
        if !summary.isEmpty || !(br?.bullets ?? []).isEmpty {
            MissionCard {
                VStack(alignment: .leading, spacing: 8) {
                    Text("AI Briefing")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.secondary)
                    if !summary.isEmpty {
                        Text(summary)
                            .font(.subheadline)
                            .fixedSize(horizontal: false, vertical: true)
                    }
                    if let bullets = br?.bullets, !bullets.isEmpty {
                        ForEach(bullets, id: \.self) { b in
                            HStack(alignment: .top, spacing: 6) {
                                Text("·").foregroundStyle(FleetTheme.pass).bold()
                                Text(stripTrailingEllipsis(b))
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                            }
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var nextActionsSection: some View {
        let nexts = active.state == .working ? [] : (active.briefing?.nextActions ?? [])
        if !nexts.isEmpty {
            MissionCard {
                VStack(alignment: .leading, spacing: 10) {
                    Text("What's Next")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.secondary)

                    ForEach(nexts) { na in
                        VStack(alignment: .leading, spacing: 6) {
                            HStack {
                                Text(stripTrailingEllipsis(na.label))
                                    .font(.subheadline.weight(.semibold))
                                Spacer()
                                Button("Send") {
                                    Task { await sendText(na.prompt) }
                                }
                                .font(.caption.weight(.semibold))
                                .buttonStyle(.borderedProminent)
                                .tint(FleetTheme.pass)
                                .disabled(isActing)

                                Button("Edit") {
                                    promptDraft = na.prompt
                                }
                                .font(.caption)
                                .buttonStyle(.bordered)
                            }
                            Text(stripTrailingEllipsis(na.prompt))
                                .font(.caption)
                                .foregroundStyle(.secondary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                        .padding(10)
                        .background(FleetTheme.elevated, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var checklistSection: some View {
        if let steps = active.steps, !steps.items.isEmpty {
            MissionCard {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Checklist (\(steps.done)/\(steps.total))")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.secondary)

                    ForEach(Array(steps.items.enumerated()), id: \.offset) { _, item in
                        HStack(alignment: .top, spacing: 8) {
                            Image(systemName: item.status == "done" ? "checkmark.circle.fill" : (item.status == "in_progress" ? "play.circle.fill" : "circle"))
                                .font(.caption)
                                .foregroundStyle(item.status == "done" ? FleetTheme.pass : (item.status == "in_progress" ? FleetTheme.info : .secondary))
                                .padding(.top, 2)
                            Text(stripTrailingEllipsis(item.title))
                                .font(.caption)
                                .foregroundStyle(item.status == "done" ? .secondary : .primary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
            }
        }
    }

    @ViewBuilder
    private var eventsSection: some View {
        if let events = active.recentEvents, !events.isEmpty {
            MissionCard {
                VStack(alignment: .leading, spacing: 8) {
                    Text("Recent Activity (\(events.count))")
                        .font(.caption.weight(.bold))
                        .foregroundStyle(.secondary)
                    ForEach(events) { ev in
                        HStack(alignment: .top, spacing: 8) {
                            Text(ev.kind)
                                .font(.caption2.monospaced())
                                .foregroundStyle(.secondary)
                                .frame(width: 48, alignment: .leading)
                            Text(stripTrailingEllipsis(ev.text))
                                .font(.caption)
                                .foregroundStyle(.primary)
                                .fixedSize(horizontal: false, vertical: true)
                        }
                    }
                }
            }
        }
    }

    private var composerBar: some View {
        VStack(spacing: 0) {
            Divider().background(FleetTheme.border)
            if let deliveryIssue {
                Button {
                    showingDeliveryIssue = true
                } label: {
                    Label("Delivery unconfirmed · Details", systemImage: "exclamationmark.bubble")
                        .font(.caption)
                        .foregroundStyle(FleetTheme.warn)
                }
                .frame(maxWidth: .infinity, alignment: .leading)
                .padding(.horizontal, 14)
                .alert("Delivery unconfirmed", isPresented: $showingDeliveryIssue) {
                    Button("OK", role: .cancel) {}
                } message: {
                    Text(deliveryIssue)
                }
            }
            HStack(alignment: .bottom, spacing: 8) {
                TextField("Send follow-up prompt to this thread", text: $promptDraft, axis: .vertical)
                    .lineLimit(1...4)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .background(FleetTheme.bg, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(FleetTheme.border, lineWidth: 1))

                Button {
                    let submittedDraft = promptDraft
                    let clean = submittedDraft.trimmingCharacters(in: .whitespacesAndNewlines)
                    guard !clean.isEmpty, !isActing else { return }
                    Task {
                        let delivered = await sendText(clean)
                        if delivered && promptDraft == submittedDraft {
                            promptDraft = ""
                        }
                    }
                } label: {
                    Image(systemName: isActing ? "hourglass" : "arrow.up.circle.fill")
                        .font(.title2)
                        .foregroundStyle(promptDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isActing ? .secondary : FleetTheme.pass)
                }
                .accessibilityLabel("Send follow-up")
                .disabled(promptDraft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isActing)
            }
            .padding(.horizontal, 14)
            .padding(.vertical, 10)
        }
        .background(FleetTheme.card)
    }

    private func loadDetail() async {
        isLoading = true
        defer { isLoading = false }
        if let fetched = try? await model.fetchThreadDetail(id: conversation.id, host: conversation.host) {
            guard !Task.isCancelled else { return }
            detail = fetched
        }
    }

    @discardableResult
    private func sendText(_ text: String) async -> Bool {
        guard !isActing else { return false }
        isActing = true
        deliveryIssue = nil
        statusBanner = nil
        defer { isActing = false }
        do {
            let res = try await model.sendAgentMessage(conversationId: active.id, text: text, host: active.host)
            guard res.ok == true, (res.error ?? "").isEmpty else {
                deliveryIssue = res.error.flatMap { $0.isEmpty ? nil : $0 } ?? "The hub did not confirm delivery."
                return false
            }
            statusBanner = res.message ?? "Prompt dispatched."
            await loadDetail()
            return true
        } catch {
            deliveryIssue = error.localizedDescription
            return false
        }
    }

    private func runFocus() async {
        isActing = true
        defer { isActing = false }
        do {
            let res = try await model.focusAgentThread(conversationId: active.id, host: active.host)
            statusBanner = res.message ?? "Focused thread in Antigravity."
        } catch {
            statusBanner = error.localizedDescription
        }
    }

    private func runRefreshBrief() async {
        isActing = true
        defer { isActing = false }
        do {
            let res = try await model.refreshAgentBrief(conversationId: active.id, host: active.host)
            statusBanner = res.message ?? "Briefing updated."
            await loadDetail()
        } catch {
            statusBanner = error.localizedDescription
        }
    }

    private func toggleWake() async {
        isActing = true
        defer { isActing = false }
        do {
            let next = !(active.autoWake ?? false)
            let res = try await model.setAgentAutoWake(conversationId: active.id, enabled: next, host: active.host)
            statusBanner = res.message ?? "Updated auto-wake."
            await loadDetail()
        } catch {
            statusBanner = error.localizedDescription
        }
    }

    private func runStop() async {
        isActing = true
        defer { isActing = false }
        do {
            let res = try await model.stopAgentThread(conversationId: active.id, title: active.title, host: active.host)
            statusBanner = res.message ?? "Stop requested."
            await loadDetail()
        } catch {
            statusBanner = error.localizedDescription
        }
    }
}

import SwiftUI

struct CommanderView: View {
    @Bindable var model: FleetAppModel
    @State private var showDigest = false
    @State private var confirmClear = false
    @State private var draft = ""
    @State private var isSubmittingDraft = false
    @State private var deliveryUnconfirmed = false
    @State private var editingAction: CommanderAction?
    @State private var editDraft = ""
    @State private var busyActionId: String?
    private struct ActionFeedback {
        let text: String
        let succeeded: Bool
    }
    @State private var actionFeedback: ActionFeedback?

    private var prefs: CommanderPreferences {
        model.commanderState?.preferences ?? CommanderPreferences()
    }

    private var messages: [CommanderMessage] {
        model.commanderState?.messages ?? []
    }

    private var digest: CommanderDigest? {
        model.commanderState?.digest
    }

    var body: some View {
        NavigationStack {
            VStack(spacing: 0) {
                statusStrip

                ScrollViewReader { proxy in
                    ScrollView {
                        LazyVStack(alignment: .leading, spacing: 12) {
                            if !model.isMCConfigured {
                                unpairedMCCard
                            }

                            if let err = model.mcErrorMessage, !err.isEmpty {
                                errorBanner(err)
                            }

                            if let feedback = actionFeedback {
                                MissionCard {
                                    HStack(spacing: 8) {
                                        Image(systemName: feedback.succeeded ? "checkmark.circle.fill" : "exclamationmark.triangle.fill")
                                            .foregroundStyle(feedback.succeeded ? FleetTheme.pass : FleetTheme.warn)
                                        Text(feedback.text)
                                            .font(.caption)
                                            .foregroundStyle(.primary)
                                        Spacer()
                                        Button {
                                            actionFeedback = nil
                                        } label: {
                                            Image(systemName: "xmark")
                                                .font(.caption2)
                                        }
                                        .buttonStyle(.plain)
                                        .accessibilityLabel("Dismiss action result")
                                    }
                                }
                            }

                            if model.isMCConfigured {
                                if digest != nil { digestCard }
                                if messages.isEmpty {
                                    quickLaunchGrid
                                }
                            }

                            ForEach(messages) { msg in
                                messageRow(msg)
                                    .id(msg.id)
                            }

                            if model.isSendingCommander {
                                HStack(spacing: 8) {
                                    ProgressView().controlSize(.small)
                                    Text("Waiting for Commander")
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                                .padding(.horizontal, 12)
                                .padding(.vertical, 8)
                                .background(FleetTheme.card, in: RoundedRectangle(cornerRadius: 12, style: .continuous))
                                .id("commander-thinking")
                            }
                        }
                        .padding(.horizontal, 14)
                        .padding(.vertical, 12)
                    }
                    .defaultScrollAnchor(messages.isEmpty ? .top : .bottom, for: .initialOffset)
                    .scrollDismissesKeyboard(.interactively)
                    .onChange(of: messages.count) { _, _ in
                        if let last = messages.last {
                            withAnimation(.easeOut(duration: 0.2)) {
                                proxy.scrollTo(last.id, anchor: .bottom)
                            }
                        }
                    }
                    .onChange(of: model.isSendingCommander) { _, sending in
                        if sending {
                            withAnimation(.easeOut(duration: 0.2)) {
                                proxy.scrollTo("commander-thinking", anchor: .bottom)
                            }
                        }
                    }
                }

                if model.isMCConfigured {
                    composerBar
                }
            }
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("Command")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .topBarLeading) {
                    if !model.mcActiveHost.isEmpty,
                       let host = URL(string: model.mcActiveHost)?.host {
                        HStack(spacing: 4) {
                            Circle()
                                .fill(model.mcBoardError == nil ? FleetTheme.pass : FleetTheme.warn)
                                .frame(width: 6, height: 6)
                            Text(host.split(separator: ".").first.map(String.init) ?? host)
                                .font(.caption2.monospaced().weight(.semibold))
                                .foregroundStyle(.secondary)
                        }
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    HStack(spacing: 12) {
                        Button {
                            Task { await model.refreshCommanderDigest(force: true) }
                        } label: {
                            if model.isRefreshingDigest {
                                ProgressView().controlSize(.small)
                            } else {
                                Image(systemName: "arrow.clockwise")
                            }
                        }
                        .accessibilityLabel("Refresh briefing")
                        .disabled(!model.isMCConfigured || model.isRefreshingDigest)

                        if !messages.isEmpty {
                            Button {
                                confirmClear = true
                            } label: {
                                Image(systemName: "trash")
                            }
                            .accessibilityLabel("Clear conversation")
                            .disabled(model.isSendingCommander)
                        }
                    }
                }
            }
            .confirmationDialog("Clear this conversation?", isPresented: $confirmClear, titleVisibility: .visible) {
                Button("Clear conversation", role: .destructive) { Task { await model.clearCommanderChat() } }
                Button("Cancel", role: .cancel) {}
            }
            .sheet(item: $editingAction) { act in
                EditActionSheet(action: act, draft: $editDraft) { confirmedText in
                    Task { await runAction(act, editedText: confirmedText) }
                }
            }
        }
    }

    // MARK: - Pictorial Top Status Strip

    private var statusStrip: some View {
        VStack(spacing: 0) {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    if let counts = model.mcBoard?.counts {
                        Button {
                            model.agentFilter = counts.attention > 0 ? .attention : (counts.working > 0 ? .working : .all)
                            model.selectedTab = .agents
                        } label: {
                            HStack(spacing: 5) {
                                Image(systemName: counts.attention > 0 ? "exclamationmark.triangle.fill" : "cpu")
                                    .foregroundStyle(counts.attention > 0 ? FleetTheme.warn : FleetTheme.info)
                                Text(counts.attention > 0 ? "\(counts.attention) waiting" : "\(counts.working) working")
                                    .font(.caption.weight(.semibold).monospacedDigit())
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 5)
                            .background(FleetTheme.elevated, in: Capsule())
                        }
                        .buttonStyle(.plain)
                    }

                    if let ov = model.overview {
                        Button {
                            if ov.counts.fail > 0 || ov.counts.warn > 0 {
                                model.openBoard(filter: .issues)
                            } else {
                                model.fleetSubtab = .overview
                                model.selectedTab = .fleet
                            }
                        } label: {
                            let color = ov.counts.fail > 0 ? FleetTheme.fail : (ov.counts.warn > 0 ? FleetTheme.warn : FleetTheme.pass)
                            HStack(spacing: 5) {
                                Circle().fill(color).frame(width: 7, height: 7)
                                Text(ov.counts.fail > 0 ? "\(ov.counts.fail) fail" : (ov.counts.warn > 0 ? "\(ov.counts.warn) warn" : "Fleet OK"))
                                    .font(.caption.weight(.semibold).monospacedDigit())
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 5)
                            .background(FleetTheme.elevated, in: Capsule())
                        }
                        .buttonStyle(.plain)
                    }

                    if model.pendingRelaysCount > 0 {
                        Button {
                            model.selectedTab = .agents
                        } label: {
                            HStack(spacing: 4) {
                                Image(systemName: "arrow.left.arrow.right")
                                    .foregroundStyle(FleetTheme.warn)
                                Text(model.pendingRelaysCount == 1 ? "1 relay" : "\(model.pendingRelaysCount) relays")
                                    .font(.caption.weight(.semibold).monospacedDigit())
                            }
                            .padding(.horizontal, 10)
                            .padding(.vertical, 5)
                            .background(FleetTheme.warn.opacity(0.16), in: Capsule())
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.horizontal, 14)
                .padding(.vertical, 8)
            }

            if let counts = model.mcBoard?.counts, counts.total > 0 {
                proportionalBar(counts)
            }
            Divider().background(FleetTheme.border)
        }
        .background(FleetTheme.card)
    }

    private func proportionalBar(_ counts: MCBoardCounts) -> some View {
        let total = max(1, counts.total)
        let segs: [(Int, Color)] = [
            (counts.attention, FleetTheme.warn),
            (counts.working, FleetTheme.info),
            (counts.bg, FleetTheme.steel),
            (counts.unread, FleetTheme.pass),
            (counts.idle, Color.secondary.opacity(0.35)),
        ].filter { $0.0 > 0 }

        return GeometryReader { geo in
            HStack(spacing: 1) {
                ForEach(Array(segs.enumerated()), id: \.offset) { _, seg in
                    Rectangle()
                        .fill(seg.1)
                        .frame(width: max(3, geo.size.width * CGFloat(seg.0) / CGFloat(total)))
                }
            }
        }
        .frame(height: 3)
    }

    // MARK: - Executive Digest & Quick Launch

    private var digestCard: some View {
        let tone: Color = {
            switch (digest?.status ?? "").lowercased() {
            case "critical": return FleetTheme.fail
            case "attention": return FleetTheme.warn
            default: return FleetTheme.pass
            }
        }()

        return MissionCard {
            DisclosureGroup(isExpanded: $showDigest) {
                ForEach(digest?.items ?? [], id: \.self) { item in
                    Text(item)
                        .font(.subheadline)
                        .frame(maxWidth: .infinity, alignment: .leading)
                        .padding(.top, 6)
                        .textSelection(.enabled)
                }
            } label: {
                Label(digest?.headline ?? "Briefing", systemImage: "list.bullet.rectangle")
                    .font(.headline)
                    .foregroundStyle(.primary)
                    .fixedSize(horizontal: false, vertical: true)
            }
            .tint(tone)
        }
    }

    private var quickLaunchGrid: some View {
        let suggestions = digest?.starterPrompts ?? []
        let prompts = suggestions.isEmpty
            ? ["What needs my attention?", "What changed across my projects?"]
            : suggestions
        return VStack(alignment: .leading, spacing: 0) {
            ForEach(Array(prompts.enumerated()), id: \.offset) { index, prompt in
                if index > 0 { Divider() }
                Button {
                    draft = prompt
                } label: {
                    HStack {
                        Text(prompt).multilineTextAlignment(.leading)
                        Spacer()
                        Image(systemName: "arrow.down.left").foregroundStyle(.secondary)
                    }
                    .font(.subheadline)
                    .padding(.horizontal, 14)
                    .padding(.vertical, 10)
                    .frame(minHeight: 44)
                    .contentShape(Rectangle())
                }
                .buttonStyle(.plain)
                .accessibilityHint("Places this question in the message field")
            }
        }
        .background(FleetTheme.card, in: RoundedRectangle(cornerRadius: 14))
    }

    // MARK: - Chat Messages & Action Cards

    @ViewBuilder
    private func messageRow(_ msg: CommanderMessage) -> some View {
        if msg.role == "user" {
            HStack {
                Spacer(minLength: 40)
                Text(stripTrailingEllipsis(msg.text))
                    .font(.subheadline)
                    .foregroundStyle(.primary)
                    .padding(.horizontal, 12)
                    .padding(.vertical, 9)
                    .background(FleetTheme.info.opacity(0.18), in: RoundedRectangle(cornerRadius: 14, style: .continuous))
                    .overlay(
                        RoundedRectangle(cornerRadius: 14, style: .continuous)
                            .stroke(FleetTheme.info.opacity(0.35), lineWidth: 0.8)
                    )
            }
        } else {
            MissionCard {
                VStack(alignment: .leading, spacing: 10) {
                    let inspected = msg.inspectedCount.flatMap { $0 > 0 ? "Inspected \($0)" : nil }
                    let header = ["Commander", msg.modelTier, inspected].compactMap { $0 }.filter { !$0.isEmpty }
                    Text(header.joined(separator: " · "))
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.primary)
                        .fixedSize(horizontal: false, vertical: true)

                    Text(stripTrailingEllipsis(msg.text))
                        .font(.subheadline)
                        .foregroundStyle(.primary)
                        .fixedSize(horizontal: false, vertical: true)

                    if let actions = msg.actions, !actions.isEmpty {
                        VStack(spacing: 8) {
                            ForEach(actions) { act in
                                actionCard(act)
                            }
                        }
                    }

                    if let followUps = msg.followUps, !followUps.isEmpty {
                        ScrollView(.horizontal, showsIndicators: false) {
                            HStack(spacing: 6) {
                                ForEach(followUps, id: \.self) { chip in
                                    Button {
                                        Task { await model.sendCommanderMessage(chip) }
                                    } label: {
                                        Text(stripTrailingEllipsis(chip))
                                            .font(.caption.weight(.medium))
                                            .padding(.horizontal, 10)
                                            .padding(.vertical, 5)
                                            .background(FleetTheme.elevated, in: Capsule())
                                            .overlay(Capsule().stroke(FleetTheme.border, lineWidth: 0.8))
                                    }
                                    .buttonStyle(.plain)
                                    .disabled(model.isSendingCommander)
                                }
                            }
                        }
                    }
                }
            }
        }
    }

    private func actionCard(_ act: CommanderAction) -> some View {
        let riskColor: Color = act.risk == .high ? FleetTheme.warn : (act.risk == .medium ? FleetTheme.info : FleetTheme.pass)
        let busy = busyActionId == act.id
        return VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 8) {
                Circle().fill(riskColor).frame(width: 7, height: 7).padding(.top, 5)
                VStack(alignment: .leading, spacing: 2) {
                    Text(stripTrailingEllipsis(act.label))
                        .font(.subheadline.weight(.semibold))
                    Text(stripTrailingEllipsis(act.summary))
                        .font(.caption)
                        .foregroundStyle(FleetTheme.secondaryText)
                }
                Spacer(minLength: 4)
            }

            if act.status == .pending || act.status == .failed {
                HStack(spacing: 8) {
                    Button {
                        Task { await runAction(act, editedText: nil) }
                    } label: {
                        HStack(spacing: 4) {
                            if busy {
                                ProgressView().controlSize(.mini)
                            } else if act.risk == .high {
                                Image(systemName: "faceid")
                            }
                            Text("Run")
                        }
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.white)
                    }
                    .buttonStyle(.borderedProminent)
                    .tint(FleetTheme.actionFill)
                    .disabled(busy)

                    if act.isEditableKind {
                        Button("Edit") {
                            editDraft = act.defaultEditableText ?? ""
                            editingAction = act
                        }
                        .font(.caption)
                        .buttonStyle(.bordered)
                        .tint(.primary)
                        .disabled(busy)
                    }

                    Button("Skip") {
                        Task { await skipAction(act) }
                    }
                    .font(.caption)
                    .buttonStyle(.bordered)
                    .tint(.primary)
                    .disabled(busy)
                }
            } else {
                HStack(spacing: 6) {
                    StatusPill(label: act.status == .executed ? "Done" : "Skipped", color: act.status == .executed ? FleetTheme.pass : .secondary)
                    if let result = act.resultMessage, !result.isEmpty {
                        Text(stripTrailingEllipsis(result))
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }
            }
        }
        .padding(10)
        .frame(maxWidth: .infinity, alignment: .leading)
        .background(FleetTheme.elevated, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
        .overlay(
            RoundedRectangle(cornerRadius: 10, style: .continuous)
                .stroke(act.risk == .high ? FleetTheme.warn.opacity(0.45) : FleetTheme.border, lineWidth: 1)
        )
    }

    // MARK: - Composer Bar

    private var composerBar: some View {
        VStack(spacing: 8) {
            Divider().background(FleetTheme.border)
            if deliveryUnconfirmed {
                Label("Delivery unconfirmed", systemImage: "exclamationmark.bubble")
                    .font(.caption)
                    .foregroundStyle(FleetTheme.warn)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, 12)
            }

            HStack(alignment: .bottom, spacing: 8) {
                Menu {
                    Picker("Reasoning", selection: Binding(
                        get: { prefs.routingMode },
                        set: { mode in Task { await model.updateCommanderSettings(routingMode: mode) } }
                    )) {
                        ForEach(CommanderRoutingMode.allCases) { mode in Text(mode.displayLabel).tag(mode) }
                    }
                    Toggle("Direct dispatch", isOn: Binding(
                        get: { prefs.directDispatch },
                        set: { value in Task { await model.updateCommanderSettings(directDispatch: value) } }
                    ))
                } label: {
                    Image(systemName: prefs.directDispatch ? "bolt.circle.fill" : "slider.horizontal.3")
                        .frame(width: 44, height: 44)
                }
                .accessibilityLabel("Dispatch options")
                .accessibilityValue(prefs.routingMode.displayLabel + (prefs.directDispatch ? ", direct" : ", confirmation required"))
                TextField("Message", text: $draft, prompt: Text("Message").foregroundColor(FleetTheme.secondaryText), axis: .vertical)
                    .accessibilityLabel("Message Commander")
                    .accessibilityIdentifier("commander.message")
                    .lineLimit(1...5)
                    .padding(.horizontal, 10)
                    .padding(.vertical, 8)
                    .background(FleetTheme.bg, in: RoundedRectangle(cornerRadius: 10, style: .continuous))
                    .overlay(RoundedRectangle(cornerRadius: 10, style: .continuous).stroke(FleetTheme.border, lineWidth: 1))

                Button {
                    guard !isSubmittingDraft, !model.isSendingCommander else { return }
                    let submittedDraft = draft
                    isSubmittingDraft = true
                    deliveryUnconfirmed = false
                    Task {
                        let delivered = await model.sendCommanderMessage(submittedDraft)
                        if delivered && draft == submittedDraft {
                            draft = ""
                        }
                        deliveryUnconfirmed = !delivered
                        isSubmittingDraft = false
                    }
                } label: {
                    Image(systemName: model.isSendingCommander ? "hourglass" : "arrow.up.circle.fill")
                        .font(.title2)
                        .foregroundStyle(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || model.isSendingCommander ? .secondary : FleetTheme.pass)
                }
                .frame(minWidth: 44, minHeight: 44)
                .keyboardShortcut(.return, modifiers: .command)
                .accessibilityLabel("Send message")
                .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isSubmittingDraft || model.isSendingCommander)
            }
            .padding(.horizontal, 12)
            .padding(.bottom, 10)
        }
        .background(FleetTheme.card)
    }

    // MARK: - Helpers

    private var unpairedMCCard: some View {
        MissionCard {
            VStack(alignment: .leading, spacing: 8) {
                Label("Connect to your Mac", systemImage: "link")
                    .font(.headline)
                    .foregroundStyle(FleetTheme.warn)
                Text("Paste your Mission Control pairing link in Controls to talk to Commander and see your agents.")
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

    private func errorBanner(_ msg: String) -> some View {
        MissionCard {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: "wifi.exclamationmark")
                    .foregroundStyle(FleetTheme.fail)
                Text(msg)
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
        }
    }

    private func runAction(_ action: CommanderAction, editedText: String?) async {
        busyActionId = action.id
        defer { busyActionId = nil }
        do {
            let res = try await model.executeCommanderAction(action, editedText: editedText)
            actionFeedback = ActionFeedback(
                text: res.error ?? res.message ?? (res.ok ? "Action updated." : "Action failed."),
                succeeded: res.ok && (res.error ?? "").isEmpty
            )
        } catch {
            actionFeedback = ActionFeedback(text: error.localizedDescription, succeeded: false)
        }
    }

    private func skipAction(_ action: CommanderAction) async {
        busyActionId = action.id
        defer { busyActionId = nil }
        do {
            let res = try await model.dismissCommanderAction(action)
            if !res.ok || !(res.error ?? "").isEmpty {
                actionFeedback = ActionFeedback(text: res.error ?? res.message ?? "Couldn’t skip action.", succeeded: false)
            }
        } catch {
            actionFeedback = ActionFeedback(text: error.localizedDescription, succeeded: false)
        }
    }
}

private struct EditActionSheet: View {
    let action: CommanderAction
    @Binding var draft: String
    let onSaveAndRun: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section(stripTrailingEllipsis(action.label)) {
                    TextField("Action text", text: $draft, axis: .vertical)
                        .lineLimit(3...8)
                }
                Section {
                    Button {
                        let clean = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                        dismiss()
                        if !clean.isEmpty { onSaveAndRun(clean) }
                    } label: {
                        HStack {
                            Spacer()
                            if action.risk == .high { Image(systemName: "faceid") }
                            Text("Save & Run")
                                .fontWeight(.semibold)
                            Spacer()
                        }
                    }
                    .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
            .scrollContentBackground(.hidden)
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("Edit Action")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}

import SwiftUI

struct AgentsView: View {
    @Bindable var model: FleetAppModel

    @State private var hostFilter: String = "all"
    @State private var searchText = ""
    @State private var showNewSheet = false
    @State private var editingRelay: MCRelayItem?
    @State private var editRelayText = ""
    @State private var busyId: String?
    @State private var feedback: String?

    enum StateFilter: String, CaseIterable, Identifiable {
        case all = "All"
        case attention = "Wait"
        case working = "Work"
        case bg = "Bg"
        case idle = "Idle"
        var id: String { rawValue }
    }

    private var hosts: [MCHostInfo] {
        model.mcBoard?.hosts ?? []
    }

    private var pendingRelays: [MCRelayItem] {
        (model.mcBoard?.relays ?? []).filter { $0.status == "pending" }
    }

    private var filteredThreads: [MCConversation] {
        var list = model.mcBoard?.conversations ?? []
        if hostFilter != "all" {
            list = list.filter { ($0.host ?? "").lowercased() == hostFilter.lowercased() }
        }
        switch model.agentFilter {
        case .all: break
        case .attention: list = list.filter { $0.state == .attention }
        case .working: list = list.filter { $0.state == .working }
        case .bg: list = list.filter { $0.state == .bg }
        case .idle: list = list.filter { $0.state == .idle || $0.state == .unread }
        }
        let q = searchText.trimmingCharacters(in: .whitespacesAndNewlines).lowercased()
        if !q.isEmpty {
            list = list.filter {
                "\($0.title) \($0.projectName) \($0.headlineClean) \($0.host ?? "")".lowercased().contains(q)
            }
        }
        return list
    }

    var body: some View {
        NavigationStack {
            List {
                if !model.isMCConfigured {
                    ContentUnavailableView {
                        Label("Pair Mission Control", systemImage: "link")
                    } description: {
                        Text("Connect to your Mac to manage its chats.")
                    } actions: {
                        Button("Open Controls") { model.openControls() }
                            .buttonStyle(.borderedProminent)
                    }
                } else {
                    if let failure = model.mcBoardError {
                        Section {
                            Label(model.mcBoard == nil ? "Agents unavailable" : "Showing the last agent board",
                                  systemImage: "wifi.exclamationmark")
                                .font(.headline)
                            Text(failure).font(.footnote).foregroundStyle(.secondary)
                            Button("Retry") { Task { await model.refreshAgentBoard() } }
                        } footer: {
                            if let date = model.mcBoardUpdatedAt {
                                Text("Last updated \(date.formatted(date: .omitted, time: .shortened))")
                            }
                        }
                    } else if model.mcBoard == nil {
                        ProgressView("Loading agents")
                    }
                }

                if let feedback {
                    Section {
                        Text(stripTrailingEllipsis(feedback))
                            .font(.caption)
                            .foregroundStyle(FleetTheme.info)
                    }
                }

                if model.isMCConfigured, model.mcBoard != nil {
                    filterChipsSection

                    if !pendingRelays.isEmpty {
                        Section("Pending Agent Relays (\(pendingRelays.count))") {
                            ForEach(pendingRelays) { relay in
                                relayCard(relay)
                            }
                        }
                    }

                    Section("Conversations (\(filteredThreads.count))") {
                        if filteredThreads.isEmpty {
                            Text("No agent conversations match this filter.")
                                .font(.subheadline)
                                .foregroundStyle(.secondary)
                        } else {
                            ForEach(filteredThreads) { conv in
                                NavigationLink {
                                    AgentDetailView(model: model, conversation: conv)
                                } label: {
                                    threadRow(conv)
                                }
                            }
                        }
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("Agents")
            .navigationBarTitleDisplayMode(.inline)
            .searchable(text: $searchText, prompt: "Filter agents, projects, or hosts")
            .toolbar {
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showNewSheet = true
                    } label: {
                        Label("New", systemImage: "plus")
                    }
                    .disabled(!model.isMCConfigured)
                }
            }
            .refreshable {
                await model.refreshAgentBoard()
            }
            .sheet(isPresented: $showNewSheet) {
                NewAgentThreadSheet(model: model, hosts: hosts, projects: model.mcBoard?.projects ?? []) { msg in
                    feedback = msg
                }
            }
            .sheet(item: $editingRelay) { relay in
                EditRelaySheet(relay: relay, draft: $editRelayText) { edited in
                    Task { await sendRelay(relay, editedText: edited) }
                }
            }
        }
    }

    private var filterChipsSection: some View {
        Section {
            VStack(alignment: .leading, spacing: 8) {
                if hosts.count > 1 {
                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            hostPill(name: "All", key: "all", online: nil)
                            ForEach(hosts) { h in
                                hostPill(name: h.name.uppercased(), key: h.name, online: h.online)
                            }
                        }
                    }
                }

                ScrollView(.horizontal, showsIndicators: false) {
                    HStack(spacing: 8) {
                        ForEach(StateFilter.allCases) { f in
                            let selected = model.agentFilter == f
                            Button {
                                model.agentFilter = f
                            } label: {
                                Text(f.rawValue)
                                    .font(.caption.weight(.semibold))
                                    .padding(.horizontal, 11)
                                    .padding(.vertical, 5)
                                    .background(selected ? FleetTheme.pass.opacity(0.24) : FleetTheme.elevated, in: Capsule())
                                    .overlay(Capsule().stroke(selected ? FleetTheme.pass : FleetTheme.border, lineWidth: 1))
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }
            }
            .listRowInsets(EdgeInsets(top: 6, leading: 12, bottom: 6, trailing: 12))
            .listRowBackground(Color.clear)
        }
    }

    private func hostPill(name: String, key: String, online: Bool?) -> some View {
        let selected = hostFilter.lowercased() == key.lowercased()
        return Button {
            hostFilter = key
        } label: {
            HStack(spacing: 5) {
                if let online {
                    Circle()
                        .fill(online ? FleetTheme.pass : .secondary)
                        .frame(width: 6, height: 6)
                }
                Text(name)
                    .font(.caption.weight(.semibold).monospaced())
            }
            .padding(.horizontal, 10)
            .padding(.vertical, 5)
            .background(selected ? FleetTheme.info.opacity(0.24) : FleetTheme.elevated, in: Capsule())
            .overlay(Capsule().stroke(selected ? FleetTheme.info : FleetTheme.border, lineWidth: 1))
        }
        .buttonStyle(.plain)
    }

    private func relayCard(_ relay: MCRelayItem) -> some View {
        let busy = busyId == relay.id
        return VStack(alignment: .leading, spacing: 6) {
            HStack {
                Image(systemName: "arrow.left.arrow.right")
                    .foregroundStyle(FleetTheme.warn)
                Text("\(relay.fromLabel) → \(relay.targetTitle)")
                    .font(.caption.weight(.semibold))
                Spacer()
                if !relay.targetHost.isEmpty {
                    StatusPill(label: relay.targetHost, color: FleetTheme.info)
                }
            }

            Text(stripTrailingEllipsis(relay.message))
                .font(.caption)
                .foregroundStyle(.primary)
                .fixedSize(horizontal: false, vertical: true)

            HStack(spacing: 8) {
                Button("Approve & Send") {
                    Task { await sendRelay(relay, editedText: nil) }
                }
                .font(.caption.weight(.semibold))
                .buttonStyle(.borderedProminent)
                .tint(FleetTheme.pass)
                .disabled(busy)

                Button("Edit") {
                    editRelayText = relay.message
                    editingRelay = relay
                }
                .font(.caption)
                .buttonStyle(.bordered)
                .disabled(busy)

                Button("Dismiss") {
                    Task { await rejectRelay(relay) }
                }
                .font(.caption)
                .buttonStyle(.bordered)
                .disabled(busy)
            }
        }
        .padding(.vertical, 4)
    }

    private func threadRow(_ conv: MCConversation) -> some View {
        let stateColor = FleetTheme.color(forThreadState: conv.state)
        let phase = (conv.briefing?.phase ?? defaultPhase(for: conv.state))
        return VStack(alignment: .leading, spacing: 6) {
            HStack(alignment: .top, spacing: 8) {
                Image(systemName: conv.state.sfSymbol)
                    .font(.subheadline)
                    .foregroundStyle(stateColor)
                    .padding(.top, 2)

                VStack(alignment: .leading, spacing: 3) {
                    Text(stripTrailingEllipsis(conv.title))
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(.primary)
                        .fixedSize(horizontal: false, vertical: true)

                    HStack(spacing: 6) {
                        phaseGlyph(phase)
                        Text(conv.projectName)
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                        if hosts.count > 1, !conv.hostBadge.isEmpty {
                            Text(conv.hostBadge)
                                .font(.caption2.monospaced().weight(.semibold))
                                .padding(.horizontal, 5)
                                .padding(.vertical, 1)
                                .background(FleetTheme.elevated, in: RoundedRectangle(cornerRadius: 4, style: .continuous))
                        }
                        if conv.autoWake == true {
                            Image(systemName: "bolt.fill")
                                .font(.caption2)
                                .foregroundStyle(FleetTheme.pass)
                        }
                    }
                }
                Spacer(minLength: 4)
                if let steps = conv.steps, steps.total > 0 {
                    stepGauge(steps)
                }
            }

            Text(conv.headlineClean)
                .font(.caption)
                .foregroundStyle(conv.state == .attention ? FleetTheme.warn : .secondary)
                .fixedSize(horizontal: false, vertical: true)

            if conv.state != .working, let next = conv.briefing?.nextActions.first {
                Button {
                    Task { await quickSendNext(conv, action: next) }
                } label: {
                    HStack(spacing: 4) {
                        Image(systemName: "play.fill").font(.caption2)
                        Text(stripTrailingEllipsis(next.label))
                            .font(.caption.weight(.semibold))
                    }
                    .padding(.horizontal, 9)
                    .padding(.vertical, 4)
                    .foregroundStyle(FleetTheme.pass)
                    .background(FleetTheme.pass.opacity(0.16), in: Capsule())
                    .overlay(Capsule().stroke(FleetTheme.pass.opacity(0.4), lineWidth: 0.8))
                }
                .buttonStyle(.plain)
            }
        }
        .padding(.vertical, 4)
    }

    private func stepGauge(_ steps: MCStepsInfo) -> some View {
        let ratio = "\(steps.done)/\(steps.total)"
        return HStack(spacing: 5) {
            if steps.total <= 8 {
                HStack(spacing: 2) {
                    ForEach(0..<steps.total, id: \.self) { idx in
                        let status = idx < steps.items.count ? steps.items[idx].status : (idx < steps.done ? "done" : "pending")
                        RoundedRectangle(cornerRadius: 1.5, style: .continuous)
                            .fill(status == "done" ? FleetTheme.pass : (status == "in_progress" ? FleetTheme.info : FleetTheme.elevated))
                            .frame(width: 4, height: 10)
                    }
                }
            }
            Text(ratio)
                .font(.caption2.monospacedDigit().weight(.semibold))
                .foregroundStyle(steps.done == steps.total ? FleetTheme.pass : .secondary)
        }
    }

    private func defaultPhase(for state: MCThreadState) -> String {
        switch state {
        case .attention: return "wait"
        case .working: return "build"
        case .bg: return "test"
        default: return "done"
        }
    }

    private func phaseGlyph(_ phase: String) -> some View {
        let icon: String = {
            switch phase.lowercased() {
            case "plan": return "safari"
            case "build": return "wrench.and.screwdriver"
            case "test": return "flask"
            case "debug": return "ant"
            case "review": return "eye"
            case "deploy": return "paperplane"
            case "research": return "magnifyingglass"
            case "wait": return "clock"
            default: return "checkmark.circle"
            }
        }()
        return Image(systemName: icon)
            .font(.caption2)
            .foregroundStyle(.secondary)
    }

    private func sendRelay(_ relay: MCRelayItem, editedText: String?) async {
        busyId = relay.id
        defer { busyId = nil }
        do {
            let res = try await model.approveRelay(relayId: relay.id, editedText: editedText, host: relay.targetHost)
            feedback = res.message ?? "Sent relay."
        } catch {
            feedback = error.localizedDescription
        }
    }

    private func rejectRelay(_ relay: MCRelayItem) async {
        busyId = relay.id
        defer { busyId = nil }
        do {
            let res = try await model.dismissRelay(relayId: relay.id, host: relay.targetHost)
            feedback = res.message ?? "Dismissed relay."
        } catch {
            feedback = error.localizedDescription
        }
    }

    private func quickSendNext(_ conv: MCConversation, action: MCNextAction) async {
        busyId = conv.id
        defer { busyId = nil }
        do {
            let res = try await model.sendAgentMessage(conversationId: conv.id, text: action.prompt, host: conv.host)
            feedback = res.message ?? "Sent prompt to \(conv.title)."
        } catch {
            feedback = error.localizedDescription
        }
    }
}

private struct NewAgentThreadSheet: View {
    @Bindable var model: FleetAppModel
    let hosts: [MCHostInfo]
    let projects: [MCProjectInfo]
    let onDone: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    @State private var selectedHost = ""
    @State private var selectedProject = ""
    @State private var promptText = ""
    @State private var isStarting = false
    @State private var errorMsg: String?

    var body: some View {
        NavigationStack {
            Form {
                if !hosts.isEmpty {
                    Picker("Target Mac", selection: $selectedHost) {
                        ForEach(hosts) { h in
                            Text(h.name.uppercased()).tag(h.name)
                        }
                    }
                }
                if !projects.isEmpty {
                    Picker("Project", selection: $selectedProject) {
                        Text("Default").tag("")
                        ForEach(projects.filter { $0.host == nil || $0.host == selectedHost }) { p in
                            Text(p.name).tag(p.name)
                        }
                    }
                }
                Section("Initial Prompt") {
                    TextField("Describe the task for the new agent thread", text: $promptText, axis: .vertical)
                        .lineLimit(3...8)
                }
                if let errorMsg {
                    Section {
                        Text(errorMsg).font(.caption).foregroundStyle(FleetTheme.fail)
                    }
                }
                Section {
                    Button {
                        Task { await startThread() }
                    } label: {
                        HStack {
                            Spacer()
                            if isStarting { ProgressView().controlSize(.small) }
                            Text("Launch Conversation").fontWeight(.semibold)
                            Spacer()
                        }
                    }
                    .disabled(promptText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || isStarting)
                }
            }
            .scrollContentBackground(.hidden)
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("New Agent Thread")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
            .onChange(of: selectedHost) { _, _ in selectedProject = "" }
            .onAppear {
                if selectedHost.isEmpty { selectedHost = hosts.first(where: { $0.online })?.name ?? hosts.first?.name ?? "" }
            }
        }
    }

    private func startThread() async {
        isStarting = true
        defer { isStarting = false }
        do {
            let res = try await model.startNewAgentThread(
                text: promptText.trimmingCharacters(in: .whitespacesAndNewlines),
                projectName: selectedProject.isEmpty ? nil : selectedProject,
                host: selectedHost.isEmpty ? nil : selectedHost
            )
            onDone(res.message ?? "Started conversation.")
            dismiss()
        } catch {
            errorMsg = error.localizedDescription
        }
    }
}

private struct EditRelaySheet: View {
    let relay: MCRelayItem
    @Binding var draft: String
    let onSend: (String) -> Void
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            Form {
                Section("Relay from \(relay.fromLabel) to \(relay.targetTitle)") {
                    TextField("Relay message", text: $draft, axis: .vertical)
                        .lineLimit(3...8)
                }
                Section {
                    Button {
                        let clean = draft.trimmingCharacters(in: .whitespacesAndNewlines)
                        dismiss()
                        if !clean.isEmpty { onSend(clean) }
                    } label: {
                        HStack {
                            Spacer()
                            Text("Approve & Send").fontWeight(.semibold)
                            Spacer()
                        }
                    }
                    .disabled(draft.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
            .scrollContentBackground(.hidden)
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("Edit Relay")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
            }
        }
    }
}

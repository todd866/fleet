import SwiftUI

struct InboxView: View {
    @Bindable var model: FleetAppModel

    @State private var mailFilter: MailFilter = .all
    @State private var showComposer = false
    @State private var isClearing = false
    @State private var feedback: String?

    enum DispatchMode: String, CaseIterable, Identifiable {
        case mail = "Agent Mail"
        case voice = "Voice Note"
        var id: String { rawValue }
    }

    enum MailFilter: String, CaseIterable, Identifiable {
        case all = "All"
        case unread = "Unread"
        var id: String { rawValue }
    }

    private var unreadTotal: Int {
        if let total = model.mail?.unreadTotal {
            return total
        }
        if let byHost = model.mail?.unreadByHost, !byHost.isEmpty {
            return byHost.values.reduce(0, +)
        }
        return (model.mail?.messages ?? []).filter { $0.read == false }.count
    }

    private var suggestedTargets: [String] {
        let hostIDs = (model.overview?.hosts ?? []).map(\.id)
        if hostIDs.isEmpty {
            return ["hub:fleet", "primary:fleet"]
        }
        return hostIDs.map { "\($0):fleet" }
    }

    var body: some View {
        NavigationStack {
            List {
                if let feedback {
                    Section {
                        Text(feedback)
                            .font(.caption)
                            .foregroundStyle(FleetTheme.info)
                    }
                }

                unreadSection
                claimsSection
                messagesSection
            }
            .scrollContentBackground(.hidden)
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("Inbox")
            .toolbar {
                if unreadTotal > 0 {
                    ToolbarItem(placement: .topBarLeading) {
                        Button {
                            Task { await clearAllUnread() }
                        } label: {
                            if isClearing {
                                ProgressView().controlSize(.small)
                            } else {
                                Label("Mark All Read", systemImage: "checkmark.circle")
                            }
                        }
                        .disabled(isClearing || !model.isConfigured)
                    }
                }
                ToolbarItem(placement: .topBarTrailing) {
                    Button {
                        showComposer = true
                    } label: {
                        Label("Compose", systemImage: "square.and.pencil")
                    }
                    .disabled(!model.isConfigured)
                }
            }
            .refreshable {
                await model.refreshAll()
            }
            .sheet(isPresented: $showComposer) {
                ComposeMessageSheet(
                    model: model,
                    suggestedTargets: suggestedTargets,
                    onSent: { msg in feedback = msg }
                )
            }
        }
    }

    @ViewBuilder
    private var unreadSection: some View {
        if let mail = model.mail {
            Section("Mail Status") {
                if unreadTotal > 0 {
                    HStack {
                        VStack(alignment: .leading, spacing: 2) {
                            Text("\(unreadTotal) unread across fleet")
                                .font(.subheadline.weight(.semibold))
                            if let acked = mail.ackedAt {
                                Text("Last cleared \(FleetFormatter.relativeTime(fromTimestamp: acked))")
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        Spacer()
                        Button("Clear All") {
                            Task { await clearAllUnread() }
                        }
                        .font(.caption.weight(.semibold))
                        .buttonStyle(.borderedProminent)
                        .tint(FleetTheme.pass)
                        .disabled(isClearing || !model.isConfigured)
                    }

                    let byHost = mail.unreadByHost ?? [:]
                    let nonZeroHosts = byHost.keys.filter { (byHost[$0] ?? 0) > 0 }.sorted()
                    ForEach(nonZeroHosts, id: \.self) { host in
                        HStack {
                            Text(host.uppercased())
                                .font(.subheadline.monospaced())
                            Spacer()
                            StatusPill(label: "\(byHost[host] ?? 0)", color: FleetTheme.info)
                        }
                    }
                } else {
                    HStack {
                        Label("All caught up", systemImage: "checkmark.circle.fill")
                            .font(.subheadline.weight(.medium))
                            .foregroundStyle(FleetTheme.pass)
                        Spacer()
                        if let acked = mail.ackedAt {
                            Text("Cleared \(FleetFormatter.relativeTime(fromTimestamp: acked))")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                }
            }
        }
    }

    private var claimsSection: some View {
        let claims = model.mail?.claims ?? []
        return Section("Active Cowork Claims (\(claims.count))") {
            if claims.isEmpty {
                Text("No active workspace claims.")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(claims) { claim in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text(claim.work)
                                .font(.subheadline.weight(.semibold))
                            Spacer()
                            StatusPill(label: "\(claim.host) · \(claim.provider ?? "agent")", color: FleetTheme.info)
                        }
                        if let note = claim.note, !note.isEmpty {
                            Text(note)
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        if let hb = claim.heartbeat {
                            Text("Heartbeat \(FleetFormatter.relativeTime(fromTimestamp: hb))")
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                    }
                    .padding(.vertical, 2)
                }
            }
        }
    }

    private var messagesSection: some View {
        let allMsgs = model.mail?.messages ?? []
        let shownMsgs = mailFilter == .unread ? allMsgs.filter { $0.read != true } : allMsgs
        return Section("Recent Messages (\(shownMsgs.count))") {
            Picker("Filter", selection: $mailFilter) {
                ForEach(MailFilter.allCases) { item in
                    Text(item.rawValue).tag(item)
                }
            }
            .pickerStyle(.segmented)

            if shownMsgs.isEmpty {
                Text(mailFilter == .unread ? "No unread messages." : "Inbox is clear.")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(shownMsgs, id: \.stableID) { msg in
                    let isRead = msg.read == true
                    VStack(alignment: .leading, spacing: 4) {
                        HStack(spacing: 6) {
                            if !isRead {
                                Circle()
                                    .fill(FleetTheme.info)
                                    .frame(width: 7, height: 7)
                            }
                            Text("\(msg.from ?? "agent") → \(msg.to ?? msg.project ?? "fleet")")
                                .font(.caption.weight(.bold).monospaced())
                                .foregroundStyle(isRead ? Color.secondary : FleetTheme.pass)
                            Spacer()
                            if let at = msg.at {
                                Text(FleetFormatter.relativeTime(fromTimestamp: at))
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        Text(msg.displayText)
                            .font(.subheadline)
                            .foregroundStyle(isRead ? .secondary : .primary)
                    }
                    .padding(.vertical, 3)
                    .swipeActions(edge: .trailing, allowsFullSwipe: true) {
                        if !isRead, let id = msg.id, !id.isEmpty {
                            Button("Mark Read") {
                                Task { await markSingleRead(id) }
                            }
                            .tint(FleetTheme.pass)
                        }
                    }
                }
            }
        }
    }

    private func clearAllUnread() async {
        isClearing = true
        defer { isClearing = false }
        do {
            _ = try await model.markAllMailRead()
            feedback = "Cleared unread mail."
        } catch {
            feedback = error.localizedDescription
        }
    }

    private func markSingleRead(_ id: String) async {
        do {
            _ = try await model.markMailRead(id: id)
        } catch {
            feedback = error.localizedDescription
        }
    }
}

private struct ComposeMessageSheet: View {
    @Bindable var model: FleetAppModel
    let suggestedTargets: [String]
    let onSent: (String) -> Void

    @Environment(\.dismiss) private var dismiss
    @State private var mode: InboxView.DispatchMode = .mail
    @State private var target = ""
    @State private var messageText = ""
    @State private var isSending = false
    @State private var errorText: String?

    var body: some View {
        NavigationStack {
            Form {
                Section("Mode") {
                    Picker("Mode", selection: $mode) {
                        ForEach(InboxView.DispatchMode.allCases) { item in
                            Text(item.rawValue).tag(item)
                        }
                    }
                    .pickerStyle(.segmented)
                }

                Section("Destination") {
                    TextField(
                        mode == .mail ? "Target (host:project)" : "Optional mail target (host:project)",
                        text: $target
                    )
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .font(.subheadline.monospaced())

                    ScrollView(.horizontal, showsIndicators: false) {
                        HStack(spacing: 8) {
                            ForEach(suggestedTargets, id: \.self) { chip in
                                Button(chip) {
                                    target = chip
                                }
                                .font(.caption.monospaced())
                                .padding(.horizontal, 10)
                                .padding(.vertical, 5)
                                .background(FleetTheme.elevated, in: Capsule())
                            }
                        }
                    }
                }

                Section("Message") {
                    TextField("Instruction or note for your Macs…", text: $messageText, axis: .vertical)
                        .lineLimit(3...8)
                }

                if let errorText {
                    Section {
                        Text(errorText)
                            .font(.caption)
                            .foregroundStyle(FleetTheme.fail)
                    }
                }
            }
            .navigationTitle("New Message")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .cancellationAction) {
                    Button("Cancel") { dismiss() }
                }
                ToolbarItem(placement: .confirmationAction) {
                    Button {
                        Task { await submit() }
                    } label: {
                        if isSending {
                            ProgressView().controlSize(.small)
                        } else {
                            Text("Send").fontWeight(.semibold)
                        }
                    }
                    .disabled(isSending || messageText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
                }
            }
            .onAppear {
                if target.isEmpty {
                    target = suggestedTargets.first ?? "hub:fleet"
                }
            }
        }
        .preferredColorScheme(.dark)
    }

    private func submit() async {
        let bodyText = messageText.trimmingCharacters(in: .whitespacesAndNewlines)
        let dest = target.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !bodyText.isEmpty else { return }
        isSending = true
        defer { isSending = false }
        do {
            if mode == .mail {
                _ = try await model.sendMail(to: dest, text: bodyText)
                onSent("Sent mail to \(dest).")
            } else {
                _ = try await model.sendVoiceNote(to: dest.isEmpty ? nil : dest, text: bodyText)
                onSent("Recorded voice note.")
            }
            dismiss()
        } catch {
            errorText = error.localizedDescription
        }
    }
}

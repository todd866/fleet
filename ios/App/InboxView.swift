import SwiftUI

struct InboxView: View {
    @Bindable var model: FleetAppModel

    @State private var mode: DispatchMode = .mail
    @State private var target = ""
    @State private var messageText = ""
    @State private var isSending = false
    @State private var feedback: String?

    enum DispatchMode: String, CaseIterable, Identifiable {
        case mail = "Agent Mail"
        case voice = "Voice Note"
        var id: String { rawValue }
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
                composerSection
                unreadSection
                claimsSection
                messagesSection
            }
            .scrollContentBackground(.hidden)
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("Inbox")
            .refreshable {
                await model.refreshAll()
            }
            .onAppear {
                if target.isEmpty {
                    target = suggestedTargets.first ?? "hub:fleet"
                }
            }
        }
    }

    private var composerSection: some View {
        Section("Dispatch") {
            Picker("Mode", selection: $mode) {
                ForEach(DispatchMode.allCases) { item in
                    Text(item.rawValue).tag(item)
                }
            }
            .pickerStyle(.segmented)

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
                        .padding(.vertical, 4)
                        .background(FleetTheme.elevated, in: Capsule())
                    }
                }
            }
            .listRowSeparator(.hidden)

            TextField("Instruction or note for your Macs…", text: $messageText, axis: .vertical)
                .lineLimit(2...5)

            Button {
                Task { await submitMessage() }
            } label: {
                HStack {
                    if isSending { ProgressView().controlSize(.small) }
                    Text(mode == .mail ? "Send Agent Mail" : "Save Voice Note")
                        .fontWeight(.semibold)
                }
                .frame(maxWidth: .infinity)
            }
            .buttonStyle(.borderedProminent)
            .tint(FleetTheme.pass)
            .disabled(isSending || messageText.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty || !model.isConfigured)

            if let feedback {
                Text(feedback)
                    .font(.caption)
                    .foregroundStyle(FleetTheme.info)
            }
        }
    }

    @ViewBuilder
    private var unreadSection: some View {
        if let unread = model.mail?.unreadByHost, !unread.isEmpty {
            Section("Unread By Host") {
                ForEach(unread.keys.sorted(), id: \.self) { host in
                    HStack {
                        Text(host.uppercased())
                            .font(.subheadline.monospaced())
                        Spacer()
                        let count = unread[host] ?? 0
                        StatusPill(label: "\(count)", color: count > 0 ? FleetTheme.warn : FleetTheme.pass)
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
        let msgs = model.mail?.messages ?? []
        return Section("Recent Messages (\(msgs.count))") {
            if msgs.isEmpty {
                Text("Inbox is clear.")
                    .foregroundStyle(.secondary)
            } else {
                ForEach(msgs, id: \.stableID) { msg in
                    VStack(alignment: .leading, spacing: 4) {
                        HStack {
                            Text("\(msg.from ?? "agent") → \(msg.to ?? msg.project ?? "fleet")")
                                .font(.caption.weight(.bold).monospaced())
                                .foregroundStyle(FleetTheme.pass)
                            Spacer()
                            if let at = msg.at {
                                Text(FleetFormatter.relativeTime(fromTimestamp: at))
                                    .font(.caption2)
                                    .foregroundStyle(.secondary)
                            }
                        }
                        Text(msg.displayText)
                            .font(.subheadline)
                    }
                    .padding(.vertical, 3)
                }
            }
        }
    }

    private func submitMessage() async {
        let body = messageText.trimmingCharacters(in: .whitespacesAndNewlines)
        let dest = target.trimmingCharacters(in: .whitespacesAndNewlines)
        guard !body.isEmpty else { return }
        isSending = true
        defer { isSending = false }
        do {
            if mode == .mail {
                _ = try await model.sendMail(to: dest, text: body)
                feedback = "Sent mail to \(dest)."
            } else {
                _ = try await model.sendVoiceNote(to: dest.isEmpty ? nil : dest, text: body)
                feedback = "Recorded voice note."
            }
            messageText = ""
        } catch {
            feedback = error.localizedDescription
        }
    }
}

import SwiftUI

struct ControlsView: View {
    @Bindable var model: FleetAppModel

    @State private var editServerURL = ""
    @State private var editToken = ""
    @State private var editMCPrimaryURL = ""
    @State private var editMCFallbackURL = ""
    @State private var editMCToken = ""
    @State private var statusBanner: String?
    @State private var isTesting = false
    @State private var isTestingMC = false
    @State private var isTriggeringMorning = false
    @State private var busyProvider: String?

    private let managedProviders = ["claude", "codex", "zai", "cursor"]

    var body: some View {
        NavigationStack {
            List {
                if let statusBanner {
                    Section {
                        Text(stripTrailingEllipsis(statusBanner))
                            .font(.subheadline)
                            .foregroundStyle(FleetTheme.info)
                    }
                }

                pairingSection
                Section("Connections") {
                    HStack {
                        Label(
                            model.isConfigured ? "Fleet paired" : "Fleet unpaired",
                            systemImage: model.isConfigured ? "checkmark.shield.fill" : "exclamationmark.triangle"
                        )
                        .foregroundStyle(model.isConfigured ? FleetTheme.pass : FleetTheme.warn)
                        Spacer()
                        if model.isConfigured, let last = model.lastRefreshDate {
                            Text("Synced \(FleetFormatter.relativeTime(fromEpoch: Int(last.timeIntervalSince1970)))")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                    }
                    HStack {
                        Label(
                            model.isMCConfigured ? "Mission Control paired" : "Mission Control unpaired",
                            systemImage: model.isMCConfigured ? "sparkles" : "exclamationmark.triangle"
                        )
                        .foregroundStyle(model.isMCConfigured ? FleetTheme.info : FleetTheme.warn)
                        Spacer()
                        if !model.mcActiveHost.isEmpty, let activeHost = URL(string: model.mcActiveHost)?.host {
                            Text(activeHost)
                                .font(.caption2.monospaced())
                                .foregroundStyle(.secondary)
                        }
                    }
                    if model.isMCConfigured, let problem = model.mcBoardError {
                        Label(problem, systemImage: "wifi.exclamationmark")
                            .font(.footnote).foregroundStyle(FleetTheme.warn)
                    }
                    if model.isConfigured, let host = URL(string: model.serverURL)?.host {
                        Text(host)
                            .font(.caption.monospaced())
                            .foregroundStyle(.secondary)
                    }
                }

                Section {
                    NavigationLink("Manual connections") {
                        Form {
                            if let statusBanner { Text(statusBanner).font(.footnote) }
                            mcConfigSection
                            manualConfigSection
                        }
                        .navigationTitle("Connections")
                        .navigationBarTitleDisplayMode(.inline)
                    }
                }
                if model.isMCConfigured { mcCommanderPrefsSection }
                if model.isConfigured {
                    operationsSection
                    providersSection
                }
            }
            .scrollContentBackground(.hidden)
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("Controls")
            .onAppear {
                editServerURL = model.serverURL
                editToken = ""
                editMCPrimaryURL = model.mcPrimaryURL
                editMCFallbackURL = model.mcFallbackURL
                editMCToken = ""
            }
        }
    }

    private var mcCommanderPrefsSection: some View {
        let prefs = model.commanderState?.preferences ?? CommanderPreferences()
        return Section("AI Commander Defaults") {
            Picker("Model Routing", selection: Binding(
                get: { prefs.routingMode },
                set: { mode in Task { await model.updateCommanderSettings(routingMode: mode) } }
            )) {
                ForEach(CommanderRoutingMode.allCases) { mode in
                    Text(mode.displayLabel).tag(mode)
                }
            }
            .disabled(!model.isMCConfigured)

            Toggle(
                "Direct Dispatch (low and medium)",
                isOn: Binding(
                    get: { prefs.directDispatch },
                    set: { enabled in Task { await model.updateCommanderSettings(directDispatch: enabled) } }
                )
            )
            .tint(FleetTheme.pass)
            .disabled(!model.isMCConfigured)
        }
    }

    private var mcConfigSection: some View {
        Section("Mission Control") {
            VStack(alignment: .leading, spacing: 4) {
                Text("Primary URL")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                TextField("https://primary.your-tailnet.ts.net:8446", text: $editMCPrimaryURL)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .font(.subheadline.monospaced())
            }

            VStack(alignment: .leading, spacing: 4) {
                Text("Fallback URL")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                TextField("https://secondary.your-tailnet.ts.net:8446", text: $editMCFallbackURL)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .font(.subheadline.monospaced())
            }

            VStack(alignment: .leading, spacing: 4) {
                Text("Mission Control Token")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                SecureField("Access token", text: $editMCToken)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .font(.subheadline.monospaced())
            }

            Button("Save Mission Control connection") {
                saveMCCredentials()
            }
            .disabled(resolvedMCToken.isEmpty)
            .foregroundStyle(FleetTheme.pass)

            Button {
                Task { await testMCConnection() }
            } label: {
                HStack {
                    Text("Test Mission Control Connection")
                    Spacer()
                    if isTestingMC { ProgressView().controlSize(.small) }
                }
            }
            .disabled(isTestingMC || resolvedMCToken.isEmpty)

            if model.isMCConfigured {
                Button("Clear Mission Control Token", role: .destructive) {
                    model.clearMCCredentials()
                    editMCToken = ""
                    statusBanner = "Mission Control token cleared from Keychain."
                }
            }
        }
    }

    private var operationsSection: some View {
        Section {
            Button {
                Task { await triggerRefresh() }
            } label: {
                HStack {
                    Label("Run Morning Audit Refresh", systemImage: "arrow.triangle.2.circlepath")
                    Spacer()
                    if isTriggeringMorning {
                        ProgressView().controlSize(.small)
                    } else {
                        Image(systemName: "faceid")
                            .foregroundStyle(.secondary)
                    }
                }
            }
            .disabled(!model.isConfigured || isTriggeringMorning)
        } header: {
            Text("Fleet Operations")
        } footer: {
            Text("Mutating commands require Face ID or device passcode confirmation and are audited on your hub.")
        }
    }

    private var providersSection: some View {
        let pausedSet = Set((model.overview?.capacity.paused ?? []).map { $0.lowercased() })
        return Section {
            ForEach(managedProviders, id: \.self) { provider in
                HStack {
                    VStack(alignment: .leading, spacing: 2) {
                        Text(provider.uppercased())
                            .font(.subheadline.weight(.semibold).monospaced())
                        Text(pausedSet.contains(provider) ? "Paused in agentkit" : "Active")
                            .font(.caption)
                            .foregroundStyle(pausedSet.contains(provider) ? FleetTheme.warn : FleetTheme.pass)
                    }
                    Spacer()
                    if busyProvider == provider {
                        ProgressView().controlSize(.small)
                    } else {
                        Toggle(
                            "",
                            isOn: Binding(
                                get: { pausedSet.contains(provider) },
                                set: { nextPaused in
                                    Task { await toggleProvider(provider, paused: nextPaused) }
                                }
                            )
                        )
                        .labelsHidden()
                        .tint(FleetTheme.warn)
                        .disabled(!model.isConfigured)
                    }
                }
            }
        } header: {
            Text("Paused AI Providers")
        }
    }

    private var pairingSection: some View {
        Section {
            HStack {
                Label("Pair with a Mac", systemImage: "link")
                Spacer()
                PasteButton(payloadType: String.self) { values in
                    if let link = values.first { applyPastedPairingLink(link) }
                }
                .accessibilityLabel("Paste pairing link")
            }
        } header: {
            Text("Pairing")
        } footer: {
            if !model.isAnyConfigured { Text("Paste the pairing link from your Mac to connect.") }
        }
    }

    private var manualConfigSection: some View {
        Section("Fleet") {
            VStack(alignment: .leading, spacing: 4) {
                Text("Server URL")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                TextField("https://hub.example.ts.net:8444", text: $editServerURL)
                    .keyboardType(.URL)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .font(.subheadline.monospaced())
            }

            VStack(alignment: .leading, spacing: 4) {
                Text("Bearer Token")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                SecureField("Access token", text: $editToken)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .font(.subheadline.monospaced())
            }

            Button("Save Credentials") {
                saveManualCredentials()
            }
            .disabled(resolvedFleetToken.isEmpty)
            .foregroundStyle(FleetTheme.pass)

            Button {
                Task { await testConnection() }
            } label: {
                HStack {
                    Text("Test Connection")
                    Spacer()
                    if isTesting { ProgressView().controlSize(.small) }
                }
            }
            .disabled(isTesting || resolvedFleetToken.isEmpty)

            if model.isConfigured {
                Button("Clear Stored Token", role: .destructive) {
                    model.clearCredentials()
                    editToken = ""
                    statusBanner = "Stored token cleared from Keychain."
                }
            }
        }
    }

    private var resolvedFleetToken: String {
        if !editToken.isEmpty { return editToken }
        return model.serverURL == editServerURL ? model.tokenInput : ""
    }

    private var resolvedMCToken: String {
        if !editMCToken.isEmpty { return editMCToken }
        return model.mcPrimaryURL == editMCPrimaryURL && model.mcFallbackURL == editMCFallbackURL ? model.mcTokenInput : ""
    }

    private func applyPastedPairingLink(_ pairingInput: String) {
        let trimmed = pairingInput.trimmingCharacters(in: .whitespacesAndNewlines)
        guard let url = URL(string: trimmed) else {
            statusBanner = PairingLinkError.invalidURL.localizedDescription
            return
        }
        Task {
            await model.applyPairingURL(url)
            editServerURL = model.serverURL
            editToken = ""
            editMCPrimaryURL = model.mcPrimaryURL
            editMCFallbackURL = model.mcFallbackURL
            editMCToken = ""
            statusBanner = model.errorMessage ?? model.mcErrorMessage ?? "Pairing saved."
        }
    }

    private func saveMCCredentials() {
        do {
            try model.saveMCCredentials(primaryURL: editMCPrimaryURL, fallbackURL: editMCFallbackURL, token: resolvedMCToken)
            statusBanner = "Saved Mission Control credentials to Keychain."
            Task { await model.refreshAll() }
        } catch {
            statusBanner = error.localizedDescription
        }
    }

    private func testMCConnection() async {
        isTestingMC = true
        defer { isTestingMC = false }
        do {
            let health = try await model.testMCConnection(primaryURL: editMCPrimaryURL, fallbackURL: editMCFallbackURL, token: resolvedMCToken)
            statusBanner = "Connected to Mission Control on \(health.host ?? "hub") (ok=\(health.ok))."
        } catch {
            statusBanner = error.localizedDescription
        }
    }

    private func saveManualCredentials() {
        do {
            try model.saveCredentials(serverURL: editServerURL, token: resolvedFleetToken)
            statusBanner = "Saved hub credentials to Keychain."
            Task { await model.refreshAll() }
        } catch {
            statusBanner = error.localizedDescription
        }
    }

    private func testConnection() async {
        isTesting = true
        defer { isTesting = false }
        do {
            let health = try await model.testConnection(serverURL: editServerURL, token: resolvedFleetToken)
            statusBanner = "Connected to \(health.host ?? "hub") (API v\(health.apiVersion ?? 1), ok=\(health.ok))."
        } catch {
            statusBanner = error.localizedDescription
        }
    }

    private func triggerRefresh() async {
        isTriggeringMorning = true
        defer { isTriggeringMorning = false }
        do {
            let res = try await model.triggerMorningRefresh()
            statusBanner = res.started == true ? "Morning refresh started on hub. Pull Overview to refresh shortly." : "Requested refresh."
        } catch {
            statusBanner = error.localizedDescription
        }
    }

    private func toggleProvider(_ provider: String, paused: Bool) async {
        busyProvider = provider
        defer { busyProvider = nil }
        do {
            _ = try await model.setProviderPaused(provider: provider, paused: paused)
            statusBanner = "\(provider) \(paused ? "paused" : "resumed")."
        } catch {
            statusBanner = error.localizedDescription
        }
    }
}

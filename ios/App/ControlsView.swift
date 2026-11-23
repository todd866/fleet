import SwiftUI

struct ControlsView: View {
    @Bindable var model: FleetAppModel

    @State private var pairingInput = ""
    @State private var editServerURL = ""
    @State private var editToken = ""
    @State private var statusBanner: String?
    @State private var isTesting = false
    @State private var isTriggeringMorning = false
    @State private var busyProvider: String?

    private let managedProviders = ["claude", "codex", "zai", "cursor"]

    var body: some View {
        NavigationStack {
            List {
                if let statusBanner {
                    Section {
                        Text(statusBanner)
                            .font(.subheadline)
                            .foregroundStyle(FleetTheme.info)
                    }
                }

                operationsSection
                providersSection
                pairingSection
                manualConfigSection
                aboutSection
            }
            .scrollContentBackground(.hidden)
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("Controls")
            .onAppear {
                editServerURL = model.serverURL
                editToken = model.tokenInput
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
            TextField("fleet://pair?url=https://...&token=...", text: $pairingInput)
                .textInputAutocapitalization(.never)
                .autocorrectionDisabled()
                .font(.caption.monospaced())

            Button("Apply Pairing Link") {
                applyPastedPairingLink()
            }
            .disabled(pairingInput.trimmingCharacters(in: .whitespacesAndNewlines).isEmpty)
        } header: {
            Text("Quick Pairing")
        } footer: {
            Text("Generate a pairing URL on your Mac with fleet-ios pair-url or open /v1/ota/install in Safari.")
        }
    }

    private var manualConfigSection: some View {
        Section("Hub Connection") {
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
                SecureField("Paste hub API token", text: $editToken)
                    .textInputAutocapitalization(.never)
                    .autocorrectionDisabled()
                    .font(.subheadline.monospaced())
            }

            Button("Save Credentials") {
                saveManualCredentials()
            }
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
            .disabled(isTesting)

            if model.isConfigured {
                Button("Clear Stored Token", role: .destructive) {
                    model.clearCredentials()
                    editToken = ""
                    statusBanner = "Stored token cleared from Keychain."
                }
            }
        }
    }

    private var aboutSection: some View {
        Section("Security") {
            LabeledContent("Exposure", value: "Tailnet only (HTTPS)")
            LabeledContent("Keychain", value: model.isConfigured ? "Token saved" : "Unpaired")
            if let last = model.lastRefreshDate {
                LabeledContent("Last Sync", value: last.formatted(date: .omitted, time: .shortened))
            }
        }
    }

    private func applyPastedPairingLink() {
        do {
            let link = try PairingLink.parse(pairingInput)
            try model.saveCredentials(serverURL: link.serverURL, token: link.token)
            editServerURL = link.serverURL
            editToken = link.token
            pairingInput = ""
            statusBanner = "Paired cleanly. Syncing telemetry…"
            Task { await model.refreshAll() }
        } catch {
            statusBanner = error.localizedDescription
        }
    }

    private func saveManualCredentials() {
        do {
            try model.saveCredentials(serverURL: editServerURL, token: editToken)
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
            let health = try await model.testConnection(serverURL: editServerURL, token: editToken)
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

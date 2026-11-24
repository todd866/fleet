import SwiftUI

struct FleetViewPicker: View {
    @Binding var selection: FleetSubtab

    var body: some View {
        Picker("Fleet view", selection: $selection) {
            ForEach(FleetSubtab.allCases) { tab in
                Text(tab.rawValue).tag(tab)
            }
        }
        .pickerStyle(.segmented)
    }
}

struct ServicesView: View {
    @Bindable var model: FleetAppModel
    @State private var query = ""
    @State private var directory: ServiceDirectory?
    @State private var error: String?
    @State private var client = FleetAPIClient()

    var body: some View {
        NavigationStack {
            List {
                if query.isEmpty {
                    NavigationLink {
                        StackGuide()
                    } label: { Label("How the stack fits together", systemImage: "square.3.layers.3d") }
                    .accessibilityIdentifier("fleet.stack.guide")
                }
                if !model.isConfigured {
                    Button("Pair Fleet to browse services") { model.openControls() }
                } else if let directory {
                    serviceList(directory)
                } else if let error {
                    Text(error).foregroundStyle(.secondary)
                    Button("Retry") { Task { await load() } }
                } else {
                    ProgressView("Loading services")
                }
            }
            .searchable(text: $query, placement: .navigationBarDrawer(displayMode: .always),
                        prompt: "Service, project, host or purpose")
            .refreshable { await load() }
            .navigationTitle("Stack")
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .principal) { FleetViewPicker(selection: $model.fleetSubtab) }
            }
            .task { await load() }
        }
    }

    @ViewBuilder private func serviceList(_ directory: ServiceDirectory) -> some View {
        let groups = Dictionary(grouping: directory.services.filter { $0.matches(query) }, by: \.owner)
        Group {
            if let error { Text(error).foregroundStyle(.secondary) }
            if !directory.catalogAvailable { Text("Job definitions unavailable").foregroundStyle(.secondary) }
            if groups.isEmpty {
                Text(query.isEmpty ? "No services reported" : "No matching services")
            }
            if query.isEmpty {
                Section("Projects · \(directory.services.count) services") {
                    ForEach(groups.keys.sorted(), id: \.self) { owner in
                        let jobs = groups[owner] ?? []
                        NavigationLink {
                            List {
                                if let purpose = directory.projects?[owner], !purpose.isEmpty {
                                    Text(purpose).textSelection(.enabled)
                                }
                                serviceRows(jobs, directory)
                            }
                            .navigationTitle(owner)
                        } label: {
                            LabeledContent(owner, value: String(jobs.count))
                        }
                    }
                }
            } else {
                ForEach(groups.keys.sorted(), id: \.self) { owner in
                    Section(owner) { serviceRows(groups[owner] ?? [], directory) }
                }
            }
        }

    }

    private func serviceRows(_ jobs: [FleetService], _ directory: ServiceDirectory) -> some View {
        ForEach(jobs) { service in
            NavigationLink {
                ServiceDetail(service: service, observations: directory.observations)
            } label: {
                let hosts = service.observed.keys.sorted().joined(separator: " · ")
                VStack(alignment: .leading, spacing: 3) {
                    Text(service.name).font(.headline)
                    Text(hosts.isEmpty ? "No observations" : hosts)
                        .font(.subheadline).foregroundStyle(.secondary)
                }
            }
        }
    }

    @MainActor private func load() async {
        guard model.isConfigured else { return }
        do {
            let fetched = try await client.fetchServices()
            try Task.checkCancellation()
            directory = fetched
            error = nil
        } catch {
            if !Task.isCancelled { self.error = error.localizedDescription }
        }
    }
}

private struct ServiceDetail: View {
    let service: FleetService
    let observations: [String: String]

    var body: some View {
        List {
            Section {
                Text(service.label).font(.body.monospaced()).textSelection(.enabled)
                if !service.purpose.isEmpty { Text(service.purpose).textSelection(.enabled) }
                LabeledContent("Project", value: service.owner)
                LabeledContent("Expected on", value: service.expected.isEmpty ? "Unspecified" : service.expected)
                if !service.review.isEmpty { LabeledContent("Review", value: service.review) }
            }
            Section("Last observed · host local time") {
                ForEach(observations.keys.sorted(), id: \.self) { host in
                    VStack(alignment: .leading, spacing: 3) {
                        LabeledContent(host, value: service.observed[host]?.state ?? "Absent from snapshot")
                        if let when = service.observed[host]?.schedule, !when.isEmpty { Text(when) }
                        Text(observations[host].flatMap { $0.isEmpty ? nil : $0 } ?? "Time unknown")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                if observations.isEmpty { Text("No host snapshots available") }
            }
        }
        .navigationTitle(service.name)
        .navigationBarTitleDisplayMode(.inline)
    }
}

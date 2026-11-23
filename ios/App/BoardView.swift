import SwiftUI

struct BoardView: View {
    @Bindable var model: FleetAppModel
    @State private var selectedFilter: FleetRowFilter = .all
    @State private var searchText = ""
    @State private var selectedRow: MorningRow?

    private var allRows: [MorningRow] {
        model.morning?.rows ?? model.overview?.security.rows ?? []
    }

    private var filteredRows: [MorningRow] {
        filterAndSortRows(allRows, filter: selectedFilter, searchText: searchText)
    }

    var body: some View {
        NavigationStack {
            List {
                if let sec = model.security {
                    securitySection(sec)
                }

                filterChipsSection

                Section("Checks (\(filteredRows.count))") {
                    if filteredRows.isEmpty {
                        Text("No checks match the active filter.")
                            .foregroundStyle(.secondary)
                    } else {
                        ForEach(filteredRows) { row in
                            Button {
                                selectedRow = row
                            } label: {
                                MorningRowCell(row: row)
                            }
                            .buttonStyle(.plain)
                        }
                    }
                }

                if let violations = model.backups?.placement?.violations, !violations.isEmpty {
                    Section("Placement Violations (\(violations.count))") {
                        ForEach(violations) { item in
                            VStack(alignment: .leading, spacing: 4) {
                                HStack {
                                    Text("\(item.host.uppercased()) · \(item.dataset)")
                                        .font(.subheadline.weight(.semibold))
                                    Spacer()
                                    StatusPill(label: item.code, color: FleetTheme.warn)
                                }
                                if let detail = item.detail, !detail.isEmpty {
                                    Text(detail)
                                        .font(.caption)
                                        .foregroundStyle(.secondary)
                                }
                            }
                            .padding(.vertical, 2)
                        }
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("Board")
            .searchable(text: $searchText, prompt: "Search checks, areas, or details")
            .refreshable {
                await model.refreshAll()
            }
            .sheet(item: $selectedRow) { row in
                MorningRowDetailSheet(row: row)
            }
        }
    }

    private var filterChipsSection: some View {
        Section {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(FleetRowFilter.allCases) { filter in
                        Button {
                            selectedFilter = filter
                        } label: {
                            Text(filter.rawValue)
                                .font(.caption.weight(.semibold))
                                .padding(.horizontal, 12)
                                .padding(.vertical, 6)
                                .background(
                                    selectedFilter == filter ? FleetTheme.pass.opacity(0.25) : FleetTheme.elevated,
                                    in: Capsule()
                                )
                                .overlay(
                                    Capsule().stroke(
                                        selectedFilter == filter ? FleetTheme.pass : FleetTheme.border,
                                        lineWidth: 1
                                    )
                                )
                                .foregroundStyle(selectedFilter == filter ? FleetTheme.pass : .primary)
                        }
                        .buttonStyle(.plain)
                    }
                }
                .padding(.vertical, 4)
            }
            .listRowInsets(EdgeInsets(top: 4, leading: 12, bottom: 4, trailing: 12))
            .listRowBackground(Color.clear)
        }
    }

    private func securitySection(_ sec: SecurityResponse) -> some View {
        Section("Security Evidence") {
            VStack(alignment: .leading, spacing: 8) {
                HStack {
                    StatusPill(label: sec.status, color: FleetTheme.color(forStatus: sec.status))
                    if let open = sec.localTripwireOpen {
                        StatusPill(
                            label: "\(open) tripwire open",
                            color: open == 0 ? FleetTheme.pass : FleetTheme.warn
                        )
                    }
                    Spacer()
                    Text(FleetFormatter.relativeTime(fromEpoch: sec.generated))
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                if let val = sec.value, !val.isEmpty {
                    Text(val)
                        .font(.subheadline.weight(.medium))
                }
                if let detail = sec.detail, !detail.isEmpty {
                    Text(detail)
                        .font(.caption)
                        .foregroundStyle(.secondary)
                }

                if let audits = sec.namespaceAudits, !audits.isEmpty {
                    ForEach(audits.keys.sorted(), id: \.self) { ns in
                        if let audit = audits[ns] {
                            VStack(alignment: .leading, spacing: 3) {
                                HStack {
                                    Text("Audit \(ns.uppercased())")
                                        .font(.caption.weight(.bold))
                                    Spacer()
                                    Text("\(audit.failCount ?? 0) fail · \(audit.warnCount ?? 0) warn")
                                        .font(.caption.monospacedDigit())
                                        .foregroundStyle(.secondary)
                                }
                                ForEach(audit.topFindings?.prefix(3) ?? []) { finding in
                                    Text("• [\(finding.level)] \(finding.rule): \(finding.text)")
                                        .font(.caption2)
                                        .foregroundStyle(.secondary)
                                }
                            }
                            .padding(.top, 4)
                        }
                    }
                }
            }
            .padding(.vertical, 4)
        }
    }
}

private struct MorningRowCell: View {
    let row: MorningRow

    var body: some View {
        VStack(alignment: .leading, spacing: 5) {
            HStack(alignment: .firstTextBaseline) {
                StatusPill(label: row.status, color: FleetTheme.color(forStatus: row.status))
                Text(row.name)
                    .font(.subheadline.weight(.semibold))
                    .lineLimit(2)
                Spacer()
                if let area = row.area, !area.isEmpty {
                    Text(area)
                        .font(.caption2.monospaced())
                        .foregroundStyle(.secondary)
                }
            }

            if let value = row.value, !value.isEmpty {
                Text(value)
                    .font(.caption)
                    .foregroundStyle(.primary)
            }
            if let detail = row.detail, !detail.isEmpty {
                Text(detail)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(3)
            }
        }
        .padding(.vertical, 4)
    }
}

private struct MorningRowDetailSheet: View {
    let row: MorningRow
    @Environment(\.dismiss) private var dismiss

    var body: some View {
        NavigationStack {
            List {
                Section("Summary") {
                    LabeledContent("Key", value: row.key)
                    LabeledContent("Status", value: row.status.uppercased())
                    if let area = row.area {
                        LabeledContent("Area", value: area)
                    }
                    if let value = row.value {
                        LabeledContent("Value", value: value)
                    }
                    if let datum = row.datum {
                        LabeledContent("Datum", value: datum)
                    }
                }
                if let detail = row.detail, !detail.isEmpty {
                    Section("Detail") {
                        Text(detail)
                            .font(.callout.monospaced())
                            .textSelection(.enabled)
                    }
                }
            }
            .navigationTitle(row.name)
            .navigationBarTitleDisplayMode(.inline)
            .toolbar {
                ToolbarItem(placement: .confirmationAction) {
                    Button("Done") { dismiss() }
                }
            }
        }
        .preferredColorScheme(.dark)
    }
}

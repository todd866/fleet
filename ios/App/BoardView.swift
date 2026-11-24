import SwiftUI

struct BoardView: View {
    @Bindable var model: FleetAppModel
    @State private var searchText = ""
    @State private var selectedRow: MorningRow?
    @State private var selectedViolation: PlacementViolation?

    private var allRows: [MorningRow] {
        model.morning?.rows ?? model.overview?.security.rows ?? []
    }

    private var filteredRows: [MorningRow] {
        filterAndSortRows(allRows, filter: model.boardFilter, searchText: searchText)
    }

    private var groupedRows: [BoardAreaGroup] {
        groupRowsByArea(filteredRows)
    }

    var body: some View {
        NavigationStack {
            List {
                filterChipsSection

                if let sec = model.security, (model.boardFilter == .security || model.boardFilter == .issues) {
                    securitySection(sec)
                }

                if groupedRows.isEmpty {
                    Section {
                        VStack(spacing: 6) {
                            Image(systemName: "checkmark.seal.fill")
                                .font(.title2)
                                .foregroundStyle(FleetTheme.pass)
                            Text("No matching items")
                                .font(.subheadline.weight(.semibold))
                            Text(model.boardFilter == .issues ? "No failing or warning checks right now." : "Try another filter chip or search term.")
                                .font(.caption)
                                .foregroundStyle(.secondary)
                        }
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 12)
                    }
                } else {
                    ForEach(groupedRows) { group in
                        Section {
                            ForEach(group.rows) { row in
                                Button {
                                    selectedRow = row
                                } label: {
                                    MorningRowCell(row: row)
                                }
                                .buttonStyle(.plain)
                            }
                        } header: {
                            HStack {
                                Text(group.title)
                                Spacer()
                                Text("\(group.rows.count)")
                                    .monospacedDigit()
                            }
                        }
                    }
                }

                if let violations = model.backups?.placement?.violations,
                   !violations.isEmpty,
                   (model.boardFilter == .placement || model.boardFilter == .issues || model.boardFilter == .all) {
                    Section("Placement Gaps (\(violations.count))") {
                        ForEach(violations) { item in
                            Button {
                                selectedViolation = item
                            } label: {
                                HStack {
                                    VStack(alignment: .leading, spacing: 4) {
                                        HStack {
                                            Text("\(item.host.uppercased()) · \(item.dataset)")
                                                .font(.subheadline.weight(.semibold))
                                                .foregroundStyle(.primary)
                                            Spacer()
                                            StatusPill(label: item.code, color: FleetTheme.warn)
                                        }
                                        if let detail = item.detail, !detail.isEmpty {
                                            Text(detail)
                                                .font(.caption)
                                                .foregroundStyle(.secondary)
                                        }
                                    }
                                    Image(systemName: "chevron.right")
                                        .font(.caption2.weight(.semibold))
                                        .foregroundStyle(.tertiary)
                                }
                            }
                            .buttonStyle(.plain)
                            .padding(.vertical, 2)
                        }
                    }
                }
            }
            .scrollContentBackground(.hidden)
            .background(FleetTheme.bg.ignoresSafeArea())
            .navigationTitle("Board")
            .searchable(text: $searchText, prompt: "Search checks, hosts, or jobs")
            .refreshable {
                await model.refreshAll()
            }
            .sheet(item: $selectedRow) { row in
                RowActionSheet(model: model, row: row)
            }
            .sheet(item: $selectedViolation) { violation in
                PlacementActionSheet(model: model, violation: violation)
            }
        }
    }

    private var filterChipsSection: some View {
        Section {
            ScrollView(.horizontal, showsIndicators: false) {
                HStack(spacing: 8) {
                    ForEach(FleetRowFilter.allCases) { filter in
                        let cnt = filterAndSortRows(allRows, filter: filter).count
                        let isSelected = model.boardFilter == filter
                        Button {
                            model.boardFilter = filter
                        } label: {
                            Text("\(filter.rawValue) (\(cnt))")
                                .font(.caption.weight(.semibold))
                                .monospacedDigit()
                                .padding(.horizontal, 12)
                                .padding(.vertical, 6)
                                .background(
                                    isSelected ? FleetTheme.pass.opacity(0.25) : FleetTheme.elevated,
                                    in: Capsule()
                                )
                                .overlay(
                                    Capsule().stroke(
                                        isSelected ? FleetTheme.pass : FleetTheme.border,
                                        lineWidth: 1
                                    )
                                )
                                .foregroundStyle(isSelected ? FleetTheme.pass : .primary)
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
        let namespaces = sec.namespaces ?? [:]
        let audits = sec.namespaceAudits ?? [:]
        return Section("Security Evidence") {
            if !namespaces.isEmpty {
                VStack(alignment: .leading, spacing: 6) {
                    Text("LIVE CHAINS")
                        .font(.caption2.weight(.bold))
                        .foregroundStyle(.secondary)
                    ForEach(namespaces.keys.sorted(), id: \.self) { ns in
                        if let info = namespaces[ns] {
                            HStack {
                                Text(ns.uppercased())
                                    .font(.caption.weight(.bold).monospaced())
                                Text("\(info.integrity ?? "ok")\(info.links.map { " (\($0) links)" } ?? "")")
                                    .font(.caption)
                                    .foregroundStyle(.secondary)
                                Spacer()
                                if let silent = info.silentMin {
                                    Text("\(String(format: "%.0f", silent))m")
                                        .font(.caption2.monospacedDigit())
                                        .foregroundStyle(.secondary)
                                }
                                StatusPill(
                                    label: info.status ?? "pass",
                                    color: FleetTheme.color(forStatus: info.status ?? "pass")
                                )
                            }
                        }
                    }
                }
                .padding(.vertical, 2)
            }

            if !audits.isEmpty || (sec.localTripwireOpen ?? 0) > 0 {
                VStack(alignment: .leading, spacing: 6) {
                    HStack {
                        Text("DAILY AUDIT")
                            .font(.caption2.weight(.bold))
                            .foregroundStyle(.secondary)
                        Spacer()
                        if let open = sec.localTripwireOpen {
                            StatusPill(
                                label: "\(open) tripwire open",
                                color: open == 0 ? FleetTheme.pass : FleetTheme.warn
                            )
                        }
                    }
                    ForEach(audits.keys.sorted(), id: \.self) { ns in
                        if let audit = audits[ns] {
                            VStack(alignment: .leading, spacing: 3) {
                                HStack {
                                    Text("\(ns.uppercased()) · \(audit.file ?? "latest")")
                                        .font(.caption.weight(.semibold))
                                    Spacer()
                                    Text("\(audit.failCount ?? 0) fail · \(audit.warnCount ?? 0) warn")
                                        .font(.caption.monospacedDigit())
                                        .foregroundStyle((audit.failCount ?? 0) > 0 ? FleetTheme.fail : .secondary)
                                }
                                ForEach(audit.topFindings?.prefix(3) ?? []) { finding in
                                    Text("• [\(finding.level)] \(finding.rule): \(finding.text)")
                                        .font(.caption2)
                                        .foregroundStyle(.secondary)
                                }
                            }
                            .padding(.top, 2)
                        }
                    }
                }
                .padding(.vertical, 2)
            }
        }
    }
}

private struct MorningRowCell: View {
    let row: MorningRow

    private var displayTitle: String {
        let area = (row.area ?? "").lowercased()
        if area == "job" || row.key.hasPrefix("job:") {
            if !row.name.contains(" "), row.name.split(separator: ".").count >= 3 {
                return shortJobName(row.name)
            }
        }
        return row.name
    }

    var body: some View {
        HStack(spacing: 10) {
            RoundedRectangle(cornerRadius: 2)
                .fill(row.isSnoozed ? FleetTheme.info : FleetTheme.color(forSeverity: row.severity))
                .frame(width: 4)
                .padding(.vertical, 2)

            VStack(alignment: .leading, spacing: 4) {
                HStack(alignment: .firstTextBaseline, spacing: 6) {
                    Text(displayTitle)
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(row.isSnoozed ? .secondary : .primary)
                        .lineLimit(2)
                    Spacer()
                    if row.isSnoozed {
                        StatusPill(label: "Snoozed", color: FleetTheme.info)
                    } else if let val = row.value, !val.isEmpty {
                        Text(val)
                            .font(.caption.weight(.medium).monospacedDigit())
                            .foregroundStyle(FleetTheme.color(forSeverity: row.severity))
                            .lineLimit(1)
                    } else {
                        StatusPill(label: row.status, color: FleetTheme.color(forSeverity: row.severity))
                    }
                }

                if let detail = row.detail, !detail.isEmpty {
                    Text(detail)
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                        .lineLimit(2)
                }
            }
        }
        .padding(.vertical, 3)
    }
}

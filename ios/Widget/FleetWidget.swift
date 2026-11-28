import SwiftUI
import WidgetKit

struct FleetWidgetEntry: TimelineEntry {
    let date: Date
    let overview: FleetOverviewResponse?
}

struct FleetTimelineProvider: TimelineProvider {
    func placeholder(in context: Context) -> FleetWidgetEntry {
        FleetWidgetEntry(date: Date(), overview: Self.sampleOverview)
    }

    func getSnapshot(in context: Context, completion: @escaping (FleetWidgetEntry) -> Void) {
        let cached = FleetStore.shared.loadOverview()
        completion(FleetWidgetEntry(date: Date(), overview: cached ?? (context.isPreview ? Self.sampleOverview : nil)))
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<FleetWidgetEntry>) -> Void) {
        Task {
            var snapshot = FleetStore.shared.loadOverview()
            if KeychainStore.shared.loadCredentials().isConfigured,
               let fresh = try? await FleetAPIClient().fetchOverview() {
                FleetStore.shared.saveOverview(fresh)
                snapshot = fresh
            }
            let now = Date()
            let nextRefresh = Calendar.current.date(byAdding: .minute, value: 15, to: now) ?? now.addingTimeInterval(900)
            let entry = FleetWidgetEntry(date: now, overview: snapshot)
            completion(Timeline(entries: [entry], policy: .after(nextRefresh)))
        }
    }

    private static var sampleOverview: FleetOverviewResponse {
        FleetOverviewResponse(
            generatedAt: Int(Date().timeIntervalSince1970) - 300,
            date: "Today",
            counts: CountsSummary(pass: 312, warn: 18, fail: 2),
            cadence: nil,
            agyMorning: nil,
            hosts: [],
            capacity: CapacityResponse(generatedAt: nil, route: nil, paused: [], pools: []),
            backups: OverviewBackups(idrive: nil, gdrive: nil, homeMirror: nil, placementViolations: 0),
            security: SecuritySummaryBlock(status: "pass"),
            unreadMail: 1,
            activeClaims: 0
        )
    }
}

struct FleetStatusWidgetView: View {
    @Environment(\.widgetFamily) private var family
    let entry: FleetWidgetEntry

    private let bgColor = Color(red: 0.055, green: 0.075, blue: 0.110)
    private let passColor = Color(red: 0.133, green: 0.773, blue: 0.369)
    private let warnColor = Color(red: 0.961, green: 0.620, blue: 0.043)
    private let failColor = Color(red: 0.937, green: 0.267, blue: 0.267)

    var body: some View {
        switch family {
        case .accessoryRectangular:
            accessoryView
        case .systemSmall:
            smallView
        default:
            mediumView
        }
    }

    private var accessoryView: some View {
        VStack(alignment: .leading, spacing: 2) {
            if let ov = entry.overview {
                Text("FLEET \(ov.counts.fail > 0 ? "ALERT" : "OK")")
                    .font(.caption.weight(.bold))
                Text("\(ov.counts.pass)P · \(ov.counts.warn)W · \(ov.counts.fail)F")
                    .font(.caption2.monospacedDigit())
                Text("Sec \(ov.security.status) · Mail \(ov.unreadMail)")
                    .font(.caption2)
            } else {
                Text("FLEET")
                    .font(.caption.weight(.bold))
                Text("Open app to sync")
                    .font(.caption2)
            }
        }
        .containerBackground(.clear, for: .widget)
    }

    private var smallView: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack {
                Text("FLEET")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(.secondary)
                Spacer()
                if let ov = entry.overview {
                    Circle()
                        .fill(ov.counts.fail > 0 ? failColor : (ov.counts.warn > 0 ? warnColor : passColor))
                        .frame(width: 8, height: 8)
                }
            }

            if let ov = entry.overview {
                HStack(spacing: 8) {
                    statColumn(ov.counts.pass, "PASS", passColor)
                    statColumn(ov.counts.warn, "WARN", warnColor)
                    statColumn(ov.counts.fail, "FAIL", failColor)
                }
                Spacer()
                Text("Sec \(ov.security.status) · \(FleetFormatter.relativeTime(fromEpoch: ov.generatedAt))")
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            } else {
                Spacer()
                Text("Open Fleet to sync")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
            }
        }
        .containerBackground(bgColor, for: .widget)
    }

    private var mediumView: some View {
        VStack(alignment: .leading, spacing: 10) {
            HStack {
                Text("FLEET MISSION CONTROL")
                    .font(.caption2.weight(.bold))
                    .foregroundStyle(.secondary)
                Spacer()
                if let ov = entry.overview {
                    Text(FleetFormatter.relativeTime(fromEpoch: ov.generatedAt))
                        .font(.caption2)
                        .foregroundStyle(.secondary)
                }
            }

            if let ov = entry.overview {
                HStack(spacing: 16) {
                    statColumn(ov.counts.pass, "PASS", passColor)
                    statColumn(ov.counts.warn, "WARN", warnColor)
                    statColumn(ov.counts.fail, "FAIL", failColor)
                    Spacer()
                    VStack(alignment: .trailing, spacing: 3) {
                        Text("Sec: \(ov.security.status.uppercased())")
                            .font(.caption.weight(.semibold))
                            .foregroundStyle(ov.security.status == "pass" ? passColor : failColor)
                        Text("Mail: \(ov.unreadMail) · Claims: \(ov.activeClaims)")
                            .font(.caption2)
                            .foregroundStyle(.secondary)
                    }
                }

                if !ov.hosts.isEmpty {
                    HStack(spacing: 8) {
                        ForEach(ov.hosts.prefix(4)) { host in
                            HStack(spacing: 4) {
                                Circle()
                                    .fill((host.failCount ?? 0) > 0 ? failColor : passColor)
                                    .frame(width: 6, height: 6)
                                Text(host.id.uppercased())
                                    .font(.caption2.monospaced().weight(.semibold))
                                if let free = host.diskFreeGB {
                                    Text(FleetFormatter.formatGB(free))
                                        .font(.caption2.monospacedDigit())
                                        .foregroundStyle(.secondary)
                                }
                            }
                        }
                    }
                }
            } else {
                Spacer()
                Text("Open Fleet and pair your hub to populate telemetry.")
                    .font(.caption)
                    .foregroundStyle(.secondary)
                Spacer()
            }
        }
        .containerBackground(bgColor, for: .widget)
    }

    private func statColumn(_ count: Int, _ title: String, _ color: Color) -> some View {
        VStack(alignment: .leading, spacing: 1) {
            Text("\(count)")
                .font(.title3.weight(.bold).monospacedDigit())
                .foregroundStyle(color)
            Text(title)
                .font(.system(size: 9, weight: .bold))
                .foregroundStyle(.secondary)
        }
    }
}

struct FleetStatusWidget: Widget {
    let kind: String = "FleetStatusWidget"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: FleetTimelineProvider()) { entry in
            FleetStatusWidgetView(entry: entry)
        }
        .configurationDisplayName("Fleet Status")
        .description("Live scoreboard, security posture, and Mac disk health.")
        .supportedFamilies([.systemSmall, .systemMedium, .accessoryRectangular])
    }
}

@main
struct FleetWidgetBundle: WidgetBundle {
    var body: some Widget {
        FleetStatusWidget()
        FleetDriveWidget()
    }
}

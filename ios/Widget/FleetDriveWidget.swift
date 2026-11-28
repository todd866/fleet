import SwiftUI
import WidgetKit

struct FleetDriveWidgetEntry: TimelineEntry {
    let date: Date
    let brief: DriveBriefResponse
    let isPlaceholder: Bool
}

struct FleetDriveTimelineProvider: TimelineProvider {
    func placeholder(in context: Context) -> FleetDriveWidgetEntry {
        FleetDriveWidgetEntry(date: Date(), brief: Self.sampleBrief, isPlaceholder: true)
    }

    func getSnapshot(in context: Context, completion: @escaping (FleetDriveWidgetEntry) -> Void) {
        if let cached = FleetStore.shared.loadDriveBrief() {
            completion(FleetDriveWidgetEntry(date: Date(), brief: cached, isPlaceholder: false))
        } else {
            completion(FleetDriveWidgetEntry(date: Date(), brief: Self.sampleBrief, isPlaceholder: context.isPreview))
        }
    }

    func getTimeline(in context: Context, completion: @escaping (Timeline<FleetDriveWidgetEntry>) -> Void) {
        Task {
            var brief = FleetStore.shared.loadDriveBrief()
            if KeychainStore.shared.loadCredentials().isConfigured,
               let fresh = try? await FleetAPIClient().fetchDriveBrief() {
                FleetStore.shared.saveDriveBrief(fresh)
                brief = fresh
            }
            let now = Date()
            let nextRefresh = Calendar.current.date(byAdding: .minute, value: 15, to: now) ?? now.addingTimeInterval(900)
            let entry = FleetDriveWidgetEntry(date: now, brief: brief ?? Self.sampleBrief, isPlaceholder: false)
            completion(Timeline(entries: [entry], policy: .after(nextRefresh)))
        }
    }

    static var sampleBrief: DriveBriefResponse {
        DriveBriefResponse(
            ok: true,
            generatedAt: Int(Date().timeIntervalSince1970),
            destination: DriveDestination(
                name: "City Clinic",
                query: "120 St Georges Tce",
                source: "calendar",
                eventStart: nil
            ),
            wazeURL: "waze://?q=120%20St%20Georges%20Tce&navigate=yes",
            music: DriveMusicPick(
                stationID: "alt",
                stationName: "Alt",
                lensID: "home-station:alt",
                mood: "morning",
                source: "schedule:morning",
                url: "ontotheradio://play?station=alt"
            ),
            volume: 0.45,
            volumePct: 45,
            summary: "Waze → City Clinic · Alt · 45%",
            expiresAt: nil
        )
    }
}

struct FleetDriveWidgetView: View {
    @Environment(\.widgetFamily) private var family
    @Environment(\.colorScheme) private var scheme
    let entry: FleetDriveWidgetEntry

    var body: some View {
        FleetDriveCardView(brief: entry.brief, isMedium: family == .systemMedium)
            .containerBackground(FleetDriveCardView.surfaceColor(for: scheme), for: .widget)
    }
}

struct FleetDriveWidget: Widget {
    let kind: String = "FleetDriveWidget"

    var body: some WidgetConfiguration {
        StaticConfiguration(kind: kind, provider: FleetDriveTimelineProvider()) { entry in
            FleetDriveWidgetView(entry: entry)
        }
        .configurationDisplayName("Drive Brief")
        .description("Next Waze destination and curated CarPlay radio station.")
        .supportedFamilies([.systemSmall, .systemMedium])
    }
}

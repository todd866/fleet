import AppIntents
import Foundation

struct DriveBriefEntity: TransientAppEntity {
    static let typeDisplayRepresentation: TypeDisplayRepresentation = "Drive Brief"

    @Property(title: "Waze URL")
    var wazeURL: URL

    @Property(title: "Destination")
    var destinationName: String

    @Property(title: "Station ID")
    var stationID: String

    @Property(title: "Station Name")
    var stationName: String

    @Property(title: "Volume Level")
    var volumeLevel: Double

    @Property(title: "Volume Percent")
    var volumePercent: Int

    @Property(title: "Summary")
    var summary: String

    var displayRepresentation: DisplayRepresentation {
        DisplayRepresentation(
            title: LocalizedStringResource(stringLiteral: destinationName),
            subtitle: LocalizedStringResource(stringLiteral: "\(stationName) · \(volumePercent)%")
        )
    }

    init() {
        self.wazeURL = URL(string: "waze://")!
        self.destinationName = "Waze"
        self.stationID = "alt"
        self.stationName = "Alt"
        self.volumeLevel = 0.45
        self.volumePercent = 45
        self.summary = "Waze · Alt · 45%"
    }

    init(brief: DriveBriefResponse) {
        self.wazeURL = brief.resolvedWazeURL
        self.destinationName = brief.destination?.name ?? "Waze"
        self.stationID = brief.music.stationID
        self.stationName = brief.music.stationName
        self.volumeLevel = brief.volume
        self.volumePercent = brief.volumePct
        self.summary = brief.summary
    }
}

struct PrepareDriveIntent: AppIntent {
    static let title: LocalizedStringResource = "Prepare Drive"
    static let description = IntentDescription("Computes the next driving destination, curated station, and media volume for CarPlay.")
    static let openAppWhenRun: Bool = false

    @Parameter(title: "Event Title")
    var eventTitle: String?

    @Parameter(title: "Event Location")
    var eventLocation: String?

    @Parameter(title: "Event Start")
    var eventStart: String?

    @Parameter(title: "Latitude")
    var latitude: Double?

    @Parameter(title: "Longitude")
    var longitude: Double?

    @Parameter(title: "Shuffle Mix", default: false)
    var shuffle: Bool

    init() {}

    func perform() async throws -> some IntentResult & ReturnsValue<DriveBriefEntity> & ProvidesDialog {
        let brief: DriveBriefResponse
        do {
            brief = try await FleetAPIClient().fetchDriveBrief(
                lat: latitude,
                lon: longitude,
                eventTitle: eventTitle,
                eventLocation: eventLocation,
                eventStart: eventStart,
                shuffle: shuffle
            )
        } catch {
            brief = DriveBriefResponse.offlineFallback(
                eventTitle: eventTitle,
                eventLocation: eventLocation,
                eventStart: eventStart
            )
        }
        FleetStore.shared.saveDriveBrief(brief)
        let entity = DriveBriefEntity(brief: brief)
        return .result(value: entity, dialog: IntentDialog(stringLiteral: brief.summary))
    }
}

struct ShuffleDriveMixIntent: AppIntent {
    static let title: LocalizedStringResource = "Next Drive Mix"
    static let description = IntentDescription("Cycles to the next curated CarPlay music station and updates the Drive widget.")
    static let openAppWhenRun: Bool = false

    init() {}

    func perform() async throws -> some IntentResult & ReturnsValue<DriveBriefEntity> & ProvidesDialog {
        let brief = try await FleetAPIClient().fetchDriveBrief(shuffle: true)
        FleetStore.shared.saveDriveBrief(brief)
        return .result(value: DriveBriefEntity(brief: brief), dialog: IntentDialog(stringLiteral: brief.music.stationName))
    }
}

struct SetDriveDestinationIntent: AppIntent {
    static let title: LocalizedStringResource = "Set Drive Destination"
    static let description = IntentDescription("Pre-sets a destination, music station, or volume for the next drive.")
    static let openAppWhenRun: Bool = false

    @Parameter(title: "Destination")
    var destination: String

    @Parameter(title: "Station")
    var station: String?

    @Parameter(title: "Volume Percent")
    var volumePercent: Int?

    init() {}

    func perform() async throws -> some IntentResult & ProvidesDialog {
        let brief = try await FleetAPIClient().setDrivePlan(
            destinationQuery: destination,
            destinationName: destination,
            stationID: station,
            volumePct: volumePercent
        )
        FleetStore.shared.saveDriveBrief(brief)
        return .result(dialog: IntentDialog(stringLiteral: brief.summary))
    }
}

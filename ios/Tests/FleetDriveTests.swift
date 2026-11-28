import SwiftUI
import UIKit
import XCTest
@testable import Fleet

final class FleetDriveTests: XCTestCase {
    func testDecodeDriveBriefAndResolvedWazeURL() throws {
        let json = """
        {
          "ok": true,
          "generated_at": 1795474800,
          "destination": {
            "name": "North Clinic",
            "query": "200 Hospital Ave",
            "lat": -31.91,
            "lon": 115.87,
            "source": "routine",
            "place_id": "work"
          },
          "waze_url": "waze://?ll=-31.910000,115.870000&navigate=yes",
          "music": {
            "station_id": "alt",
            "station_name": "Alt",
            "lens_id": "home-station:alt",
            "mood": "morning",
            "source": "schedule:morning",
            "url": "ontotheradio://play?station=alt"
          },
          "volume": 0.45,
          "volume_pct": 45,
          "summary": "Waze → North Clinic · Alt · 45%",
          "expires_at": null
        }
        """
        let brief = try JSONDecoder().decode(DriveBriefResponse.self, from: Data(json.utf8))
        XCTAssertEqual(brief.destination?.name, "North Clinic")
        XCTAssertEqual(brief.music.stationID, "alt")
        XCTAssertEqual(brief.volumePct, 45)
        XCTAssertEqual(brief.resolvedWazeURL.scheme, "waze")
    }

    func testOfflineFallbackFiltersVirtualLocations() {
        let virtual = DriveBriefResponse.offlineFallback(
            eventTitle: "Team Sync",
            eventLocation: "https://zoom.us/j/12345"
        )
        XCTAssertNil(virtual.destination)
        XCTAssertEqual(virtual.wazeURL, "waze://")

        let physical = DriveBriefResponse.offlineFallback(
            eventTitle: "Dentist",
            eventLocation: "45 Bay Rd"
        )
        XCTAssertEqual(physical.destination?.name, "Dentist")
        XCTAssertEqual(physical.wazeURL, "waze://?q=45%20Bay%20Rd&navigate=yes")
    }

    func testFleetStorePersistsDriveBrief() throws {
        let tmp = FileManager.default.temporaryDirectory.appendingPathComponent("fleet-drive-\(UUID().uuidString)")
        defer { try? FileManager.default.removeItem(at: tmp) }
        let store = FleetStore(appGroup: nil, overrideDirectory: tmp)
        let sample = DriveBriefResponse.offlineFallback(eventTitle: "Clinic", eventLocation: "1 Main St")
        store.saveDriveBrief(sample)
        let loaded = store.loadDriveBrief()
        XCTAssertEqual(loaded?.destination?.name, "Clinic")
        XCTAssertEqual(loaded?.music.stationID, "alt")
    }

    @MainActor
    func testRenderDriveWidgetLightAndDarkSnapshots() throws {
        let sample = DriveBriefResponse(
            ok: true,
            generatedAt: Int(Date().timeIntervalSince1970) - 60,
            destination: DriveDestination(
                name: "City Clinic",
                query: "120 St Georges Tce",
                source: "calendar"
            ),
            wazeURL: "waze://?q=120%20St%20Georges%20Tce&navigate=yes",
            music: DriveMusicPick(
                stationID: "jazz-miles-orbit",
                stationName: "Jazz, Miles orbit",
                lensID: "home-station:lens:radio:jazz-miles-orbit",
                mood: "evening",
                source: "schedule:evening",
                url: "ontotheradio://play?station=jazz-miles-orbit"
            ),
            volume: 0.45,
            volumePct: 45,
            summary: "Waze → City Clinic · Jazz, Miles orbit · 45%",
            expiresAt: nil
        )

        let outDir = FileManager.default.temporaryDirectory.appendingPathComponent("drive-widget-snaps", isDirectory: true)
        try FileManager.default.createDirectory(at: outDir, withIntermediateDirectories: true)

        let specs: [(String, Bool, ColorScheme, CGSize)] = [
            ("drive-widget-small-light.png", false, .light, CGSize(width: 169, height: 169)),
            ("drive-widget-small-dark.png", false, .dark, CGSize(width: 169, height: 169)),
            ("drive-widget-medium-light.png", true, .light, CGSize(width: 360, height: 169)),
            ("drive-widget-medium-dark.png", true, .dark, CGSize(width: 360, height: 169)),
        ]

        for (filename, isMedium, scheme, size) in specs {
            let root = FleetDriveCardView(brief: sample, isMedium: isMedium)
                .padding(14)
                .frame(width: size.width, height: size.height)
                .background(FleetDriveCardView.surfaceColor(for: scheme), in: RoundedRectangle(cornerRadius: 22, style: .continuous))
                .environment(\.colorScheme, scheme)
            let renderer = ImageRenderer(content: root)
            renderer.scale = 2.0
            let image = try XCTUnwrap(renderer.uiImage)
            let data = try XCTUnwrap(image.pngData())
            XCTAssertGreaterThan(data.count, 1000)
            try data.write(to: outDir.appendingPathComponent(filename))
        }
    }
}

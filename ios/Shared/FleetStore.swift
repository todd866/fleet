import Foundation
#if canImport(WidgetKit)
import WidgetKit
#endif

public final class FleetStore: Sendable {
    public static let shared = FleetStore()

    private let appGroup: String?
    private let overrideDirectory: URL?
    private let lock = NSLock()

    public init(
        appGroup: String? = Bundle.main.object(forInfoDictionaryKey: "FleetAppGroup") as? String,
        overrideDirectory: URL? = nil
    ) {
        if let appGroup, !appGroup.isEmpty, !appGroup.contains("$(") {
            self.appGroup = appGroup
        } else {
            self.appGroup = nil
        }
        self.overrideDirectory = overrideDirectory
    }

    public func cacheDirectory() -> URL {
        if let overrideDirectory {
            try? FileManager.default.createDirectory(at: overrideDirectory, withIntermediateDirectories: true)
            return overrideDirectory
        }
        if let appGroup,
           let groupURL = FileManager.default.containerURL(forSecurityApplicationGroupIdentifier: appGroup) {
            let dir = groupURL.appendingPathComponent("FleetCache", isDirectory: true)
            try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
            return dir
        }
        let appSupport = FileManager.default.urls(for: .applicationSupportDirectory, in: .userDomainMask).first
            ?? FileManager.default.temporaryDirectory
        let dir = appSupport.appendingPathComponent("FleetCache", isDirectory: true)
        try? FileManager.default.createDirectory(at: dir, withIntermediateDirectories: true)
        return dir
    }

    public func saveOverview(_ overview: FleetOverviewResponse) {
        saveJSON(overview, filename: "overview.json")
        #if canImport(WidgetKit)
        WidgetCenter.shared.reloadAllTimelines()
        #endif
    }

    public func loadOverview() -> FleetOverviewResponse? {
        loadJSON(FleetOverviewResponse.self, filename: "overview.json")
    }

    public func saveMorning(_ morning: MorningResponse) {
        saveJSON(morning, filename: "morning.json")
    }

    public func loadMorning() -> MorningResponse? {
        loadJSON(MorningResponse.self, filename: "morning.json")
    }

    private func saveJSON<T: Encodable>(_ value: T, filename: String) {
        lock.lock()
        defer { lock.unlock() }
        let dst = cacheDirectory().appendingPathComponent(filename)
        let encoder = JSONEncoder()
        guard let data = try? encoder.encode(value) else { return }
        try? data.write(to: dst, options: [.atomic])
    }

    private func loadJSON<T: Decodable>(_ type: T.Type, filename: String) -> T? {
        lock.lock()
        defer { lock.unlock() }
        let src = cacheDirectory().appendingPathComponent(filename)
        guard let data = try? Data(contentsOf: src) else { return nil }
        return try? JSONDecoder().decode(type, from: data)
    }
}

import Foundation

struct ServiceDirectory: Decodable {
    var catalogAvailable: Bool
    var projects: [String: String]?
    var observations: [String: String]
    var services: [FleetService]
}

struct FleetService: Decodable, Identifiable {
    struct Observation: Decodable {
        var state: String
        var schedule: String
    }

    var label: String
    var project: String
    var purpose: String
    var expected: String
    var review: String
    var observed: [String: Observation]
    var id: String { label }

    var name: String { label.components(separatedBy: ".").last ?? label }
    var owner: String { project.isEmpty ? "Unassigned" : project }

    func matches(_ query: String) -> Bool {
        query.isEmpty || ([label, project, purpose, expected] + observed.keys)
            .contains { $0.localizedCaseInsensitiveContains(query) }
    }
}

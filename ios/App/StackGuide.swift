import SwiftUI

/// The bundled source is the same Markdown table agents read in docs/stack.md.
struct StackGuide: View {
    struct Layer: Identifiable {
        var name: String
        var purpose: String
        var sources: String
        var id: String { name }
    }

    static func layers(_ markdown: String) -> [Layer] {
        markdown.components(separatedBy: .newlines).compactMap { line in
            guard line.hasPrefix("| ") else { return nil }
            let fields = line.split(separator: "|").map { $0.trimmingCharacters(in: .whitespaces) }
            guard fields.count == 4, fields[0] != "Layer" else { return nil }
            return Layer(name: fields[0], purpose: fields[1], sources: fields[2])
        }
    }

    private let layers: [Layer] = {
        guard let url = Bundle.main.url(forResource: "stack", withExtension: "md"),
              let markdown = try? String(contentsOf: url, encoding: .utf8) else { return [] }
        return StackGuide.layers(markdown)
    }()

    var body: some View {
        List(layers) { layer in
            NavigationLink(layer.name) {
                List {
                    Section { Text(Self.plain(layer.purpose)).textSelection(.enabled) }
                    Section("Source references · relative to docs/stack.md") {
                        ForEach(Self.references(layer.sources), id: \.self) { reference in
                            Text(reference).font(.callout.monospaced()).textSelection(.enabled)
                        }
                    }
                }
                .navigationTitle(layer.name)
            }
            .accessibilityIdentifier("fleet.stack.layer.\(layer.name)")
        }
        .overlay {
            if layers.isEmpty { ContentUnavailableView("Stack guide unavailable", systemImage: "doc") }
        }
        .navigationTitle("Stack guide")
    }

    private static func references(_ text: String) -> [String] {
        guard let regex = try? NSRegularExpression(pattern: #"\]\(([^)]+)\)"#) else { return [] }
        let source = text as NSString
        return regex.matches(in: text, range: NSRange(location: 0, length: source.length))
            .map { source.substring(with: $0.range(at: 1)) }
    }

    private static func plain(_ text: String) -> String {
        (try? AttributedString(markdown: text)).map { String($0.characters) } ?? text
    }
}

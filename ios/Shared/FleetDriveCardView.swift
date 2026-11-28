import AppIntents
import SwiftUI

public struct FleetDriveCardView: View {
    public let brief: DriveBriefResponse
    public let isMedium: Bool
    @Environment(\.colorScheme) private var scheme

    public init(brief: DriveBriefResponse, isMedium: Bool) {
        self.brief = brief
        self.isMedium = isMedium
    }

    public static func surfaceColor(for scheme: ColorScheme) -> Color {
        scheme == .dark
            ? Color(red: 0.086, green: 0.102, blue: 0.129)
            : Color(red: 0.957, green: 0.961, blue: 0.973)
    }

    private var controlSurface: Color {
        scheme == .dark
            ? Color.white.opacity(0.10)
            : Color.black.opacity(0.06)
    }

    private var accentColor: Color {
        scheme == .dark
            ? Color(red: 0.255, green: 0.780, blue: 0.569)
            : Color(red: 0.067, green: 0.502, blue: 0.341)
    }

    private var destIconName: String {
        switch brief.destination?.source {
        case "calendar": return "calendar"
        case "override": return "pin.fill"
        case "return_home": return "house.fill"
        case "routine": return "clock"
        default: return "map"
        }
    }

    private var destLabel: String {
        brief.destination?.name ?? "Waze Open"
    }

    public var body: some View {
        if isMedium {
            mediumView
        } else {
            smallView
        }
    }

    private var smallView: some View {
        VStack(alignment: .leading, spacing: 8) {
            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Image(systemName: "car.fill")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(accentColor)
                    .frame(width: 14)
                Text(destLabel)
                    .font(.subheadline.weight(.semibold))
                    .foregroundStyle(.primary)
                    .lineLimit(2)
                    .minimumScaleFactor(0.85)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 4)
                Image(systemName: destIconName)
                    .font(.caption2)
                    .foregroundStyle(.secondary)
                    .accessibilityLabel(brief.destination?.source ?? "route")
            }

            HStack(alignment: .firstTextBaseline, spacing: 6) {
                Image(systemName: "music.note")
                    .font(.caption.weight(.semibold))
                    .foregroundStyle(.secondary)
                    .frame(width: 14)
                Text(brief.music.stationName)
                    .font(.caption.weight(.medium))
                    .foregroundStyle(.primary)
                    .lineLimit(2)
                    .minimumScaleFactor(0.85)
                    .fixedSize(horizontal: false, vertical: true)
                Spacer(minLength: 4)
                Text("\(brief.volumePct)%")
                    .font(.caption.weight(.semibold).monospacedDigit())
                    .foregroundStyle(.secondary)
            }

            Spacer(minLength: 0)

            HStack(spacing: 6) {
                Link(destination: brief.resolvedWazeURL) {
                    Label("Go", systemImage: "location.fill")
                        .font(.caption.weight(.semibold))
                        .frame(maxWidth: .infinity)
                        .padding(.vertical, 7)
                        .background(accentColor.opacity(scheme == .dark ? 0.22 : 0.16), in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                        .foregroundStyle(accentColor)
                }
                .accessibilityLabel("Navigate to \(destLabel) in Waze")

                Button(intent: ShuffleDriveMixIntent()) {
                    Image(systemName: "shuffle")
                        .font(.caption.weight(.semibold))
                        .padding(.horizontal, 10)
                        .padding(.vertical, 7)
                        .background(controlSurface, in: RoundedRectangle(cornerRadius: 8, style: .continuous))
                        .foregroundStyle(.primary)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Next music mix")
            }
        }
    }

    private var mediumView: some View {
        HStack(spacing: 12) {
            VStack(alignment: .leading, spacing: 8) {
                HStack(alignment: .top, spacing: 8) {
                    Image(systemName: "car.fill")
                        .font(.subheadline.weight(.semibold))
                        .foregroundStyle(accentColor)
                        .frame(width: 16, alignment: .center)
                        .padding(.top, 2)
                    VStack(alignment: .leading, spacing: 2) {
                        HStack(spacing: 5) {
                            Text(destLabel)
                                .font(.headline.weight(.semibold))
                                .foregroundStyle(.primary)
                                .lineLimit(1)
                                .minimumScaleFactor(0.8)
                            Image(systemName: destIconName)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                        }
                        if let q = brief.destination?.query, !q.isEmpty, q != destLabel {
                            Text(q)
                                .font(.caption2)
                                .foregroundStyle(.secondary)
                                .lineLimit(1)
                                .minimumScaleFactor(0.85)
                        }
                    }
                }

                Divider()
                    .opacity(0.45)

                HStack(spacing: 8) {
                    Image(systemName: "music.note")
                        .font(.caption.weight(.semibold))
                        .foregroundStyle(.secondary)
                        .frame(width: 16, alignment: .center)
                    Text(brief.music.stationName)
                        .font(.subheadline.weight(.medium))
                        .foregroundStyle(.primary)
                        .lineLimit(1)
                    Spacer(minLength: 4)
                    Text("\(brief.volumePct)%")
                        .font(.caption.weight(.semibold).monospacedDigit())
                        .foregroundStyle(.primary)
                    Text(FleetFormatter.relativeTime(fromEpoch: brief.generatedAt))
                        .font(.caption2.monospacedDigit())
                        .foregroundStyle(.secondary)
                }
            }

            VStack(spacing: 8) {
                Link(destination: brief.resolvedWazeURL) {
                    Label("Go", systemImage: "location.fill")
                        .font(.caption.weight(.semibold))
                        .frame(width: 72, height: 34)
                        .background(accentColor.opacity(scheme == .dark ? 0.22 : 0.16), in: RoundedRectangle(cornerRadius: 9, style: .continuous))
                        .foregroundStyle(accentColor)
                }
                .accessibilityLabel("Navigate to \(destLabel) in Waze")

                Button(intent: ShuffleDriveMixIntent()) {
                    Label("Mix", systemImage: "shuffle")
                        .font(.caption.weight(.semibold))
                        .frame(width: 72, height: 34)
                        .background(controlSurface, in: RoundedRectangle(cornerRadius: 9, style: .continuous))
                        .foregroundStyle(.primary)
                }
                .buttonStyle(.plain)
                .accessibilityLabel("Next music mix")
            }
        }
    }
}

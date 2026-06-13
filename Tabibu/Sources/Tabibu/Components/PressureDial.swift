// PressureDial — a compact ring gauge for memory/CPU/thermal pressure.
// Color carries meaning but the numeric label is always present (a11y +
// color-blind safety, design.md §7).

import SwiftUI

struct PressureDial: View {
    let title: String
    /// 0.0 – 1.0
    let value: Double
    let label: String
    var tint: Color = .accentColor

    var body: some View {
        VStack(spacing: Space.s) {
            ZStack {
                Circle()
                    .stroke(.quaternary, lineWidth: 8)
                Circle()
                    .trim(from: 0, to: min(max(value, 0), 1))
                    .stroke(tint, style: StrokeStyle(lineWidth: 8, lineCap: .round))
                    .rotationEffect(.degrees(-90))
                    .animation(.easeOut(duration: Dur.normal), value: value)
                Text(label)
                    .font(.system(.title3, design: .rounded).weight(.semibold))
                    .contentTransition(.numericText())
            }
            .frame(width: 84, height: 84)
            Text(title)
                .font(.caption)
                .foregroundStyle(.secondary)
        }
        .accessibilityElement(children: .ignore)
        .accessibilityLabel("\(title): \(label)")
    }
}

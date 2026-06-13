// TierBadge — design.md §5. Text + color (color is never the sole signal).

import SwiftUI

struct TierBadge: View {
    let tier: String

    var body: some View {
        Text(tier)
            .font(.caption2.weight(.semibold))
            .padding(.horizontal, Space.s)
            .padding(.vertical, 2)
            .foregroundStyle(TierStyle.color(tier))
            .background(TierStyle.color(tier).opacity(0.16), in: Capsule())
            .accessibilityLabel("\(tier) tier")
    }
}

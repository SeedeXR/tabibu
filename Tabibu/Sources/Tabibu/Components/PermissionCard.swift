// PermissionCard — shown on views that need Full Disk Access when it is not
// granted (design.md §6 partial state). Explains *why* with a concrete
// example and deep-links to System Settings; never nags, never fakes data.

import SwiftUI

struct PermissionCard: View {
    let example: String
    let onOpenSettings: () -> Void

    var body: some View {
        HStack(alignment: .top, spacing: Space.m) {
            Image(systemName: "lock.shield")
                .font(.title2)
                .foregroundStyle(.orange)
            VStack(alignment: .leading, spacing: Space.xs) {
                Text("Full Disk Access needed for complete results")
                    .font(.headline)
                Text(example)
                    .font(.callout)
                    .foregroundStyle(.secondary)
                    .fixedSize(horizontal: false, vertical: true)
                Button("Open Privacy Settings", action: onOpenSettings)
                    .padding(.top, Space.xs)
            }
            Spacer()
        }
        .padding(Space.l)
        .background(Color.orange.opacity(0.10), in: RoundedRectangle(cornerRadius: Radius.card))
        .overlay(
            RoundedRectangle(cornerRadius: Radius.card).strokeBorder(.orange.opacity(0.4))
        )
        .padding(.horizontal, Space.l)
        .padding(.top, Space.l)
    }
}

// ProgressStream — design.md §5: live progress + count + bytes + Cancel.
// Totals are unknown up front, so the bar is indeterminate but the counts
// are real and update as items stream in (no spinner-then-dump).

import SwiftUI

struct ProgressStream: View {
    let label: String
    let itemCount: Int
    let bytes: UInt64
    let onCancel: () -> Void

    var body: some View {
        HStack(spacing: Space.m) {
            ProgressView()
                .controlSize(.small)
            VStack(alignment: .leading, spacing: Space.xs) {
                Text(label)
                    .font(.headline)
                Text("\(itemCount) items · \(Fmt.bytes(bytes)) found so far")
                    .font(.system(.caption, design: .monospaced))
                    .foregroundStyle(.secondary)
                    .contentTransition(.numericText())
                    .animation(.easeOut(duration: Dur.fast), value: itemCount)
            }
            Spacer()
            Button("Cancel", action: onCancel)
                .accessibilityLabel("Cancel \(label)")
        }
        .padding(Space.l)
        .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: Radius.card))
        .accessibilityElement(children: .combine)
    }
}

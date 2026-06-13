// ReclaimBar — the action bar under a review table (design.md §5).
// Shows the live selected total and the primary action; the destination
// (Trash) is restated in the confirmation sheet before anything happens.

import SwiftUI

struct ReclaimBar: View {
    let selectedCount: Int
    let selectedBytes: UInt64
    let onSelectAllSafe: () -> Void
    let onDeselectAll: () -> Void
    let onReclaim: () -> Void

    @State private var confirming = false

    var body: some View {
        HStack(spacing: Space.m) {
            Button("Select All Safe", action: onSelectAllSafe)
            Button("Deselect All", action: onDeselectAll)
                .disabled(selectedCount == 0)
            Spacer()
            VStack(alignment: .trailing, spacing: 0) {
                Text(Fmt.bytes(selectedBytes))
                    .font(.system(.title3, design: .monospaced).weight(.semibold))
                    .contentTransition(.numericText())
                    .animation(.easeOut(duration: Dur.fast), value: selectedBytes)
                Text("\(selectedCount) selected")
                    .font(.caption)
                    .foregroundStyle(.secondary)
            }
            Button {
                confirming = true
            } label: {
                Label("Move to Trash", systemImage: "trash")
            }
            .buttonStyle(.borderedProminent)
            .controlSize(.large)
            .disabled(selectedCount == 0)
            .keyboardShortcut(.return, modifiers: .command)
        }
        .padding(Space.l)
        .background(.bar)
        .confirmationDialog(
            "Move \(selectedCount) item\(selectedCount == 1 ? "" : "s") to the Trash?",
            isPresented: $confirming, titleVisibility: .visible
        ) {
            Button("Move \(Fmt.bytes(selectedBytes)) to Trash", role: .destructive, action: onReclaim)
            Button("Cancel", role: .cancel) {}
        } message: {
            Text(
                "Items go to the Trash, so you can restore them. Tabibu also writes an undo record before anything is moved."
            )
        }
    }
}

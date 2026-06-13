// ReviewTable — the heart of the product (design.md §5/§6). Sectioned by
// category, every row shows path · size · tier · reason with a checkbox.
// Full paths are revealable/copyable; nothing is hidden from the user.

import AppKit
import SwiftUI

struct ReviewTable: View {
    @Bindable var session: ScanSession
    let home: String

    var body: some View {
        ScrollView {
            LazyVStack(alignment: .leading, spacing: Space.l, pinnedViews: [.sectionHeaders]) {
                ForEach(session.grouped, id: \.category) { group in
                    Section {
                        ForEach(group.items) { item in
                            ReviewRow(
                                item: item,
                                home: home,
                                isSelected: session.selection.contains(item.path),
                                toggle: { session.toggle(item.path) }
                            )
                            Divider().opacity(0.4)
                        }
                    } header: {
                        categoryHeader(group)
                    }
                }
            }
            .padding(.horizontal, Space.l)
            .padding(.bottom, Space.xl)
        }
    }

    private func categoryHeader(_ group: (category: String, items: [CleanupItem])) -> some View {
        let total = group.items.reduce(UInt64(0)) { $0 + $1.sizeBytes }
        return HStack {
            Text(group.category)
                .font(.headline)
            Text("\(group.items.count) · \(Fmt.bytes(total))")
                .font(.system(.subheadline, design: .monospaced))
                .foregroundStyle(.secondary)
            Spacer()
        }
        .padding(.vertical, Space.s)
        .padding(.horizontal, Space.s)
        .background(.bar)
    }
}

private struct ReviewRow: View {
    let item: CleanupItem
    let home: String
    let isSelected: Bool
    let toggle: () -> Void

    var body: some View {
        HStack(spacing: Space.m) {
            Toggle("", isOn: Binding(get: { isSelected }, set: { _ in toggle() }))
                .labelsHidden()
                .toggleStyle(.checkbox)
                .accessibilityLabel("Select \(Naming.displayPath(item.path, home: home))")

            VStack(alignment: .leading, spacing: 2) {
                Text(Naming.displayPath(item.path, home: home))
                    .font(.system(.body, design: .monospaced))
                    .lineLimit(1)
                    .truncationMode(.middle)
                    .help(item.path)
                Text(item.reason)
                    .font(.caption)
                    .foregroundStyle(.secondary)
                    .lineLimit(1)
            }
            Spacer(minLength: Space.m)

            Text(Fmt.bytes(item.sizeBytes))
                .font(.system(.body, design: .monospaced))
                .foregroundStyle(.secondary)
                .frame(width: 90, alignment: .trailing)

            TierBadge(tier: item.tier)
                .frame(width: 64, alignment: .leading)
        }
        .padding(.vertical, Space.xs)
        .contentShape(Rectangle())
        .onTapGesture(perform: toggle)
        .contextMenu {
            Button("Copy Path") {
                NSPasteboard.general.clearContents()
                NSPasteboard.general.setString(item.path, forType: .string)
            }
            Button("Reveal in Finder") {
                NSWorkspace.shared.selectFile(item.path, inFileViewerRootedAtPath: "")
            }
        }
    }
}

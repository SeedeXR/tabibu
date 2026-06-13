// ScanFlowView — the shared scan→review→reclaim→done presentation over a
// ScanSession. Smart Scan, Junk, and Large&Old all render through this, so
// the canonical flow (design.md §3) is defined exactly once.

import SwiftUI

struct ScanFlowView: View {
    let title: String
    let subtitle: String
    let icon: String
    @Bindable var session: ScanSession
    @Environment(AppModel.self) private var model

    var body: some View {
        VStack(spacing: 0) {
            switch session.phase {
            case .idle:
                EmptyState(
                    lucide: icon,
                    title: title,
                    message: subtitle,
                    actionTitle: "Scan",
                    action: { session.start() }
                )
            case .scanning:
                VStack(spacing: 0) {
                    ProgressStream(
                        label: "Scanning…",
                        itemCount: session.items.count,
                        bytes: session.foundBytes,
                        onCancel: { session.cancel() }
                    )
                    .padding(Space.l)
                    if !session.items.isEmpty {
                        ReviewTable(session: session, home: model.home)
                    } else {
                        Spacer()
                    }
                }
            case .review:
                reviewLayer
            case .reclaiming:
                VStack(spacing: Space.l) {
                    ProgressView()
                    Text("Moving items to the Trash…").foregroundStyle(.secondary)
                }
                .frame(maxWidth: .infinity, maxHeight: .infinity)
            case .done(let report):
                ResultView(report: report, onDone: { session.reset() })
            case .error(let message):
                ErrorState(message: message, retry: { session.reset() })
            }
        }
        .navigationTitle(title)
        .toolbar {
            if case .review = session.phase {
                ToolbarItem(placement: .primaryAction) {
                    Button("Rescan") { session.reset(); session.start() }
                }
            }
        }
    }

    private var summaryFooter: some View {
        Group {
            if let summary = session.summary {
                let blocked = summary.scanners.reduce(0) { $0 + Int($1.guardRejected) }
                let errored = summary.scanners.filter { $0.error != nil }
                if blocked > 0 || !errored.isEmpty {
                    HStack(spacing: Space.m) {
                        if blocked > 0 {
                            Label("\(blocked) blocked by safety guard", systemImage: "shield")
                                .foregroundStyle(.secondary)
                        }
                        ForEach(errored, id: \.id) { o in
                            Label("\(Naming.scanner(o.id)): \(o.error ?? "")", systemImage: "exclamationmark.triangle")
                                .foregroundStyle(.secondary)
                        }
                        Spacer()
                    }
                    .font(.caption)
                    .padding(.horizontal, Space.l)
                    .padding(.vertical, Space.s)
                }
            }
        }
    }

    @ViewBuilder private var reviewLayer: some View {
        VStack(spacing: 0) {
            if !model.fullDiskAccess {
                PermissionCard(
                    example:
                        "Without it, caches inside other apps' containers and Safari/Mail data won't be counted — results are partial but honest.",
                    onOpenSettings: { AppModel.openFullDiskAccessSettings() }
                )
            }
            summaryFooter
            ReviewTable(session: session, home: model.home)
            ReclaimBar(
                selectedCount: session.selection.count,
                selectedBytes: session.selectedBytes,
                onSelectAllSafe: { session.selectAllSafe() },
                onDeselectAll: { session.deselectAll() },
                onReclaim: { session.reclaim() }
            )
        }
    }
}

// DuplicatesView — pick a folder, find true duplicates (3-stage blake3
// funnel in Rust), keep the newest copy by default. Duplicates are Review
// tier: never auto-selected, never auto-deleted.

import AppKit
import SwiftUI

@Observable
@MainActor
private final class DuplicatesModel {
    enum Phase: Equatable { case idle, scanning, review, reclaiming, done(ReclaimReport), error(String) }
    var phase: Phase = .idle
    var groups: [DuplicateGroup] = []
    /// Paths the user marked for trashing (keepers excluded by default).
    var selection: Set<String> = []
    var folder: URL?
    private var op: CoreBridge.Operation?

    var totalDuplicateBytes: UInt64 {
        groups.reduce(0) { acc, g in acc + g.sizeBytes * UInt64(max(g.paths.count - 1, 0)) }
    }
    func bytes(forSelected: Bool) -> UInt64 {
        var total: UInt64 = 0
        for g in groups {
            for p in g.paths where selection.contains(p) { total += g.sizeBytes }
        }
        return total
    }

    func start(model: AppModel) {
        guard let folder else { return }
        phase = .scanning
        groups = []
        selection = []
        let op = CoreBridge.Operation()
        self.op = op
        let path = folder.path
        Task {
            do {
                let result = try await Task.detached {
                    try CoreBridge.findDuplicates(roots: [path], minSize: 4096, op: op)
                }.value
                groups = result
                phase = result.isEmpty ? .idle : .review
            } catch {
                phase = .error(error.localizedDescription)
            }
        }
    }

    func cancel() { op?.cancel(); phase = groups.isEmpty ? .idle : .review }

    func reclaim(model: AppModel) {
        guard let folder, !selection.isEmpty else { return }
        phase = .reclaiming
        var items: [CleanupItem] = []
        for g in groups {
            for p in g.paths where selection.contains(p) {
                items.append(
                    CleanupItem(
                        path: p, category: "Duplicate", sizeBytes: g.sizeBytes, tier: "Review",
                        reason: "Duplicate of \(g.paths.first.map { URL(fileURLWithPath: $0).lastPathComponent } ?? "kept copy")",
                        selected: true, action: "Trash"))
            }
        }
        let ctx = model.scanContext(extraRoots: [folder.path])
        let undo = model.undoDirectory
        Task {
            do {
                let report = try await Task.detached {
                    try CoreBridge.reclaim(items: items, ctx: ctx, undoDir: undo)
                }.value
                phase = .done(report)
            } catch { phase = .error(error.localizedDescription) }
        }
    }
}

struct DuplicatesView: View {
    @Environment(AppModel.self) private var model
    @State private var dm = DuplicatesModel()

    var body: some View {
        Group {
            switch dm.phase {
            case .idle:
                EmptyState(
                    lucide: "copy",
                    title: "Find Duplicate Files",
                    message: "Choose a folder. Tabibu compares files by content (not just name) and keeps the newest copy by default.",
                    actionTitle: "Choose Folder…",
                    action: pickFolder)
            case .scanning:
                VStack(spacing: Space.l) {
                    ProgressView()
                    Text("Comparing files by content…").foregroundStyle(.secondary)
                    Button("Cancel") { dm.cancel() }
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            case .review:
                reviewLayer
            case .reclaiming:
                ProgressView("Moving duplicates to the Trash…")
                    .frame(maxWidth: .infinity, maxHeight: .infinity)
            case .done(let report):
                ResultView(report: report, onDone: { dm = DuplicatesModel() })
            case .error(let msg):
                ErrorState(message: msg, retry: { dm = DuplicatesModel() })
            }
        }
        .navigationTitle("Duplicates")
    }

    private var reviewLayer: some View {
        VStack(spacing: 0) {
            HStack {
                Text("\(dm.groups.count) duplicate sets · \(Fmt.bytes(dm.totalDuplicateBytes)) reclaimable")
                    .font(.headline)
                Spacer()
                Button("Change Folder…", action: pickFolder)
            }
            .padding(Space.l)

            ScrollView {
                LazyVStack(alignment: .leading, spacing: Space.l) {
                    ForEach(dm.groups) { group in
                        DuplicateGroupView(group: group, home: model.home, selection: $dm.selection)
                    }
                }
                .padding(.horizontal, Space.l)
                .padding(.bottom, Space.xl)
            }

            ReclaimBar(
                selectedCount: dm.selection.count,
                selectedBytes: dm.bytes(forSelected: true),
                onSelectAllSafe: selectAllNonKeepers,
                onDeselectAll: { dm.selection.removeAll() },
                onReclaim: { dm.reclaim(model: model) })
        }
    }

    private func selectAllNonKeepers() {
        for g in dm.groups { for p in g.paths.dropFirst() { dm.selection.insert(p) } }
    }

    private func pickFolder() {
        let panel = NSOpenPanel()
        panel.canChooseDirectories = true
        panel.canChooseFiles = false
        panel.allowsMultipleSelection = false
        if panel.runModal() == .OK, let url = panel.url {
            dm.folder = url
            dm.start(model: model)
        }
    }
}

private struct DuplicateGroupView: View {
    let group: DuplicateGroup
    let home: String
    @Binding var selection: Set<String>

    var body: some View {
        VStack(alignment: .leading, spacing: Space.xs) {
            Text("\(Fmt.bytes(group.sizeBytes)) each · \(group.paths.count) copies")
                .font(.subheadline.weight(.semibold))
            ForEach(Array(group.paths.enumerated()), id: \.element) { idx, path in
                HStack(spacing: Space.m) {
                    if idx == 0 {
                        Image(systemName: "star.fill").foregroundStyle(.yellow)
                            .help("Keeping the newest copy")
                    } else {
                        Toggle("", isOn: Binding(
                            get: { selection.contains(path) },
                            set: { on in if on { selection.insert(path) } else { selection.remove(path) } }))
                            .labelsHidden().toggleStyle(.checkbox)
                    }
                    Text(Naming.displayPath(path, home: home))
                        .font(.system(.callout, design: .monospaced))
                        .lineLimit(1).truncationMode(.middle).help(path)
                    if idx == 0 {
                        Text("keeping newest").font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                }
            }
        }
        .padding(Space.m)
        .background(.quaternary.opacity(0.3), in: RoundedRectangle(cornerRadius: Radius.card))
    }
}

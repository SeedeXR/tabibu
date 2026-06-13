// UninstallerView — pick an installed app, hunt its remnants (Rust), review,
// then trash. The .app itself is never auto-added; the user opts in. Remnant
// tiers come from the engine (exact bundle-id match = Review, fuzzy = Risky).

import AppKit
import SwiftUI

private struct InstalledApp: Identifiable {
    let url: URL
    let name: String
    let bundleID: String
    var id: String { url.path }
    var icon: NSImage { NSWorkspace.shared.icon(forFile: url.path) }
}

@Observable
@MainActor
private final class UninstallModel {
    enum Phase: Equatable { case browsing, hunting, review, reclaiming, done(ReclaimReport), error(String) }
    var phase: Phase = .browsing
    var apps: [InstalledApp] = []
    var query = ""
    var selected: InstalledApp?
    var remnants: [CleanupItem] = []
    var chosen: Set<String> = []
    var alsoTrashApp = true

    var filtered: [InstalledApp] {
        query.isEmpty ? apps : apps.filter { $0.name.localizedCaseInsensitiveContains(query) }
    }

    func loadApps() {
        var found: [InstalledApp] = []
        for dir in ["/Applications", NSHomeDirectory() + "/Applications"] {
            let urls = (try? FileManager.default.contentsOfDirectory(
                at: URL(fileURLWithPath: dir), includingPropertiesForKeys: nil)) ?? []
            for url in urls where url.pathExtension == "app" {
                guard let bundle = Bundle(url: url), let bid = bundle.bundleIdentifier else { continue }
                let name = (bundle.infoDictionary?["CFBundleName"] as? String)
                    ?? url.deletingPathExtension().lastPathComponent
                found.append(InstalledApp(url: url, name: name, bundleID: bid))
            }
        }
        apps = found.sorted { $0.name.localizedCaseInsensitiveCompare($1.name) == .orderedAscending }
    }

    func hunt(_ app: InstalledApp, model: AppModel) {
        selected = app
        phase = .hunting
        let ctx = model.scanContext(extraRoots: [model.home + "/Library"])
        Task {
            do {
                let items = try await Task.detached {
                    try CoreBridge.findRemnants(bundleId: app.bundleID, appName: app.name, ctx: ctx)
                }.value
                remnants = items
                chosen = Set(items.filter { $0.tier == "Review" }.map(\.path)) // exact matches preselected
                phase = .review
            } catch { phase = .error(error.localizedDescription) }
        }
    }

    func uninstall(model: AppModel) {
        guard let app = selected else { return }
        phase = .reclaiming
        var items = remnants.filter { chosen.contains($0.path) }
        for i in items.indices { items[i].selected = true }
        let ctx = model.scanContext(extraRoots: [model.home + "/Library"])
        let undo = model.undoDirectory
        let trashApp = alsoTrashApp
        let appURL = app.url
        Task {
            do {
                let report = try await Task.detached {
                    try CoreBridge.reclaim(items: items, ctx: ctx, undoDir: undo)
                }.value
                if trashApp {
                    // The .app lives in /Applications (outside the engine's user
                    // roots), so we trash it via Finder, not the core reclaimer.
                    try? FileManager.default.trashItem(at: appURL, resultingItemURL: nil)
                }
                phase = .done(report)
            } catch { phase = .error(error.localizedDescription) }
        }
    }
}

struct UninstallerView: View {
    @Environment(AppModel.self) private var model
    @State private var um = UninstallModel()

    var body: some View {
        Group {
            switch um.phase {
            case .browsing: browser
            case .hunting: ProgressView("Finding leftover files…").frame(maxWidth: .infinity, maxHeight: .infinity)
            case .review: review
            case .reclaiming: ProgressView("Moving to the Trash…").frame(maxWidth: .infinity, maxHeight: .infinity)
            case .done(let report): ResultView(report: report, onDone: { um = UninstallModel(); um.loadApps() })
            case .error(let msg): ErrorState(message: msg, retry: { um.phase = .browsing })
            }
        }
        .navigationTitle("Uninstaller")
        .task { if um.apps.isEmpty { um.loadApps() } }
    }

    private var browser: some View {
        VStack(spacing: 0) {
            if !model.fullDiskAccess {
                PermissionCard(
                    example: "Some apps store data inside protected containers. Without Full Disk Access, a few leftovers may not be found.",
                    onOpenSettings: { AppModel.openFullDiskAccessSettings() })
            }
            List(um.filtered) { app in
                HStack(spacing: Space.m) {
                    Image(nsImage: app.icon).resizable().frame(width: 28, height: 28)
                    VStack(alignment: .leading, spacing: 0) {
                        Text(app.name)
                        Text(app.bundleID).font(.caption).foregroundStyle(.secondary)
                    }
                    Spacer()
                    Button("Uninstall…") { um.hunt(app, model: model) }
                }
                .padding(.vertical, 2)
            }
            .searchable(text: $um.query, placement: .toolbar, prompt: "Search apps")
        }
    }

    private var review: some View {
        VStack(spacing: 0) {
            HStack(spacing: Space.m) {
                if let app = um.selected {
                    Image(nsImage: app.icon).resizable().frame(width: 32, height: 32)
                    VStack(alignment: .leading) {
                        Text("Uninstall \(app.name)").font(.headline)
                        Text("\(um.remnants.count) related item\(um.remnants.count == 1 ? "" : "s") found")
                            .font(.caption).foregroundStyle(.secondary)
                    }
                }
                Spacer()
                Button("Back") { um.phase = .browsing }
            }
            .padding(Space.l)

            Toggle("Also move the app to the Trash", isOn: $um.alsoTrashApp)
                .padding(.horizontal, Space.l)

            if um.remnants.isEmpty {
                EmptyState(systemImage: "checkmark.circle", title: "No leftovers found",
                    message: "This app didn't leave behind support files we could detect.")
            } else {
                ScrollView {
                    LazyVStack(spacing: 0) {
                        ForEach(um.remnants) { item in
                            HStack(spacing: Space.m) {
                                Toggle("", isOn: Binding(
                                    get: { um.chosen.contains(item.path) },
                                    set: { on in if on { um.chosen.insert(item.path) } else { um.chosen.remove(item.path) } }))
                                    .labelsHidden().toggleStyle(.checkbox)
                                VStack(alignment: .leading, spacing: 0) {
                                    Text(Naming.displayPath(item.path, home: model.home))
                                        .font(.system(.callout, design: .monospaced)).lineLimit(1).truncationMode(.middle).help(item.path)
                                    Text(item.reason).font(.caption).foregroundStyle(.secondary)
                                }
                                Spacer()
                                Text(Fmt.bytes(item.sizeBytes)).font(.system(.callout, design: .monospaced)).foregroundStyle(.secondary)
                                TierBadge(tier: item.tier)
                            }
                            .padding(.vertical, Space.xs)
                            Divider().opacity(0.3)
                        }
                    }.padding(.horizontal, Space.l)
                }
            }

            HStack {
                Spacer()
                Button {
                    um.uninstall(model: model)
                } label: {
                    Label("Uninstall", systemImage: "trash")
                }
                .buttonStyle(.borderedProminent).controlSize(.large)
                .disabled(um.chosen.isEmpty && !um.alsoTrashApp)
            }
            .padding(Space.l).background(.bar)
        }
    }
}

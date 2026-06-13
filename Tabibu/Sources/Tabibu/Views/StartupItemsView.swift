// StartupItemsView — surface launch agents/daemons that run at login. We do
// NOT fake a disable toggle (that needs launchctl/SMAppService from a signed
// helper); instead we show each item honestly with Reveal in Finder and a
// deep link to Login Items settings, where the user is in control.

import AppKit
import SwiftUI

private struct StartupItem: Identifiable {
    let path: String
    let label: String
    let program: String
    let scope: String        // "User", "System (LaunchAgents)", "System (LaunchDaemons)"
    var id: String { path }
}

@Observable
@MainActor
private final class StartupModel {
    var items: [StartupItem] = []
    var partial = false      // true if a system dir was unreadable (needs FDA)

    func load(home: String) {
        var found: [StartupItem] = []
        partial = false
        let dirs: [(String, String)] = [
            (home + "/Library/LaunchAgents", "User"),
            ("/Library/LaunchAgents", "System (LaunchAgents)"),
            ("/Library/LaunchDaemons", "System (LaunchDaemons)"),
        ]
        for (dir, scope) in dirs {
            guard let entries = try? FileManager.default.contentsOfDirectory(atPath: dir) else {
                if scope != "User" { partial = true }
                continue
            }
            for name in entries where name.hasSuffix(".plist") {
                let path = dir + "/" + name
                let (label, program) = Self.parse(path) ?? (name, "—")
                found.append(StartupItem(path: path, label: label, program: program, scope: scope))
            }
        }
        items = found.sorted { $0.label.localizedCaseInsensitiveCompare($1.label) == .orderedAscending }
    }

    private static func parse(_ path: String) -> (String, String)? {
        guard let data = FileManager.default.contents(atPath: path),
            let plist = try? PropertyListSerialization.propertyList(from: data, format: nil),
            let dict = plist as? [String: Any]
        else { return nil }
        let label = (dict["Label"] as? String) ?? URL(fileURLWithPath: path).lastPathComponent
        let program: String
        if let p = dict["Program"] as? String {
            program = p
        } else if let args = dict["ProgramArguments"] as? [String], let first = args.first {
            program = first
        } else {
            program = "—"
        }
        return (label, program)
    }
}

struct StartupItemsView: View {
    @Environment(AppModel.self) private var model
    @State private var sm = StartupModel()

    var body: some View {
        VStack(spacing: 0) {
            HStack {
                Text("\(sm.items.count) startup item\(sm.items.count == 1 ? "" : "s")").font(.headline)
                Spacer()
                Button("Open Login Items Settings") {
                    if let url = URL(string: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension") {
                        NSWorkspace.shared.open(url)
                    }
                }
            }
            .padding(Space.l)

            if sm.partial {
                Text("Some system startup folders need Full Disk Access to read. The list below may be incomplete.")
                    .font(.callout).foregroundStyle(.secondary)
                    .frame(maxWidth: .infinity, alignment: .leading)
                    .padding(.horizontal, Space.l).padding(.bottom, Space.s)
            }

            if sm.items.isEmpty {
                EmptyState(lucide: "activity", title: "No startup items found",
                    message: "Nothing is configured to launch at login in the folders Tabibu can read.")
            } else {
                List(sm.items) { item in
                    HStack(spacing: Space.m) {
                        VStack(alignment: .leading, spacing: 0) {
                            Text(item.label)
                            Text(item.program).font(.caption).foregroundStyle(.secondary)
                                .lineLimit(1).truncationMode(.middle)
                        }
                        Spacer()
                        Text(item.scope).font(.caption).foregroundStyle(.secondary)
                        Button("Reveal") {
                            NSWorkspace.shared.selectFile(item.path, inFileViewerRootedAtPath: "")
                        }
                    }
                    .padding(.vertical, 2)
                }
            }
        }
        .navigationTitle("Startup Items")
        .task { sm.load(home: model.home) }
    }
}

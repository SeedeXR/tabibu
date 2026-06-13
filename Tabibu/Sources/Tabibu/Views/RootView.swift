// RootView — sidebar navigation shell (design.md §2). Premium feel: Lucide
// glyphs, accent-tinted selection, generous spacing. Each destination is a
// focused feature view; the heavy lifting lives in the Rust core.

import SwiftUI

enum Destination: String, CaseIterable, Identifiable {
    case smartScan, junk, duplicates, largeOld
    case uninstaller, startup
    case disk, memory, battery
    case security

    var id: String { rawValue }

    var title: String {
        switch self {
        case .smartScan: "Smart Scan"
        case .junk: "Junk"
        case .duplicates: "Duplicates"
        case .largeOld: "Large & Old Files"
        case .uninstaller: "Uninstaller"
        case .startup: "Startup Items"
        case .disk: "Disk"
        case .memory: "Memory & CPU"
        case .battery: "Battery"
        case .security: "Security"
        }
    }

    var icon: String {
        switch self {
        case .smartScan: "sparkles"
        case .junk: "trash-2"
        case .duplicates: "copy"
        case .largeOld: "files"
        case .uninstaller: "rocket"
        case .startup: "activity"
        case .disk: "hard-drive"
        case .memory: "cpu"
        case .battery: "battery"
        case .security: "shield"
        }
    }

    var section: String {
        switch self {
        case .smartScan: "Overview"
        case .junk, .duplicates, .largeOld: "Cleanup"
        case .uninstaller, .startup: "Applications"
        case .disk, .memory, .battery: "Health"
        case .security: "Security"
        }
    }
}

struct RootView: View {
    @Environment(AppModel.self) private var model
    @State private var selection: Destination = .smartScan

    private var sections: [(String, [Destination])] {
        let order = ["Overview", "Cleanup", "Applications", "Health", "Security"]
        let grouped = Dictionary(grouping: Destination.allCases, by: \.section)
        return order.compactMap { key in grouped[key].map { (key, $0) } }
    }

    var body: some View {
        NavigationSplitView {
            List(selection: $selection) {
                ForEach(sections, id: \.0) { section, items in
                    Section(section) {
                        ForEach(items) { dest in
                            Label {
                                Text(dest.title)
                            } icon: {
                                LucideIcon(name: dest.icon, size: 16)
                            }
                            .tag(dest)
                        }
                    }
                }
            }
            .navigationSplitViewColumnWidth(min: 200, ideal: 220)
            .safeAreaInset(edge: .bottom) {
                ThermalFooter(state: model.thermalState)
            }
        } detail: {
            detail(for: selection)
        }
        .onReceive(
            NotificationCenter.default.publisher(for: NSApplication.didBecomeActiveNotification)
        ) { _ in
            model.refreshPermissions()
        }
    }

    @ViewBuilder private func detail(for dest: Destination) -> some View {
        switch dest {
        case .smartScan:
            ScanFlowView(
                title: "Smart Scan",
                subtitle:
                    "One scan across caches, temporary files, logs, and large old files. Nothing is removed until you review and choose.",
                icon: "sparkles",
                session: model.smartScan)
        case .junk:
            ScanFlowView(
                title: "Junk",
                subtitle:
                    "Caches, temporary files, and logs that are safe to clear. Caches of running apps are skipped automatically.",
                icon: "trash-2",
                session: model.junkScan)
        case .largeOld:
            ScanFlowView(
                title: "Large & Old Files",
                subtitle:
                    "Suggestions only — big files in Downloads you may have forgotten. Nothing is selected for you.",
                icon: "files",
                session: model.largeOldScan)
        case .duplicates:
            DuplicatesView()
        case .disk:
            SpaceMapView()
        case .memory:
            MemoryCPUView()
        case .battery:
            BatteryView()
        case .uninstaller:
            UninstallerView()
        case .startup:
            StartupItemsView()
        case .security:
            EmptyState(
                lucide: "shield",
                title: "Malware scan arrives in M7",
                message:
                    "On-demand adware heuristics and a quarantine vault are built in the core; the scan UI lands next. Tabibu never runs a resident background scanner.")
        }
    }
}

private struct ThermalFooter: View {
    let state: ProcessInfo.ThermalState

    private var label: String {
        switch state {
        case .nominal: "Thermals: Normal"
        case .fair: "Thermals: Fair"
        case .serious: "Thermals: Serious"
        case .critical: "Thermals: Critical"
        @unknown default: "Thermals: Unknown"
        }
    }

    private var color: Color {
        switch state {
        case .nominal: .green
        case .fair: .yellow
        case .serious, .critical: .red
        @unknown default: .gray
        }
    }

    var body: some View {
        HStack(spacing: Space.s) {
            Circle().fill(color).frame(width: 8, height: 8)
            Text(label).font(.caption).foregroundStyle(.secondary)
            Spacer()
        }
        .padding(.horizontal, Space.m)
        .padding(.vertical, Space.s)
        .accessibilityElement(children: .combine)
    }
}

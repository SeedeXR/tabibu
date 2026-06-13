// TabibuMonitor — menu-bar agent, pure AppKit (no SwiftUI) to stay inside the
// resource budget (memory/test.md §5). A performance tool that is itself a hog
// is dead on arrival, so this deliberately avoids the SwiftUI runtime: an
// NSStatusItem with a hand-drawn NSPopover view.
//
// Cadence (budget discipline):
//   • menu-bar label refreshes every 5s,
//   • full sampling (top processes) runs every 2s ONLY while the popover is
//     open, and stops the moment it closes.

import AppKit

final class AppDelegate: NSObject, NSApplicationDelegate, NSPopoverDelegate {
    private var statusItem: NSStatusItem!
    private let popover = NSPopover()
    private let content = MonitorContentView()

    private var labelTimer: Timer?
    private var popoverTimer: Timer?

    func applicationDidFinishLaunching(_ note: Notification) {
        CoreBridge.assertVersion()

        statusItem = NSStatusBar.system.statusItem(withLength: NSStatusItem.variableLength)
        if let button = statusItem.button {
            button.image = NSImage(
                systemSymbolName: "gauge.with.dots.needle.50percent",
                accessibilityDescription: "Tabibu Monitor")
            button.imagePosition = .imageLeading
            button.target = self
            button.action = #selector(togglePopover)
        }

        popover.behavior = .transient
        popover.delegate = self
        popover.contentViewController = NSViewController()
        popover.contentViewController?.view = content
        content.onOpenTabibu = { Self.openMainApp() }

        refreshLabel()
        labelTimer = Timer.scheduledTimer(withTimeInterval: 5, repeats: true) { [weak self] _ in
            self?.refreshLabel()
        }
    }

    @objc private func togglePopover() {
        if popover.isShown {
            popover.performClose(nil)
        } else if let button = statusItem.button {
            sampleIntoPopover()
            popover.show(relativeTo: button.bounds, of: button, preferredEdge: .minY)
            popoverTimer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) {
                [weak self] _ in self?.sampleIntoPopover()
            }
        }
    }

    func popoverDidClose(_ notification: Notification) {
        popoverTimer?.invalidate()
        popoverTimer = nil
    }

    // MARK: Sampling

    private func refreshLabel() {
        // Cheap: top-1 by CPU, just to render a compact percentage.
        DispatchQueue.global(qos: .utility).async {
            let sample = try? CoreBridge.monitorSample(topN: 1, byCPU: true)
            DispatchQueue.main.async {
                guard let s = sample, let button = self.statusItem.button else { return }
                button.title = "  \(Int(s.cpuPercent))%"
            }
        }
    }

    private func sampleIntoPopover() {
        DispatchQueue.global(qos: .userInitiated).async {
            let sample = try? CoreBridge.monitorSample(topN: 6, byCPU: true)
            DispatchQueue.main.async {
                if let s = sample { self.content.update(with: s) }
            }
        }
    }

    static func openMainApp() {
        let ws = NSWorkspace.shared
        if let url = ws.urlForApplication(withBundleIdentifier: "xr.seede.tabibu") {
            ws.openApplication(at: url, configuration: NSWorkspace.OpenConfiguration())
        } else {
            let fallback = URL(fileURLWithPath: "/Applications/Tabibu.app")
            ws.openApplication(at: fallback, configuration: NSWorkspace.OpenConfiguration())
        }
    }
}

// Accessory app: no Dock icon, no menu bar of its own.
let app = NSApplication.shared
app.setActivationPolicy(.accessory)
let delegate = AppDelegate()
app.delegate = delegate
app.run()

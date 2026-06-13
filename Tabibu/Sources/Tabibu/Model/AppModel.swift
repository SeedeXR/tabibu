// App-wide state. Owns the scan context (home, FDA status, running apps)
// and the per-feature view models. UI never touches CoreBridge directly.

import AppKit
import Observation
import SwiftUI

@Observable
@MainActor
final class AppModel {
    // MARK: Environment facts

    let home = FileManager.default.homeDirectoryForCurrentUser.path
    private(set) var fullDiskAccess = false
    private(set) var thermalState = ProcessInfo.processInfo.thermalState

    init() {
        refreshPermissions()
        NotificationCenter.default.addObserver(
            forName: ProcessInfo.thermalStateDidChangeNotification, object: nil, queue: .main
        ) { [weak self] _ in
            Task { @MainActor in
                self?.thermalState = ProcessInfo.processInfo.thermalState
            }
        }
    }

    /// FDA probe: TCC gives no query API; the honest check is attempting to
    /// read a TCC-protected path we never otherwise touch. Readable ⇒ FDA.
    func refreshPermissions() {
        let probe = home + "/Library/Safari"
        fullDiskAccess =
            (try? FileManager.default.contentsOfDirectory(atPath: probe)) != nil
    }

    static func openFullDiskAccessSettings() {
        let url = URL(
            string:
                "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles"
        )!
        NSWorkspace.shared.open(url)
    }

    /// Bundle IDs of running apps — the running-process guard input.
    var runningBundleIds: [String] {
        NSWorkspace.shared.runningApplications.compactMap(\.bundleIdentifier)
    }

    /// Roots the junk scanners may report from (engine re-verifies).
    var scanContext: ScanContext {
        ScanContext(
            home: home,
            allowedRoots: [
                home + "/.Trash",
                home + "/Library/Caches",
                home + "/Library/Logs",
                home + "/Library/Developer/Xcode/DerivedData",
                home + "/Library/Developer/CoreSimulator/Caches",
                home + "/.npm",
                home + "/.cargo/registry/cache",
                NSTemporaryDirectory(),
            ],
            runningBundleIds: runningBundleIds,
            fullDiskAccess: fullDiskAccess
        )
    }

    /// Context for reclaiming items outside the standard junk roots (e.g.
    /// duplicates / remnants in a user-chosen folder). The engine still
    /// re-verifies every path against the hard denylist.
    func scanContext(extraRoots: [String]) -> ScanContext {
        var ctx = scanContext
        ctx.allowedRoots += extraRoots
        return ctx
    }

    var undoDirectory: String {
        home + "/Library/Application Support/Tabibu/undo"
    }

    // MARK: Feature sessions (created once; each is independently observable)

    @ObservationIgnored lazy var smartScan = ScanSession(
        model: self,
        scanners: ["trash", "user_cache", "dev_cache", "temp", "log", "large_old"]
    )
    @ObservationIgnored lazy var junkScan = ScanSession(
        model: self, scanners: ["trash", "user_cache", "dev_cache", "temp", "log"]
    )
    @ObservationIgnored lazy var largeOldScan = ScanSession(
        model: self, scanners: ["large_old"]
    )
}

// ScanSession — the reusable engine behind Smart Scan, Junk, and Large&Old.
// Owns the scan → review → reclaim → done state machine (design.md §3) so
// each feature view is just a thin presentation over it. UI-facing; @MainActor.

import Observation
import SwiftUI

@Observable
@MainActor
final class ScanSession {
    enum Phase: Equatable {
        case idle
        case scanning
        case review
        case reclaiming
        case done(ReclaimReport)
        case error(String)
    }

    private(set) var phase: Phase = .idle
    private(set) var items: [CleanupItem] = []
    private(set) var summary: ScanSummary?
    /// Selection is tracked by path so streaming new rows never resets it.
    var selection: Set<String> = []

    private var op: CoreBridge.Operation?
    private let model: AppModel
    private let scannerIDs: [String]

    init(model: AppModel, scanners: [String]) {
        self.model = model
        self.scannerIDs = scanners
    }

    // MARK: Derived

    var selectedItems: [CleanupItem] {
        items.filter { selection.contains($0.path) }
    }

    var selectedBytes: UInt64 {
        selectedItems.reduce(0) { $0 + $1.sizeBytes }
    }

    var foundBytes: UInt64 {
        items.reduce(0) { $0 + $1.sizeBytes }
    }

    /// Items grouped by category for the sectioned review table.
    var grouped: [(category: String, items: [CleanupItem])] {
        Dictionary(grouping: items, by: \.category)
            .map { (Naming.category($0.key), $0.value) }
            .sorted { $0.items.reduce(0) { $0 + $1.sizeBytes } > $1.items.reduce(0) { $0 + $1.sizeBytes } }
    }

    // MARK: Lifecycle

    func start() {
        guard phase == .idle || isFinished else { return }
        items = []
        selection = []
        summary = nil
        phase = .scanning
        let op = CoreBridge.Operation()
        self.op = op

        Task {
            for await event in CoreBridge.scan(
                ctx: model.scanContext, scanners: scannerIDs, op: op)
            {
                switch event {
                case .item(let item):
                    items.append(item)
                    if item.tier == "Safe" { selection.insert(item.path) }
                case .done(let s):
                    summary = s
                    phase = items.isEmpty ? .idle : .review
                }
            }
        }
    }

    func cancel() {
        op?.cancel()
        phase = items.isEmpty ? .idle : .review
    }

    func selectAllSafe() {
        for item in items where item.tier == "Safe" { selection.insert(item.path) }
    }

    func deselectAll() { selection.removeAll() }

    func toggle(_ path: String) {
        if selection.contains(path) { selection.remove(path) } else { selection.insert(path) }
    }

    func reclaim() {
        guard !selectedItems.isEmpty else { return }
        phase = .reclaiming
        // Carry the user's selection into the items handed to the engine.
        var batch = selectedItems
        for i in batch.indices { batch[i].selected = true }
        let ctx = model.scanContext
        let undo = model.undoDirectory

        Task {
            do {
                let report = try await Task.detached {
                    try CoreBridge.reclaim(items: batch, ctx: ctx, undoDir: undo)
                }.value
                phase = .done(report)
            } catch {
                phase = .error(error.localizedDescription)
            }
        }
    }

    func reset() {
        phase = .idle
        items = []
        selection = []
        summary = nil
    }

    var isFinished: Bool {
        if case .done = phase { return true }
        if case .error = phase { return true }
        return false
    }
}

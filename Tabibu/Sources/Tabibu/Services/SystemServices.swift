// MemoryPressureService + SnapshotService — the honest system signals.
// Memory pressure is the metric that actually predicts slowdowns (the
// product refuses the "free RAM" placebo). APFS snapshots are surfaced
// read-only; we never delete them silently.

import Foundation

@MainActor
final class MemoryPressureService {
    enum Level: String { case normal = "Normal", warning = "Warning", critical = "Critical" }

    private var source: DispatchSourceMemoryPressure?
    private let onChange: (Level) -> Void

    init(onChange: @escaping (Level) -> Void) {
        self.onChange = onChange
        let src = DispatchSource.makeMemoryPressureSource(
            eventMask: [.normal, .warning, .critical], queue: .main)
        src.setEventHandler { [weak self] in
            guard let self, let src = self.source else { return }
            let level: Level
            switch src.data {
            case .critical: level = .critical
            case .warning: level = .warning
            default: level = .normal
            }
            self.onChange(level)
        }
        src.resume()
        self.source = src
    }

    deinit { source?.cancel() }
}

enum SnapshotService {
    /// Local Time Machine snapshots on "/". Read-only: we report the count
    /// and explain purgeable space; we never offer a silent delete.
    static func localSnapshots() -> [String] {
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/bin/tmutil")
        task.arguments = ["listlocalsnapshots", "/"]
        let pipe = Pipe()
        task.standardOutput = pipe
        task.standardError = Pipe()
        do {
            try task.run()
            task.waitUntilExit()
        } catch {
            return []
        }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        guard let out = String(data: data, encoding: .utf8) else { return [] }
        return out.split(separator: "\n")
            .map { $0.trimmingCharacters(in: .whitespaces) }
            .filter { $0.contains("com.apple.TimeMachine") }
    }

    /// Free / total bytes on the boot volume.
    static func volumeSpace() -> (free: Int64, total: Int64)? {
        let url = URL(fileURLWithPath: "/")
        guard
            let values = try? url.resourceValues(forKeys: [
                .volumeAvailableCapacityForImportantUsageKey,
                .volumeTotalCapacityKey,
            ])
        else { return nil }
        let free = values.volumeAvailableCapacityForImportantUsage ?? 0
        let total = Int64(values.volumeTotalCapacity ?? 0)
        return (Int64(free), total)
    }
}

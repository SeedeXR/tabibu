// CoreBridge: the only Swift file that touches the C ABI. Everything above
// this speaks typed Swift. Ownership rules (ADR-0001): strings returned by
// Rust are copied immediately and released via tabibu_string_free.

import CTabibuCore
import Foundation

// MARK: - Wire types (mirror serde's JSON for the Rust structs)

struct CleanupItem: Codable, Identifiable, Hashable {
    var path: String
    var category: String
    var sizeBytes: UInt64
    var tier: String
    var reason: String
    var selected: Bool
    var action: String

    var id: String { path }

    enum CodingKeys: String, CodingKey {
        case path, category, tier, reason, selected, action
        case sizeBytes = "size_bytes"
    }
}

struct ScannerOutcome: Codable, Hashable {
    var id: String
    var items: UInt64
    var guardRejected: UInt64
    var error: String?
    enum CodingKeys: String, CodingKey {
        case id, items, error
        case guardRejected = "guard_rejected"
    }
}

struct ScanSummary: Codable {
    var cancelled: Bool
    var scanners: [ScannerOutcome]
}

struct ReclaimOutcome: Codable, Hashable, Equatable {
    var path: String
    var reclaimedBytes: UInt64
    var error: String?
    enum CodingKeys: String, CodingKey {
        case path, error
        case reclaimedBytes = "reclaimed_bytes"
    }
}

struct ReclaimReport: Codable, Equatable {
    var reclaimedBytes: UInt64
    var succeeded: Int
    var failed: Int
    var manifestPath: String?
    var outcomes: [ReclaimOutcome]
    enum CodingKeys: String, CodingKey {
        case succeeded, failed, outcomes
        case reclaimedBytes = "reclaimed_bytes"
        case manifestPath = "manifest_path"
    }
}

struct DirNode: Codable, Identifiable {
    var path: String
    var sizeBytes: UInt64
    var isDir: Bool
    var children: [DirNode]
    var id: String { path }
    enum CodingKeys: String, CodingKey {
        case path, children
        case sizeBytes = "size_bytes"
        case isDir = "is_dir"
    }
}

struct DuplicateGroup: Codable, Identifiable, Hashable {
    var sizeBytes: UInt64
    var hashHex: String
    var paths: [String]
    var id: String { hashHex }
    enum CodingKeys: String, CodingKey {
        case paths
        case sizeBytes = "size_bytes"
        case hashHex = "hash_hex"
    }
}

struct ProcessSample: Codable, Identifiable, Hashable {
    var pid: UInt32
    var name: String
    var cpuPercent: Float
    var memoryBytes: UInt64
    var exePath: String?
    var id: UInt32 { pid }
    enum CodingKeys: String, CodingKey {
        case pid, name
        case cpuPercent = "cpu_percent"
        case memoryBytes = "memory_bytes"
        case exePath = "exe_path"
    }
}

struct SystemSample: Codable {
    var totalMemoryBytes: UInt64
    var usedMemoryBytes: UInt64
    var totalSwapBytes: UInt64
    var usedSwapBytes: UInt64
    var cpuPercent: Float
    var topProcesses: [ProcessSample]
    enum CodingKeys: String, CodingKey {
        case totalMemoryBytes = "total_memory_bytes"
        case usedMemoryBytes = "used_memory_bytes"
        case totalSwapBytes = "total_swap_bytes"
        case usedSwapBytes = "used_swap_bytes"
        case cpuPercent = "cpu_percent"
        case topProcesses = "top_processes"
    }
}

struct ScanContext: Codable {
    var home: String
    var allowedRoots: [String]
    var runningBundleIds: [String]
    var fullDiskAccess: Bool
    enum CodingKeys: String, CodingKey {
        case home
        case allowedRoots = "allowed_roots"
        case runningBundleIds = "running_bundle_ids"
        case fullDiskAccess = "full_disk_access"
    }
}

enum CoreError: Error, LocalizedError {
    case ffi(String)
    case decode(String)

    var errorDescription: String? {
        switch self {
        case .ffi(let m): "Core error: \(m)"
        case .decode(let m): "Decoding error: \(m)"
        }
    }
}

// MARK: - Bridge

enum CoreBridge {
    static let expectedFFIVersion: UInt32 = 1

    /// Call once at launch; aborts honestly if the linked core doesn't match.
    static func assertVersion() {
        let v = tabibu_ffi_version()
        precondition(
            v == expectedFFIVersion,
            "FFI version mismatch: core=\(v) app=\(expectedFFIVersion) — rebuild scripts/build-core.sh"
        )
    }

    /// Copy + free a Rust-owned string, then decode JSON into `T`.
    private static func take<T: Decodable>(_ ptr: UnsafeMutablePointer<CChar>?, as type: T.Type)
        throws -> T
    {
        guard let ptr else { throw CoreError.ffi("core returned NULL") }
        defer { tabibu_string_free(ptr) }
        let data = Data(bytes: ptr, count: strlen(ptr))
        if let err = try? JSONDecoder().decode([String: String].self, from: data),
            let msg = err["error"]
        {
            throw CoreError.ffi(msg)
        }
        do {
            return try JSONDecoder().decode(T.self, from: data)
        } catch {
            throw CoreError.decode("\(error)")
        }
    }

    private static func encode(_ value: some Encodable) -> String {
        (try? JSONEncoder().encode(value)).flatMap { String(data: $0, encoding: .utf8) } ?? "{}"
    }

    // MARK: Cancellable ops

    final class Operation: @unchecked Sendable {
        let handle: UInt64
        init() { handle = tabibu_op_new() }
        func cancel() { tabibu_op_cancel(handle) }
        deinit { tabibu_op_free(handle) }
    }

    // MARK: Scan (streaming)

    enum ScanEvent: Sendable {
        case item(CleanupItem)
        case done(ScanSummary)
    }

    /// Box passed through the C callback; retained until `done` fires.
    private final class ScanBox: @unchecked Sendable {
        let continuation: AsyncStream<ScanEvent>.Continuation
        init(_ c: AsyncStream<ScanEvent>.Continuation) { continuation = c }
    }

    /// Start a streaming scan. The stream finishes after `.done`.
    static func scan(ctx: ScanContext, scanners: [String], op: Operation)
        -> AsyncStream<ScanEvent>
    {
        struct Config: Codable {
            var home: String
            var allowed_roots: [String]
            var running_bundle_ids: [String]
            var full_disk_access: Bool
            var scanners: [String]
        }
        let cfg = Config(
            home: ctx.home, allowed_roots: ctx.allowedRoots,
            running_bundle_ids: ctx.runningBundleIds,
            full_disk_access: ctx.fullDiskAccess, scanners: scanners)

        return AsyncStream { continuation in
            let box = ScanBox(continuation)
            let ud = Unmanaged.passRetained(box).toOpaque()

            let onItem: tabibu_json_cb = { json, ud in
                guard let json, let ud else { return }
                let box = Unmanaged<ScanBox>.fromOpaque(ud).takeUnretainedValue()
                let data = Data(bytes: json, count: strlen(json))
                if let item = try? JSONDecoder().decode(CleanupItem.self, from: data) {
                    box.continuation.yield(.item(item))
                }
            }
            let onDone: tabibu_json_cb = { json, ud in
                guard let ud else { return }
                let box = Unmanaged<ScanBox>.fromOpaque(ud).takeRetainedValue()
                if let json,
                    let summary = try? JSONDecoder().decode(
                        ScanSummary.self, from: Data(bytes: json, count: strlen(json)))
                {
                    box.continuation.yield(.done(summary))
                }
                box.continuation.finish()
            }

            let started = encode(cfg).withCString {
                tabibu_scan_start($0, op.handle, onItem, onDone, ud)
            }
            if started == 0 {
                Unmanaged<ScanBox>.fromOpaque(ud).release()
                continuation.finish()
            }
        }
    }

    // MARK: Synchronous calls (run from background tasks)

    static func reclaim(items: [CleanupItem], ctx: ScanContext, undoDir: String) throws
        -> ReclaimReport
    {
        try encode(items).withCString { itemsC in
            try encode(ctx).withCString { ctxC in
                try undoDir.withCString { undoC in
                    try take(tabibu_reclaim(itemsC, ctxC, undoC), as: ReclaimReport.self)
                }
            }
        }
    }

    static func sizeTree(root: String, maxDepth: Int64, op: Operation) throws -> DirNode {
        try root.withCString {
            try take(tabibu_size_tree($0, maxDepth, op.handle), as: DirNode.self)
        }
    }

    static func findDuplicates(roots: [String], minSize: UInt64, op: Operation) throws
        -> [DuplicateGroup]
    {
        try encode(roots).withCString {
            try take(
                tabibu_dupes_find($0, minSize, op.handle, nil, nil), as: [DuplicateGroup].self)
        }
    }

    static func findRemnants(bundleId: String, appName: String, ctx: ScanContext) throws
        -> [CleanupItem]
    {
        try bundleId.withCString { bid in
            try appName.withCString { name in
                try encode(ctx).withCString { ctxC in
                    try take(tabibu_find_remnants(bid, name, ctxC), as: [CleanupItem].self)
                }
            }
        }
    }

    static func monitorSample(topN: UInt32, byCPU: Bool) throws -> SystemSample {
        try take(tabibu_monitor_sample(topN, byCPU), as: SystemSample.self)
    }
}

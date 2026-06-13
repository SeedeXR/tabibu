// CoreBridge (monitor subset): the only file that touches the C ABI.
// Deliberately duplicated from Tabibu/Sources/Tabibu/Core/CoreBridge.swift —
// each package is self-contained — trimmed to exactly what the menu-bar
// agent needs: version assert + tabibu_monitor_sample. Ownership rules
// (ADR-0001): strings returned by Rust are copied immediately and released
// via tabibu_string_free.

import CTabibuCore
import Foundation

// MARK: - Wire types (mirror serde's JSON for the Rust structs)

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

    /// System + top-N process sample. Call off the main thread.
    static func monitorSample(topN: UInt32, byCPU: Bool) throws -> SystemSample {
        try take(tabibu_monitor_sample(topN, byCPU), as: SystemSample.self)
    }
}

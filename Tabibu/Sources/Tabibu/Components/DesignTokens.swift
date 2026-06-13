// Design tokens — memory/design.md §4. Views use these, never magic numbers.

import SwiftUI

enum Space {
    static let xs: CGFloat = 4
    static let s: CGFloat = 8
    static let m: CGFloat = 12
    static let l: CGFloat = 16
    static let xl: CGFloat = 24
}

enum Dur {
    static let fast: Double = 0.15
    static let normal: Double = 0.30
}

enum Radius {
    static let card: CGFloat = 8
}

enum Fmt {
    private static let byteFormatter: ByteCountFormatter = {
        let f = ByteCountFormatter()
        f.countStyle = .file
        return f
    }()

    /// Humanized size ("2.4 GB"). Exact bytes belong in tooltips/detail.
    static func bytes(_ n: UInt64) -> String {
        byteFormatter.string(fromByteCount: Int64(clamping: n))
    }

    static func bytes(_ n: Int64) -> String {
        byteFormatter.string(fromByteCount: n)
    }
}

enum TierStyle {
    static func color(_ tier: String) -> Color {
        switch tier {
        case "Safe": .green
        case "Review": .orange
        case "Risky": .red
        default: .gray
        }
    }
}

/// Friendly names for scanner ids and item categories.
enum Naming {
    static func scanner(_ id: String) -> String {
        switch id {
        case "trash": "Trash"
        case "user_cache": "User Caches"
        case "dev_cache": "Developer Caches"
        case "temp": "Temporary Files"
        case "log": "Logs"
        case "large_old": "Large & Old Files"
        default: id
        }
    }

    static func category(_ raw: String) -> String {
        switch raw {
        case "Trash": "Trash"
        case "UserCache": "User Caches"
        case "DevCache": "Developer Caches"
        case "Temp": "Temporary Files"
        case "Log": "Logs"
        case "Duplicate": "Duplicates"
        case "LargeOldFile": "Large & Old Files"
        case "AppRemnant": "App Remnants"
        default: raw
        }
    }

    /// Abbreviate $HOME to "~" for display; full path stays in tooltips.
    static func displayPath(_ path: String, home: String) -> String {
        path.hasPrefix(home) ? "~" + path.dropFirst(home.count) : path
    }
}

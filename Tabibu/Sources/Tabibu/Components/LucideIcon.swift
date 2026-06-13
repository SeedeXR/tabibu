// LucideIcon — renders a Lucide SVG (ISC) from the bundle as a tintable
// template image. Verified on macOS 26.5: NSImage loads these stroke SVGs
// and, as templates, they tint with the current foreground style.
// Falls back to an SF Symbol if the asset is missing, so the UI never breaks.

import AppKit
import SwiftUI

struct LucideIcon: View {
    let name: String
    var size: CGFloat = 18
    var fallbackSymbol: String = "square.dashed"

    var body: some View {
        if let image = Self.image(named: name) {
            Image(nsImage: image)
                .resizable()
                .renderingMode(.template)
                .interpolation(.high)
                .frame(width: size, height: size)
        } else {
            Image(systemName: fallbackSymbol)
                .font(.system(size: size * 0.9))
        }
    }

    /// Cache loaded template images; the set is tiny and reused everywhere.
    private static var cache: [String: NSImage] = [:]

    private static func image(named name: String) -> NSImage? {
        if let hit = cache[name] { return hit }
        guard
            let url = Bundle.module.url(
                forResource: name, withExtension: "svg", subdirectory: "Icons"),
            let img = NSImage(contentsOf: url)
        else { return nil }
        img.isTemplate = true
        cache[name] = img
        return img
    }
}

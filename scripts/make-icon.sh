#!/bin/zsh
# Generate the Tabibu app icon programmatically (no design tool dependency):
# a macOS-style rounded-rect with a teal→emerald gradient (Tabibu = physician,
# a health/medical feel) and a white "activity/pulse" glyph (the Lucide
# `activity` polyline). Renders an .iconset at all required sizes, then
# iconutil → build/AppIcon.icns. Run: scripts/make-icon.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
OUT="$ROOT/build"
ICONSET="$OUT/Tabibu.iconset"
mkdir -p "$ICONSET"

SWIFT_SRC="$(mktemp /tmp/tabibu-icon-XXXX.swift)"
cat > "$SWIFT_SRC" <<'SWIFT'
import AppKit
import Foundation

let arg = CommandLine.arguments
guard arg.count == 3, let px = Int(arg[1]) else {
    FileHandle.standardError.write("usage: <px> <out.png>\n".data(using: .utf8)!)
    exit(2)
}
let size = CGFloat(px)
let out = arg[2]

let rep = NSBitmapImageRep(
    bitmapDataPlanes: nil, pixelsWide: px, pixelsHigh: px,
    bitsPerSample: 8, samplesPerPixel: 4, hasAlpha: true, isPlanar: false,
    colorSpaceName: .deviceRGB, bytesPerRow: 0, bitsPerPixel: 0)!
NSGraphicsContext.saveGraphicsState()
NSGraphicsContext.current = NSGraphicsContext(bitmapImageRep: rep)
let ctx = NSGraphicsContext.current!.cgContext

// Rounded-rect mask (macOS continuous-ish corner ≈ 0.2285 of the side, with
// a small margin so the tile isn't edge-to-edge).
let margin = size * 0.06
let rect = CGRect(x: margin, y: margin, width: size - 2 * margin, height: size - 2 * margin)
let radius = rect.width * 0.2285
let path = CGPath(roundedRect: rect, cornerWidth: radius, cornerHeight: radius, transform: nil)
ctx.addPath(path)
ctx.clip()

// Vertical gradient: deep teal → emerald.
let colors = [
    CGColor(red: 0.043, green: 0.369, blue: 0.349, alpha: 1.0),  // #0B5E59
    CGColor(red: 0.063, green: 0.725, blue: 0.506, alpha: 1.0),  // #10B981
] as CFArray
let grad = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(), colors: colors,
                      locations: [0, 1])!
ctx.drawLinearGradient(grad, start: CGPoint(x: 0, y: rect.maxY),
                       end: CGPoint(x: 0, y: rect.minY), options: [])

// Subtle top inner highlight.
ctx.addPath(path); ctx.clip()
let hi = CGGradient(colorsSpace: CGColorSpaceCreateDeviceRGB(),
    colors: [CGColor(red: 1, green: 1, blue: 1, alpha: 0.18),
             CGColor(red: 1, green: 1, blue: 1, alpha: 0)] as CFArray,
    locations: [0, 1])!
ctx.drawLinearGradient(hi, start: CGPoint(x: 0, y: rect.maxY),
                       end: CGPoint(x: 0, y: rect.midY), options: [])

// Lucide "activity" pulse glyph, drawn in a 24x24 viewBox scaled to ~58%.
// Path: M22 12h-2.48a2 2 0 0 0-1.93 1.46l-2.35 8.36a.25.25 0 0 1-.48 0
//       L9.24 2.18a.25.25 0 0 0-.48 0l-2.35 8.36A2 2 0 0 1 4.49 12H2
let glyphScale = size * 0.58 / 24.0
let glyphSide = 24.0 * glyphScale
let ox = (size - glyphSide) / 2
let oy = (size - glyphSide) / 2
func P(_ x: CGFloat, _ y: CGFloat) -> CGPoint {
    // SVG y grows downward; flip into the bitmap's bottom-left origin.
    CGPoint(x: ox + x * glyphScale, y: size - (oy + y * glyphScale))
}
let pulse = CGMutablePath()
pulse.move(to: P(22, 12))
pulse.addLine(to: P(19.52, 12))
pulse.addLine(to: P(17.59, 13.46))   // approximated control as line (small)
pulse.addLine(to: P(15.24, 21.82))
pulse.addLine(to: P(9.24, 2.18))
pulse.addLine(to: P(6.89, 10.54))
pulse.addLine(to: P(4.49, 12))
pulse.addLine(to: P(2, 12))
ctx.setStrokeColor(CGColor(red: 1, green: 1, blue: 1, alpha: 0.97))
ctx.setLineWidth(size * 0.058)
ctx.setLineCap(.round)
ctx.setLineJoin(.round)
ctx.addPath(pulse)
ctx.strokePath()

NSGraphicsContext.restoreGraphicsState()
guard let png = rep.representation(using: .png, properties: [:]) else { exit(3) }
try! png.write(to: URL(fileURLWithPath: out))
SWIFT

render() { swift "$SWIFT_SRC" "$1" "$2" >/dev/null; }

# Required iconset sizes (1x + 2x).
render 16   "$ICONSET/icon_16x16.png"
render 32   "$ICONSET/icon_16x16@2x.png"
render 32   "$ICONSET/icon_32x32.png"
render 64   "$ICONSET/icon_32x32@2x.png"
render 128  "$ICONSET/icon_128x128.png"
render 256  "$ICONSET/icon_128x128@2x.png"
render 256  "$ICONSET/icon_256x256.png"
render 512  "$ICONSET/icon_256x256@2x.png"
render 512  "$ICONSET/icon_512x512.png"
render 1024 "$ICONSET/icon_512x512@2x.png"

iconutil -c icns "$ICONSET" -o "$OUT/AppIcon.icns"
rm -f "$SWIFT_SRC"
echo "Generated $OUT/AppIcon.icns"
sips -g pixelWidth -g pixelHeight "$OUT/AppIcon.icns" | tail -2

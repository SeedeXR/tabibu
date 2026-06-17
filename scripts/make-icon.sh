#!/bin/zsh
# Generate the Tabibu app icon — Swift-free. Writes a self-contained SVG
# (teal→emerald rounded-rect + white "pulse/activity" glyph; Tabibu = healer),
# rasterizes it to a 1024px PNG with the system QuickLook tool (`qlmanage`),
# then feeds it to the Tauri icon generator which produces the full
# .icns/.png/sized set into app/src-tauri/icons/.
#
# Usage: scripts/make-icon.sh
set -euo pipefail

ROOT="$(cd "$(dirname "$0")/.." && pwd)"
TMP="$(mktemp -d /tmp/tabibu-icon-XXXX)"
SVG="$TMP/icon.svg"

cat > "$SVG" <<'EOF'
<svg xmlns="http://www.w3.org/2000/svg" width="1024" height="1024" viewBox="0 0 1024 1024">
  <defs>
    <linearGradient id="g" x1="0" y1="0" x2="0" y2="1">
      <stop offset="0" stop-color="#0B5E59"/>
      <stop offset="1" stop-color="#10B981"/>
    </linearGradient>
  </defs>
  <rect x="96" y="96" width="832" height="832" rx="232" fill="url(#g)"/>
  <path d="M232 512 h140 a40 40 0 0 0 38-28 l70-240 a8 8 0 0 1 15 0 l150 520 a8 8 0 0 0 15 0 l70-240 a40 40 0 0 1 38-28 H792"
        fill="none" stroke="#ffffff" stroke-width="56"
        stroke-linecap="round" stroke-linejoin="round"/>
</svg>
EOF

# QuickLook rasterizer (built into macOS) → 1024px PNG. Swift-free.
qlmanage -t -s 1024 -o "$TMP" "$SVG" > /dev/null 2>&1
SRC="$TMP/icon.svg.png"
[[ -f "$SRC" ]] || { echo "error: qlmanage did not produce $SRC" >&2; exit 1; }

# Tauri generates the platform icon set from the source PNG.
( cd "$ROOT/app" && npx tauri icon "$SRC" )

# Keep a copy of the source for reference/regeneration.
cp "$SRC" "$ROOT/app/src-tauri/icons/icon-source.png"
rm -rf "$TMP"
echo "Generated app/src-tauri/icons/ from the SVG (Swift-free)."
sips -g pixelWidth "$ROOT/app/src-tauri/icons/icon.png" 2>/dev/null | tail -1
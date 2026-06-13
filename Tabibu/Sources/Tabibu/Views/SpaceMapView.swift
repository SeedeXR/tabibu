// SpaceMapView — a squarified treemap of disk usage built from the Rust
// size tree, plus honest free-space and APFS-snapshot reporting. Click a
// rectangle to descend; breadcrumb to climb back.

import AppKit
import SwiftUI

@Observable
@MainActor
private final class SpaceModel {
    enum Phase: Equatable { case loading, ready, error(String) }
    var phase: Phase = .loading
    var root: DirNode?
    var path: [DirNode] = []          // breadcrumb stack; last = current
    var snapshots: [String] = []
    var space: (free: Int64, total: Int64)?
    private var op: CoreBridge.Operation?

    var current: DirNode? { path.last ?? root }

    func load(home: String) {
        phase = .loading
        space = SnapshotService.volumeSpace()
        snapshots = SnapshotService.localSnapshots()
        let op = CoreBridge.Operation()
        self.op = op
        Task {
            do {
                let tree = try await Task.detached {
                    try CoreBridge.sizeTree(root: home, maxDepth: 4, op: op)
                }.value
                root = tree
                path = [tree]
                phase = .ready
            } catch { phase = .error(error.localizedDescription) }
        }
    }

    func descend(_ node: DirNode) { if node.isDir && !node.children.isEmpty { path.append(node) } }
    func climb(to index: Int) { if index < path.count { path.removeSubrange((index + 1)...) } }
}

struct SpaceMapView: View {
    @Environment(AppModel.self) private var model
    @State private var sm = SpaceModel()

    var body: some View {
        VStack(spacing: 0) {
            switch sm.phase {
            case .loading:
                VStack(spacing: Space.m) {
                    ProgressView()
                    Text("Measuring your home folder…").foregroundStyle(.secondary)
                }.frame(maxWidth: .infinity, maxHeight: .infinity)
            case .error(let msg):
                ErrorState(message: msg, retry: { sm.load(home: model.home) })
            case .ready:
                header
                breadcrumb
                if let node = sm.current {
                    Treemap(node: node, home: model.home) { sm.descend($0) }
                        .padding(Space.l)
                }
            }
        }
        .navigationTitle("Disk")
        .task { if sm.root == nil { sm.load(home: model.home) } }
    }

    @ViewBuilder private var header: some View {
        if let space = sm.space {
            let used = space.total - space.free
            VStack(alignment: .leading, spacing: Space.s) {
                HStack {
                    Text("\(Fmt.bytes(space.free)) available")
                        .font(.title3.weight(.semibold))
                    Text("of \(Fmt.bytes(space.total))").foregroundStyle(.secondary)
                    Spacer()
                    if !sm.snapshots.isEmpty {
                        Label("\(sm.snapshots.count) local snapshot\(sm.snapshots.count == 1 ? "" : "s")",
                            systemImage: "clock.arrow.circlepath")
                            .font(.callout).foregroundStyle(.secondary)
                            .help("Time Machine keeps local snapshots that hold purgeable space. macOS frees them automatically when the disk fills — Tabibu never deletes them behind your back.")
                    }
                }
                ProgressView(value: Double(used), total: Double(max(space.total, 1)))
                    .tint(Double(space.free) / Double(max(space.total, 1)) < 0.1 ? .red : .accentColor)
            }
            .padding(Space.l)
        }
    }

    private var breadcrumb: some View {
        ScrollView(.horizontal, showsIndicators: false) {
            HStack(spacing: Space.xs) {
                ForEach(Array(sm.path.enumerated()), id: \.offset) { idx, node in
                    if idx > 0 { Image(systemName: "chevron.right").font(.caption2).foregroundStyle(.tertiary) }
                    Button(URL(fileURLWithPath: node.path).lastPathComponent) { sm.climb(to: idx) }
                        .buttonStyle(.plain)
                        .font(.callout.weight(idx == sm.path.count - 1 ? .semibold : .regular))
                }
            }
            .padding(.horizontal, Space.l)
        }
    }
}

/// Squarified-ish treemap rendered in a Canvas-backed layout. Keeps the
/// largest children prominent; hover shows name + size; click descends.
private struct Treemap: View {
    let node: DirNode
    let home: String
    let onSelect: (DirNode) -> Void
    @State private var hovered: String?

    var body: some View {
        GeometryReader { geo in
            let rects = layout(
                children: node.children.filter { $0.sizeBytes > 0 },
                in: CGRect(origin: .zero, size: geo.size))
            ZStack(alignment: .topLeading) {
                ForEach(rects, id: \.node.id) { entry in
                    cell(entry)
                }
            }
        }
    }

    private func cell(_ entry: LaidOut) -> some View {
        let depth = entry.node.path.count % 6
        let tint = Color(hue: 0.45 + Double(depth) * 0.04, saturation: 0.45, brightness: 0.85)
        return RoundedRectangle(cornerRadius: 4)
            .fill(tint.opacity(hovered == entry.node.id ? 0.95 : 0.7))
            .overlay(alignment: .topLeading) {
                if entry.rect.width > 70 && entry.rect.height > 28 {
                    VStack(alignment: .leading, spacing: 0) {
                        Text(URL(fileURLWithPath: entry.node.path).lastPathComponent)
                            .font(.caption.weight(.medium)).lineLimit(1)
                        Text(Fmt.bytes(entry.node.sizeBytes)).font(.caption2).opacity(0.8)
                    }
                    .padding(4).foregroundStyle(.white)
                }
            }
            .overlay(RoundedRectangle(cornerRadius: 4).strokeBorder(.white.opacity(0.25)))
            .frame(width: max(entry.rect.width - 2, 1), height: max(entry.rect.height - 2, 1))
            .offset(x: entry.rect.minX, y: entry.rect.minY)
            .help("\(Naming.displayPath(entry.node.path, home: home)) — \(Fmt.bytes(entry.node.sizeBytes))")
            .onHover { hovered = $0 ? entry.node.id : nil }
            .onTapGesture { onSelect(entry.node) }
            .animation(.easeOut(duration: Dur.fast), value: hovered)
    }

    private struct LaidOut { let node: DirNode; let rect: CGRect }

    /// Slice-and-dice layout: split the longer axis proportionally to size,
    /// alternating orientation by recursion depth for squarer tiles.
    private func layout(children: [DirNode], in rect: CGRect) -> [LaidOut] {
        let sorted = children.sorted { $0.sizeBytes > $1.sizeBytes }
        let total = sorted.reduce(UInt64(0)) { $0 + $1.sizeBytes }
        guard total > 0 else { return [] }
        var result: [LaidOut] = []
        var offset = rect.minX
        let horizontal = rect.width >= rect.height
        var vOffset = rect.minY
        for child in sorted {
            let frac = CGFloat(child.sizeBytes) / CGFloat(total)
            if horizontal {
                let w = rect.width * frac
                result.append(LaidOut(node: child, rect: CGRect(x: offset, y: rect.minY, width: w, height: rect.height)))
                offset += w
            } else {
                let h = rect.height * frac
                result.append(LaidOut(node: child, rect: CGRect(x: rect.minX, y: vOffset, width: rect.width, height: h)))
                vOffset += h
            }
        }
        return result
    }
}

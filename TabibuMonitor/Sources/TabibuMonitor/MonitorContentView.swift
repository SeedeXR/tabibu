// MonitorContentView — the popover body, hand-built with AppKit views (no
// SwiftUI). Shows memory + swap + CPU and the top processes, plus an
// "Open Tabibu" button. Rebuilt cheaply on each 2s sample.

import AppKit

final class MonitorContentView: NSView {
    var onOpenTabibu: (() -> Void)?

    private let titleLabel = NSTextField(labelWithString: "Tabibu Monitor")
    private let memLabel = NSTextField(labelWithString: "")
    private let swapLabel = NSTextField(labelWithString: "")
    private let cpuLabel = NSTextField(labelWithString: "")
    private let procStack = NSStackView()
    private let openButton = NSButton(title: "Open Tabibu", target: nil, action: nil)

    private static let byteFormatter: ByteCountFormatter = {
        let f = ByteCountFormatter(); f.countStyle = .memory; return f
    }()

    override init(frame: NSRect) {
        super.init(frame: NSRect(x: 0, y: 0, width: 300, height: 280))
        build()
    }

    required init?(coder: NSCoder) { nil }

    private func build() {
        titleLabel.font = .boldSystemFont(ofSize: 13)
        for l in [memLabel, swapLabel, cpuLabel] {
            l.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
            l.textColor = .secondaryLabelColor
        }
        procStack.orientation = .vertical
        procStack.alignment = .leading
        procStack.spacing = 4

        openButton.target = self
        openButton.action = #selector(openTapped)
        openButton.bezelStyle = .rounded

        let header = NSStackView(views: [memLabel, cpuLabel, swapLabel])
        header.orientation = .vertical
        header.alignment = .leading
        header.spacing = 2

        let root = NSStackView(views: [
            titleLabel, header, separator(), procStack, separator(), openButton,
        ])
        root.orientation = .vertical
        root.alignment = .leading
        root.spacing = 8
        root.edgeInsets = NSEdgeInsets(top: 12, left: 14, bottom: 12, right: 14)
        root.translatesAutoresizingMaskIntoConstraints = false
        addSubview(root)
        NSLayoutConstraint.activate([
            root.leadingAnchor.constraint(equalTo: leadingAnchor),
            root.trailingAnchor.constraint(equalTo: trailingAnchor),
            root.topAnchor.constraint(equalTo: topAnchor),
            root.bottomAnchor.constraint(equalTo: bottomAnchor),
        ])
    }

    private func separator() -> NSBox {
        let b = NSBox(); b.boxType = .separator; return b
    }

    @objc private func openTapped() { onOpenTabibu?() }

    func update(with s: SystemSample) {
        let usedPct = Int(Double(s.usedMemoryBytes) / Double(max(s.totalMemoryBytes, 1)) * 100)
        memLabel.stringValue =
            "Memory  \(Self.byteFormatter.string(fromByteCount: Int64(s.usedMemoryBytes))) / "
            + "\(Self.byteFormatter.string(fromByteCount: Int64(s.totalMemoryBytes)))  (\(usedPct)%)"
        cpuLabel.stringValue = "CPU     \(Int(s.cpuPercent))%"
        swapLabel.stringValue =
            "Swap    \(Self.byteFormatter.string(fromByteCount: Int64(s.usedSwapBytes)))"

        procStack.arrangedSubviews.forEach { $0.removeFromSuperview() }
        for p in s.topProcesses.prefix(6) {
            let row = NSTextField(
                labelWithString:
                    "\(Int(p.cpuPercent))%  "
                    + "\(Self.byteFormatter.string(fromByteCount: Int64(p.memoryBytes)))  \(p.name)")
            row.font = .monospacedSystemFont(ofSize: 11, weight: .regular)
            row.lineBreakMode = .byTruncatingTail
            procStack.addArrangedSubview(row)
        }
    }
}

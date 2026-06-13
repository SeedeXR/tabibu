// ResultView — the honest outcome screen (design.md §3 step 4). Shows the
// MEASURED bytes reclaimed (from the engine, not an estimate), per-item
// failures if any, and an Undo affordance that reveals the manifest.

import AppKit
import SwiftUI

struct ResultView: View {
    let report: ReclaimReport
    let onDone: () -> Void

    private var failures: [ReclaimOutcome] {
        report.outcomes.filter { $0.error != nil }
    }

    var body: some View {
        VStack(spacing: Space.l) {
            LucideIcon(name: "sparkles", size: 52, fallbackSymbol: "checkmark.seal")
                .foregroundStyle(.green)
            Text("\(Fmt.bytes(report.reclaimedBytes)) reclaimed")
                .font(.largeTitle.weight(.semibold))
            Text("\(report.succeeded) item\(report.succeeded == 1 ? "" : "s") moved to the Trash"
                + (report.failed > 0 ? " · \(report.failed) skipped" : ""))
                .font(.callout)
                .foregroundStyle(.secondary)

            if !failures.isEmpty {
                VStack(alignment: .leading, spacing: Space.xs) {
                    Text("Skipped (with reason):").font(.subheadline.weight(.semibold))
                    ForEach(failures, id: \.path) { f in
                        Text("• \(f.path) — \(f.error ?? "")")
                            .font(.system(.caption, design: .monospaced))
                            .foregroundStyle(.secondary)
                            .textSelection(.enabled)
                    }
                }
                .padding(Space.m)
                .frame(maxWidth: 520, alignment: .leading)
                .background(.quaternary.opacity(0.4), in: RoundedRectangle(cornerRadius: Radius.card))
            }

            HStack(spacing: Space.m) {
                if let manifest = report.manifestPath {
                    Button {
                        NSWorkspace.shared.selectFile(manifest, inFileViewerRootedAtPath: "")
                    } label: {
                        Label("Show Undo Record", systemImage: "arrow.uturn.backward")
                    }
                    .help("Items are in the Trash; this is the record of what was moved.")
                }
                Button("Done", action: onDone)
                    .buttonStyle(.borderedProminent)
                    .keyboardShortcut(.defaultAction)
            }
            .padding(.top, Space.s)
        }
        .padding(Space.xl)
        .frame(maxWidth: .infinity, maxHeight: .infinity)
    }
}

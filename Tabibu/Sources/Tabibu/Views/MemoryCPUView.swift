// MemoryCPUView — live system + per-process view. Reports memory PRESSURE
// (the metric that predicts slowdowns), never a "free RAM" button. When
// pressure is high it suggests quitting the heaviest apps instead.

import AppKit
import SwiftUI

@Observable
@MainActor
private final class MonitorModel {
    var sample: SystemSample?
    var byCPU = true
    var pressure: MemoryPressureService.Level = .normal
    var error: String?
    private var timer: Timer?
    private var pressureService: MemoryPressureService?

    func start() {
        pressureService = MemoryPressureService { [weak self] level in self?.pressure = level }
        tick()
        timer = Timer.scheduledTimer(withTimeInterval: 2, repeats: true) { [weak self] _ in
            Task { @MainActor in self?.tick() }
        }
    }

    func stop() { timer?.invalidate(); timer = nil; pressureService = nil }

    private func tick() {
        let byCPU = self.byCPU
        Task {
            do {
                let s = try await Task.detached { try CoreBridge.monitorSample(topN: 12, byCPU: byCPU) }.value
                self.sample = s
                self.error = nil
            } catch { self.error = error.localizedDescription }
        }
    }
}

struct MemoryCPUView: View {
    @Environment(AppModel.self) private var model
    @State private var mm = MonitorModel()

    private static let daemonNotes: [String: String] = [
        "kernel_task": "Not a bug to fix: macOS uses kernel_task to absorb CPU time and keep the Mac cool when it heats up. High usage usually means something else is generating heat.",
        "mds_stores": "Spotlight building its search index. Spikes after updates or large file changes, then settles. Excluding huge folders in Spotlight settings reduces it.",
        "mdworker": "Spotlight indexing helper. Same story as mds_stores — temporary while indexing.",
        "photoanalysisd": "Photos analyzing your library for faces and scenes. Runs after imports/migration; finishes faster plugged in and idle.",
        "WindowServer": "Draws everything on screen. High usage often comes from many windows, scaled/external displays, or a busy app redrawing constantly.",
        "bird": "iCloud Drive sync. Activity here means files are uploading or downloading.",
        "cloudd": "iCloud sync engine. Busy during large syncs.",
        "trustd": "Verifies code signatures and certificates. Brief spikes when launching new apps.",
        "backupd": "Time Machine backup in progress.",
    ]

    var body: some View {
        VStack(spacing: 0) {
            if let s = mm.sample {
                dials(s)
                if mm.pressure != .normal {
                    suggestion(s)
                }
                processList(s)
            } else if let e = mm.error {
                ErrorState(message: e, retry: { mm.stop(); mm.start() })
            } else {
                ProgressView().frame(maxWidth: .infinity, maxHeight: .infinity)
            }
        }
        .navigationTitle("Memory & CPU")
        .onAppear { mm.start() }
        .onDisappear { mm.stop() }
    }

    private func dials(_ s: SystemSample) -> some View {
        let memFrac = Double(s.usedMemoryBytes) / Double(max(s.totalMemoryBytes, 1))
        let pressureColor: Color = switch mm.pressure {
            case .normal: .green; case .warning: .yellow; case .critical: .red
        }
        return HStack(spacing: Space.xl) {
            PressureDial(title: "Memory", value: memFrac,
                label: "\(Int(memFrac * 100))%", tint: pressureColor)
            PressureDial(title: "CPU", value: Double(s.cpuPercent) / 100,
                label: "\(Int(s.cpuPercent))%", tint: .accentColor)
            VStack(alignment: .leading, spacing: Space.xs) {
                row("Memory pressure", mm.pressure.rawValue, pressureColor)
                row("Memory used", "\(Fmt.bytes(s.usedMemoryBytes)) / \(Fmt.bytes(s.totalMemoryBytes))", .primary)
                row("Swap used", Fmt.bytes(s.usedSwapBytes), s.usedSwapBytes > 0 ? .orange : .primary)
            }
            Spacer()
        }
        .padding(Space.l)
    }

    private func row(_ k: String, _ v: String, _ color: Color) -> some View {
        HStack {
            Text(k).foregroundStyle(.secondary)
            Spacer(minLength: Space.l)
            Text(v).font(.system(.body, design: .monospaced)).foregroundStyle(color)
        }
        .font(.callout)
    }

    private func suggestion(_ s: SystemSample) -> some View {
        let heaviest = s.topProcesses.max(by: { $0.memoryBytes < $1.memoryBytes })
        return HStack(spacing: Space.m) {
            Image(systemName: "exclamationmark.circle").foregroundStyle(.orange)
            Text(heaviest.map {
                "Memory pressure is \(mm.pressure.rawValue.lowercased()). The heaviest app right now is \($0.name) (\(Fmt.bytes($0.memoryBytes))). Quitting apps you're not using frees memory honestly — there's no magic button."
            } ?? "Memory pressure is elevated. Quitting unused apps is the real fix.")
                .font(.callout).fixedSize(horizontal: false, vertical: true)
            Spacer()
        }
        .padding(Space.m)
        .background(Color.orange.opacity(0.1), in: RoundedRectangle(cornerRadius: Radius.card))
        .padding(.horizontal, Space.l)
    }

    private func processList(_ s: SystemSample) -> some View {
        VStack(spacing: 0) {
            HStack {
                Text("Top processes").font(.headline)
                Spacer()
                Picker("", selection: $mm.byCPU) {
                    Text("By CPU").tag(true); Text("By Memory").tag(false)
                }.pickerStyle(.segmented).frame(width: 220).labelsHidden()
            }
            .padding(.horizontal, Space.l).padding(.vertical, Space.s)

            ScrollView {
                LazyVStack(spacing: 0) {
                    ForEach(s.topProcesses) { p in
                        ProcessRow(p: p, note: Self.daemonNotes[p.name])
                        Divider().opacity(0.3)
                    }
                }.padding(.horizontal, Space.l)
            }
        }
    }
}

private struct ProcessRow: View {
    let p: ProcessSample
    let note: String?
    @State private var showNote = false
    @State private var confirmQuit = false

    var body: some View {
        HStack(spacing: Space.m) {
            Text(p.name).lineLimit(1).frame(maxWidth: 240, alignment: .leading)
            if note != nil {
                Button { showNote = true } label: { Image(systemName: "info.circle") }
                    .buttonStyle(.plain).foregroundStyle(.secondary)
                    .popover(isPresented: $showNote) {
                        Text(note ?? "").font(.callout).padding(Space.m).frame(width: 320)
                    }
            }
            Spacer()
            Text("\(Int(p.cpuPercent))%").font(.system(.body, design: .monospaced))
                .frame(width: 60, alignment: .trailing).foregroundStyle(p.cpuPercent > 80 ? .red : .secondary)
            Text(Fmt.bytes(p.memoryBytes)).font(.system(.body, design: .monospaced))
                .frame(width: 90, alignment: .trailing).foregroundStyle(.secondary)
            Menu {
                Button("Quit \(p.name)…", role: .destructive) { confirmQuit = true }
                    .disabled(runningApp == nil)
            } label: { Image(systemName: "ellipsis.circle") }
                .menuStyle(.borderlessButton).frame(width: 28)
        }
        .padding(.vertical, Space.xs)
        .confirmationDialog("Quit \(p.name)?", isPresented: $confirmQuit, titleVisibility: .visible) {
            Button("Quit", role: .destructive) { runningApp?.terminate() }
            Button("Cancel", role: .cancel) {}
        } message: {
            Text("This asks the app to quit. Save your work first — Tabibu can't recover unsaved changes.")
        }
    }

    /// Match the sampled pid to a running app we can ask to quit.
    private var runningApp: NSRunningApplication? {
        NSRunningApplication(processIdentifier: pid_t(p.pid))
    }
}

// BatteryView — real IOKit battery facts, or an honest "no battery" state on
// desktops. Only fields that actually read are shown; no invented numbers.

import SwiftUI

struct BatteryView: View {
    @State private var info = BatteryService.read()
    private let refresh = Timer.publish(every: 10, on: .main, in: .common).autoconnect()

    var body: some View {
        Group {
            if info.hasBattery {
                content
            } else {
                EmptyState(
                    lucide: "battery",
                    title: "No battery on this Mac",
                    message: "This appears to be a desktop Mac or has no internal battery, so there's nothing to report here.")
            }
        }
        .navigationTitle("Battery")
        .onReceive(refresh) { _ in info = BatteryService.read() }
    }

    private var content: some View {
        VStack(alignment: .leading, spacing: Space.l) {
            if let charge = info.chargePercent {
                HStack(spacing: Space.m) {
                    PressureDial(
                        title: "Charge", value: Double(charge) / 100, label: "\(charge)%",
                        tint: charge < 20 ? .red : .green)
                    VStack(alignment: .leading, spacing: Space.s) {
                        if let src = info.powerSource { stat("Power source", src) }
                        if let charging = info.isCharging { stat("Charging", charging ? "Yes" : "No") }
                        if let mins = info.timeToEmptyMinutes {
                            stat("Time remaining", "\(mins / 60)h \(mins % 60)m")
                        }
                    }
                    Spacer()
                }
            }
            Divider()
            VStack(alignment: .leading, spacing: Space.s) {
                Text("Battery health").font(.headline)
                if let health = info.healthPercent { stat("Capacity vs. design", "\(health)%") }
                if let cycles = info.cycleCount { stat("Cycle count", "\(cycles)") }
                if let condition = info.condition { stat("Condition", condition) }
                if info.healthPercent == nil && info.cycleCount == nil {
                    Text("Detailed health metrics weren't available from this Mac's battery controller.")
                        .font(.callout).foregroundStyle(.secondary)
                }
            }
            Spacer()
        }
        .padding(Space.xl)
        .frame(maxWidth: .infinity, alignment: .leading)
    }

    private func stat(_ k: String, _ v: String) -> some View {
        HStack {
            Text(k).foregroundStyle(.secondary)
            Spacer(minLength: Space.xl)
            Text(v).font(.system(.body, design: .monospaced))
        }
        .font(.callout).frame(maxWidth: 360, alignment: .leading)
    }
}

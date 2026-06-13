// BatteryService — real battery facts via IOKit. Charge/time from
// IOPSCopyPowerSourcesInfo; cycle count + capacity from the AppleSmartBattery
// IORegistry node. Every field is optional: desktops have no battery, and
// some keys vary by model — we report only what actually reads. No fakery.

import Foundation
import IOKit
import IOKit.ps

struct BatteryInfo {
    var hasBattery: Bool
    var chargePercent: Int?
    var isCharging: Bool?
    var powerSource: String?
    var timeToEmptyMinutes: Int?
    var cycleCount: Int?
    var healthPercent: Int?
    var condition: String?
}

enum BatteryService {
    static func read() -> BatteryInfo {
        var info = BatteryInfo(hasBattery: false)
        readPowerSources(into: &info)
        readSmartBattery(into: &info)
        return info
    }

    private static func readPowerSources(into info: inout BatteryInfo) {
        guard
            let blob = IOPSCopyPowerSourcesInfo()?.takeRetainedValue(),
            let sources = IOPSCopyPowerSourcesList(blob)?.takeRetainedValue() as? [CFTypeRef]
        else { return }

        for source in sources {
            guard
                let desc = IOPSGetPowerSourceDescription(blob, source)?.takeUnretainedValue()
                    as? [String: Any]
            else { continue }
            if let type = desc[kIOPSTypeKey] as? String, type == kIOPSInternalBatteryType {
                info.hasBattery = true
                if let cur = desc[kIOPSCurrentCapacityKey] as? Int,
                    let max = desc[kIOPSMaxCapacityKey] as? Int, max > 0
                {
                    info.chargePercent = Int((Double(cur) / Double(max) * 100).rounded())
                }
                if let state = desc[kIOPSPowerSourceStateKey] as? String {
                    info.powerSource = state
                    info.isCharging = (desc[kIOPSIsChargingKey] as? Bool) ?? false
                }
                if let mins = desc[kIOPSTimeToEmptyKey] as? Int, mins > 0 {
                    info.timeToEmptyMinutes = mins
                }
            }
        }
    }

    private static func readSmartBattery(into info: inout BatteryInfo) {
        let service = IOServiceGetMatchingService(
            kIOMainPortDefault, IOServiceMatching("AppleSmartBattery"))
        guard service != 0 else { return }
        defer { IOObjectRelease(service) }

        func intProp(_ key: String) -> Int? {
            guard
                let ref = IORegistryEntryCreateCFProperty(
                    service, key as CFString, kCFAllocatorDefault, 0)?.takeRetainedValue()
            else { return nil }
            return (ref as? NSNumber)?.intValue
        }

        if let cycles = intProp("CycleCount") { info.cycleCount = cycles }
        // Health = current full-charge capacity vs design capacity, when both read.
        if let design = intProp("DesignCapacity"),
            let full = intProp("AppleRawMaxCapacity") ?? intProp("MaxCapacity"),
            design > 0
        {
            info.healthPercent = Int((Double(full) / Double(design) * 100).rounded())
        }
        if let serviceFlag = intProp("PermanentFailureStatus") {
            info.condition = serviceFlag == 0 ? "Normal" : "Service recommended"
        }
    }
}

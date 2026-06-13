// Entry point. `--version` exists so CI and packaging scripts can verify the
// FFI link without launching UI.

import SwiftUI

@main
enum Entry {
    static func main() {
        if CommandLine.arguments.contains("--version") {
            print("Tabibu 0.1.0 (ffi v\(tabibuFFIVersion()))")
            return
        }
        CoreBridge.assertVersion()
        TabibuApp.main()
    }
}

private func tabibuFFIVersion() -> UInt32 {
    CTabibuCore.tabibu_ffi_version()
}

import CTabibuCore

struct TabibuApp: App {
    @State private var model = AppModel()

    var body: some Scene {
        WindowGroup {
            RootView()
                .environment(model)
                .frame(minWidth: 960, minHeight: 620)
        }
        .windowStyle(.automatic)
    }
}

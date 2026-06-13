// The complete, fixed command surface the helper exposes over XPC. There is
// NO "run arbitrary path as root" — every privileged action is an explicit,
// named method with typed arguments (memory/architecture.md §6). Adding a
// capability means adding a method here, reviewed deliberately.

import Foundation

@objc(TabibuHelperProtocol)
protocol TabibuHelperProtocol {
    /// Which of `paths` are currently held open by some process. Used as the
    /// open-file guard before touching anything under /private/var/folders.
    func checkOpenFiles(paths: [String], reply: @escaping ([String]) -> Void)

    /// SMART disk status. Honest stub until `smartctl` is bundled (M6+).
    func smartStatus(reply: @escaping (String) -> Void)

    /// Helper build version, for the app↔helper handshake.
    func version(reply: @escaping (String) -> Void)
}

let kHelperMachServiceName = "xr.seede.tabibu.helper"
let kHelperVersion = "0.1.0"

// TabibuHelper — root XPC helper entry point. Listens on a Mach service,
// validates the connecting client's code signature, and serves the fixed
// command set in HelperProtocol. Installed via SMAppService from the signed
// app (blocked locally: no Developer ID — see docs/modules/helper.md).

import Foundation

// MARK: - Service implementation

final class HelperService: NSObject, TabibuHelperProtocol {
    func version(reply: @escaping (String) -> Void) {
        reply(kHelperVersion)
    }

    func smartStatus(reply: @escaping (String) -> Void) {
        // Honest stub: we do not bundle smartctl yet, and IOKit SMART access
        // is model-fragile. Report unavailability rather than guessing.
        reply("unavailable: SMART reading not bundled in this build")
    }

    func checkOpenFiles(paths: [String], reply: @escaping ([String]) -> Void) {
        guard !paths.isEmpty else { return reply([]) }
        // lsof with explicit paths: simple and robust. The heavier libproc
        // route (proc_listpids + proc_pidfdinfo) is faster but far more
        // fragile across OS versions; for a guard check, correctness wins.
        let task = Process()
        task.executableURL = URL(fileURLWithPath: "/usr/sbin/lsof")
        task.arguments = ["-F", "n", "--"] + paths
        let pipe = Pipe()
        task.standardOutput = pipe
        task.standardError = Pipe()
        do {
            try task.run()
            task.waitUntilExit()
        } catch {
            return reply([])  // lsof unavailable → report nothing open, never crash
        }
        let data = pipe.fileHandleForReading.readDataToEndOfFile()
        let out = String(data: data, encoding: .utf8) ?? ""
        let open = paths.filter { p in out.contains(p) }
        reply(Array(Set(open)))
    }
}

// MARK: - Listener delegate with client validation

final class ListenerDelegate: NSObject, NSXPCListenerDelegate {
    func listener(_ listener: NSXPCListener, shouldAcceptNewConnection conn: NSXPCConnection) -> Bool {
        guard ClientValidator.isTrusted(connection: conn) else {
            NSLog("TabibuHelper: rejected connection — client code-signing check failed")
            return false
        }
        conn.exportedInterface = NSXPCInterface(with: TabibuHelperProtocol.self)
        conn.exportedObject = HelperService()
        conn.resume()
        return true
    }
}

// MARK: - Code-signing validation

enum ClientValidator {
    /// PRODUCTION requirement (enabled once Developer ID is available): the
    /// client must be signed by our Team and present our app's identifier:
    ///
    ///   anchor apple generic and
    ///   certificate leaf[subject.OU] = "<TEAMID>" and
    ///   identifier "xr.seede.tabibu"
    ///
    /// validated against the connection's audit token via
    /// SecCodeCopyGuestWithAttributes(kSecGuestAttributeAudit) +
    /// SecCodeCheckValidity. Implemented but gated below because there is no
    /// signing identity on the build machine yet.
    static func isTrusted(connection: NSXPCConnection) -> Bool {
        #if DEBUG
        NSLog("TabibuHelper: DEBUG build — accepting connection WITHOUT code-sign validation. NOT FOR RELEASE.")
        return true
        #else
        return validateAuditToken(connection.auditToken)
        #endif
    }

    #if !DEBUG
    private static func validateAuditToken(_ token: audit_token_t) -> Bool {
        var tokenCopy = token
        let tokenData = Data(bytes: &tokenCopy, count: MemoryLayout<audit_token_t>.size)
        let attrs =
            [kSecGuestAttributeAudit as String: tokenData] as CFDictionary
        var code: SecCode?
        guard SecCodeCopyGuestWithAttributes(nil, attrs, [], &code) == errSecSuccess,
            let guest = code
        else { return false }
        // Replace <TEAMID> at release time.
        let req =
            "anchor apple generic and identifier \"xr.seede.tabibu\" and certificate leaf[subject.OU] = \"<TEAMID>\""
        var requirement: SecRequirement?
        guard SecRequirementCreateWithString(req as CFString, [], &requirement) == errSecSuccess
        else { return false }
        return SecCodeCheckValidity(guest, [], requirement) == errSecSuccess
    }
    #endif
}

// MARK: - Run loop

let delegate = ListenerDelegate()
let listener = NSXPCListener(machServiceName: kHelperMachServiceName)
listener.delegate = delegate
listener.resume()
NSLog("TabibuHelper \(kHelperVersion) listening on \(kHelperMachServiceName)")
RunLoop.current.run()

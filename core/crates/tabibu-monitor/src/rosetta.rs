//! Rosetta 2 translation detection for a given pid.
//!
//! On Apple Silicon, x86_64 binaries run under Rosetta 2. The kernel records
//! this by setting the `P_TRANSLATED` (`0x0002_0000`) bit in the process's
//! `p_flag`. We fetch that flag via `sysctl(KERN_PROC_PID)`, which returns a
//! `kinfo_proc` for the target pid; `p_flag` lives in its embedded
//! `kp_proc` (`struct extern_proc`).
//!
//! ## Why we parse raw bytes rather than use `libc::kinfo_proc`
//!
//! `kinfo_proc` is the canonical reference for this check, but the pinned
//! `libc` (0.2.186) does NOT expose the `kinfo_proc` type on
//! `*-apple-darwin`, so we cannot name the struct. We therefore issue the
//! `sysctl` ourselves and read `p_flag` at its fixed offset within the
//! returned buffer.
//!
//! Offset rationale (`sys/proc.h`, 64-bit Darwin): `kinfo_proc` begins with
//! `kp_proc` (a `struct extern_proc`) at offset 0. `extern_proc` starts with
//! `p_un` (a union of two pointers, 16 bytes), then `p_vmspace` (pointer, 8),
//! then `p_sigacts` (pointer, 8), placing the `int p_flag` at byte offset 32.
//! This offset is verified empirically in this crate's tests: an x86_64 (under
//! Rosetta) process reports `p_flag = 0x0003_4004` (bit 0x20000 set) while a
//! native arm64 process reports `0x0000_4004` (bit clear).
//!
//! ## Why NOT `proc_pidinfo(PROC_PIDTBSDINFO)`
//!
//! That route IS exposed by libc (`proc_bsdinfo`), but it was tried and
//! empirically rejected: `proc_bsdinfo.pbi_flags` does not carry the
//! `P_TRANSLATED` bit (it reads `0x0340_4010` for a confirmed-x86_64 process,
//! bit clear). The `sysctl`/`p_flag` path is the one that actually works.
//!
//! This module is the only place in the crate that uses `unsafe`. The crate
//! otherwise denies `unsafe_code`; per memory/instruction.md §2, the syscall
//! wrapper is the allowed exception, hence the file-wide allow below.
//!
//! Non-aarch64 targets cannot be running under Rosetta, so on those targets we
//! short-circuit to `Some(false)` without touching the syscall — see
//! [`process_is_translated`].
#![allow(unsafe_code)]

/// `P_TRANSLATED`: set in `extern_proc.p_flag` when the process is running
/// translated under Rosetta 2. Defined in the XNU headers (`sys/proc.h`) and
/// stable across macOS releases.
const P_TRANSLATED: i32 = 0x0002_0000;

/// Byte offset of the `int p_flag` field within the `kinfo_proc` buffer
/// returned by `sysctl(KERN_PROC_PID)` on 64-bit Darwin. See module docs for
/// the derivation; verified empirically by the tests below.
#[cfg(target_arch = "aarch64")]
const P_FLAG_OFFSET: usize = 32;

/// Returns whether the process `pid` is running translated under Rosetta 2.
///
/// - `Some(true)`  — translated (x86_64 on Apple Silicon).
/// - `Some(false)` — native.
/// - `None`        — unknown: the process is gone, `sysctl` failed / returned a
///   short read, OR the hardcoded `p_flag` offset failed self-validation on
///   this OS (see [`OFFSET_TRUSTED`]). Never panics, never unwraps on FFI.
///
/// On non-aarch64 build targets there is no Rosetta, so we return `Some(false)`
/// without issuing the syscall.
#[must_use]
pub fn process_is_translated(pid: u32) -> Option<bool> {
    #[cfg(not(target_arch = "aarch64"))]
    {
        let _ = pid;
        Some(false)
    }

    #[cfg(target_arch = "aarch64")]
    {
        // Fail safe to `None` if the byte offset doesn't agree with the
        // kernel's documented self-report on this OS (guards against a future
        // struct-layout change silently producing confident-but-wrong answers).
        if !*OFFSET_TRUSTED {
            return None;
        }
        read_translated_via_offset(pid)
    }
}

/// One-time check that `P_FLAG_OFFSET` is correct on the running OS: compare
/// our offset-based read of the *current* process against the documented
/// `sysctl.proc_translated` scalar (valid only for self). If they disagree the
/// hardcoded layout is wrong here, so we distrust the offset method entirely.
/// If the key is unavailable we keep trusting (the offset is correct on every
/// known release; this only catches a future regression).
#[cfg(target_arch = "aarch64")]
static OFFSET_TRUSTED: std::sync::LazyLock<bool> = std::sync::LazyLock::new(|| {
    let Some(via_offset) = read_translated_via_offset(std::process::id()) else {
        return false;
    };
    let mut val: libc::c_int = 0;
    let mut len = std::mem::size_of::<libc::c_int>();
    // SAFETY: `sysctlbyname` with a static NUL-terminated name and a correctly
    // sized scalar output buffer; no pointers escape.
    let rc = unsafe {
        libc::sysctlbyname(
            c"sysctl.proc_translated".as_ptr(),
            std::ptr::addr_of_mut!(val).cast::<libc::c_void>(),
            std::ptr::addr_of_mut!(len),
            std::ptr::null_mut(),
            0,
        )
    };
    if rc != 0 {
        return true; // key unavailable — can't validate; offset is known-good
    }
    (val == 1) == via_offset
});

/// The raw offset-based read (no self-validation). See module docs.
#[cfg(target_arch = "aarch64")]
fn read_translated_via_offset(pid: u32) -> Option<bool> {
    {
        // A real `kinfo_proc` is ~648 bytes on Darwin; over-allocate so the
        // kernel never has to truncate, and we only need the leading bytes.
        let mut buf = [0u8; 1024];
        let mut size = buf.len();

        // SAFETY: We call `sysctl` with a fixed, valid 4-element MIB
        // (`CTL_KERN`/`KERN_PROC`/`KERN_PROC_PID`/pid) and a correctly sized,
        // fully-owned output buffer. `oldlenp` is initialised to the buffer
        // length and updated by the kernel with the number of bytes written;
        // we only read past offset 0 after confirming the call succeeded and
        // the kernel wrote at least `P_FLAG_OFFSET + 4` bytes. No raw pointers
        // escape this block.
        let rc = unsafe {
            let mut mib: [libc::c_int; 4] = [
                libc::CTL_KERN,
                libc::KERN_PROC,
                libc::KERN_PROC_PID,
                pid as libc::c_int,
            ];
            libc::sysctl(
                mib.as_mut_ptr(),
                mib.len() as libc::c_uint,
                buf.as_mut_ptr().cast::<libc::c_void>(),
                std::ptr::addr_of_mut!(size),
                std::ptr::null_mut(),
                0,
            )
        };

        if rc != 0 {
            return None;
        }
        // A dead/unknown pid yields size 0 or a short buffer.
        if size < P_FLAG_OFFSET + std::mem::size_of::<i32>() {
            return None;
        }

        let flag = i32::from_ne_bytes(
            buf[P_FLAG_OFFSET..P_FLAG_OFFSET + 4]
                .try_into()
                .expect("4-byte slice"),
        );
        Some(flag & P_TRANSLATED != 0)
    }
}

#[cfg(test)]
mod tests {
    use super::process_is_translated;

    #[test]
    fn self_is_native_and_never_panics() {
        // The test binary is built for the host (arm64 here), so it is native.
        assert_eq!(process_is_translated(std::process::id()), Some(false));
    }

    #[test]
    fn invalid_pid_returns_none() {
        // u32::MAX is never a live pid; sysctl reports ESRCH / a short read.
        assert_eq!(process_is_translated(u32::MAX), None);
    }

    /// Empirical Rosetta check. Ignored by default because it spawns an
    /// x86_64 process via `arch -x86_64` and therefore needs Rosetta 2
    /// installed. Run with: `cargo test -p tabibu-monitor -- --ignored`.
    #[test]
    #[ignore = "requires Rosetta 2; launches a real x86_64 process"]
    fn x86_64_process_is_detected_as_translated() {
        use std::process::Command;

        // Native self is false.
        assert_eq!(process_is_translated(std::process::id()), Some(false));

        // Launch a translated x86_64 process and probe it. `arch` execs into
        // the target in place, so the spawned pid is the x86_64 process.
        let mut child = Command::new("arch")
            .args(["-x86_64", "/bin/sleep", "120"])
            .spawn()
            .expect("failed to spawn arch -x86_64 sleep");

        // Give the kernel a moment to set up the translated task.
        std::thread::sleep(std::time::Duration::from_millis(400));

        let verdict = process_is_translated(child.id());
        let _ = child.kill();
        let _ = child.wait();

        assert_eq!(
            verdict,
            Some(true),
            "x86_64 process should be detected as translated"
        );
    }
}

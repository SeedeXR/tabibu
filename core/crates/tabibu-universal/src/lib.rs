//! Universal ("fat") binary analysis — READ-ONLY.
//!
//! A universal binary carries machine code for more than one CPU architecture
//! (e.g. `x86_64` + `arm64`). Your Mac only ever runs the slice matching its
//! hardware; the other slice is dead weight on disk. This crate *detects and
//! measures* that reclaimable weight per app bundle. It never modifies a file.
//!
//! Reclaiming the space means `lipo`-thinning the binary in place, which
//! invalidates the app's code signature and can stop signed/notarized apps
//! from launching — so Tabibu reports the opportunity and the signing status
//! and leaves the decision (a manual `lipo`) to the user. Nothing here writes.
//!
//! ## Mach-O fat format (all fields big-endian on disk)
//! `fat_header { magic: u32, nfat_arch: u32 }` then `nfat_arch` × `fat_arch`.
//! `magic` is `0xCAFEBABE` (32-bit offsets) or `0xCAFEBABF` (64-bit offsets).
//! Note `0xCAFEBABE` also begins a Java `.class` file, so every parse is
//! validated (arch count within a sane cap, and every slice's offset+size
//! within the file) before a file is accepted as a Mach-O fat binary.

use std::fs::{self, File};
use std::io::Read;
use std::path::{Path, PathBuf};

use serde::Serialize;
use tabibu_engine::CancelToken;

const FAT_MAGIC: u32 = 0xcafe_babe; // 32-bit fat_arch entries
const FAT_MAGIC_64: u32 = 0xcafe_babf; // 64-bit fat_arch entries
const CPU_TYPE_X86_64: i32 = 0x0100_0007;
const CPU_TYPE_ARM64: i32 = 0x0100_000c;
const CPU_TYPE_X86: i32 = 7;
const CPU_TYPE_ARM: i32 = 12;
/// A genuine fat binary has a handful of slices; anything larger is almost
/// certainly a `0xCAFEBABE` Java class file (its bytes 4..8 are a version).
const MAX_SANE_ARCHS: u32 = 32;

/// One architecture slice within a fat binary.
struct Slice {
    cpu_type: i32,
    size: u64,
}

fn arch_name(cpu_type: i32) -> &'static str {
    match cpu_type {
        CPU_TYPE_X86_64 => "x86_64",
        CPU_TYPE_ARM64 => "arm64",
        CPU_TYPE_X86 => "i386",
        CPU_TYPE_ARM => "arm",
        _ => "other", // arm64_32, ppc, … — still counted, just unnamed
    }
}

/// Parse a file's fat-binary slices, or `None` if it is not a valid Mach-O fat
/// binary (thin Mach-O, non-Mach-O, or a `0xCAFEBABE` false positive such as a
/// Java class file). Reads only the header, never the whole file.
fn read_fat_slices(path: &Path) -> Option<Vec<Slice>> {
    let mut f = File::open(path).ok()?;
    let file_len = f.metadata().ok()?.len();
    // magic(4) + nfat_arch(4); fat_arch_64 is the largest entry at 32 bytes.
    let mut head = [0u8; 8];
    f.read_exact(&mut head).ok()?;
    let magic = u32::from_be_bytes([head[0], head[1], head[2], head[3]]);
    let is_64 = match magic {
        FAT_MAGIC => false,
        FAT_MAGIC_64 => true,
        _ => return None,
    };
    let nfat = u32::from_be_bytes([head[4], head[5], head[6], head[7]]);
    if nfat == 0 || nfat > MAX_SANE_ARCHS {
        return None; // implausible → not a real fat binary (e.g. Java .class)
    }

    let entry_len = if is_64 { 32 } else { 20 };
    let mut buf = vec![0u8; entry_len * nfat as usize];
    f.read_exact(&mut buf).ok()?;

    let mut slices = Vec::with_capacity(nfat as usize);
    for i in 0..nfat as usize {
        let e = &buf[i * entry_len..(i + 1) * entry_len];
        let cpu_type = i32::from_be_bytes([e[0], e[1], e[2], e[3]]);
        let (offset, size) = if is_64 {
            let offset = u64::from_be_bytes(e[8..16].try_into().ok()?);
            let size = u64::from_be_bytes(e[16..24].try_into().ok()?);
            (offset, size)
        } else {
            let offset = u32::from_be_bytes(e[8..12].try_into().ok()?) as u64;
            let size = u32::from_be_bytes(e[12..16].try_into().ok()?) as u64;
            (offset, size)
        };
        // Integrity + false-positive guard: every slice must lie fully within
        // the file. (A Java `.class` is already rejected by the nfat cap above:
        // its bytes 4..8 are a version, major >= 45 > MAX_SANE_ARCHS.) An
        // UNKNOWN cpu type is kept, not rejected — a real universal app that
        // also ships an exotic slice (arm64_32, ppc) must still be detected and
        // have its reclaimable x86_64/arm64 weight counted.
        if offset.checked_add(size)? > file_len {
            return None;
        }
        slices.push(Slice { cpu_type, size });
    }
    Some(slices)
}

/// The CPU type this Mac's hardware runs natively — the slice worth keeping.
fn native_cpu_type() -> i32 {
    if syscall::is_apple_silicon() {
        CPU_TYPE_ARM64
    } else {
        CPU_TYPE_X86_64
    }
}

/// The one place this crate uses `unsafe`: a single sysctl read. The crate
/// otherwise denies `unsafe_code` (syscall wrappers are the allowed exception,
/// mirroring `tabibu-monitor::rosetta`).
mod syscall {
    #![allow(unsafe_code)]

    /// `true` on Apple Silicon. `hw.optional.arm64` == 1 reflects the hardware
    /// even under a Rosetta-translated caller (unlike `env::consts::ARCH`).
    #[must_use]
    pub fn is_apple_silicon() -> bool {
        let mut val: i32 = 0;
        let mut len = std::mem::size_of::<i32>();
        let name = c"hw.optional.arm64";
        // SAFETY: standard sysctlbyname read of a single i32; `name` is a valid
        // C string and the value pointer/length match `val`.
        let rc = unsafe {
            libc::sysctlbyname(
                name.as_ptr(),
                std::ptr::from_mut(&mut val).cast(),
                &mut len,
                std::ptr::null_mut(),
                0,
            )
        };
        rc == 0 && val == 1
    }
}

/// Coarse signing status, so the user knows how risky a manual strip would be.
/// Best-effort: anything we cannot classify with confidence reports
/// `"unknown"`, which the UI treats as risky — we NEVER guess `"unsigned"`
/// (which the UI paints green/"strippable"), since misreporting a signed app
/// as unsigned would encourage a strip that breaks it.
fn signing_status(app: &Path) -> &'static str {
    let out = std::process::Command::new("/usr/bin/codesign")
        .args(["-dv", "--verbose=2"])
        .arg(app)
        .output();
    let Ok(out) = out else { return "unknown" };
    // codesign writes its detail to stderr.
    let text = String::from_utf8_lossy(&out.stderr);
    if text.contains("flags=0x2(adhoc)") || text.contains("Signature=adhoc") {
        return "ad-hoc";
    }
    if text.contains("Authority=Developer ID Application") {
        return "developer-id";
    }
    if text.contains("Authority=Apple") {
        return "apple";
    }
    if text.contains("Authority=") {
        return "signed";
    }
    // Only codesign's explicit "not signed at all" message is trusted as
    // unsigned; every other unclassified outcome stays "unknown" (risky).
    if text.contains("code object is not signed at all") {
        return "unsigned";
    }
    "unknown"
}

/// How safe it is to thin this app's non-native slice, derived from its signing
/// status. This is the axis the UI filters on — "show me only what I can strip
/// without breaking the app."
///
/// - `safe`  — `ad-hoc`/`unsigned`: no Developer-ID/notarization seal to void,
///   and `lipo`-thinning keeps the per-slice (ad-hoc) signature the loader
///   checks, so the app still launches.
/// - `risky` — `developer-id`/`apple`/`signed`: thinning changes the executable
///   bytes and voids the notarized/Team-signed seal; the app can refuse to
///   launch or fail Gatekeeper. Also covers Apple/SIP-protected apps you can't
///   modify at all.
/// - `unknown` — signing couldn't be classified with confidence; treated as
///   risky (never guessed safe).
#[must_use]
pub fn strip_safety(signing: &str) -> &'static str {
    match signing {
        "ad-hoc" | "unsigned" => "safe",
        "developer-id" | "apple" | "signed" => "risky",
        _ => "unknown",
    }
}

/// One app bundle carrying a reclaimable non-native slice.
#[derive(Debug, Serialize)]
pub struct UniversalApp {
    pub name: String,
    pub path: PathBuf,
    /// Architectures present across the bundle's fat binaries, e.g.
    /// `["arm64", "x86_64"]`.
    pub arches: Vec<String>,
    /// Bytes occupied by non-native slices — reclaimable by thinning.
    pub reclaimable_bytes: u64,
    /// Number of fat Mach-O files in the bundle (executable, frameworks, …).
    pub fat_file_count: u32,
    /// `"developer-id" | "apple" | "signed" | "ad-hoc" | "unsigned" | "unknown"`.
    /// Anything other than `ad-hoc`/`unsigned` means stripping likely breaks it.
    pub signing: String,
    /// Safety bucket for filtering: `"safe" | "risky" | "unknown"`. See
    /// [`strip_safety`].
    pub category: String,
}

#[derive(Debug, Serialize)]
pub struct UniversalReport {
    /// The slice this Mac keeps, e.g. `"arm64"`.
    pub native_arch: String,
    pub total_reclaimable_bytes: u64,
    /// Bytes reclaimable from `safe` apps only — the headline "you can free this
    /// without breaking anything" figure the UI leads with.
    pub safe_reclaimable_bytes: u64,
    /// Count of `safe`-category apps (for the filter chip badge).
    pub safe_app_count: u32,
    /// Apps with a reclaimable slice, largest first.
    pub apps: Vec<UniversalApp>,
}

/// Walk `.app` bundles under each of `roots`, measuring the reclaimable
/// (non-native) slice bytes in every fat Mach-O file. Read-only.
///
/// # Errors
/// Never returns an error type; unreadable files/dirs are skipped. Cancellation
/// via `cancel` stops the walk between apps and returns what was found so far.
#[must_use]
pub fn scan(roots: &[PathBuf], cancel: &CancelToken) -> UniversalReport {
    let native = native_cpu_type();
    let mut apps: Vec<UniversalApp> = Vec::new();

    for app in discover_app_bundles(roots) {
        if cancel.is_cancelled() {
            break;
        }
        let mut reclaimable = 0u64;
        let mut fat_files = 0u32;
        let mut arches: Vec<i32> = Vec::new();

        walk_files(&app, cancel, &mut |file| {
            if let Some(slices) = read_fat_slices(file) {
                // Only universal binaries that actually contain this Mac's
                // native slice contribute reclaimable bytes: if the native
                // slice were absent you would NEED the others.
                if slices.iter().any(|s| s.cpu_type == native) {
                    fat_files += 1;
                    for s in &slices {
                        if !arches.contains(&s.cpu_type) {
                            arches.push(s.cpu_type);
                        }
                        if s.cpu_type != native {
                            reclaimable += s.size;
                        }
                    }
                }
            }
        });

        // `reclaimable > 0` already implies the native slice was seen (it only
        // increments inside the `any(native)` branch, which also pushes native
        // into `arches`), so no separate has-native guard is needed.
        if reclaimable > 0 {
            // native slice first, then the rest — stable, readable display.
            arches.sort_by_key(|&c| (c != native, arch_name(c)));
            let signing = signing_status(&app).to_owned();
            let category = strip_safety(&signing).to_owned();
            apps.push(UniversalApp {
                name: app
                    .file_name()
                    .map(|n| n.to_string_lossy().trim_end_matches(".app").to_owned())
                    .unwrap_or_default(),
                arches: arches.iter().map(|&c| arch_name(c).to_owned()).collect(),
                reclaimable_bytes: reclaimable,
                fat_file_count: fat_files,
                signing,
                category,
                path: app,
            });
        }
    }

    apps.sort_by_key(|a| std::cmp::Reverse(a.reclaimable_bytes));
    let safe: Vec<&UniversalApp> = apps.iter().filter(|a| a.category == "safe").collect();
    UniversalReport {
        native_arch: arch_name(native).to_owned(),
        total_reclaimable_bytes: apps.iter().map(|a| a.reclaimable_bytes).sum(),
        safe_reclaimable_bytes: safe.iter().map(|a| a.reclaimable_bytes).sum(),
        safe_app_count: safe.len() as u32,
        apps,
    }
}

/// Top-level `.app` bundles under each root (plus `/Applications/Utilities`).
fn discover_app_bundles(roots: &[PathBuf]) -> Vec<PathBuf> {
    let mut out = Vec::new();
    for root in roots {
        let Ok(entries) = fs::read_dir(root) else {
            continue;
        };
        for entry in entries.flatten() {
            let p = entry.path();
            if p.extension().is_some_and(|e| e == "app") {
                out.push(p);
            } else if entry.file_type().is_ok_and(|t| t.is_dir()) {
                // One level down (e.g. /Applications/Utilities/*.app).
                if let Ok(sub) = fs::read_dir(&p) {
                    for s in sub.flatten() {
                        let sp = s.path();
                        if sp.extension().is_some_and(|e| e == "app") {
                            out.push(sp);
                        }
                    }
                }
            }
        }
    }
    out
}

/// Recursively invoke `visit` on every regular file under `dir`, skipping
/// symlinks (never followed) and unreadable entries. Cancellation-aware.
fn walk_files(dir: &Path, cancel: &CancelToken, visit: &mut dyn FnMut(&Path)) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        if cancel.is_cancelled() {
            return;
        }
        let Ok(ft) = entry.file_type() else { continue };
        if ft.is_symlink() {
            continue;
        }
        let p = entry.path();
        if ft.is_dir() {
            walk_files(&p, cancel, visit);
        } else if ft.is_file() {
            visit(&p);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Write;

    /// Write a minimal 32-bit fat binary with the given `(cpu_type, size)`
    /// slices. Slice payloads are zero-filled so offset+size stay within file.
    fn write_fat(path: &Path, is_64: bool, slices: &[(i32, u64)]) {
        let magic = if is_64 { FAT_MAGIC_64 } else { FAT_MAGIC };
        let entry_len = if is_64 { 32 } else { 20 };
        let header_len = 8 + entry_len * slices.len();
        let mut buf = Vec::new();
        buf.extend_from_slice(&magic.to_be_bytes());
        buf.extend_from_slice(&(slices.len() as u32).to_be_bytes());
        let mut offset = header_len as u64;
        for (cpu, size) in slices {
            buf.extend_from_slice(&cpu.to_be_bytes()); // cputype
            buf.extend_from_slice(&0i32.to_be_bytes()); // cpusubtype
            if is_64 {
                buf.extend_from_slice(&offset.to_be_bytes());
                buf.extend_from_slice(&size.to_be_bytes());
                buf.extend_from_slice(&0u32.to_be_bytes()); // align
                buf.extend_from_slice(&0u32.to_be_bytes()); // reserved
            } else {
                buf.extend_from_slice(&(offset as u32).to_be_bytes());
                buf.extend_from_slice(&(*size as u32).to_be_bytes());
                buf.extend_from_slice(&0u32.to_be_bytes()); // align
            }
            offset += size;
        }
        // Zero-fill the declared slice payloads so offset+size ≤ file length.
        buf.resize(offset as usize, 0);
        let mut f = File::create(path).unwrap();
        f.write_all(&buf).unwrap();
    }

    #[test]
    fn detects_slices_and_sizes_32_and_64() {
        for is_64 in [false, true] {
            let dir = tempfile::tempdir().unwrap();
            let p = dir.path().join("bin");
            write_fat(&p, is_64, &[(CPU_TYPE_ARM64, 100), (CPU_TYPE_X86_64, 200)]);
            let slices = read_fat_slices(&p).unwrap();
            assert_eq!(slices.len(), 2);
            assert_eq!(slices[0].cpu_type, CPU_TYPE_ARM64);
            assert_eq!(slices[0].size, 100);
            assert_eq!(slices[1].size, 200);
        }
    }

    #[test]
    fn rejects_java_class_false_positive() {
        // A real Java `.class` is `0xCAFEBABE` then minor:u16, major:u16. major
        // is >= 45 (Java 1.1), so the u32 at bytes 4..8 is >= 45 > MAX_SANE_ARCHS
        // — the nfat cap rejects it. Use major=52 (Java 8).
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("Foo.class");
        let mut f = File::create(&p).unwrap();
        f.write_all(&FAT_MAGIC.to_be_bytes()).unwrap(); // CAFEBABE
        f.write_all(&0u16.to_be_bytes()).unwrap(); // minor = 0
        f.write_all(&52u16.to_be_bytes()).unwrap(); // major = 52 → nfat = 52 > 32
        f.write_all(&[0u8; 200]).unwrap(); // class body
        assert!(
            read_fat_slices(&p).is_none(),
            "Java class (nfat=52) must be rejected by the arch-count cap"
        );
    }

    #[test]
    fn keeps_unknown_slice_but_counts_it() {
        // A real universal app may also ship an exotic slice (arm64_32, ppc).
        // It must still be detected, the unknown slice named "other", and its
        // bytes counted as non-native (you don't run it on this Mac).
        let dir = tempfile::tempdir().unwrap();
        let p = dir.path().join("bin");
        let unknown: i32 = 0x0100_0100; // not x86_64/arm64/i386/arm
        write_fat(
            &p,
            true,
            &[(CPU_TYPE_ARM64, 100), (CPU_TYPE_X86_64, 200), (unknown, 40)],
        );
        let slices = read_fat_slices(&p).unwrap();
        assert_eq!(slices.len(), 3, "unknown slice is kept, not rejected");
        assert_eq!(arch_name(unknown), "other");
    }

    #[test]
    fn thin_binary_and_nonmacho_are_none() {
        let dir = tempfile::tempdir().unwrap();
        let thin = dir.path().join("thin");
        // Thin Mach-O 64 magic (0xFEEDFACF) — not a fat header.
        File::create(&thin)
            .unwrap()
            .write_all(&0xfeed_facfu32.to_be_bytes())
            .unwrap();
        assert!(read_fat_slices(&thin).is_none());

        let txt = dir.path().join("readme.txt");
        File::create(&txt).unwrap().write_all(b"hello").unwrap();
        assert!(read_fat_slices(&txt).is_none());
    }

    #[test]
    fn scan_reports_reclaimable_non_native_slice() {
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Demo.app/Contents/MacOS");
        fs::create_dir_all(&app).unwrap();
        // A universal binary: arm64 (100) + x86_64 (250).
        write_fat(
            &app.join("Demo"),
            false,
            &[(CPU_TYPE_ARM64, 100), (CPU_TYPE_X86_64, 250)],
        );

        let cancel = CancelToken::new();
        let report = scan(&[dir.path().to_path_buf()], &cancel);
        assert_eq!(report.apps.len(), 1);
        let a = &report.apps[0];
        assert_eq!(a.name, "Demo");
        assert!(a.arches.contains(&"arm64".to_string()));
        assert!(a.arches.contains(&"x86_64".to_string()));
        // Reclaimable is whichever slice is NOT this Mac's native arch.
        let expected = if native_cpu_type() == CPU_TYPE_ARM64 {
            250
        } else {
            100
        };
        assert_eq!(a.reclaimable_bytes, expected);
        assert_eq!(report.total_reclaimable_bytes, expected);
    }

    #[test]
    fn strip_safety_buckets_signing() {
        // Only ad-hoc/unsigned are safe to thin; every signed/notarized state is
        // risky; anything unclassifiable is unknown (never guessed safe).
        assert_eq!(strip_safety("ad-hoc"), "safe");
        assert_eq!(strip_safety("unsigned"), "safe");
        assert_eq!(strip_safety("developer-id"), "risky");
        assert_eq!(strip_safety("apple"), "risky");
        assert_eq!(strip_safety("signed"), "risky");
        assert_eq!(strip_safety("unknown"), "unknown");
        assert_eq!(strip_safety("anything-else"), "unknown");
    }

    #[test]
    fn scan_tags_category_and_safe_totals() {
        // An ad-hoc-signed app (the temp binary carries no real signature, so
        // codesign reports it as unsigned → "safe"). Its reclaimable bytes must
        // flow into both the total and the safe-only headline.
        let dir = tempfile::tempdir().unwrap();
        let app = dir.path().join("Demo.app/Contents/MacOS");
        fs::create_dir_all(&app).unwrap();
        write_fat(
            &app.join("Demo"),
            false,
            &[(CPU_TYPE_ARM64, 100), (CPU_TYPE_X86_64, 250)],
        );
        let report = scan(&[dir.path().to_path_buf()], &CancelToken::new());
        assert_eq!(report.apps.len(), 1);
        let a = &report.apps[0];
        // category is always one of the three buckets and matches its signing.
        assert_eq!(a.category, strip_safety(&a.signing));
        assert!(["safe", "risky", "unknown"].contains(&a.category.as_str()));
        // safe totals are consistent with the per-app category.
        if a.category == "safe" {
            assert_eq!(report.safe_reclaimable_bytes, a.reclaimable_bytes);
            assert_eq!(report.safe_app_count, 1);
        } else {
            assert_eq!(report.safe_reclaimable_bytes, 0);
            assert_eq!(report.safe_app_count, 0);
        }
        // safe bytes never exceed the total.
        assert!(report.safe_reclaimable_bytes <= report.total_reclaimable_bytes);
    }
}

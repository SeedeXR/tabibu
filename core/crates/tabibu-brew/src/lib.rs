//! tabibu-brew — analyze and safely clean up Homebrew (terminal-installed
//! software) on macOS.
//!
//! # Safety model
//! This crate **never deletes Homebrew-managed files itself**. Every
//! destructive operation is delegated to the `brew` binary, whose own rules are
//! the safety net:
//! * `brew cleanup` removes only *old* versions and *stale* download cache —
//!   the currently-installed version of every package is left untouched.
//! * `brew autoremove` removes only formulae that were pulled in as
//!   dependencies and that nothing installed still needs.
//! * `brew uninstall <name>` (invoked **without** `--force` /
//!   `--ignore-dependencies`) refuses when another installed formula depends on
//!   the target, so a single uninstall can never break a dependency graph.
//!
//! All parsing is done by pure functions (unit-tested without a `brew`
//! install); the [`Brew`] type is the only thing that shells out, and it always
//! runs `brew` by absolute path with auto-update/analytics disabled.

use std::path::{Path, PathBuf};
use std::process::Command;

use rayon::prelude::*;
use serde::Serialize;

/// Well-known absolute locations of the `brew` binary (Apple Silicon, then
/// Intel). A GUI app launched from Finder does not inherit the shell `PATH`, so
/// we never rely on `PATH` to find `brew`.
const BREW_PATHS: &[&str] = &["/opt/homebrew/bin/brew", "/usr/local/bin/brew"];

/// Whether Homebrew is present and, if so, where/which version.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    pub installed: bool,
    pub prefix: Option<String>,
    pub version: Option<String>,
}

/// A Homebrew package is either a CLI formula or a GUI/binary cask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "lowercase")]
pub enum PackageKind {
    Formula,
    Cask,
}

/// One installed package, with the honest signals Homebrew actually records:
/// install time and dependency status (Homebrew does **not** track last-used
/// time, so we never claim to).
#[derive(Debug, Clone, Serialize)]
pub struct Package {
    pub name: String,
    pub kind: PackageKind,
    pub version: String,
    /// On-disk size of the package's Cellar/Caskroom directory.
    pub size_bytes: u64,
    /// Install time (unix seconds), when Homebrew recorded one.
    pub installed_unix: Option<i64>,
    /// The user explicitly asked for this (vs. it arriving as a dependency).
    pub on_request: bool,
    /// Homebrew installed this as a dependency of something else.
    pub as_dependency: bool,
    /// `brew autoremove` would remove this (orphaned dependency).
    pub autoremovable: bool,
}

/// Preview of what `brew cleanup` would remove.
#[derive(Debug, Clone, Default, Serialize)]
pub struct CleanupPreview {
    /// Homebrew's own "would free approximately" estimate, in bytes.
    pub freeable_bytes: u64,
}

/// The full analysis returned to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub status: Status,
    pub cleanup: CleanupPreview,
    /// Names of orphaned dependencies `brew autoremove` would remove.
    pub autoremovable: Vec<String>,
    pub packages: Vec<Package>,
}

/// Result of a destructive `brew` action.
#[derive(Debug, Clone, Serialize)]
pub struct ActionOutcome {
    pub ok: bool,
    pub freed_bytes: u64,
    /// Trimmed `brew` output (shown verbatim — honest about what happened).
    pub message: String,
}

/// Handle to a located Homebrew install.
pub struct Brew {
    bin: PathBuf,
}

impl Brew {
    /// Locate Homebrew (first existing well-known binary), or `None`.
    #[must_use]
    pub fn detect() -> Option<Self> {
        BREW_PATHS
            .iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
            .map(|bin| Self { bin })
    }

    /// Build a `brew` command with a sane `PATH` (GUI apps lack one) and
    /// auto-update/analytics disabled so analysis never mutates state or hits
    /// the network unexpectedly.
    fn command(&self, args: &[&str]) -> Command {
        // Fail closed: only prepend brew's own bin dir when we actually have one
        // (a `None` parent would otherwise yield a leading ':' in PATH, which
        // Unix treats as the CWD — letting a stray binary there shadow git/curl).
        const BASE_PATH: &str = "/opt/homebrew/bin:/usr/local/bin:/usr/bin:/bin:/usr/sbin:/sbin";
        let path = match self.bin.parent() {
            Some(dir) => format!("{}:{BASE_PATH}", dir.display()),
            None => BASE_PATH.to_string(),
        };
        let mut c = Command::new(&self.bin);
        c.args(args)
            .env("PATH", path)
            .env("HOMEBREW_NO_AUTO_UPDATE", "1")
            .env("HOMEBREW_NO_ANALYTICS", "1")
            .env("HOMEBREW_NO_ENV_HINTS", "1")
            .env("HOMEBREW_NO_COLOR", "1");
        c
    }

    /// Run `brew`, returning stdout only on success (used for machine-readable
    /// output like `--json`).
    fn stdout(&self, args: &[&str]) -> Option<String> {
        let out = self.command(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Run `brew`, returning `(success, stdout+stderr)`. Dry-run previews and
    /// destructive actions emit human text across both streams, so we parse the
    /// merged output for specific line patterns.
    fn combined(&self, args: &[&str]) -> Option<(bool, String)> {
        let out = self.command(args).output().ok()?;
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        Some((out.status.success(), s))
    }

    /// Homebrew prefix, derived from the binary location WITHOUT spawning
    /// `brew --prefix`: the canonical brew binary lives at `<prefix>/bin/brew`,
    /// so the prefix is its grandparent (`/opt/homebrew/bin/brew` →
    /// `/opt/homebrew`). Saves a (Ruby-startup-heavy) subprocess per analysis.
    #[must_use]
    pub fn prefix(&self) -> Option<String> {
        self.bin.parent()?.parent().map(|p| p.display().to_string())
    }

    /// The first line of `brew --version` (cosmetic; shown in the header).
    fn version(&self) -> Option<String> {
        self.stdout(&["--version"])
            .and_then(|s| s.lines().next().map(|l| l.trim().to_string()))
    }

    /// Installed/prefix/version snapshot.
    #[must_use]
    pub fn status(&self) -> Status {
        Status {
            installed: true,
            prefix: self.prefix(),
            version: self.version(),
        }
    }

    /// What `brew cleanup` would remove (old versions + stale cache). Dry run —
    /// nothing is touched.
    #[must_use]
    pub fn cleanup_preview(&self) -> CleanupPreview {
        self.combined(&["cleanup", "-n"])
            .map(|(_, text)| parse_cleanup(&text))
            .unwrap_or_default()
    }

    /// Orphaned dependencies `brew autoremove` would remove. Dry run.
    #[must_use]
    pub fn autoremovable(&self) -> Vec<String> {
        self.combined(&["autoremove", "-n"])
            .map(|(_, text)| parse_autoremove(&text))
            .unwrap_or_default()
    }

    /// Run the full analysis (status + cleanup preview + autoremove + packages).
    /// Read-only: nothing is removed.
    #[must_use]
    pub fn analyze(&self) -> Report {
        // Prefix/Cellar/Caskroom are derived from the binary path — no subprocess.
        let prefix = self.prefix();
        // `cleanup -n` and `autoremove -n` are each ~1.5s from a cold process
        // (Ruby startup + dependency-graph / cache work), so run every
        // independent piece concurrently — the three read-only/dry-run brew calls
        // plus the on-disk package read + sizing. Measured ~2.5s vs ~3.3s serial.
        // The brew calls don't mutate the prefix (auto-update off), so overlapping
        // them is safe; only the serial destructive actions need ordering.
        let (version, cleanup, autoremovable, mut packages) = std::thread::scope(|scope| {
            let ver = scope.spawn(|| self.version());
            let cln = scope.spawn(|| self.cleanup_preview());
            let arm = scope.spawn(|| self.autoremovable());
            let pkg = scope.spawn(|| {
                prefix
                    .as_deref()
                    .map(packages_from_disk)
                    .unwrap_or_default()
            });
            (
                ver.join().unwrap_or(None),
                cln.join().unwrap_or_default(),
                arm.join().unwrap_or_default(),
                pkg.join().unwrap_or_default(),
            )
        });
        if packages.is_empty() {
            // On-disk layout unreadable → fall back to the authoritative query.
            let json = self.stdout(&["info", "--json=v2", "--installed"]);
            packages = packages_from_json(json.as_deref(), &autoremovable, prefix.as_deref());
        } else {
            apply_autoremovable(&mut packages, &autoremovable);
        }
        Report {
            status: Status {
                installed: true,
                prefix,
                version,
            },
            cleanup,
            autoremovable,
            packages,
        }
    }

    /// Run `brew cleanup` for real (old versions + stale cache only).
    #[must_use]
    pub fn run_cleanup(&self) -> ActionOutcome {
        self.action(&["cleanup"], "brew cleanup")
    }

    /// Run `brew autoremove` for real (orphaned dependencies only).
    #[must_use]
    pub fn run_autoremove(&self) -> ActionOutcome {
        self.action(&["autoremove"], "brew autoremove")
    }

    /// Uninstall one package by name. **Never forces** — `brew uninstall`
    /// refuses if another installed formula depends on it, surfaced as an
    /// error. The name is validated to a strict token set so it can never be a
    /// flag (e.g. `--ignore-dependencies`) or shell/path injection.
    #[must_use]
    pub fn uninstall(&self, name: &str, cask: bool) -> ActionOutcome {
        if !is_valid_token(name) {
            return ActionOutcome {
                ok: false,
                freed_bytes: 0,
                message: format!("refusing suspicious package name: {name:?}"),
            };
        }
        let args = uninstall_args(name, cask);
        let arg_refs: Vec<&str> = args.iter().map(String::as_str).collect();
        self.action(&arg_refs, "brew uninstall")
    }

    fn action(&self, args: &[&str], label: &str) -> ActionOutcome {
        match self.combined(args) {
            Some((ok, text)) => ActionOutcome {
                ok,
                freed_bytes: parse_freed(&text),
                message: summarize(&text),
            },
            None => ActionOutcome {
                ok: false,
                freed_bytes: 0,
                message: format!("could not run {label}"),
            },
        }
    }
}

// ---------------------------------------------------------------------------
// Pure parsers (unit-tested without a brew install)
// ---------------------------------------------------------------------------

/// Reclaimable-bytes estimate from `brew cleanup -n`'s "...free approximately
/// <SIZE>..." summary line (the only consumer reads just this total).
#[must_use]
pub fn parse_cleanup(text: &str) -> CleanupPreview {
    CleanupPreview {
        freeable_bytes: parse_freed(text),
    }
}

/// Parse `brew autoremove -n` output into formula names. Only bare package
/// tokens pass — headers, warnings, and prose are ignored. The list is
/// informational; the real removal is still delegated to `brew autoremove`.
#[must_use]
pub fn parse_autoremove(text: &str) -> Vec<String> {
    text.lines()
        .map(str::trim)
        .filter(|l| is_valid_token(l)) // a bare package token, never a header/prose line
        .map(String::from)
        .collect()
}

/// Read installed packages straight from Homebrew's on-disk layout — far faster
/// than `brew info --json` (no Ruby startup, no metadata loading). Formulae come
/// from `<prefix>/Cellar/<name>/<version>/INSTALL_RECEIPT.json`; casks from
/// `<prefix>/Caskroom/<token>/<version>/`. Sizing runs in parallel. The
/// `autoremovable` flag is applied separately by the caller. Returns empty if
/// the prefix has no Cellar (caller then falls back to `brew info`).
#[must_use]
pub fn packages_from_disk(prefix: &str) -> Vec<Package> {
    let prefix = Path::new(prefix);
    let mut pkgs = read_cellar(&prefix.join("Cellar"));
    pkgs.extend(read_caskroom(&prefix.join("Caskroom")));
    pkgs
}

/// Direct child directories of `dir`, excluding dotfiles (e.g. `.metadata`).
fn child_dirs(dir: &Path) -> Vec<PathBuf> {
    std::fs::read_dir(dir)
        .map(|rd| {
            rd.flatten()
                .filter(|e| !e.file_name().to_string_lossy().starts_with('.'))
                .map(|e| e.path())
                .filter(|p| p.is_dir())
                .collect()
        })
        .unwrap_or_default()
}

/// The most-recently-modified version sub-directory (the live install).
fn latest_version_dir(pkg_dir: &Path) -> Option<PathBuf> {
    child_dirs(pkg_dir)
        .into_iter()
        .max_by_key(|p| std::fs::metadata(p).and_then(|m| m.modified()).ok())
}

fn dir_mtime_unix(dir: &Path) -> Option<i64> {
    let modified = std::fs::metadata(dir).and_then(|m| m.modified()).ok()?;
    modified
        .duration_since(std::time::UNIX_EPOCH)
        .ok()
        .and_then(|d| i64::try_from(d.as_secs()).ok())
}

fn read_cellar(cellar: &Path) -> Vec<Package> {
    child_dirs(cellar)
        .par_iter()
        .filter_map(|dir| {
            let name = dir.file_name()?.to_string_lossy().into_owned();
            let ver_dir = latest_version_dir(dir)?;
            let version = ver_dir.file_name()?.to_string_lossy().into_owned();
            let (on_request, as_dependency, time) =
                parse_receipt(&ver_dir.join("INSTALL_RECEIPT.json"));
            Some(Package {
                name,
                kind: PackageKind::Formula,
                version,
                size_bytes: dir_size(dir),
                installed_unix: time,
                on_request,
                as_dependency,
                autoremovable: false,
            })
        })
        .collect()
}

fn read_caskroom(caskroom: &Path) -> Vec<Package> {
    child_dirs(caskroom)
        .par_iter()
        .filter_map(|dir| {
            let token = dir.file_name()?.to_string_lossy().into_owned();
            let ver_dir = latest_version_dir(dir)?;
            let version = ver_dir.file_name()?.to_string_lossy().into_owned();
            Some(Package {
                name: token,
                kind: PackageKind::Cask,
                version,
                size_bytes: dir_size(dir),
                // Casks have no install receipt; the version dir's mtime is the
                // best available install-time signal, and casks are always
                // user-requested (never a dependency).
                installed_unix: dir_mtime_unix(&ver_dir),
                on_request: true,
                as_dependency: false,
                autoremovable: false,
            })
        })
        .collect()
}

/// Parse an `INSTALL_RECEIPT.json` → `(on_request, as_dependency, time)`.
/// Missing/unreadable receipts degrade to `(false, false, None)`.
fn parse_receipt(path: &Path) -> (bool, bool, Option<i64>) {
    let Ok(txt) = std::fs::read_to_string(path) else {
        return (false, false, None);
    };
    let Ok(v) = serde_json::from_str::<serde_json::Value>(&txt) else {
        return (false, false, None);
    };
    (
        v.get("installed_on_request")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        v.get("installed_as_dependency")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false),
        v.get("time").and_then(serde_json::Value::as_i64),
    )
}

/// Set each package's `autoremovable` flag from the names brew reported.
fn apply_autoremovable(pkgs: &mut [Package], autoremovable: &[String]) {
    let auto: std::collections::HashSet<&str> = autoremovable.iter().map(String::as_str).collect();
    for p in pkgs.iter_mut() {
        p.autoremovable = auto.contains(p.name.as_str());
    }
}

/// Parse the installed-packages JSON and size each on disk. `prefix` locates
/// the Cellar (`<prefix>/Cellar/<name>`) and Caskroom (`<prefix>/Caskroom/
/// <token>`), both derived (no `brew --cellar` subprocess). Sizing runs in
/// parallel across packages.
#[must_use]
pub fn packages_from_json(
    json: Option<&str>,
    autoremovable: &[String],
    prefix: Option<&str>,
) -> Vec<Package> {
    let Some(json) = json else {
        return Vec::new();
    };
    let cellar = prefix.map(|p| PathBuf::from(p).join("Cellar"));
    let caskroom = prefix.map(|p| PathBuf::from(p).join("Caskroom"));
    let auto: std::collections::HashSet<&str> = autoremovable.iter().map(String::as_str).collect();

    let mut pkgs = parse_installed_json(json);
    pkgs.par_iter_mut().for_each(|p| {
        let dir = match p.kind {
            PackageKind::Formula => cellar.as_ref().map(|c| c.join(&p.name)),
            PackageKind::Cask => caskroom.as_ref().map(|c| c.join(&p.name)),
        };
        p.size_bytes = dir.map_or(0, |d| dir_size(&d));
        p.autoremovable = auto.contains(p.name.as_str());
    });
    pkgs
}

/// Parse `brew info --json=v2 --installed` into packages (sizes filled later).
#[must_use]
pub fn parse_installed_json(json: &str) -> Vec<Package> {
    let Ok(v) = serde_json::from_str::<serde_json::Value>(json) else {
        return Vec::new();
    };
    let mut out = Vec::new();
    if let Some(arr) = v.get("formulae").and_then(serde_json::Value::as_array) {
        for f in arr {
            let name = f.get("name").and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            // The last `installed` entry is the live install.
            let inst = f
                .get("installed")
                .and_then(serde_json::Value::as_array)
                .and_then(|a| a.last());
            out.push(Package {
                name: name.to_string(),
                kind: PackageKind::Formula,
                version: inst
                    .and_then(|i| i.get("version"))
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                size_bytes: 0,
                installed_unix: inst
                    .and_then(|i| i.get("time"))
                    .and_then(serde_json::Value::as_i64),
                on_request: inst
                    .and_then(|i| i.get("installed_on_request"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                as_dependency: inst
                    .and_then(|i| i.get("installed_as_dependency"))
                    .and_then(serde_json::Value::as_bool)
                    .unwrap_or(false),
                autoremovable: false,
            });
        }
    }
    if let Some(arr) = v.get("casks").and_then(serde_json::Value::as_array) {
        for c in arr {
            let name = c.get("token").and_then(|x| x.as_str()).unwrap_or("");
            if name.is_empty() {
                continue;
            }
            out.push(Package {
                name: name.to_string(),
                kind: PackageKind::Cask,
                version: c
                    .get("installed")
                    .and_then(|x| x.as_str())
                    .unwrap_or("")
                    .to_string(),
                size_bytes: 0,
                installed_unix: c.get("installed_time").and_then(serde_json::Value::as_i64),
                // Casks are always explicitly installed (never a dependency).
                on_request: true,
                as_dependency: false,
                autoremovable: false,
            });
        }
    }
    out
}

/// Parse a Homebrew human size (`6MB`, `714KB`, `1.2GB`, `512B`) into bytes.
/// Homebrew formats with 1024-based units.
#[must_use]
pub fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    let split = s.find(|c: char| c.is_ascii_alphabetic()).unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let val: f64 = num.trim().parse().unwrap_or(0.0);
    // Strip any trailing punctuation the caller didn't (e.g. "MB." / "MB)" /
    // "MB,") so the unit always matches regardless of how the token was split.
    let unit = unit
        .trim()
        .trim_end_matches(|c: char| !c.is_ascii_alphabetic());
    let mult = match unit.to_ascii_uppercase().as_str() {
        "KB" | "K" | "KIB" => 1024.0,
        "MB" | "M" | "MIB" => 1024.0 * 1024.0,
        "GB" | "G" | "GIB" => 1024.0_f64.powi(3),
        "TB" | "T" | "TIB" => 1024.0_f64.powi(4),
        // "B" and unknown/empty units fall through to bytes.
        _ => 1.0,
    };
    (val * mult) as u64
}

/// Build the argument vector for a single uninstall. Kept separate (and pure)
/// so a test can lock the safety invariant: it must never contain a forcing
/// flag (`--force` / `--ignore-dependencies`) — those would bypass Homebrew's
/// own dependency guard and could break other installed software.
#[must_use]
pub fn uninstall_args(name: &str, cask: bool) -> Vec<String> {
    let mut args = vec!["uninstall".to_string()];
    if cask {
        args.push("--cask".to_string());
    }
    args.push(name.to_string());
    args
}

/// Strict package-token validator: prevents a name from ever being a `brew`
/// flag or a shell/path injection when passed to `uninstall`.
#[must_use]
pub fn is_valid_token(name: &str) -> bool {
    !name.is_empty()
        && name.len() <= 128
        && name
            .chars()
            .next()
            .is_some_and(|c| c.is_ascii_alphanumeric())
        && name
            .chars()
            .all(|c| c.is_ascii_alphanumeric() || matches!(c, '@' | '.' | '_' | '+' | '-' | '/'))
}

/// Extract the byte size from any "...approximately <SIZE>..." line.
fn parse_freed(text: &str) -> u64 {
    text.lines()
        .find_map(|l| {
            let rest = l.split("approximately ").nth(1)?;
            let tok = rest.split_whitespace().next()?;
            Some(parse_size(tok))
        })
        .unwrap_or(0)
}

fn summarize(text: &str) -> String {
    let t = text.trim();
    if t.is_empty() {
        "Done.".to_string()
    } else {
        t.to_string()
    }
}

/// Recursive on-disk size; never follows symlinks, unreadable entries count 0.
fn dir_size(path: &Path) -> u64 {
    let Ok(meta) = std::fs::symlink_metadata(path) else {
        return 0;
    };
    if meta.file_type().is_symlink() {
        return 0;
    }
    if meta.is_dir() {
        std::fs::read_dir(path)
            .map(|rd| rd.flatten().map(|e| dir_size(&e.path())).sum())
            .unwrap_or(0)
    } else {
        meta.len()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parses_sizes() {
        assert_eq!(parse_size("512B"), 512);
        assert_eq!(parse_size("6MB"), 6 * 1024 * 1024);
        assert_eq!(parse_size("714KB"), 714 * 1024);
        assert_eq!(parse_size("1.5GB"), (1.5 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert_eq!(parse_size("  2 MB "), 2 * 1024 * 1024);
        assert_eq!(parse_size("garbage"), 0);
        assert_eq!(parse_size("123"), 123);
        // Trailing punctuation on the unit must not collapse it to raw bytes.
        assert_eq!(parse_size("6MB."), 6 * 1024 * 1024);
        assert_eq!(parse_size("4MB)"), 4 * 1024 * 1024);
    }

    #[test]
    fn parses_cleanup_total_from_summary() {
        let text = "\
Warning: Skipping aom: most recent version not installed
Would remove: /Users/x/Library/Caches/Homebrew/bar (3 files, 4MB)
==> This operation would free approximately 6MB of disk space.";
        // The only consumer reads the authoritative summary total.
        assert_eq!(parse_cleanup(text).freeable_bytes, 6 * 1024 * 1024);
        // No summary line → no estimate.
        assert_eq!(parse_cleanup("Would remove: /a/b (1MB)").freeable_bytes, 0);
    }

    #[test]
    fn parses_autoremove_names_ignoring_prose() {
        let text = "\
==> Would remove 3 unneeded formulae:
libfoo
python@3.11
gtk+3
Warning: something
This line has spaces and is ignored";
        let names = parse_autoremove(text);
        assert_eq!(names, vec!["libfoo", "python@3.11", "gtk+3"]);
    }

    #[test]
    fn parses_installed_json_formulae_and_casks() {
        let json = r#"{
          "formulae": [
            {"name":"wget","installed":[{"version":"1.25.0","installed_on_request":true,"installed_as_dependency":false,"time":1780082400}]},
            {"name":"libidn2","installed":[{"version":"2.3.7","installed_on_request":false,"installed_as_dependency":true,"time":1770000000}]},
            {"name":"broken","installed":[]}
          ],
          "casks": [
            {"token":"copilot-cli","installed":"1.0.54","installed_time":1779777815}
          ]
        }"#;
        let pkgs = parse_installed_json(json);
        assert_eq!(pkgs.len(), 4);
        let wget = pkgs.iter().find(|p| p.name == "wget").unwrap();
        assert_eq!(wget.kind, PackageKind::Formula);
        assert!(wget.on_request && !wget.as_dependency);
        assert_eq!(wget.installed_unix, Some(1780082400));
        let dep = pkgs.iter().find(|p| p.name == "libidn2").unwrap();
        assert!(!dep.on_request && dep.as_dependency);
        let cask = pkgs.iter().find(|p| p.name == "copilot-cli").unwrap();
        assert_eq!(cask.kind, PackageKind::Cask);
        assert!(cask.on_request);
        assert_eq!(cask.installed_unix, Some(1779777815));
    }

    #[test]
    fn packages_from_json_applies_autoremovable_and_handles_none() {
        assert!(packages_from_json(None, &[], None).is_empty());
        let json = r#"{"formulae":[
            {"name":"libidn2","installed":[{"version":"2.3.7","installed_on_request":false,"installed_as_dependency":true,"time":1}]},
            {"name":"wget","installed":[{"version":"1.25","installed_on_request":true,"installed_as_dependency":false,"time":2}]}
        ],"casks":[]}"#;
        // prefix None → no real dirs, sizes are 0, but flags still resolve.
        let pkgs = packages_from_json(Some(json), &["libidn2".to_string()], None);
        let libidn2 = pkgs.iter().find(|p| p.name == "libidn2").unwrap();
        let wget = pkgs.iter().find(|p| p.name == "wget").unwrap();
        assert!(libidn2.autoremovable, "named in autoremovable list");
        assert!(!wget.autoremovable);
        assert_eq!(libidn2.size_bytes, 0);
    }

    #[test]
    fn token_validator_blocks_flags_and_injection() {
        assert!(is_valid_token("wget"));
        assert!(is_valid_token("python@3.11"));
        assert!(is_valid_token("gtk+"));
        assert!(!is_valid_token("--ignore-dependencies"));
        assert!(!is_valid_token("-rf"));
        assert!(!is_valid_token("foo; rm -rf /"));
        assert!(!is_valid_token("a b"));
        assert!(!is_valid_token(""));
        assert!(!is_valid_token(&"x".repeat(200)));
    }

    #[test]
    fn uninstall_never_forces() {
        // Locks the core safety invariant: a single uninstall must never carry a
        // flag that bypasses Homebrew's dependency guard.
        for (name, cask) in [("wget", false), ("some-cask", true)] {
            let args = uninstall_args(name, cask);
            assert_eq!(args[0], "uninstall");
            assert!(args.last().is_some_and(|a| a == name));
            for forbidden in ["--force", "-f", "--ignore-dependencies", "--zap"] {
                assert!(
                    !args.iter().any(|a| a == forbidden),
                    "uninstall args must never contain {forbidden}: {args:?}"
                );
            }
            if cask {
                assert!(args.contains(&"--cask".to_string()));
            }
        }
    }

    #[test]
    fn cleanup_summary_matches_loose_wording() {
        // Future/variant phrasing must still yield the authoritative total.
        let text = "Would remove: /a (1MB)\n==> This operation would free up approximately 7MB of disk space.";
        assert_eq!(parse_cleanup(text).freeable_bytes, 7 * 1024 * 1024);
    }

    #[test]
    fn parses_freed_from_real_cleanup_output() {
        let text = "==> This operation has freed approximately 1.2GB of disk space.";
        assert_eq!(parse_freed(text), (1.2 * 1024.0 * 1024.0 * 1024.0) as u64);
        assert_eq!(parse_freed("nothing here"), 0);
    }
}

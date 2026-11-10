//! Docker artifact analysis + safe pruning.
//!
//! Mirrors `tabibu-brew`'s philosophy: Tabibu never removes Docker artifacts
//! itself — it **delegates every removal to the `docker` CLI's own `prune`
//! commands**, which are the canonical, supported way to reclaim space. The
//! read-only analysis comes straight from `docker system df` (the same numbers
//! `docker system df` shows a user in the terminal), so reported reclaimable
//! bytes are measured by Docker, not estimated by us.
//!
//! Space categories (what `docker system df` reports and what we prune):
//!   - Build Cache      → `docker builder prune`   (rebuilt on next build)
//!   - Unused images    → `docker image prune`     (`-a` = all unused, not just dangling)
//!   - Stopped containers → `docker container prune`
//!   - Unused volumes   → `docker volume rm <the ones the user picked>` ⚠ volumes
//!     hold PERSISTENT DATA (databases, etc.), so they are listed one by one
//!     and never bulk-pruned; the UI tiers this as risky and confirms hard.
//!
//! On macOS, Docker keeps everything inside a VM disk image; pruning frees
//! space *inside* the VM (and Docker Desktop reclaims it), which is exactly
//! what `docker system df` measures.

use serde::{Deserialize, Serialize};
use std::path::PathBuf;
use std::process::Command;

/// Well-known `docker` CLI locations (Docker Desktop symlinks into
/// /usr/local/bin; Homebrew's cask into /opt/homebrew/bin).
const DOCKER_PATHS: &[&str] = &[
    "/usr/local/bin/docker",
    "/opt/homebrew/bin/docker",
    "/usr/bin/docker",
];

/// Whether the `docker` CLI is present and its daemon reachable.
#[derive(Debug, Clone, Serialize)]
pub struct Status {
    /// The `docker` binary exists.
    pub installed: bool,
    /// The daemon answered (`docker system df` succeeded). Analysis and prune
    /// need a running daemon; the UI shows a "start Docker" hint otherwise.
    pub running: bool,
    pub version: Option<String>,
}

/// One artifact category from `docker system df`.
#[derive(Debug, Clone, Serialize)]
pub struct Artifact {
    /// Stable id for the UI: `images` | `containers` | `volumes` | `build_cache`.
    pub kind: String,
    pub total_count: u64,
    pub active_count: u64,
    /// Bytes Docker reports as reclaimable for this category.
    pub reclaimable_bytes: u64,
}

/// Full read-only analysis returned to the UI.
#[derive(Debug, Clone, Serialize)]
pub struct Report {
    pub status: Status,
    pub artifacts: Vec<Artifact>,
    pub total_reclaimable_bytes: u64,
}

/// One UNUSED Docker volume — no container references it.
#[derive(Debug, Clone, Serialize, PartialEq, Eq)]
pub struct Volume {
    pub name: String,
    pub size_bytes: u64,
    /// Docker-generated (64-hex name): nobody named it, so it is usually
    /// disposable. A NAMED volume is someone's database / persistent data.
    pub anonymous: bool,
}

/// Result of a `docker … prune` / `docker volume rm` action.
#[derive(Debug, Clone, Serialize)]
pub struct ActionOutcome {
    pub ok: bool,
    /// Bytes Docker reported freed ("Total reclaimed space: …").
    pub freed_bytes: u64,
    /// Trimmed `docker` output (shown verbatim — honest about what happened).
    pub message: String,
}

/// Handle to a located `docker` CLI.
pub struct Docker {
    bin: PathBuf,
}

impl Docker {
    /// Locate the `docker` CLI (first existing well-known path), or `None`.
    #[must_use]
    pub fn detect() -> Option<Self> {
        DOCKER_PATHS
            .iter()
            .map(PathBuf::from)
            .find(|p| p.exists())
            .map(|bin| Self { bin })
    }

    /// Build a `docker` command with a sane `PATH` (GUI apps launched from
    /// Finder inherit almost none). No shell — args are passed directly, so
    /// nothing we pass can be re-parsed by a shell.
    fn command(&self, args: &[&str]) -> Command {
        const BASE_PATH: &str = "/usr/local/bin:/opt/homebrew/bin:/usr/bin:/bin:/usr/sbin:/sbin";
        let path = match self.bin.parent() {
            Some(dir) => format!("{}:{BASE_PATH}", dir.display()),
            None => BASE_PATH.to_string(),
        };
        let mut c = Command::new(&self.bin);
        c.args(args).env("PATH", path);
        c
    }

    /// Run `docker`, returning stdout only on success (machine-readable output).
    fn stdout(&self, args: &[&str]) -> Option<String> {
        let out = self.command(args).output().ok()?;
        out.status
            .success()
            .then(|| String::from_utf8_lossy(&out.stdout).into_owned())
    }

    /// Run `docker`, returning `(success, stdout+stderr)`.
    fn combined(&self, args: &[&str]) -> Option<(bool, String)> {
        let out = self.command(args).output().ok()?;
        let mut s = String::from_utf8_lossy(&out.stdout).into_owned();
        s.push_str(&String::from_utf8_lossy(&out.stderr));
        Some((out.status.success(), s))
    }

    fn version(&self) -> Option<String> {
        self.stdout(&["--version"]).map(|s| s.trim().to_string())
    }

    /// Read-only analysis: `docker system df --format json`. Nothing is pruned.
    /// If the daemon is down the df call fails → `status.running = false`.
    #[must_use]
    pub fn analyze(&self) -> Report {
        let version = self.version();
        match self.stdout(&["system", "df", "--format", "json"]) {
            Some(json) => {
                let artifacts = parse_df(&json);
                let total = artifacts.iter().map(|a| a.reclaimable_bytes).sum();
                Report {
                    status: Status {
                        installed: true,
                        running: true,
                        version,
                    },
                    total_reclaimable_bytes: total,
                    artifacts,
                }
            }
            None => Report {
                status: Status {
                    installed: true,
                    running: false,
                    version,
                },
                artifacts: Vec::new(),
                total_reclaimable_bytes: 0,
            },
        }
    }

    /// `docker builder prune` — unused build cache (rebuilt on next build).
    /// Not `--all` (that also drops in-use cache): the product only ever prunes
    /// the unused portion `docker system df` counts as reclaimable.
    #[must_use]
    pub fn prune_build_cache(&self) -> ActionOutcome {
        self.action(&["builder", "prune", "-f"], "docker builder prune")
    }

    /// `docker image prune -a` — every image not used by a container (the
    /// product's meaning of "unused images"; re-pulled/rebuilt when needed).
    #[must_use]
    pub fn prune_images(&self) -> ActionOutcome {
        self.action(&["image", "prune", "-f", "-a"], "docker image prune")
    }

    /// `docker container prune` — all stopped containers.
    #[must_use]
    pub fn prune_containers(&self) -> ActionOutcome {
        self.action(&["container", "prune", "-f"], "docker container prune")
    }

    /// Read-only: every volume no container references, largest first, from
    /// `docker system df -v` — the same accounting as the "reclaimable" figure
    /// [`Docker::analyze`] reports, so this list adds up to the number shown.
    #[must_use]
    pub fn unused_volumes(&self) -> Vec<Volume> {
        self.stdout(&["system", "df", "-v", "--format", "json"])
            .map(|json| parse_unused_volumes(&json))
            .unwrap_or_default()
    }

    /// `docker volume rm` the requested volumes — but ONLY those unused right
    /// now. The unused set is re-read here, so a stale or hostile request can
    /// never name an in-use volume (Docker also refuses those without `-f`,
    /// which is never passed). Volumes are removed one by one by explicit
    /// choice rather than with a bulk `docker volume prune`: plain `prune` only
    /// takes anonymous volumes (so it freed nothing of what `system df` calls
    /// reclaimable when the unused volumes are named), and `prune -a` would
    /// take every named database at once. ⚠ PERMANENT — nothing goes to the Trash.
    #[must_use]
    pub fn remove_volumes(&self, names: &[String]) -> ActionOutcome {
        // ponytail: the re-check re-reads `system df -v` (one more disk-usage
        // pass, ~0.2 s on a typical install) because it yields names AND sizes.
        // If a huge install makes that slow, validate with the cheap
        // `docker volume ls -q -f dangling=true` and take sizes from the caller.
        let targets: Vec<Volume> = self
            .unused_volumes()
            .into_iter()
            .filter(|v| names.contains(&v.name))
            .collect();
        if targets.is_empty() {
            return ActionOutcome {
                ok: false,
                freed_bytes: 0,
                message: "None of the selected volumes is unused any more — nothing removed."
                    .to_string(),
            };
        }
        let mut args = vec!["volume", "rm"];
        args.extend(targets.iter().map(|v| v.name.as_str()));
        match self.combined(&args) {
            Some((ok, text)) => ActionOutcome {
                ok,
                freed_bytes: freed_by_removal(&targets, &text),
                message: summarize(&text),
            },
            None => ActionOutcome {
                ok: false,
                freed_bytes: 0,
                message: "could not run docker volume rm".to_string(),
            },
        }
    }

    fn action(&self, args: &[&str], label: &str) -> ActionOutcome {
        match self.combined(args) {
            Some((ok, text)) => ActionOutcome {
                ok,
                freed_bytes: parse_reclaimed(&text),
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
// Pure parsers (unit-tested without a docker install)
// ---------------------------------------------------------------------------

/// One record of `docker system df --format json`.
#[derive(Deserialize)]
struct DfLine {
    #[serde(rename = "Type")]
    type_: String,
    #[serde(rename = "TotalCount", default)]
    total: String,
    #[serde(rename = "Active", default)]
    active: String,
    #[serde(rename = "Reclaimable", default)]
    reclaimable: String,
}

/// Parse `docker system df --format json`. Current Docker emits one JSON object
/// per line (NDJSON); to survive a format change (some `--format json` paths
/// emit a single JSON array), try the whole blob as an array first, then fall
/// back to NDJSON. Without this a variant format would deserialize to nothing
/// and silently show "nothing to reclaim" despite reclaimable space.
#[must_use]
pub fn parse_df(json: &str) -> Vec<Artifact> {
    let records: Vec<DfLine> =
        serde_json::from_str::<Vec<DfLine>>(json.trim()).unwrap_or_else(|_| {
            json.lines()
                .filter(|l| !l.trim().is_empty())
                .filter_map(|line| serde_json::from_str::<DfLine>(line).ok())
                .collect()
        });
    records
        .into_iter()
        .filter_map(|d| {
            let kind = match d.type_.as_str() {
                "Images" => "images",
                "Containers" => "containers",
                "Local Volumes" => "volumes",
                "Build Cache" => "build_cache",
                _ => return None,
            };
            Some(Artifact {
                kind: kind.to_string(),
                total_count: d.total.trim().parse().unwrap_or(0),
                active_count: d.active.trim().parse().unwrap_or(0),
                reclaimable_bytes: parse_size(&d.reclaimable),
            })
        })
        .collect()
}

/// The part of `docker system df -v --format json` we read: one JSON object
/// whose `Volumes` array carries every volume as string fields.
#[derive(Deserialize)]
struct DfVerbose {
    #[serde(rename = "Volumes", default)]
    volumes: Vec<DfVolume>,
}

#[derive(Deserialize)]
struct DfVolume {
    #[serde(rename = "Name")]
    name: String,
    /// Number of containers referencing the volume, as a string.
    #[serde(rename = "Links", default)]
    links: String,
    #[serde(rename = "Size", default)]
    size: String,
}

/// The unused volumes (`Links == "0"`) in `docker system df -v --format json`,
/// largest first. A volume whose link count is missing or unreadable is treated
/// as IN USE — it is simply not offered, never offered by mistake.
#[must_use]
pub fn parse_unused_volumes(json: &str) -> Vec<Volume> {
    let Ok(df) = serde_json::from_str::<DfVerbose>(json.trim()) else {
        return Vec::new();
    };
    let mut out: Vec<Volume> = df
        .volumes
        .into_iter()
        .filter(|v| v.links.trim() == "0")
        .map(|v| Volume {
            // Anonymous volumes get a Docker-generated 64-hex name.
            anonymous: v.name.len() == 64 && v.name.bytes().all(|b| b.is_ascii_hexdigit()),
            size_bytes: parse_size(&v.size),
            name: v.name,
        })
        .collect();
    out.sort_by(|a, b| {
        b.size_bytes
            .cmp(&a.size_bytes)
            .then_with(|| a.name.cmp(&b.name))
    });
    out
}

/// Bytes freed by `docker volume rm`: it echoes each removed volume's name on
/// its own line (a refusal is an `Error response …` line instead), so sum the
/// sizes of the targets that were echoed back.
fn freed_by_removal(targets: &[Volume], output: &str) -> u64 {
    targets
        .iter()
        .filter(|v| output.lines().any(|l| l.trim() == v.name))
        .map(|v| v.size_bytes)
        .sum()
}

/// Parse a Docker human size to bytes. Docker uses **decimal** units
/// (`units.HumanSize`): `B`, `kB`, `MB`, `GB`, `TB`, `PB` = 1000ⁿ. A trailing
/// reclaimable percentage (`"542.1MB (10%)"`) is ignored.
#[must_use]
pub fn parse_size(s: &str) -> u64 {
    let s = s.trim();
    // Drop a trailing " (NN%)" that Reclaimable carries.
    let s = s.split('(').next().unwrap_or(s).trim();
    if s.is_empty() {
        return 0;
    }
    let split = s
        .find(|c: char| !(c.is_ascii_digit() || c == '.'))
        .unwrap_or(s.len());
    let (num, unit) = s.split_at(split);
    let Ok(value) = num.trim().parse::<f64>() else {
        return 0;
    };
    let mult = match unit.trim().to_ascii_lowercase().as_str() {
        "" | "b" => 1.0,
        "kb" => 1e3,
        "mb" => 1e6,
        "gb" => 1e9,
        "tb" => 1e12,
        "pb" => 1e15,
        _ => return 0, // unrecognized unit: don't guess a size
    };
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    {
        (value * mult) as u64
    }
}

/// Bytes freed, from Docker's `Total reclaimed space: <SIZE>` summary line.
#[must_use]
pub fn parse_reclaimed(text: &str) -> u64 {
    text.lines()
        .find_map(|l| l.trim().strip_prefix("Total reclaimed space:"))
        .map(parse_size)
        .unwrap_or(0)
}

/// Trim `docker` output to a compact, honest summary for the UI: the deleted-id
/// lines are noise; keep the last few non-empty lines (which include the
/// "Deleted …" counts and the "Total reclaimed space" total), capped.
#[must_use]
pub fn summarize(text: &str) -> String {
    let lines: Vec<&str> = text
        .lines()
        .map(str::trim)
        .filter(|l| !l.is_empty())
        .collect();
    if lines.is_empty() {
        return "Nothing to remove.".to_string();
    }
    // Prefer the "Total reclaimed space" line + a little context; otherwise the
    // tail. Docker prints a long list of deleted ids we don't need to show.
    let total_idx = lines
        .iter()
        .rposition(|l| l.starts_with("Total reclaimed space:"));
    let tail: Vec<&str> = match total_idx {
        Some(i) => lines[i.saturating_sub(3)..=i].to_vec(),
        None => lines[lines.len().saturating_sub(4)..].to_vec(),
    };
    tail.join("\n")
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_size_decimal_units_and_percent() {
        assert_eq!(parse_size("0B"), 0);
        assert_eq!(parse_size("542.1MB (10%)"), 542_100_000);
        assert_eq!(parse_size("5.078GB"), 5_078_000_000);
        assert_eq!(parse_size("2.367MB (100%)"), 2_367_000);
        assert_eq!(parse_size("10.96GB"), 10_960_000_000);
        assert_eq!(parse_size("1kB"), 1_000);
        assert_eq!(parse_size(""), 0);
        assert_eq!(parse_size("garbage"), 0);
    }

    #[test]
    fn parse_df_maps_types_and_sizes() {
        // Real shape from `docker system df --format json` (NDJSON).
        let json = concat!(
            r#"{"Active":"5","Reclaimable":"542.1MB (10%)","Size":"5.078GB","TotalCount":"11","Type":"Images"}"#,
            "\n",
            r#"{"Active":"0","Reclaimable":"2.367MB (100%)","Size":"2.367MB","TotalCount":"5","Type":"Containers"}"#,
            "\n",
            r#"{"Active":"2","Reclaimable":"516.7MB (65%)","Size":"784.6MB","TotalCount":"14","Type":"Local Volumes"}"#,
            "\n",
            r#"{"Active":"36","Reclaimable":"4.328GB","Size":"10.96GB","TotalCount":"85","Type":"Build Cache"}"#,
        );
        let a = parse_df(json);
        assert_eq!(a.len(), 4);
        assert_eq!(a[0].kind, "images");
        assert_eq!(a[0].total_count, 11);
        assert_eq!(a[0].active_count, 5);
        assert_eq!(a[0].reclaimable_bytes, 542_100_000);
        let vols = a.iter().find(|x| x.kind == "volumes").unwrap();
        assert_eq!(vols.reclaimable_bytes, 516_700_000);
        let bc = a.iter().find(|x| x.kind == "build_cache").unwrap();
        assert_eq!(bc.reclaimable_bytes, 4_328_000_000);
        assert_eq!(bc.total_count, 85);
    }

    #[test]
    fn parse_df_tolerates_a_json_array() {
        // Robustness: if a docker variant emits a single JSON array instead of
        // NDJSON, we must still parse it (not silently return empty).
        let json = concat!(
            r#"[{"Active":"5","Reclaimable":"542.1MB (10%)","Size":"5.078GB","TotalCount":"11","Type":"Images"},"#,
            r#"{"Active":"36","Reclaimable":"4.328GB","Size":"10.96GB","TotalCount":"85","Type":"Build Cache"}]"#,
        );
        let a = parse_df(json);
        assert_eq!(a.len(), 2);
        assert_eq!(a[0].kind, "images");
        assert_eq!(a[1].kind, "build_cache");
        assert_eq!(a[1].reclaimable_bytes, 4_328_000_000);
    }

    #[test]
    fn unused_volumes_are_only_unlinked_and_freed_counts_only_removed() {
        // Real shape from `docker system df -v --format json` (one object).
        let anon = "5e9d02490fa48505529eaa5eddaece2152b2f525314711372cc01aefb49946d9";
        let json = format!(
            r#"{{"Images":[],"Containers":[],"BuildCache":[],"Volumes":[
              {{"Links":"1","Name":"fulusi_pgdata","Size":"208.7MB"}},
              {{"Links":"0","Name":"njozi_pgdata","Size":"86.34MB"}},
              {{"Links":"0","Name":"docker_sonarqube_data","Size":"576.5MB"}},
              {{"Links":"0","Name":"{anon}","Size":"1kB"}},
              {{"Name":"no_link_count","Size":"9GB"}}]}}"#
        );
        let v = parse_unused_volumes(&json);
        // In-use (Links 1) and unknown-link volumes are never offered.
        let names: Vec<&str> = v.iter().map(|x| x.name.as_str()).collect();
        assert_eq!(names, ["docker_sonarqube_data", "njozi_pgdata", anon]); // largest first
        assert_eq!(v[0].size_bytes, 576_500_000);
        assert!(!v[0].anonymous && v[2].anonymous);
        assert!(parse_unused_volumes("not json").is_empty());

        // `docker volume rm` echoed one name and refused the other: only the
        // echoed volume counts as freed.
        let out = "njozi_pgdata\nError response from daemon: remove docker_sonarqube_data: volume is in use\n";
        assert_eq!(freed_by_removal(&v, out), 86_340_000);
    }

    #[test]
    fn parse_reclaimed_finds_total() {
        let out = "deleted: sha256:abc\ndeleted: sha256:def\nTotal reclaimed space: 4.328GB\n";
        assert_eq!(parse_reclaimed(out), 4_328_000_000);
        assert_eq!(parse_reclaimed("nothing here"), 0);
    }

    #[test]
    fn summarize_keeps_total_line() {
        let out =
            "Deleted build cache objects:\nabc\ndef\nghi\njkl\nmno\nTotal reclaimed space: 1.2GB";
        let s = summarize(out);
        assert!(s.contains("Total reclaimed space: 1.2GB"));
        assert!(s.lines().count() <= 4);
        assert_eq!(summarize("   \n  "), "Nothing to remove.");
    }
}

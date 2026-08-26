// The clap command definition, in its own module so `build.rs` can `include!`
// it and generate the man page (`tabibu.1`) from the SAME source the binary
// parses — the help text and the man page can never drift. (Line comments, not
// `//!`, so it's valid both as `mod cli` and when `include!`d into build.rs.)

use clap::{Parser, Subcommand};

#[derive(Parser)]
#[command(
    name = "tabibu",
    version,
    about = "Tabibu — a doctor for your Mac (health, cleanup, privacy) from the terminal",
    long_about = "Tabibu is the terminal companion to the Tabibu app. It drives the same \
core as the desktop UI, so you can script health checks, cleanup, and privacy \
from a shell, CI, or cron.\n\n\
Every command supports --json for machine-readable output. Destructive actions \
are a DRY RUN by default and require --yes to execute.",
    propagate_version = true
)]
pub struct Cli {
    /// Emit machine-readable JSON instead of human text.
    #[arg(long, global = true)]
    pub json: bool,
    /// Suppress human progress/hint/confirmation lines on stdout. Errors
    /// (stderr) and `--json` output are unaffected — useful in cron.
    #[arg(long, global = true)]
    pub quiet: bool,
    /// Accepted for script/CI compatibility. Output is already plain text with
    /// no color, so this is a documented no-op (Tabibu also never colors when
    /// `NO_COLOR` is set — because it never colors at all).
    #[arg(long, global = true)]
    pub no_color: bool,
    #[command(subcommand)]
    pub command: Command,
}

#[derive(Subcommand)]
pub enum Command {
    /// Quick vitals: CPU, memory, and your public IP (one glance).
    Status,
    /// Full checkup: vitals plus swap and the top resource-hungry processes.
    Doctor,
    /// Inspect or empty the Trash.
    Trash {
        #[command(subcommand)]
        cmd: TrashCmd,
    },
    /// Slim universal apps to this Mac's architecture (reclaim the other slice).
    /// With no APP, lists reclaimable apps; with an APP, thins it (needs `--yes`).
    Slim {
        /// `.app` bundle path to thin. Omit to list candidates.
        app: Option<String>,
        /// Actually thin it (otherwise this is a dry run).
        #[arg(long)]
        yes: bool,
    },
    /// Privacy readout: what the network sees (IP/ISP) and whether DNS is encrypted.
    Privacy,
    /// One snapshot — health (CPU/memory/swap), disk space, and privacy — in a
    /// single call. Ideal piped through `--json` for CI or a cron health check.
    Report,
    /// Disk-usage tree for a folder, largest entries first (read-only).
    Space {
        /// Folder to measure.
        path: String,
        /// How many levels of children to show. [default: 1, or `depth` in
        /// ~/.config/tabibu/config.toml]
        #[arg(long)]
        depth: Option<usize>,
    },
    /// Find things worth reviewing — report only, never removes anything.
    Scan {
        #[command(subcommand)]
        what: ScanCmd,
    },
    /// Flush the macOS DNS resolver cache (maintenance; needs admin — uses sudo
    /// if not already root). Fixes stale DNS after network changes.
    FlushDns,
    /// Reclaim space by moving items to the Trash (reversible). Prints a report
    /// first; pass `--yes` to actually move them.
    Clean {
        #[command(subcommand)]
        what: CleanCmd,
    },
    /// Homebrew maintenance: read its status, or clean its cache / orphans.
    /// Destructive verbs report first and need `--yes`; the removal is done by
    /// `brew` itself (Homebrew manages what it deletes).
    Brew {
        #[command(subcommand)]
        cmd: BrewCmd,
    },
    /// Docker maintenance: read reclaimable space, or prune build cache + unused
    /// images. Prune reports first and needs `--yes`; the removal is done by
    /// `docker` itself (images re-pull, build cache rebuilds).
    Docker {
        #[command(subcommand)]
        cmd: DockerCmd,
    },
    /// Uninstall an app AND its leftover support files (caches, preferences,
    /// containers, …). Give a `.app` path or an app name; reports the bundle
    /// plus every remnant first, and `--yes` moves them all to the Trash
    /// (reversible — restore from Trash).
    Uninstall {
        /// A `.app` bundle path, or an app name to match in /Applications.
        app: String,
        /// Move the app and its remnants to the Trash (otherwise just report).
        #[arg(long)]
        yes: bool,
    },
    /// Manage protected paths — a safety list shared with the desktop app.
    /// Nothing under a protected path is ever trashed or deleted by any
    /// cleanup (`clean`, `uninstall`, the app), no matter which asked.
    Protect {
        #[command(subcommand)]
        cmd: ProtectCmd,
    },
    /// Print a shell completion script to stdout. Source it from your shell rc,
    /// e.g. `tabibu completions zsh > ~/.zfunc/_tabibu`.
    Completions {
        /// Shell to generate completions for (bash, zsh, fish, …).
        shell: clap_complete::Shell,
    },
}

#[derive(Subcommand)]
pub enum ProtectCmd {
    /// Show the protected paths.
    List,
    /// Protect a path (nothing under it will ever be reclaimed).
    Add {
        /// Path to protect (relative or `~` paths are resolved).
        path: String,
    },
    /// Stop protecting a path.
    Remove {
        /// Path to unprotect (must match a listed entry).
        path: String,
    },
}

#[derive(Subcommand)]
pub enum DockerCmd {
    /// Docker readout: daemon state, version, reclaimable space per category
    /// (images / containers / volumes / build cache) — read-only.
    Status,
    /// Prune build cache + unused images (`docker builder prune` + `image prune
    /// -a`). Reclaimed space regenerates; volumes/containers are left untouched.
    Prune {
        /// Actually prune (otherwise report the reclaimable size).
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum BrewCmd {
    /// Homebrew readout: version, prefix, package count, reclaimable cache,
    /// orphaned dependencies (read-only).
    Status,
    /// Clear Homebrew's old versions and stale download cache (`brew cleanup`).
    Clean {
        /// Actually run `brew cleanup` (otherwise report the reclaimable size).
        #[arg(long)]
        yes: bool,
    },
    /// Remove orphaned dependencies nothing depends on (`brew autoremove`).
    Autoremove {
        /// Actually run `brew autoremove` (otherwise list what would go).
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum CleanCmd {
    /// Caches, temp, logs, trash, large-old files.
    Junk {
        /// Move them to the Trash (otherwise just report).
        #[arg(long)]
        yes: bool,
    },
    /// App + developer caches only (`~/Library/Caches`, npm/cargo/gradle/…).
    Caches {
        #[arg(long)]
        yes: bool,
    },
    /// Log files only (`~/Library/Logs`).
    Logs {
        #[arg(long)]
        yes: bool,
    },
    /// Everything: all junk plus rebuildable dev artifacts across your home.
    All {
        #[arg(long)]
        yes: bool,
    },
    /// Rebuildable dev build artifacts (target, node_modules, dist, …).
    DevArtifacts {
        /// Folder to clean (default: current directory).
        path: Option<String>,
        /// Clean across your whole home instead of one folder.
        #[arg(long)]
        global: bool,
        /// Move them to the Trash (otherwise just report).
        #[arg(long)]
        yes: bool,
    },
}

#[derive(Subcommand)]
pub enum ScanCmd {
    /// Large, old files (in Downloads) you may have forgotten.
    Large,
    /// Byte-identical duplicate files under a folder (keep the newest copy).
    Dupes {
        /// Folder to scan.
        path: String,
        /// Ignore files smaller than this many bytes. [default: 4096, or
        /// `min_size` in ~/.config/tabibu/config.toml]
        #[arg(long)]
        min_size: Option<u64>,
    },
    /// Reclaimable junk (caches, temp, logs, trash, large-old) — summary only.
    Junk,
    /// Adware / rogue configuration-profile heuristics — report only.
    Malware,
    /// Rebuildable dev build artifacts (target, node_modules, dist, …).
    DevArtifacts {
        /// Folder to scan (default: current directory).
        path: Option<String>,
        /// Scan your whole home instead of one folder.
        #[arg(long)]
        global: bool,
    },
}

#[derive(Subcommand)]
pub enum TrashCmd {
    /// Show how much is in the Trash.
    Status,
    /// Permanently empty the Trash (needs `--yes`; otherwise a dry run).
    Empty {
        /// Actually delete (otherwise reports what would be freed).
        #[arg(long)]
        yes: bool,
    },
}

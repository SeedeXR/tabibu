//! `tabibu` — the terminal companion to the Tabibu app. It drives the SAME core
//! crates the desktop app does (monitor, junk, salama, universal), so anything
//! you can do in the UI you can script in CI or a cron job.
//!
//! Design notes (how this differs from typical Mac cleaners):
//!   • every command supports `--json` for machine-readable output (scripting);
//!   • destructive actions are DRY-RUN by default and need `--yes` to execute
//!     (safe in scripts — nothing is removed unless you ask);
//!   • the command vocabulary follows the app's "doctor" metaphor
//!     (`tabibu doctor`, `tabibu status`) — Tabibu means "doctor" in Swahili.
//!
//! Roadmap (grounded on existing core crates; see docs/tabibu-cli.md + todo.md):
//!   implemented — status, doctor, trash, slim, privacy, space, flush-dns, free-memory,
//!                 scan (large/dupes/junk/malware/dev-artifacts [--min-size]),
//!                 clean (junk/caches/logs/all/dev-artifacts; report-first → Trash),
//!                 brew (status/clean/autoremove) + docker (status/prune),
//!                 report-first → delegated to brew/docker;
//!                 uninstall <app> (bundle + remnants; report-first → Trash),
//!                 protect (shared protected-paths list; enforced in reclaim),
//!                 completions (bash/zsh/fish via clap_complete),
//!                 report (health+disk+privacy snapshot),
//!                 global flags --quiet/--no-color + documented exit codes,
//!                 config file (~/.config/tabibu/config.toml; flag defaults),
//!                 opt-in launchd/cron schedule recipe, JSON/app parity
//!   Phase 5 complete — CLI feature-complete against the roadmap.

use std::path::PathBuf;

use std::sync::Mutex;

use clap::{CommandFactory, Parser};
use tabibu_engine::{smart_scan, CancelToken, CleanupItem, ScanCtx, Scanner};
use tabibu_monitor::{Sampler, TopBy};

mod cli;
mod config;
use cli::{BrewCmd, CleanCmd, Cli, Command, DockerCmd, ProtectCmd, ScanCmd, TrashCmd};

use std::sync::atomic::{AtomicBool, Ordering};

/// Set once from `--quiet`. Read by the `println!` shadow below.
static QUIET: AtomicBool = AtomicBool::new(false);
fn quiet() -> bool {
    QUIET.load(Ordering::Relaxed)
}

/// Shadow `println!` for this file so every human line honors `--quiet`. JSON
/// data goes through `print_json` (which uses `std::println!` and is never
/// silenced); errors use `eprintln!` on stderr and are always shown.
macro_rules! println {
    ($($arg:tt)*) => {
        if !crate::quiet() {
            std::println!($($arg)*);
        }
    };
}

fn main() {
    let cli = Cli::parse();
    if cli.quiet {
        QUIET.store(true, Ordering::Relaxed);
    }
    // Optional ~/.config/tabibu/config.toml supplies flag defaults; absent → the
    // built-in defaults. Precedence: explicit flag > config > built-in.
    let cfg = config::load(
        &std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default(),
    );
    let code = match cli.command {
        Command::Status => cmd_status(cli.json, false),
        Command::Doctor => cmd_status(cli.json, true),
        Command::Trash { cmd } => cmd_trash(cli.json, cmd),
        Command::Slim { app, yes } => cmd_slim(cli.json, app, yes),
        Command::Privacy => cmd_privacy(cli.json),
        Command::Report => cmd_report(cli.json),
        Command::Space { path, depth } => cmd_space(cli.json, path, cfg.depth(depth)),
        Command::Scan { what } => cmd_scan(cli.json, what, &cfg),
        Command::FlushDns => cmd_flush_dns(cli.json),
        Command::FreeMemory => cmd_free_memory(cli.json),
        Command::Clean { what } => cmd_clean(cli.json, what),
        Command::Brew { cmd } => cmd_brew(cli.json, cmd),
        Command::Docker { cmd } => cmd_docker(cli.json, cmd),
        Command::Uninstall { app, yes } => cmd_uninstall(cli.json, app, yes),
        Command::Protect { cmd } => cmd_protect(cli.json, cmd),
        Command::Completions { shell } => cmd_completions(shell),
    };
    std::process::exit(code);
}

// ---- helpers -------------------------------------------------------------

/// Decimal GB/MB (storage convention — matches Finder). Bytes below 1 MB show MB.
fn human_bytes(b: u64) -> String {
    let bf = b as f64;
    if bf >= 1e9 {
        format!("{:.1} GB", bf / 1e9)
    } else {
        format!("{:.0} MB", bf / 1e6)
    }
}

fn print_json(v: &serde_json::Value) {
    // `std::println!` on purpose: JSON is the requested data and must print even
    // under `--quiet` (which only silences the human `println!` shadow).
    std::println!(
        "{}",
        serde_json::to_string_pretty(v).unwrap_or_else(|_| "{}".into())
    );
}

/// `~/.Trash` plus every mounted volume's per-user trash (same set the app uses).
fn trash_dirs() -> Vec<PathBuf> {
    use std::os::unix::fs::MetadataExt;
    let Some(home) = std::env::var_os("HOME").map(PathBuf::from) else {
        return Vec::new();
    };
    let mut dirs = vec![home.join(".Trash")];
    if let Ok(uid) = std::fs::metadata(&home).map(|m| m.uid()) {
        dirs.extend(tabibu_junk::per_volume_trash_dirs(
            std::path::Path::new("/Volumes"),
            uid,
        ));
    }
    dirs
}

// ---- commands ------------------------------------------------------------

/// Sample twice over a short interval so CPU% reflects a real delta (sysinfo
/// derives CPU usage from the elapsed time between refreshes).
/// Sample twice ~300ms apart so CPU% is a real delta (sysinfo derives it from
/// the gap between refreshes), returning the second sample plus its rounded
/// memory-used percent. Shared by `status`/`doctor` and `report`.
fn sampled_health(top_n: usize) -> (tabibu_monitor::SystemSample, u64) {
    let mut sampler = Sampler::new();
    let _ = sampler.sample(0, TopBy::Cpu);
    std::thread::sleep(std::time::Duration::from_millis(300));
    let s = sampler.sample(top_n, TopBy::Cpu);
    let mem_pct = if s.total_memory_bytes > 0 {
        (s.used_memory_bytes as f64 / s.total_memory_bytes as f64 * 100.0).round() as u64
    } else {
        0
    };
    (s, mem_pct)
}

fn cmd_status(json: bool, full: bool) -> i32 {
    let (s, mem_pct) = sampled_health(if full { 5 } else { 0 });
    let ip = tabibu_salama::exposure().ip;

    if json {
        let mut v = serde_json::json!({
            "cpu_percent": s.cpu_percent,
            "memory": { "used_bytes": s.used_memory_bytes, "total_bytes": s.total_memory_bytes, "used_percent": mem_pct },
            "public_ip": ip,
        });
        if full {
            v["swap"] = serde_json::json!({ "used_bytes": s.used_swap_bytes, "total_bytes": s.total_swap_bytes });
            v["top_processes"] =
                serde_json::to_value(&s.top_processes).unwrap_or(serde_json::Value::Null);
        }
        print_json(&v);
        return 0;
    }

    println!("CPU     {:>5.1}%", s.cpu_percent);
    println!(
        "Memory  {}%  ({} of {})",
        mem_pct,
        human_bytes(s.used_memory_bytes),
        human_bytes(s.total_memory_bytes)
    );
    println!("IP      {}", ip.as_deref().unwrap_or("—"));
    if full {
        println!(
            "Swap    {} of {}",
            human_bytes(s.used_swap_bytes),
            human_bytes(s.total_swap_bytes)
        );
        if !s.top_processes.is_empty() {
            println!("\nTop processes (by CPU):");
            for p in &s.top_processes {
                println!("  {:>5.1}%  {}", p.cpu_percent, p.name);
            }
        }
    }
    0
}

fn cmd_trash(json: bool, cmd: TrashCmd) -> i32 {
    let dirs = trash_dirs();
    // macOS gates ~/.Trash behind Full Disk Access, so an un-granted process
    // sees an empty, un-emptyable Trash. Warn (once, to stderr) so the user
    // knows WHY, instead of silently reporting 0.
    let gated = !tabibu_junk::trash_accessible(&dirs);
    if gated {
        eprintln!(
            "⚠ Can't read the Trash — macOS needs Full Disk Access. Grant it in \
             System Settings → Privacy & Security → Full Disk Access (for your \
             terminal), or run `sudo tabibu trash …`. Figures below may be incomplete."
        );
    }
    match cmd {
        TrashCmd::Status => {
            let bytes = tabibu_junk::trash_total_size(&dirs, &CancelToken::new());
            if json {
                print_json(
                    &serde_json::json!({ "trash_bytes": bytes, "full_disk_access": !gated }),
                );
            } else {
                println!("Trash: {}", human_bytes(bytes));
            }
            // `status` printed a value — exit 0 even when gated (the FDA gate is
            // reported via the stderr warning + the `full_disk_access` JSON
            // field, so scripts checking `$?` for "command ran" aren't broken).
            0
        }
        TrashCmd::Empty { yes } => {
            if !yes {
                let bytes = tabibu_junk::trash_total_size(&dirs, &CancelToken::new());
                if json {
                    print_json(&serde_json::json!({ "dry_run": true, "would_free_bytes": bytes }));
                } else {
                    println!(
                        "Dry run: emptying the Trash would free {}.",
                        human_bytes(bytes)
                    );
                    println!("Re-run with --yes to permanently delete.");
                }
                return 0;
            }
            let out = tabibu_junk::empty_trash_dirs(&dirs);
            if json {
                print_json(&serde_json::json!({
                    "freed_bytes": out.freed_bytes, "deleted_items": out.deleted_items, "errors": out.errors,
                }));
            } else {
                println!(
                    "Emptied the Trash — freed {} ({} item(s)).",
                    human_bytes(out.freed_bytes),
                    out.deleted_items
                );
                for e in &out.errors {
                    eprintln!("  ! {e}");
                }
            }
            i32::from(!out.errors.is_empty())
        }
    }
}

fn cmd_slim(json: bool, app: Option<String>, yes: bool) -> i32 {
    match app {
        None => {
            let roots = vec![
                PathBuf::from("/Applications"),
                std::env::var_os("HOME")
                    .map(PathBuf::from)
                    .unwrap_or_default()
                    .join("Applications"),
            ];
            let report = tabibu_universal::scan(&roots, &CancelToken::new());
            if json {
                print_json(&serde_json::to_value(&report).unwrap_or(serde_json::Value::Null));
                return 0;
            }
            println!(
                "This Mac runs {}. {} reclaimable across {} app(s) ({} safe).",
                report.native_arch,
                human_bytes(report.total_reclaimable_bytes),
                report.apps.len(),
                human_bytes(report.safe_reclaimable_bytes),
            );
            for a in &report.apps {
                println!(
                    "  {:>9}  [{}]  {}",
                    human_bytes(a.reclaimable_bytes),
                    a.category,
                    a.name
                );
            }
            println!("\nThin one with:  tabibu slim \"/Applications/Name.app\" --yes");
            0
        }
        Some(path) => {
            if !yes {
                if !json {
                    println!("Dry run: would thin {path} to the native arch and re-sign it.");
                    println!("Re-run with --yes to apply (irreversible; reinstall to restore both arches).");
                }
                return 0;
            }
            let res = tabibu_universal::strip_app(std::path::Path::new(&path));
            if json {
                print_json(&serde_json::to_value(&res).unwrap_or(serde_json::Value::Null));
            } else {
                println!(
                    "Thinned {} — freed {} across {} file(s); re-signed: {}.",
                    res.app,
                    human_bytes(res.reclaimed_bytes),
                    res.files_thinned,
                    res.resigned
                );
                for e in &res.errors {
                    eprintln!("  ! {e}");
                }
            }
            i32::from(res.files_thinned == 0)
        }
    }
}

fn cmd_space(json: bool, path: String, depth: usize) -> i32 {
    match tabibu_walk::size_tree(
        std::path::Path::new(&path),
        &CancelToken::new(),
        Some(depth),
    ) {
        Ok(node) => {
            if json {
                print_json(&serde_json::to_value(&node).unwrap_or(serde_json::Value::Null));
                return 0;
            }
            println!(
                "{:>9}  {}",
                human_bytes(node.size_bytes),
                node.path.display()
            );
            print_tree(&node, 1);
            0
        }
        Err(e) => {
            eprintln!("space: {e}");
            1
        }
    }
}

/// Print a size-sorted tree (children already come largest-first from the walk).
fn print_tree(node: &tabibu_walk::DirNode, indent: usize) {
    for child in &node.children {
        let name = child.path.file_name().map_or_else(
            || child.path.display().to_string(),
            |n| n.to_string_lossy().into_owned(),
        );
        let slash = if child.is_dir { "/" } else { "" };
        println!(
            "{:>9}  {}{}{}",
            human_bytes(child.size_bytes),
            "  ".repeat(indent),
            name,
            slash
        );
        print_tree(child, indent + 1);
    }
}

/// Run report-only scanners and return their items largest-first. The engine's
/// GuardedSink still enforces allowed-roots/denylist. `scan` NEVER touches a
/// removal path — it is strictly read-only.
fn collect_sorted(scanners: &[Box<dyn Scanner>], ctx: &ScanCtx) -> Vec<CleanupItem> {
    let items = Mutex::new(Vec::new());
    smart_scan(scanners, ctx, &CancelToken::new(), &|it| {
        items.lock().expect("scan sink poisoned").push(it);
    });
    let mut items = items.into_inner().expect("scan items poisoned");
    items.sort_by_key(|i| std::cmp::Reverse(i.size_bytes));
    items
}

fn cmd_scan(json: bool, what: ScanCmd, cfg: &config::Config) -> i32 {
    match what {
        ScanCmd::Large => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let ctx = ScanCtx {
                home: home.clone(),
                allowed_roots: vec![home.join("Downloads")],
                running_bundle_ids: std::collections::HashSet::new(),
                full_disk_access: false,
            };
            let scanners: Vec<Box<dyn Scanner>> =
                vec![Box::new(tabibu_junk::LargeOldScanner::new())];
            let items = collect_sorted(&scanners, &ctx);
            if json {
                print_json(&serde_json::to_value(&items).unwrap_or(serde_json::Value::Null));
                return 0;
            }
            if items.is_empty() {
                println!("No large, old files found in Downloads.");
            } else {
                let total: u64 = items.iter().map(|i| i.size_bytes).sum();
                println!(
                    "{} large/old file(s) in Downloads — {} total (review only, nothing removed):",
                    items.len(),
                    human_bytes(total)
                );
                for i in &items {
                    println!(
                        "  {:>9}  {}  ({})",
                        human_bytes(i.size_bytes),
                        i.path.display(),
                        i.reason
                    );
                }
            }
            0
        }
        ScanCmd::Dupes { path, min_size } => {
            let min_size = cfg.min_size(min_size);
            let cancel = CancelToken::new();
            let root = std::path::Path::new(&path);
            let files = match tabibu_dupes::collect_candidates(root, min_size, &cancel) {
                Ok(f) => f,
                Err(e) => {
                    eprintln!("scan dupes: {e}");
                    return 1;
                }
            };
            let groups = match tabibu_dupes::find_duplicates(
                &files,
                &tabibu_dupes::DupeConfig { min_size },
                &cancel,
                &|_g| {},
            ) {
                Ok(g) => g,
                Err(e) => {
                    eprintln!("scan dupes: {e}");
                    return 1;
                }
            };
            let reclaimable = reclaimable_bytes(&groups);
            if json {
                print_json(
                    &serde_json::json!({ "reclaimable_bytes": reclaimable, "groups": groups }),
                );
                return 0;
            }
            if groups.is_empty() {
                println!("No duplicate files found under {path}.");
            } else {
                println!(
                    "{} duplicate set(s) — up to {} reclaimable (keep the newest, remove the rest):",
                    groups.len(),
                    human_bytes(reclaimable)
                );
                for g in &groups {
                    println!("  {} × {} each", g.paths.len(), human_bytes(g.size_bytes));
                    for (i, p) in g.paths.iter().enumerate() {
                        let tag = if i == 0 { "keep" } else { "dup " };
                        println!("     [{tag}] {}", p.display());
                    }
                }
            }
            0
        }
        ScanCmd::Junk => {
            let ctx = junk_ctx();
            let scanners = tabibu_junk::scanners();
            let items = collect_sorted(&scanners, &ctx);
            let total: u64 = items.iter().map(|i| i.size_bytes).sum();
            if json {
                print_json(&serde_json::json!({
                    "total_bytes": total, "item_count": items.len(), "items": items,
                }));
                return 0;
            }
            if items.is_empty() {
                println!("No reclaimable junk found.");
            } else {
                println!(
                    "{} reclaimable across {} item(s) — report only; review & remove in the app or with `tabibu clean`:",
                    human_bytes(total),
                    items.len()
                );
                for (cat, n, bytes) in summarize_by_category(&items) {
                    println!("  {:>9}  {}  ({} item(s))", human_bytes(bytes), cat, n);
                }
            }
            0
        }
        ScanCmd::Malware => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let ctx = ScanCtx {
                home: home.clone(),
                allowed_roots: vec![
                    home.join("Library/LaunchAgents"),
                    PathBuf::from("/Library/Managed Preferences"),
                ],
                running_bundle_ids: std::collections::HashSet::new(),
                full_disk_access: false,
            };
            let scanners = tabibu_malware::scanners();
            let items = collect_sorted(&scanners, &ctx);
            if json {
                print_json(&serde_json::to_value(&items).unwrap_or(serde_json::Value::Null));
                return 0;
            }
            if items.is_empty() {
                println!("No adware or rogue configuration profiles detected.");
            } else {
                println!(
                    "{} suspicious item(s) — review carefully and quarantine in the app:",
                    items.len()
                );
                for i in &items {
                    println!("  {}  ({})", i.path.display(), i.reason);
                }
            }
            0
        }
        ScanCmd::DevArtifacts {
            path,
            global,
            min_size,
        } => {
            let roots = devscan_roots(path.as_deref(), global);
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let mut arts = tabibu_devscan::scan(&roots, &home, &CancelToken::new());
            arts.retain(|a| a.size_bytes >= min_size);
            if json {
                print_json(&serde_json::to_value(&arts).unwrap_or(serde_json::Value::Null));
                return 0;
            }
            print_dev_artifacts(&arts, &roots);
            0
        }
    }
}

/// Roots for a dev-artifact scan: an explicit path, else the whole home with
/// `--global`, else the current directory.
fn devscan_roots(path: Option<&str>, global: bool) -> Vec<PathBuf> {
    if let Some(p) = path {
        vec![PathBuf::from(p)]
    } else if global {
        vec![std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default()]
    } else {
        vec![std::env::current_dir().unwrap_or_else(|_| PathBuf::from("."))]
    }
}

/// Human report for a dev-artifact scan (largest first).
fn print_dev_artifacts(arts: &[tabibu_devscan::DevArtifact], roots: &[PathBuf]) {
    if arts.is_empty() {
        println!(
            "No rebuildable dev artifacts found under {}.",
            roots_display(roots)
        );
        return;
    }
    let total: u64 = arts.iter().map(|a| a.size_bytes).sum();
    println!(
        "{} rebuildable across {} artifact dir(s) under {} (all regenerable from source):",
        human_bytes(total),
        arts.len(),
        roots_display(roots)
    );
    for a in arts {
        println!(
            "  {:>9}  {:<16}  {}   (rebuild: {})",
            human_bytes(a.size_bytes),
            a.kind,
            a.path.display(),
            a.rebuild
        );
    }
}

fn roots_display(roots: &[PathBuf]) -> String {
    roots
        .iter()
        .map(|r| r.display().to_string())
        .collect::<Vec<_>>()
        .join(", ")
}

// ---- clean (report-first; --yes moves to Trash, reversible) --------------

fn cmd_clean(json: bool, what: CleanCmd) -> i32 {
    match what {
        CleanCmd::Junk { yes } => {
            let ctx = junk_ctx();
            let items = collect_sorted(&tabibu_junk::scanners(), &ctx);
            clean_items(json, &ctx, items, yes)
        }
        CleanCmd::Caches { yes } => {
            let ctx = junk_ctx();
            let scanners: Vec<Box<dyn Scanner>> = vec![
                Box::new(tabibu_junk::UserCacheScanner),
                Box::new(tabibu_junk::DevCacheScanner),
            ];
            let items = collect_sorted(&scanners, &ctx);
            clean_items(json, &ctx, items, yes)
        }
        CleanCmd::Logs { yes } => {
            let ctx = junk_ctx();
            let scanners: Vec<Box<dyn Scanner>> = vec![Box::new(tabibu_junk::LogScanner)];
            let items = collect_sorted(&scanners, &ctx);
            clean_items(json, &ctx, items, yes)
        }
        CleanCmd::All { yes } => {
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let jctx = junk_ctx();
            let mut items = collect_sorted(&tabibu_junk::scanners(), &jctx);
            let arts =
                tabibu_devscan::scan(std::slice::from_ref(&home), &home, &CancelToken::new());
            items.extend(arts.iter().map(dev_to_item));
            items.sort_by_key(|i| std::cmp::Reverse(i.size_bytes));
            // Cover both the junk roots and dev artifacts anywhere in home.
            let mut roots = jctx.allowed_roots.clone();
            roots.push(home.clone());
            let ctx = ScanCtx {
                home,
                allowed_roots: roots,
                running_bundle_ids: std::collections::HashSet::new(),
                full_disk_access: false,
            };
            clean_items(json, &ctx, items, yes)
        }
        CleanCmd::DevArtifacts {
            path,
            global,
            min_size,
            yes,
        } => {
            let roots = devscan_roots(path.as_deref(), global);
            let home = std::env::var_os("HOME")
                .map(PathBuf::from)
                .unwrap_or_default();
            let mut arts = tabibu_devscan::scan(&roots, &home, &CancelToken::new());
            arts.retain(|a| a.size_bytes >= min_size);
            if !yes {
                if json {
                    print_json(&serde_json::to_value(&arts).unwrap_or(serde_json::Value::Null));
                } else {
                    print_dev_artifacts(&arts, &roots);
                    if !arts.is_empty() {
                        println!("\nRe-run with --yes to move these to the Trash (rebuild from source when needed).");
                    }
                }
                return 0;
            }
            let ctx = ScanCtx {
                home,
                allowed_roots: roots,
                running_bundle_ids: std::collections::HashSet::new(),
                full_disk_access: false,
            };
            let items: Vec<CleanupItem> = arts.iter().map(dev_to_item).collect();
            reclaim_report(json, &ctx, items)
        }
    }
}

/// Report-first clean of pre-collected items: with `yes=false` prints a
/// category summary and touches nothing; with `yes=true` moves them to the
/// Trash (reversible) via the engine.
fn clean_items(json: bool, ctx: &ScanCtx, items: Vec<CleanupItem>, yes: bool) -> i32 {
    if yes {
        return reclaim_report(json, ctx, items);
    }
    let total: u64 = items.iter().map(|i| i.size_bytes).sum();
    if json {
        print_json(&serde_json::json!({
            "dry_run": true, "would_free_bytes": total, "item_count": items.len(), "items": items,
        }));
    } else if items.is_empty() {
        println!("Nothing to clean.");
    } else {
        println!(
            "Would move {} to the Trash ({} item(s)):",
            human_bytes(total),
            items.len()
        );
        for (cat, n, b) in summarize_by_category(&items) {
            println!("  {:>9}  {}  ({} item(s))", human_bytes(b), cat, n);
        }
        println!(
            "\nRe-run with --yes to move these to the Trash (reversible — restore from Trash)."
        );
    }
    0
}

/// Move the given items to the Trash via the engine (undo manifest + denylist),
/// and print what was freed. Items are forced to `Trash` (reversible) here too.
fn reclaim_report(json: bool, ctx: &ScanCtx, items: Vec<CleanupItem>) -> i32 {
    if items.is_empty() {
        if json {
            print_json(&serde_json::json!({ "reclaimed_bytes": 0, "succeeded": 0, "failed": 0 }));
        } else {
            println!("Nothing to clean.");
        }
        return 0;
    }
    match run_reclaim(ctx, items) {
        Ok(r) => {
            if json {
                print_json(&serde_json::json!({
                    "reclaimed_bytes": r.reclaimed_bytes, "succeeded": r.succeeded, "failed": r.failed,
                }));
            } else {
                println!(
                    "Moved {} to the Trash — {} item(s) freed{}.",
                    human_bytes(r.reclaimed_bytes),
                    r.succeeded,
                    if r.failed > 0 {
                        format!(", {} failed", r.failed)
                    } else {
                        String::new()
                    }
                );
            }
            i32::from(r.failed > 0)
        }
        Err(e) => {
            eprintln!("clean: {e}");
            1
        }
    }
}

/// Reclaim helper — mirrors the app's safety: every item is coerced to a Trash
/// move (never a permanent delete), so cleanup is always reversible.
fn run_reclaim(
    ctx: &ScanCtx,
    mut items: Vec<CleanupItem>,
) -> Result<tabibu_engine::ReclaimReport, String> {
    for i in &mut items {
        i.selected = true;
        i.action = tabibu_engine::ReclaimAction::Trash;
    }
    let undo = cli_undo_dir();
    let _ = std::fs::create_dir_all(&undo);
    tabibu_engine::reclaim(ctx, &items, &undo).map_err(|e| e.to_string())
}

fn cli_undo_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
        .join("Library/Application Support/Tabibu/cli-undo")
}

/// Convert a rebuildable artifact into a Trash-bound cleanup item.
fn dev_to_item(a: &tabibu_devscan::DevArtifact) -> CleanupItem {
    CleanupItem {
        path: a.path.clone(),
        category: tabibu_engine::Category::DevCache,
        size_bytes: a.size_bytes,
        tier: tabibu_engine::SafetyTier::Safe,
        reason: format!("{} — rebuild: {}", a.kind, a.rebuild),
        selected: true,
        action: tabibu_engine::ReclaimAction::Trash,
    }
}

/// Space freed if every duplicate set is reduced to one copy: for each group,
/// `size × (copies − 1)`. A group always has ≥ 2 members, so no underflow.
fn reclaimable_bytes(groups: &[tabibu_dupes::DuplicateGroup]) -> u64 {
    groups
        .iter()
        .map(|g| g.size_bytes * (g.paths.len() as u64 - 1))
        .sum()
}

/// Group cleanup items by category id → `(category, count, bytes)`, largest
/// total first. Pure so it's unit-tested.
fn summarize_by_category(items: &[CleanupItem]) -> Vec<(&'static str, u64, u64)> {
    let mut map: std::collections::BTreeMap<&'static str, (u64, u64)> =
        std::collections::BTreeMap::new();
    for i in items {
        let e = map.entry(i.category.id()).or_default();
        e.0 += 1;
        e.1 += i.size_bytes;
    }
    let mut v: Vec<_> = map.into_iter().map(|(k, (n, b))| (k, n, b)).collect();
    v.sort_by_key(|&(_, _, b)| std::cmp::Reverse(b));
    v
}

/// Allowed roots for the junk scan — mirrors the app's default scan context so
/// the engine's GuardedSink doesn't drop legitimate junk. (Holistic follow-up
/// in todo.md P5: hoist this into a shared core helper used by app + CLI.)
fn junk_ctx() -> ScanCtx {
    use std::os::unix::fs::MetadataExt;
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let mut roots: Vec<PathBuf> = [
        "Library/Caches",
        "Library/Logs",
        "Library/Developer/Xcode/DerivedData",
        "Library/Developer/CoreSimulator/Caches",
        ".npm",
        ".cargo/registry/cache",
        ".cache",
        ".gradle/caches",
        "Downloads",
        ".Trash",
    ]
    .iter()
    .map(|r| home.join(r))
    .collect();
    roots.push(std::env::temp_dir());
    if let Ok(uid) = std::fs::metadata(&home).map(|m| m.uid()) {
        roots.extend(tabibu_junk::per_volume_trash_dirs(
            std::path::Path::new("/Volumes"),
            uid,
        ));
    }
    ScanCtx {
        home,
        allowed_roots: roots,
        running_bundle_ids: std::collections::HashSet::new(),
        full_disk_access: false,
    }
}

/// True if we're already running as root (uid 0) — then admin commands run
/// directly; otherwise they go through `sudo`.
fn is_root() -> bool {
    std::process::Command::new("/usr/bin/id")
        .arg("-u")
        .output()
        .ok()
        .and_then(|o| String::from_utf8(o.stdout).ok())
        .is_some_and(|s| s.trim() == "0")
}

/// Flush the DNS resolver cache. Signalling mDNSResponder needs root, so if we
/// aren't already root the work runs via `sudo` (which prompts in a terminal; in
/// a non-interactive shell it fails and we report that). This is maintenance —
/// it frees no disk and removes no files.
fn cmd_flush_dns(json: bool) -> i32 {
    let is_root = is_root();
    // Both steps in ONE privileged shell so the user is asked for a password at
    // most once (not once per command). Both run regardless of the first's
    // result; the script exits non-zero if EITHER failed.
    let script = "/usr/bin/dscacheutil -flushcache; a=$?; \
                  /usr/bin/killall -HUP mDNSResponder; b=$?; exit $((a||b))";
    let mut cmd = if is_root {
        std::process::Command::new("/bin/sh")
    } else {
        let mut c = std::process::Command::new("/usr/bin/sudo");
        c.arg("/bin/sh");
        c
    };
    let ok = cmd
        .arg("-c")
        .arg(script)
        .status()
        .is_ok_and(|s| s.success());
    if json {
        print_json(&serde_json::json!({ "flushed": ok }));
    } else if ok {
        println!("DNS cache flushed.");
    } else {
        eprintln!("flush-dns: failed — run `sudo tabibu flush-dns`, or grant the admin prompt.");
    }
    i32::from(!ok)
}

/// `free-memory` — run macOS `purge` to return inactive/cached memory to the
/// free pool. Harmless (flushes disk caches; no data loss). Needs root, so uses
/// `sudo` when not already root. `purge` moves reclaimable pages into *free*
/// memory (it barely changes `used`), so the freed amount is the rise in FREE
/// memory across the operation.
fn cmd_free_memory(json: bool) -> i32 {
    let root = is_root();
    if !root {
        // Prime sudo (prompts once) so the purge below runs non-interactively.
        // This also means `before` is sampled AFTER authentication, isolating
        // the measurement from time spent typing the password.
        let primed = std::process::Command::new("/usr/bin/sudo")
            .arg("-v")
            .status()
            .is_ok_and(|s| s.success());
        if !primed {
            if json {
                print_json(&serde_json::json!({ "freed": false }));
            } else {
                eprintln!("free-memory: admin authentication failed or was cancelled.");
            }
            return 1;
        }
    }
    let before = tabibu_monitor::memory_snapshot();
    let status = if root {
        std::process::Command::new("/usr/sbin/purge").status()
    } else {
        std::process::Command::new("/usr/bin/sudo")
            .args(["-n", "/usr/sbin/purge"])
            .status()
    };
    if !status.is_ok_and(|s| s.success()) {
        if json {
            print_json(&serde_json::json!({ "freed": false }));
        } else {
            eprintln!("free-memory: purge failed — try `sudo tabibu free-memory`.");
        }
        return 1;
    }
    let after = tabibu_monitor::memory_snapshot();
    let freed = after.free_bytes.saturating_sub(before.free_bytes);
    if json {
        print_json(&serde_json::json!({
            "freed": true,
            "freed_bytes": freed,
            "before_free_bytes": before.free_bytes,
            "after_free_bytes": after.free_bytes,
            "total_bytes": before.total_bytes,
        }));
    } else {
        println!(
            "Freed {} back to the free pool (free {} → {} of {}).",
            human_bytes(freed),
            human_bytes(before.free_bytes),
            human_bytes(after.free_bytes),
            human_bytes(before.total_bytes)
        );
        if freed == 0 {
            println!("(Nothing to reclaim right now — macOS already had it optimized.)");
        }
    }
    0
}

/// `brew status|clean|autoremove`. Status is read-only. Clean/autoremove report
/// first and need `--yes`; the removal itself is delegated to `brew` (Homebrew
/// decides what to delete from its own cache/orphans).
fn cmd_brew(json: bool, cmd: BrewCmd) -> i32 {
    let Some(brew) = tabibu_brew::Brew::detect() else {
        if json {
            print_json(&serde_json::json!({ "installed": false }));
        } else {
            println!("Homebrew isn't installed (nothing to do).");
        }
        return 0;
    };
    match cmd {
        BrewCmd::Status => {
            let r = brew.analyze();
            if json {
                print_json(&serde_json::to_value(&r).unwrap_or_default());
            } else {
                println!(
                    "{}  ({})",
                    r.status.version.as_deref().unwrap_or("Homebrew"),
                    r.status.prefix.as_deref().unwrap_or("prefix unknown"),
                );
                println!("  {} package(s) installed", r.packages.len());
                println!(
                    "  {} reclaimable via `brew cleanup`",
                    human_bytes(r.cleanup.freeable_bytes)
                );
                println!("  {} orphaned dependencies", r.autoremovable.len());
            }
            0
        }
        BrewCmd::Clean { yes } => {
            if !yes {
                let freeable = brew.cleanup_preview().freeable_bytes;
                if json {
                    print_json(&serde_json::json!({
                        "dry_run": true, "would_free_bytes": freeable,
                    }));
                } else {
                    println!(
                        "`brew cleanup` would free about {}.\nRe-run with --yes to run it.",
                        human_bytes(freeable)
                    );
                }
                return 0;
            }
            brew_outcome(json, brew.run_cleanup())
        }
        BrewCmd::Autoremove { yes } => {
            if !yes {
                let orphans = brew.autoremovable();
                if json {
                    print_json(&serde_json::json!({
                        "dry_run": true, "orphans": orphans, "count": orphans.len(),
                    }));
                } else if orphans.is_empty() {
                    println!("No orphaned dependencies.");
                } else {
                    println!(
                        "`brew autoremove` would remove {} package(s):",
                        orphans.len()
                    );
                    for name in &orphans {
                        println!("  {name}");
                    }
                    println!("\nRe-run with --yes to remove them.");
                }
                return 0;
            }
            brew_outcome(json, brew.run_autoremove())
        }
    }
}

/// Present a brew `ActionOutcome` (shared by clean/autoremove `--yes`).
fn brew_outcome(json: bool, o: tabibu_brew::ActionOutcome) -> i32 {
    if json {
        print_json(&serde_json::json!({
            "ok": o.ok, "freed_bytes": o.freed_bytes, "message": o.message,
        }));
    } else if o.ok {
        println!("Done — freed about {}.", human_bytes(o.freed_bytes));
        if !o.message.is_empty() {
            println!("{}", o.message);
        }
    } else {
        eprintln!("brew: {}", o.message);
    }
    i32::from(!o.ok)
}

/// `docker status|prune`. Status is read-only. Prune reports first and needs
/// `--yes`; it delegates to `docker` and only touches build cache + unused
/// images (both regenerate). Volumes/containers are intentionally left alone.
fn cmd_docker(json: bool, cmd: DockerCmd) -> i32 {
    let Some(docker) = tabibu_docker::Docker::detect() else {
        if json {
            print_json(&serde_json::json!({ "installed": false }));
        } else {
            println!("Docker isn't installed (nothing to do).");
        }
        return 0;
    };
    let r = docker.analyze();
    match cmd {
        DockerCmd::Status => {
            if json {
                print_json(&serde_json::to_value(&r).unwrap_or_default());
            } else if !r.status.running {
                println!(
                    "{} installed, but the daemon isn't running — start Docker to analyze.",
                    r.status.version.as_deref().unwrap_or("Docker").trim()
                );
            } else {
                println!(
                    "{}  ({} reclaimable)",
                    r.status.version.as_deref().unwrap_or("Docker").trim(),
                    human_bytes(r.total_reclaimable_bytes)
                );
                for a in &r.artifacts {
                    println!(
                        "  {:>9}  {}  ({} of {} unused)",
                        human_bytes(a.reclaimable_bytes),
                        a.kind,
                        a.total_count.saturating_sub(a.active_count),
                        a.total_count
                    );
                }
            }
            0
        }
        DockerCmd::Prune { yes } => {
            if !r.status.running {
                if json {
                    print_json(&serde_json::json!({ "running": false }));
                } else {
                    eprintln!("Docker daemon isn't running — start Docker first.");
                }
                return 1;
            }
            // Only build cache + unused images: both regenerate (rebuild / re-pull).
            let target: u64 = r
                .artifacts
                .iter()
                .filter(|a| a.kind == "build_cache" || a.kind == "images")
                .map(|a| a.reclaimable_bytes)
                .sum();
            if !yes {
                if json {
                    print_json(&serde_json::json!({
                        "dry_run": true, "would_free_bytes": target,
                    }));
                } else {
                    println!(
                        "`docker` would reclaim about {} (build cache + unused images).",
                        human_bytes(target)
                    );
                    println!("Re-run with --yes to prune (images re-pull, cache rebuilds).");
                }
                return 0;
            }
            let cache = docker.prune_build_cache();
            let images = docker.prune_images();
            let freed = cache.freed_bytes + images.freed_bytes;
            let ok = cache.ok && images.ok;
            if json {
                print_json(&serde_json::json!({
                    "ok": ok, "freed_bytes": freed,
                    "build_cache": cache.message, "images": images.message,
                }));
            } else if ok {
                println!("Pruned — reclaimed about {}.", human_bytes(freed));
            } else {
                eprintln!(
                    "docker prune failed:\n  {}\n  {}",
                    cache.message, images.message
                );
            }
            i32::from(!ok)
        }
    }
}

/// `uninstall <app> [--yes]`. Resolves the argument to a `.app` bundle, then
/// reports the bundle + its `~/Library` remnants; `--yes` moves them all to the
/// Trash (reversible) via the shared reclaim path. Read-only until `--yes`.
fn cmd_uninstall(json: bool, app: String, yes: bool) -> i32 {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    let bundle = match resolve_app(&app, &home) {
        Ok(p) => p,
        Err(candidates) => {
            if json {
                print_json(&serde_json::json!({
                    "error": "no unique app match", "query": app, "candidates": candidates,
                }));
            } else if candidates.is_empty() {
                eprintln!("No app matching {app:?} (give a .app path or an installed app name).");
            } else {
                eprintln!("{app:?} matches more than one app — be specific:");
                for c in &candidates {
                    eprintln!("  {c}");
                }
            }
            return 1;
        }
    };
    let app_name = bundle
        .file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_default();
    let bundle_id = tabibu_uninstall::bundle_id_of(&bundle);

    // Roots the reclaim is allowed to touch: the app's own location plus
    // `~/Library` (where remnants live). `find_remnants` reads under ctx.home.
    let mut roots = vec![home.join("Library"), PathBuf::from("/Applications")];
    if let Some(parent) = bundle.parent() {
        roots.push(parent.to_path_buf());
    }
    let ctx = ScanCtx {
        home: home.clone(),
        allowed_roots: roots,
        running_bundle_ids: std::collections::HashSet::new(),
        full_disk_access: false,
    };

    let mut items = vec![tabibu_engine::CleanupItem::new(
        bundle.clone(),
        tabibu_engine::Category::UnusedApp,
        tabibu_walk::dir_size(&bundle, &CancelToken::new()).unwrap_or(0),
        tabibu_engine::SafetyTier::Review,
        format!("{app_name} application bundle"),
    )];
    match &bundle_id {
        Some(id) => items.extend(tabibu_uninstall::find_remnants(id, &app_name, &ctx)),
        None if !json => eprintln!(
            "Note: couldn't read {}'s bundle id — reporting the app bundle only, no remnant scan.",
            app_name
        ),
        None => {}
    }
    items.sort_by_key(|i| std::cmp::Reverse(i.size_bytes));
    clean_items(json, &ctx, items, yes)
}

/// Resolve a CLI argument to a single `.app` bundle: an existing `.app` path is
/// used as-is; otherwise it's matched by name against installed apps (exact
/// stem first, then substring). `Err` carries the ambiguous/empty candidate
/// list for the caller to print.
fn resolve_app(arg: &str, home: &std::path::Path) -> Result<PathBuf, Vec<String>> {
    let direct = PathBuf::from(arg);
    if direct.extension().is_some_and(|e| e == "app") && direct.is_dir() {
        return Ok(direct);
    }
    let roots = vec![PathBuf::from("/Applications"), home.join("Applications")];
    let apps: Vec<PathBuf> = tabibu_uninstall::installed_apps(&roots)
        .into_iter()
        .map(|(path, _id)| path)
        .collect();
    let q = arg.to_lowercase();
    let stem = |p: &PathBuf| p.file_stem().map(|s| s.to_string_lossy().to_lowercase());
    let exact: Vec<PathBuf> = apps
        .iter()
        .filter(|p| stem(p).as_deref() == Some(q.as_str()))
        .cloned()
        .collect();
    if exact.len() == 1 {
        return Ok(exact.into_iter().next().unwrap());
    }
    let fuzzy: Vec<PathBuf> = apps
        .iter()
        .filter(|p| stem(p).is_some_and(|s| s.contains(&q)))
        .cloned()
        .collect();
    if exact.is_empty() && fuzzy.len() == 1 {
        return Ok(fuzzy.into_iter().next().unwrap());
    }
    let shown = if exact.is_empty() { fuzzy } else { exact };
    Err(shown
        .iter()
        .map(|p| p.to_string_lossy().into_owned())
        .collect())
}

/// `protect list|add|remove <path>`. Reads/writes the shared protected-paths
/// list (`tabibu_engine::protect`) that `reclaim` honors — so protecting a path
/// here also protects it in the desktop app.
fn cmd_protect(json: bool, cmd: ProtectCmd) -> i32 {
    let home = std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default();
    match cmd {
        ProtectCmd::List => {
            let list = tabibu_engine::protect::load(&home);
            if json {
                print_json(&serde_json::json!({ "protected": list }));
            } else if list.is_empty() {
                println!("No protected paths. Add one with `tabibu protect add <path>`.");
            } else {
                println!("Protected paths ({}):", list.len());
                for p in &list {
                    println!("  {}", p.display());
                }
            }
            0
        }
        ProtectCmd::Add { path } => {
            let p = absolutize(&path, &home);
            match tabibu_engine::protect::add(&home, &p) {
                Ok(added) => {
                    if json {
                        print_json(&serde_json::json!({ "path": p, "added": added }));
                    } else if added {
                        println!(
                            "Protected {} — nothing under it will be reclaimed.",
                            p.display()
                        );
                    } else {
                        println!("{} is already protected.", p.display());
                    }
                    0
                }
                Err(e) => {
                    eprintln!("protect add: {e}");
                    1
                }
            }
        }
        ProtectCmd::Remove { path } => {
            let p = absolutize(&path, &home);
            match tabibu_engine::protect::remove(&home, &p) {
                Ok(removed) => {
                    if json {
                        print_json(&serde_json::json!({ "path": p, "removed": removed }));
                    } else if removed {
                        println!("Unprotected {}.", p.display());
                    } else {
                        println!("{} was not in the protected list.", p.display());
                    }
                    i32::from(!removed)
                }
                Err(e) => {
                    eprintln!("protect remove: {e}");
                    1
                }
            }
        }
    }
}

/// Resolve a user-typed path to absolute: expand a leading `~`, then make it
/// absolute against the current directory. Not canonicalized — a protected path
/// need not exist yet, and symlinks are matched as written.
fn absolutize(input: &str, home: &std::path::Path) -> PathBuf {
    let expanded = if input == "~" {
        home.to_path_buf()
    } else if let Some(rest) = input.strip_prefix("~/") {
        home.join(rest)
    } else {
        PathBuf::from(input)
    };
    if expanded.is_absolute() {
        expanded
    } else {
        std::env::current_dir()
            .map(|d| d.join(&expanded))
            .unwrap_or(expanded)
    }
}

/// Print a shell completion script generated from the SAME clap definition the
/// binary parses (so completions never drift from `--help`).
fn cmd_completions(shell: clap_complete::Shell) -> i32 {
    let mut cmd = Cli::command();
    let name = cmd.get_name().to_string();
    clap_complete::generate(shell, &mut cmd, name, &mut std::io::stdout());
    0
}

/// One-shot snapshot: health + disk + privacy. Reuses the exact same core calls
/// as `status`/`privacy` and the shared `tabibu_monitor::disk_space` (so the
/// figures match the app), with no new logic of its own.
fn cmd_report(json: bool) -> i32 {
    let (s, mem_pct) = sampled_health(0);
    let disk = tabibu_monitor::disk_space();
    // Shared `PrivacyStatus` (same type the app's `salama_status` returns).
    let privacy = tabibu_salama::status();
    let ex = &privacy.exposure;
    let dns = &privacy.dns;

    if json {
        print_json(&serde_json::json!({
            "health": {
                "cpu_percent": s.cpu_percent,
                "memory": { "used_bytes": s.used_memory_bytes, "total_bytes": s.total_memory_bytes, "used_percent": mem_pct },
                "swap": { "used_bytes": s.used_swap_bytes, "total_bytes": s.total_swap_bytes },
            },
            "disk": { "total_bytes": disk.total_bytes, "available_bytes": disk.available_bytes },
            "privacy": serde_json::to_value(&privacy).unwrap_or(serde_json::Value::Null),
        }));
        return 0;
    }

    println!("Health");
    println!("  CPU        {:>5.1}%", s.cpu_percent);
    println!(
        "  Memory     {}%  ({} of {})",
        mem_pct,
        human_bytes(s.used_memory_bytes),
        human_bytes(s.total_memory_bytes)
    );
    println!(
        "  Swap       {} of {}",
        human_bytes(s.used_swap_bytes),
        human_bytes(s.total_swap_bytes)
    );
    println!("Disk (/)");
    let used = disk.total_bytes.saturating_sub(disk.available_bytes);
    println!(
        "  {} free of {}  ({} used)",
        human_bytes(disk.available_bytes),
        human_bytes(disk.total_bytes),
        human_bytes(used)
    );
    println!("Privacy");
    println!("  Public IP  {}", ex.ip.as_deref().unwrap_or("—"));
    println!("  Location   {}", ex.country.as_deref().unwrap_or("—"));
    println!("  Network    {}", ex.org.as_deref().unwrap_or("—"));
    println!(
        "  DNS        {}",
        if dns.encrypted {
            "encrypted"
        } else {
            "not encrypted"
        }
    );
    0
}

fn cmd_privacy(json: bool) -> i32 {
    // Use the shared `status()` so the JSON is the exact `PrivacyStatus` type the
    // app's `salama_status` command returns (parity by construction).
    let st = tabibu_salama::status();
    let ex = &st.exposure;
    let dns = &st.dns;
    if json {
        print_json(&serde_json::to_value(&st).unwrap_or(serde_json::Value::Null));
        return 0;
    }
    println!("Public IP   {}", ex.ip.as_deref().unwrap_or("—"));
    println!("Location    {}", ex.country.as_deref().unwrap_or("—"));
    println!("Network     {}", ex.org.as_deref().unwrap_or("—"));
    println!(
        "DNS         {}",
        if dns.encrypted {
            "encrypted ✓"
        } else {
            "not encrypted — your ISP can see the sites you visit"
        }
    );
    0
}

#[cfg(test)]
mod tests {
    use super::*;
    use clap::CommandFactory;

    #[test]
    fn cli_definition_is_valid() {
        // clap's own structural lint: catches conflicting args, bad subcommands.
        Cli::command().debug_assert();
    }

    #[test]
    fn parses_key_invocations() {
        // Global --json works in any position; destructive verbs take --yes.
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "--json", "status"])
                .unwrap()
                .command,
            Command::Status
        ));
        let c = Cli::try_parse_from(["tabibu", "trash", "empty", "--yes"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Trash {
                cmd: TrashCmd::Empty { yes: true }
            }
        ));
        let c = Cli::try_parse_from(["tabibu", "slim", "/Applications/Foo.app", "--yes"]).unwrap();
        assert!(matches!(c.command, Command::Slim { yes: true, .. }));
        let c = Cli::try_parse_from(["tabibu", "space", "/tmp", "--depth", "2"]).unwrap();
        assert!(matches!(c.command, Command::Space { depth: Some(2), .. }));
        let c = Cli::try_parse_from(["tabibu", "scan", "large"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Scan {
                what: ScanCmd::Large
            }
        ));
        let c =
            Cli::try_parse_from(["tabibu", "scan", "dupes", "/tmp", "--min-size", "1024"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Scan {
                what: ScanCmd::Dupes {
                    min_size: Some(1024),
                    ..
                }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "flush-dns"])
                .unwrap()
                .command,
            Command::FlushDns
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "scan", "junk"])
                .unwrap()
                .command,
            Command::Scan {
                what: ScanCmd::Junk
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "scan", "malware"])
                .unwrap()
                .command,
            Command::Scan {
                what: ScanCmd::Malware
            }
        ));
        // Unknown command is rejected.
        assert!(Cli::try_parse_from(["tabibu", "bogus"]).is_err());
    }

    /// `scan` must collect items largest-first AND never delete anything
    /// (read-only). Uses a fake scanner emitting two real temp files.
    #[test]
    fn collect_sorted_orders_desc_and_is_read_only() {
        use tabibu_engine::{Category, ReclaimAction, SafetyTier, ScanError};

        struct Fake(Vec<(PathBuf, u64)>);
        impl Scanner for Fake {
            fn id(&self) -> &'static str {
                "fake"
            }
            fn scan(
                &self,
                _ctx: &ScanCtx,
                _cancel: &CancelToken,
                sink: &mut dyn FnMut(CleanupItem),
            ) -> Result<(), ScanError> {
                for (path, size) in &self.0 {
                    sink(CleanupItem {
                        path: path.clone(),
                        category: Category::LargeOldFile,
                        size_bytes: *size,
                        tier: SafetyTier::Review,
                        reason: "test".into(),
                        selected: false,
                        action: ReclaimAction::Trash,
                    });
                }
                Ok(())
            }
        }

        let dir = tempfile::tempdir().unwrap();
        let small = dir.path().join("small");
        let big = dir.path().join("big");
        std::fs::write(&small, vec![0u8; 100]).unwrap();
        std::fs::write(&big, vec![0u8; 300]).unwrap();
        let ctx = ScanCtx {
            home: dir.path().to_path_buf(),
            allowed_roots: vec![dir.path().to_path_buf()],
            running_bundle_ids: std::collections::HashSet::new(),
            full_disk_access: false,
        };
        let scanners: Vec<Box<dyn Scanner>> = vec![Box::new(Fake(vec![
            (small.clone(), 100),
            (big.clone(), 300),
        ]))];

        let items = collect_sorted(&scanners, &ctx);
        assert_eq!(items.len(), 2, "both items pass the allowed-roots guard");
        assert_eq!(items[0].size_bytes, 300, "largest first");
        assert_eq!(items[1].size_bytes, 100);
        // Regression: scanning removed nothing.
        assert!(small.exists() && big.exists(), "scan must be read-only");
    }

    /// `scan malware` on a clean home flags nothing (no false positives) and
    /// creates/removes nothing (read-only).
    #[test]
    fn scan_malware_clean_home_is_quiet_and_read_only() {
        let dir = tempfile::tempdir().unwrap(); // no Library/LaunchAgents inside
        let ctx = ScanCtx {
            home: dir.path().to_path_buf(),
            allowed_roots: vec![dir.path().join("Library/LaunchAgents")],
            running_bundle_ids: std::collections::HashSet::new(),
            full_disk_access: false,
        };
        let scanners = tabibu_malware::scanners();
        let items = collect_sorted(&scanners, &ctx);
        assert!(items.is_empty(), "a clean home must flag nothing");
        assert!(
            !dir.path().join("Library/LaunchAgents").exists(),
            "scanning must not create anything"
        );
    }

    #[test]
    fn summarize_by_category_groups_and_sorts_desc() {
        use tabibu_engine::{Category, ReclaimAction, SafetyTier};
        let mk = |cat, size| CleanupItem {
            path: PathBuf::from("x"),
            category: cat,
            size_bytes: size,
            tier: SafetyTier::Safe,
            reason: String::new(),
            selected: false,
            action: ReclaimAction::Trash,
        };
        let items = vec![
            mk(Category::UserCache, 100),
            mk(Category::UserCache, 200),
            mk(Category::Log, 500),
        ];
        let s = summarize_by_category(&items);
        // log (500) outranks user_cache (300); user_cache aggregates 2 items.
        assert_eq!(s[0], ("log", 1, 500));
        assert_eq!(s[1], ("user_cache", 2, 300));
    }

    #[test]
    fn parses_clean_and_devartifacts() {
        let c = Cli::try_parse_from(["tabibu", "clean", "junk", "--yes"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Clean {
                what: CleanCmd::Junk { yes: true }
            }
        ));
        let c = Cli::try_parse_from(["tabibu", "clean", "dev-artifacts", "/p", "--yes"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Clean {
                what: CleanCmd::DevArtifacts {
                    yes: true,
                    global: false,
                    ..
                }
            }
        ));
        let c = Cli::try_parse_from(["tabibu", "scan", "dev-artifacts", "--global"]).unwrap();
        assert!(matches!(
            c.command,
            Command::Scan {
                what: ScanCmd::DevArtifacts {
                    global: true,
                    path: None,
                    min_size: 0,
                }
            }
        ));
        // --min-size is parsed for dev-artifacts.
        let c = Cli::try_parse_from(["tabibu", "scan", "dev-artifacts", "--min-size", "1048576"])
            .unwrap();
        assert!(matches!(
            c.command,
            Command::Scan {
                what: ScanCmd::DevArtifacts {
                    min_size: 1_048_576,
                    ..
                }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "clean", "caches", "--yes"])
                .unwrap()
                .command,
            Command::Clean {
                what: CleanCmd::Caches { yes: true }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "clean", "logs"])
                .unwrap()
                .command,
            Command::Clean {
                what: CleanCmd::Logs { yes: false }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "clean", "all", "--yes"])
                .unwrap()
                .command,
            Command::Clean {
                what: CleanCmd::All { yes: true }
            }
        ));
    }

    #[test]
    fn parses_brew_subcommands() {
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "brew", "status"])
                .unwrap()
                .command,
            Command::Brew {
                cmd: BrewCmd::Status
            }
        ));
        // Destructive verbs default to dry-run (yes:false) until --yes.
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "brew", "clean"])
                .unwrap()
                .command,
            Command::Brew {
                cmd: BrewCmd::Clean { yes: false }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "brew", "autoremove", "--yes"])
                .unwrap()
                .command,
            Command::Brew {
                cmd: BrewCmd::Autoremove { yes: true }
            }
        ));
    }

    #[test]
    fn parses_docker_subcommands() {
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "docker", "status"])
                .unwrap()
                .command,
            Command::Docker {
                cmd: DockerCmd::Status
            }
        ));
        // Prune defaults to dry-run (yes:false) until --yes.
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "docker", "prune"])
                .unwrap()
                .command,
            Command::Docker {
                cmd: DockerCmd::Prune { yes: false }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "docker", "prune", "--yes"])
                .unwrap()
                .command,
            Command::Docker {
                cmd: DockerCmd::Prune { yes: true }
            }
        ));
    }

    #[test]
    fn parses_uninstall() {
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "uninstall", "Foo.app"])
                .unwrap()
                .command,
            Command::Uninstall { yes: false, .. }
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "uninstall", "Foo", "--yes"])
                .unwrap()
                .command,
            Command::Uninstall { yes: true, .. }
        ));
    }

    #[test]
    fn parses_protect_subcommands() {
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "protect", "list"])
                .unwrap()
                .command,
            Command::Protect {
                cmd: ProtectCmd::List
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "protect", "add", "/Users/x/keep"])
                .unwrap()
                .command,
            Command::Protect {
                cmd: ProtectCmd::Add { .. }
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "protect", "remove", "/Users/x/keep"])
                .unwrap()
                .command,
            Command::Protect {
                cmd: ProtectCmd::Remove { .. }
            }
        ));
    }

    #[test]
    fn parses_global_quiet_and_no_color() {
        // Global flags work before or after the subcommand.
        let c = Cli::try_parse_from(["tabibu", "--quiet", "status"]).unwrap();
        assert!(c.quiet && !c.no_color);
        let c = Cli::try_parse_from(["tabibu", "status", "--quiet"]).unwrap();
        assert!(c.quiet);
        let c = Cli::try_parse_from(["tabibu", "trash", "status", "--no-color"]).unwrap();
        assert!(c.no_color && !c.quiet);
        let c = Cli::try_parse_from(["tabibu", "--quiet", "--no-color", "report"]).unwrap();
        assert!(c.quiet && c.no_color);
    }

    #[test]
    fn parses_report() {
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "report"]).unwrap().command,
            Command::Report
        ));
        // --json is a global flag, valid on report.
        let c = Cli::try_parse_from(["tabibu", "report", "--json"]).unwrap();
        assert!(c.json && matches!(c.command, Command::Report));
    }

    #[test]
    fn parses_completions() {
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "completions", "bash"])
                .unwrap()
                .command,
            Command::Completions {
                shell: clap_complete::Shell::Bash
            }
        ));
        assert!(matches!(
            Cli::try_parse_from(["tabibu", "completions", "zsh"])
                .unwrap()
                .command,
            Command::Completions {
                shell: clap_complete::Shell::Zsh
            }
        ));
        // An unknown shell is rejected by clap.
        assert!(Cli::try_parse_from(["tabibu", "completions", "cmd.exe"]).is_err());
    }

    #[test]
    fn completions_script_is_generated() {
        let mut cmd = Cli::command();
        let mut buf = Vec::new();
        clap_complete::generate(clap_complete::Shell::Bash, &mut cmd, "tabibu", &mut buf);
        let script = String::from_utf8_lossy(&buf);
        assert!(!script.is_empty());
        assert!(script.contains("tabibu"));
    }

    #[test]
    fn absolutize_expands_tilde_and_relative() {
        let home = std::path::Path::new("/Users/x");
        assert_eq!(absolutize("~", home), PathBuf::from("/Users/x"));
        assert_eq!(absolutize("~/keep", home), PathBuf::from("/Users/x/keep"));
        assert_eq!(absolutize("/abs/path", home), PathBuf::from("/abs/path"));
        // Relative resolves against cwd (just assert it became absolute).
        assert!(absolutize("rel/dir", home).is_absolute());
    }

    #[test]
    fn resolve_app_accepts_a_direct_app_path() {
        let tmp = tempfile::tempdir().unwrap();
        let app = tmp.path().join("Foo.app");
        std::fs::create_dir_all(app.join("Contents")).unwrap();
        // The direct-path branch returns the bundle verbatim (home unused here).
        assert_eq!(resolve_app(app.to_str().unwrap(), tmp.path()).unwrap(), app);
    }

    #[test]
    fn uninstall_bundle_item_is_a_reversible_trash_move() {
        // The app bundle is added at Review tier; reclaim only ever Trashes it
        // (Delete/Truncate are Safe-only, enforced by tabibu-engine::reclaim).
        let item = tabibu_engine::CleanupItem::new(
            PathBuf::from("/Applications/Foo.app"),
            tabibu_engine::Category::UnusedApp,
            1234,
            tabibu_engine::SafetyTier::Review,
            "Foo application bundle",
        );
        assert!(matches!(item.action, tabibu_engine::ReclaimAction::Trash));
    }

    #[test]
    fn dev_to_item_is_a_reversible_trash_move() {
        let a = tabibu_devscan::DevArtifact {
            path: PathBuf::from("/proj/target"),
            kind: "rust-target",
            size_bytes: 123,
            rebuild: "cargo build",
        };
        let item = dev_to_item(&a);
        assert_eq!(item.path, PathBuf::from("/proj/target"));
        assert!(item.selected);
        // Reversible: always a Trash move, never a permanent Delete/Truncate.
        assert!(matches!(item.action, tabibu_engine::ReclaimAction::Trash));
        assert!(item.reason.contains("rust-target") && item.reason.contains("cargo build"));
    }

    #[test]
    fn devscan_roots_selects_scope() {
        // Explicit path wins.
        assert_eq!(
            devscan_roots(Some("/x/y"), true),
            vec![PathBuf::from("/x/y")]
        );
        // --global → home.
        let home = std::env::var_os("HOME")
            .map(PathBuf::from)
            .unwrap_or_default();
        assert_eq!(devscan_roots(None, true), vec![home]);
        // Neither → current directory.
        assert_eq!(
            devscan_roots(None, false),
            vec![std::env::current_dir().unwrap()]
        );
    }

    #[test]
    fn human_bytes_uses_decimal_units() {
        assert_eq!(human_bytes(0), "0 MB");
        assert_eq!(human_bytes(500_000_000), "500 MB");
        assert_eq!(human_bytes(2_500_000_000), "2.5 GB");
    }

    #[test]
    fn reclaimable_bytes_counts_extra_copies_only() {
        let g = |size, n| tabibu_dupes::DuplicateGroup {
            size_bytes: size,
            hash_hex: "abc".into(),
            paths: (0..n).map(|i| PathBuf::from(format!("f{i}"))).collect(),
        };
        // 3 copies @100 → free 2×100; 2 copies @50 → free 1×50. Total 250.
        assert_eq!(reclaimable_bytes(&[g(100, 3), g(50, 2)]), 250);
        assert_eq!(reclaimable_bytes(&[]), 0);
    }

    /// `scan dupes` finds byte-identical files and removes nothing (read-only).
    #[test]
    fn scan_dupes_finds_identical_and_is_read_only() {
        use tabibu_engine::CancelToken;
        let dir = tempfile::tempdir().unwrap();
        let a = dir.path().join("a.bin");
        let b = dir.path().join("copy-of-a.bin");
        let c = dir.path().join("unique.bin");
        std::fs::write(&a, vec![7u8; 5000]).unwrap();
        std::fs::write(&b, vec![7u8; 5000]).unwrap(); // identical to a
        std::fs::write(&c, vec![9u8; 5000]).unwrap(); // different content

        let cancel = CancelToken::new();
        let files = tabibu_dupes::collect_candidates(dir.path(), 4096, &cancel).unwrap();
        let groups = tabibu_dupes::find_duplicates(
            &files,
            &tabibu_dupes::DupeConfig { min_size: 4096 },
            &cancel,
            &|_g| {},
        )
        .unwrap();

        assert_eq!(groups.len(), 1, "exactly one duplicate set (a == copy)");
        assert_eq!(groups[0].paths.len(), 2);
        assert_eq!(groups[0].size_bytes, 5000);
        assert_eq!(reclaimable_bytes(&groups), 5000, "one redundant copy");
        // Regression: scanning deleted nothing.
        assert!(
            a.exists() && b.exists() && c.exists(),
            "scan must be read-only"
        );
    }
}

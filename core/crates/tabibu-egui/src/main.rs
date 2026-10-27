//! Tabibu — native egui desktop UI (phase 1: Space Analyzer).
//!
//! This replaces the Tauri webview shell with an immediate-mode egui/eframe app
//! for speed and low memory. It calls the existing pure-Rust core crates
//! (`tabibu-walk`, `tabibu-engine`) DIRECTLY — no IPC, no serialization. Tauri
//! is kept in parallel until this reaches parity (parity-first migration).
//!
//! Phase 1 ships the Space Analyzer: one whole-disk walk loads the ENTIRE tree
//! into memory, so expanding any nested folder is instant (never re-walks); a
//! virtualized tree lets you select folders/files at any level and Trash the
//! selection (reversible, via the engine's reclaim + undo manifest).

#![cfg_attr(not(debug_assertions), windows_subsystem = "windows")]

use std::collections::{HashMap, HashSet, VecDeque};
use std::path::{Path, PathBuf};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::mpsc::{channel, Receiver};
use std::sync::{Arc, Mutex};
use std::thread;
use std::time::Duration;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, Vec2,
};
use tabibu_dupes::{DupeConfig, DuplicateGroup};
use tabibu_engine::{
    CancelToken, Category, CleanupItem, ReclaimAction, SafetyTier, ScanCtx, Scanner,
};
use tabibu_monitor::{Sampler, SystemSample, TopBy};
use tabibu_net::{ConnectionTest, NetSampler, Ping, Throughput, Wifi};
use tabibu_walk::DirNode;

// ---------------------------------------------------------------------------
// Palette — ported verbatim from app/src/styles.css so the native app keeps the
// same colour language as the webview build.
// ---------------------------------------------------------------------------
struct Palette {
    bg: Color32,
    bg_elev: Color32,
    text: Color32,
    text_dim: Color32,
    text_faint: Color32,
    border: Color32,
    hover: Color32,
    sel: Color32,
    accent: Color32,
    track: Color32,
}

fn palette(dark: bool) -> Palette {
    if dark {
        Palette {
            bg: Color32::from_rgb(0x1a, 0x1b, 0x1e),
            bg_elev: Color32::from_rgb(0x25, 0x26, 0x2b),
            text: Color32::from_rgb(0xf2, 0xf2, 0xf5),
            text_dim: Color32::from_rgb(0xa3, 0xa3, 0xab),
            text_faint: Color32::from_rgb(0x6e, 0x6e, 0x78),
            border: Color32::from_rgba_unmultiplied(255, 255, 255, 26),
            hover: Color32::from_rgba_unmultiplied(255, 255, 255, 16),
            sel: Color32::from_rgba_unmultiplied(16, 185, 129, 56),
            accent: Color32::from_rgb(0x10, 0xb9, 0x81),
            track: Color32::from_rgba_unmultiplied(255, 255, 255, 30),
        }
    } else {
        Palette {
            bg: Color32::from_rgb(0xf4, 0xf5, 0xf7),
            bg_elev: Color32::from_rgb(0xff, 0xff, 0xff),
            text: Color32::from_rgb(0x1c, 0x1c, 0x1e),
            text_dim: Color32::from_rgb(0x6b, 0x6b, 0x70),
            text_faint: Color32::from_rgb(0x9a, 0x9a, 0xa0),
            border: Color32::from_rgba_unmultiplied(0, 0, 0, 26),
            hover: Color32::from_rgba_unmultiplied(0, 0, 0, 13),
            sel: Color32::from_rgba_unmultiplied(16, 185, 129, 40),
            accent: Color32::from_rgb(0x10, 0xb9, 0x81),
            track: Color32::from_rgba_unmultiplied(0, 0, 0, 26),
        }
    }
}

fn apply_theme(ctx: &egui::Context, dark: bool) {
    let p = palette(dark);
    let mut v = if dark {
        egui::Visuals::dark()
    } else {
        egui::Visuals::light()
    };
    v.panel_fill = p.bg;
    v.window_fill = p.bg_elev;
    v.extreme_bg_color = p.bg_elev;
    v.override_text_color = Some(p.text);
    v.hyperlink_color = p.accent;
    v.selection.bg_fill = p.sel;
    v.selection.stroke = Stroke::new(1.0_f32, p.accent);
    let round = CornerRadius::same(6);
    for w in [
        &mut v.widgets.noninteractive,
        &mut v.widgets.inactive,
        &mut v.widgets.hovered,
        &mut v.widgets.active,
        &mut v.widgets.open,
    ] {
        w.corner_radius = round;
    }
    v.widgets.inactive.bg_fill = p.bg_elev;
    v.widgets.hovered.bg_fill = p.hover;
    v.widgets.active.bg_fill = p.sel;
    v.widgets.noninteractive.bg_stroke = Stroke::new(1.0_f32, p.border);
    ctx.set_visuals(v);

    let mut style = (*ctx.style()).clone();
    style.spacing.item_spacing = Vec2::new(8.0, 6.0);
    style.spacing.button_padding = Vec2::new(12.0, 6.0);
    ctx.set_style(style);
}

// ---------------------------------------------------------------------------
// Minimal painter icons — the same visual language (folder / file / chevron) as
// the lucide set, drawn as vector strokes so they stay crisp and theme-tinted.
// (Porting the exact lucide SVGs via egui_extras is a later-phase refinement.)
// ---------------------------------------------------------------------------
fn draw_chevron(ui: &egui::Ui, rect: Rect, open: bool, color: Color32) {
    let c = rect.center();
    let s = 3.5;
    let pts = if open {
        // pointing down
        vec![
            Pos2::new(c.x - s, c.y - s * 0.6),
            Pos2::new(c.x + s, c.y - s * 0.6),
            Pos2::new(c.x, c.y + s * 0.8),
        ]
    } else {
        // pointing right
        vec![
            Pos2::new(c.x - s * 0.6, c.y - s),
            Pos2::new(c.x + s * 0.8, c.y),
            Pos2::new(c.x - s * 0.6, c.y + s),
        ]
    };
    ui.painter()
        .add(Shape::convex_polygon(pts, color, Stroke::NONE));
}

fn draw_folder(ui: &egui::Ui, rect: Rect, color: Color32) {
    let r = Rect::from_center_size(rect.center(), Vec2::new(15.0, 12.0));
    let body = Rect::from_min_max(
        Pos2::new(r.min.x, r.min.y + 2.5),
        Pos2::new(r.max.x, r.max.y),
    );
    let st = Stroke::new(1.4_f32, color);
    ui.painter()
        .rect_stroke(body, CornerRadius::same(2), st, StrokeKind::Middle);
    // tab
    ui.painter().line_segment(
        [
            Pos2::new(r.min.x + 1.0, r.min.y + 2.5),
            Pos2::new(r.min.x + 4.5, r.min.y + 2.5),
        ],
        st,
    );
    ui.painter().line_segment(
        [
            Pos2::new(r.min.x + 4.5, r.min.y + 2.5),
            Pos2::new(r.min.x + 6.0, r.min.y),
        ],
        st,
    );
    ui.painter().line_segment(
        [
            Pos2::new(r.min.x + 6.0, r.min.y),
            Pos2::new(r.min.x + 9.0, r.min.y),
        ],
        st,
    );
}

fn draw_file(ui: &egui::Ui, rect: Rect, color: Color32) {
    let r = Rect::from_center_size(rect.center(), Vec2::new(11.0, 14.0));
    let st = Stroke::new(1.4_f32, color);
    ui.painter()
        .rect_stroke(r, CornerRadius::same(2), st, StrokeKind::Middle);
    // folded corner
    ui.painter().line_segment(
        [
            Pos2::new(r.max.x - 4.0, r.min.y),
            Pos2::new(r.max.x - 4.0, r.min.y + 4.0),
        ],
        st,
    );
    ui.painter().line_segment(
        [
            Pos2::new(r.max.x - 4.0, r.min.y + 4.0),
            Pos2::new(r.max.x, r.min.y + 4.0),
        ],
        st,
    );
}

// ---------------------------------------------------------------------------
// Formatting — decimal (Finder) units, matching the webview build.
// ---------------------------------------------------------------------------
fn fmt_bytes(n: u64) -> String {
    let mut v = n as f64;
    if v < 1000.0 {
        return format!("{n} B");
    }
    let units = ["KB", "MB", "GB", "TB"];
    let mut i = 0;
    loop {
        v /= 1000.0;
        if v < 999.5 || i == units.len() - 1 {
            break;
        }
        i += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", units[i])
    } else {
        format!("{v:.0} {}", units[i])
    }
}

/// RAM is measured in BINARY (1 GiB = 1024³) but labelled in GB, so 16 GiB of
/// physical memory reads "16 GB" (matches the webview build + Activity Monitor).
/// Used for system memory and swap only — file/disk sizes stay decimal.
fn fmt_ram(n: u64) -> String {
    let mut v = n as f64;
    if v < 1024.0 {
        return format!("{n} B");
    }
    let units = ["KB", "MB", "GB", "TB"];
    let mut i = 0;
    loop {
        v /= 1024.0;
        if v < 1023.5 || i == units.len() - 1 {
            break;
        }
        i += 1;
    }
    if v < 10.0 {
        format!("{v:.1} {}", units[i])
    } else {
        format!("{v:.0} {}", units[i])
    }
}

/// A circular progress ring (CPU dial): a faint full-circle track with a
/// foreground arc covering `frac` of the circle, starting at 12 o'clock.
fn draw_ring(ui: &egui::Ui, center: Pos2, radius: f32, frac: f32, color: Color32, track: Color32) {
    let painter = ui.painter();
    let thickness = 6.0_f32;
    painter.circle_stroke(center, radius, Stroke::new(thickness, track));
    let frac = frac.clamp(0.0, 1.0);
    if frac <= 0.0 {
        return;
    }
    let steps = (frac * 96.0).ceil().max(1.0) as usize;
    let start = -std::f32::consts::FRAC_PI_2; // 12 o'clock
    let mut pts = Vec::with_capacity(steps + 1);
    for k in 0..=steps {
        let t = start + (k as f32 / steps as f32) * frac * std::f32::consts::TAU;
        pts.push(Pos2::new(
            center.x + radius * t.cos(),
            center.y + radius * t.sin(),
        ));
    }
    painter.add(Shape::line(pts, Stroke::new(thickness, color)));
}

/// Safety-tier accent (green = safe, amber = review, red = risky) — matches the
/// webview build's tier colouring in the Junk/Smart cleanup view.
fn tier_color(t: SafetyTier) -> Color32 {
    match t {
        SafetyTier::Safe => Color32::from_rgb(0x10, 0xb9, 0x81),
        SafetyTier::Review => Color32::from_rgb(0xf5, 0x9e, 0x0b),
        SafetyTier::Risky => Color32::from_rgb(0xdc, 0x26, 0x26),
    }
}

/// Human label for a cleanup category (the Junk view's group headers).
fn cat_label(c: Category) -> &'static str {
    match c {
        Category::Trash => "Trash",
        Category::UserCache => "Caches",
        Category::DevCache => "Developer caches",
        Category::Temp => "Temporary files",
        Category::Log => "Logs",
        Category::Duplicate => "Duplicates",
        Category::LargeOldFile => "Large & old files",
        Category::AppRemnant => "App leftovers",
        Category::OrphanedSupport => "Orphaned support files",
        Category::UnusedApp => "Unused apps",
        Category::StaleBinary => "Stale binaries",
        Category::Malware => "Threats",
    }
}

/// Reclaim context that permits trashing any user-chosen path (allowed-roots =
/// `/`), used by the Space Analyzer and Duplicates views where the user picks
/// exact files. `reclaim` still forces Trash (reversible) and the engine
/// denylist still refuses true system paths.
fn reclaim_anywhere_ctx() -> ScanCtx {
    ScanCtx {
        home: tabibu_sys::home_dir(),
        allowed_roots: vec![PathBuf::from("/")],
        running_bundle_ids: HashSet::new(),
        full_disk_access: true,
    }
}

/// What the shared cleanup-review UI reports back to its caller.
#[derive(Default)]
struct CleanupAction {
    rescan: bool,
    trash: bool,
}

/// Shared grouped-review UI for scanner-backed cleanup views (Junk, Large & Old):
/// header + per-category groups (select-all, tier dots, sizes) + Move-to-Trash
/// bar. Toggles `item.selected` in place; returns which action the user clicked.
fn cleanup_review_ui(
    ui: &mut egui::Ui,
    dark: bool,
    title: &str,
    items: &mut [CleanupItem],
) -> CleanupAction {
    let p = palette(dark);
    let total_found: u64 = items.iter().map(|i| i.size_bytes).sum();
    let n_found = items.len();
    let sel_bytes: u64 = items
        .iter()
        .filter(|i| i.selected)
        .map(|i| i.size_bytes)
        .sum();
    let sel_n = items.iter().filter(|i| i.selected).count();
    let mut action = CleanupAction::default();

    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(RichText::new(title).size(16.0).strong());
        ui.label(
            RichText::new(format!(
                "· {} found · {} items",
                fmt_bytes(total_found),
                n_found
            ))
            .color(p.text_dim),
        );
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(8.0);
            if ui.button("Rescan").clicked() {
                action.rescan = true;
            }
        });
    });
    ui.add_space(4.0);
    ui.separator();

    if n_found == 0 {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Nothing found — you're all clear.").color(p.text_dim));
        });
        return action;
    }

    // Category order (items already sorted biggest-first).
    let mut order: Vec<Category> = Vec::new();
    for it in items.iter() {
        if !order.contains(&it.category) {
            order.push(it.category);
        }
    }

    let mut toggle: Option<usize> = None;
    let mut bulk: Option<(Category, bool)> = None;

    let avail_h = ui.available_height() - 52.0;
    egui::ScrollArea::vertical()
        .auto_shrink([false, false])
        .max_height(avail_h.max(0.0))
        .show(ui, |ui| {
            for cat in &order {
                let idxs: Vec<usize> = items
                    .iter()
                    .enumerate()
                    .filter(|(_, it)| it.category == *cat)
                    .map(|(i, _)| i)
                    .collect();
                let cat_bytes: u64 = idxs.iter().map(|&i| items[i].size_bytes).sum();
                let all_sel = idxs.iter().all(|&i| items[i].selected);

                ui.add_space(8.0);
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    let mut checked = all_sel;
                    if ui.checkbox(&mut checked, "").changed() {
                        bulk = Some((*cat, checked));
                    }
                    ui.label(RichText::new(cat_label(*cat)).strong());
                    ui.label(
                        RichText::new(format!("· {} · {} items", fmt_bytes(cat_bytes), idxs.len()))
                            .color(p.text_dim),
                    );
                });

                for &i in &idxs {
                    let it = &items[i];
                    let name = it
                        .path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| it.path.to_string_lossy().into_owned());
                    let tip = it.path.to_string_lossy().into_owned();
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        let mut checked = it.selected;
                        if ui.checkbox(&mut checked, "").changed() {
                            toggle = Some(i);
                        }
                        let (dot, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                        ui.painter()
                            .circle_filled(dot.center(), 4.0, tier_color(it.tier));
                        ui.label(name);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(fmt_bytes(it.size_bytes))
                                    .color(p.text_dim)
                                    .monospace(),
                            );
                        });
                    })
                    .response
                    .on_hover_text(tip);
                }
            }
        });

    if let Some(i) = toggle {
        items[i].selected = !items[i].selected;
    }
    if let Some((cat, val)) = bulk {
        for it in items.iter_mut().filter(|it| it.category == cat) {
            it.selected = val;
        }
    }

    ui.separator();
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        if sel_n == 0 {
            ui.label(RichText::new("Tick anything to remove it (reversible).").color(p.text_dim));
        } else {
            ui.label(
                RichText::new(format!("{sel_n} selected · {}", fmt_bytes(sel_bytes))).strong(),
            );
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(8.0);
            let btn = egui::Button::new(RichText::new("Move to Trash").color(Color32::WHITE)).fill(
                if sel_n == 0 {
                    p.text_faint
                } else {
                    Color32::from_rgb(0xdc, 0x26, 0x26)
                },
            );
            if ui.add_enabled(sel_n > 0, btn).clicked() {
                action.trash = true;
            }
        });
    });
    action
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// Which top-level feature is on screen. More land here as the migration
/// progresses (Dashboard, Duplicates, …).
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Space,
    Junk,
    Large,
    Security,
    Memory,
    Dupes,
    Net,
}

enum ConnPhase {
    Idle,
    Testing,
    Done,
}

/// Network view state: a live throughput sampler (like the CPU monitor) plus
/// one-shot Wi-Fi status and an on-demand connection test. All from tabibu_net.
struct NetState {
    rx: Option<Receiver<Throughput>>,
    latest: Option<Throughput>,
    down_hist: VecDeque<u64>,
    started: bool,
    wifi: Option<Wifi>,
    wifi_rx: Option<Receiver<Wifi>>,
    conn_phase: ConnPhase,
    ping: Option<Ping>,
    conn_rx: Option<Receiver<ConnectionTest>>,
}

impl Default for NetState {
    fn default() -> Self {
        Self {
            rx: None,
            latest: None,
            down_hist: VecDeque::new(),
            started: false,
            wifi: None,
            wifi_rx: None,
            conn_phase: ConnPhase::Idle,
            ping: None,
            conn_rx: None,
        }
    }
}

enum DupePhase {
    Idle,
    Scanning,
    Review,
    Deleting,
    Done { freed: u64, items: usize },
}

/// Duplicate-finder state — reuses tabibu_dupes::{collect_candidates,
/// find_duplicates} + engine reclaim.
struct DupesState {
    phase: DupePhase,
    groups: Vec<DuplicateGroup>,
    /// Paths the user has marked to Trash (keeps at least the group's keeper by
    /// default — paths[0], newest).
    selected: HashSet<PathBuf>,
    scan_rx: Option<Receiver<Result<Vec<DuplicateGroup>, String>>>,
    del_rx: Option<Receiver<(u64, usize)>>,
}

impl Default for DupesState {
    fn default() -> Self {
        Self {
            phase: DupePhase::Idle,
            groups: Vec::new(),
            selected: HashSet::new(),
            scan_rx: None,
            del_rx: None,
        }
    }
}

/// Live Memory & CPU monitor state. A background `Sampler` thread pushes a fresh
/// `SystemSample` on an interval (CPU deltas need spaced refreshes — see
/// `tabibu_monitor::Sampler`); the UI shows the latest + a short CPU history.
struct MonitorState {
    rx: Option<Receiver<SystemSample>>,
    latest: Option<SystemSample>,
    /// Shared with the sampler thread so the By CPU / By Memory toggle re-sorts
    /// the top-process list at the source.
    by_cpu: Arc<AtomicBool>,
    cpu_hist: VecDeque<f32>,
    started: bool,
}

impl Default for MonitorState {
    fn default() -> Self {
        Self {
            rx: None,
            latest: None,
            by_cpu: Arc::new(AtomicBool::new(true)),
            cpu_hist: VecDeque::new(),
            started: false,
        }
    }
}

enum JunkPhase {
    Idle,
    Scanning,
    Review,
    Deleting,
    Done { freed: u64, items: usize },
}

/// Junk / Smart cleanup state — reuses `tabibu_junk::scanners()` +
/// `tabibu_engine::smart_scan`/`reclaim` directly (no IPC).
struct JunkState {
    phase: JunkPhase,
    items: Vec<CleanupItem>,
    scan_rx: Option<Receiver<Vec<CleanupItem>>>,
    del_rx: Option<Receiver<(u64, usize)>>,
}

impl Default for JunkState {
    fn default() -> Self {
        Self {
            phase: JunkPhase::Idle,
            items: Vec::new(),
            scan_rx: None,
            del_rx: None,
        }
    }
}
enum Phase {
    Idle,
    Scanning,
    Browse,
    Deleting,
    Done { freed: u64, items: usize },
    Error(String),
}

/// One rendered tree row — owned so rendering doesn't borrow the tree while we
/// mutate expand/selection state.
struct Row {
    depth: usize,
    path: PathBuf,
    name: String,
    alloc: u64,
    count: u64,
    is_dir: bool,
    expandable: bool,
    /// Whether this node is the last child of its parent (└ vs ├ connector).
    is_last: bool,
    /// For each ancestor depth `j` on this row's path, whether that ancestor has
    /// a following sibling (so a vertical guide line │ continues through column j).
    guides: Vec<bool>,
}

struct TabibuApp {
    dark: bool,
    view: View,
    phase: Phase,
    root: Option<DirNode>,
    expanded: HashSet<PathBuf>,
    /// path -> (physical bytes, descendant count) for selected rows.
    selected: HashMap<PathBuf, (u64, u64)>,
    scan_rx: Option<Receiver<Result<DirNode, String>>>,
    del_rx: Option<Receiver<(u64, usize)>>,
    junk: JunkState,
    large: JunkState,
    security: JunkState,
    monitor: MonitorState,
    dupes: DupesState,
    net: NetState,
}

impl Default for TabibuApp {
    fn default() -> Self {
        Self {
            dark: true,
            view: View::Space,
            phase: Phase::Idle,
            root: None,
            expanded: HashSet::new(),
            selected: HashMap::new(),
            scan_rx: None,
            del_rx: None,
            junk: JunkState::default(),
            large: JunkState::default(),
            security: JunkState::default(),
            monitor: MonitorState::default(),
            dupes: DupesState::default(),
            net: NetState::default(),
        }
    }
}

const ROW_H: f32 = 34.0;

/// Scan root: the whole startup disk by default; overridable via `TABIBU_SCAN_ROOT`
/// (handy for pointing the analyzer at a sub-tree, and for fast testing).
fn disk_root() -> String {
    std::env::var("TABIBU_SCAN_ROOT").unwrap_or_else(|_| "/".to_string())
}

impl TabibuApp {
    fn start_scan(&mut self) {
        self.phase = Phase::Scanning;
        self.root = None;
        self.expanded.clear();
        self.selected.clear();
        let (tx, rx) = channel();
        self.scan_rx = Some(rx);
        thread::spawn(move || {
            // Full depth (None) so the WHOLE tree is loaded once — expanding any
            // nested folder is then instant (no re-walk). Skip the firmlink
            // duplicate / synthetic mounts so a "/" scan isn't double-counted.
            let res = tabibu_walk::size_tree_ex(
                Path::new(&disk_root()),
                &CancelToken::new(),
                None,
                &tabibu_walk::macos_disk_excludes(),
            )
            .map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
    }

    fn start_delete(&mut self) {
        let paths = pruned_selection(&self.selected);
        if paths.is_empty() {
            return;
        }
        let items: Vec<CleanupItem> = paths
            .iter()
            .map(|p| {
                let bytes = self.selected.get(p).map(|v| v.0).unwrap_or(0);
                // Category DevCache so the engine's user-data override lets
                // explicit picks inside Documents/etc. go to the Trash. reclaim
                // still forces Trash (reversible) and refuses true system paths.
                let mut it = CleanupItem::new(
                    p.clone(),
                    Category::DevCache,
                    bytes,
                    SafetyTier::Review,
                    "Selected in Space Analyzer",
                );
                it.selected = true;
                it.action = ReclaimAction::Trash;
                it
            })
            .collect();
        let n = items.len();
        self.phase = Phase::Deleting;
        let (tx, rx) = channel();
        self.del_rx = Some(rx);
        thread::spawn(move || {
            let ctx = reclaim_anywhere_ctx();
            let freed = match tabibu_engine::reclaim(&ctx, &items, &tabibu_sys::undo_dir()) {
                Ok(r) => r.reclaimed_bytes,
                Err(_) => 0,
            };
            let _ = tx.send((freed, n));
        });
    }

    /// Junk / Smart cleanup scan: run every `tabibu_junk` scanner concurrently
    /// through the engine's denylist guard, collect the surviving items, and hand
    /// them to the review UI. Same `default_scan_ctx` the Tauri build uses — so
    /// the running-process guard and reclaim allowed-roots match exactly.
    fn start_junk_scan(&mut self) {
        self.junk.phase = JunkPhase::Scanning;
        self.junk.items.clear();
        let (tx, rx) = channel();
        self.junk.scan_rx = Some(rx);
        thread::spawn(move || {
            let ctx = tabibu_sys::default_scan_ctx(&[]);
            let cancel = CancelToken::new();
            let collected: Mutex<Vec<CleanupItem>> = Mutex::new(Vec::new());
            let _report =
                tabibu_engine::smart_scan(&tabibu_junk::scanners(), &ctx, &cancel, &|it| {
                    collected.lock().expect("junk sink poisoned").push(it);
                });
            let mut items = collected.into_inner().expect("junk sink poisoned");
            // Biggest first — the reviewer's attention goes where the space is.
            items.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
            let _ = tx.send(items);
        });
    }

    /// Move every ticked item to the Trash (reversible) via the engine's reclaim,
    /// guarded by the same ctx the scan used.
    fn start_junk_delete(&mut self) {
        let items: Vec<CleanupItem> = self
            .junk
            .items
            .iter()
            .filter(|i| i.selected)
            .cloned()
            .collect();
        if items.is_empty() {
            return;
        }
        let n = items.len();
        self.junk.phase = JunkPhase::Deleting;
        let (tx, rx) = channel();
        self.junk.del_rx = Some(rx);
        thread::spawn(move || {
            let ctx = tabibu_sys::default_scan_ctx(&[]);
            let freed = tabibu_engine::reclaim(&ctx, &items, &tabibu_sys::undo_dir())
                .map(|r| r.reclaimed_bytes)
                .unwrap_or(0);
            let _ = tx.send((freed, n));
        });
    }

    /// Large & Old scan: the Junk machinery filtered to just the `large_old`
    /// scanner (parity with the Tauri "Large & Old" view).
    fn start_large_scan(&mut self) {
        self.large.phase = JunkPhase::Scanning;
        self.large.items.clear();
        let (tx, rx) = channel();
        self.large.scan_rx = Some(rx);
        thread::spawn(move || {
            let ctx = tabibu_sys::default_scan_ctx(&[]);
            let cancel = CancelToken::new();
            let scanners: Vec<Box<dyn Scanner>> = tabibu_junk::scanners()
                .into_iter()
                .filter(|s| s.id() == "large_old")
                .collect();
            let collected: Mutex<Vec<CleanupItem>> = Mutex::new(Vec::new());
            let _ = tabibu_engine::smart_scan(&scanners, &ctx, &cancel, &|it| {
                collected.lock().expect("large sink poisoned").push(it);
            });
            let mut items = collected.into_inner().expect("large sink poisoned");
            items.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
            let _ = tx.send(items);
        });
    }

    fn start_large_delete(&mut self) {
        let items: Vec<CleanupItem> = self
            .large
            .items
            .iter()
            .filter(|i| i.selected)
            .cloned()
            .collect();
        if items.is_empty() {
            return;
        }
        let n = items.len();
        self.large.phase = JunkPhase::Deleting;
        let (tx, rx) = channel();
        self.large.del_rx = Some(rx);
        thread::spawn(move || {
            let ctx = tabibu_sys::default_scan_ctx(&[]);
            let freed = tabibu_engine::reclaim(&ctx, &items, &tabibu_sys::undo_dir())
                .map(|r| r.reclaimed_bytes)
                .unwrap_or(0);
            let _ = tx.send((freed, n));
        });
    }

    /// Security scan: run the malware scanners through the engine (allowed-roots
    /// = `/` so findings anywhere survive the guard). Reuses the cleanup machinery.
    fn start_security_scan(&mut self) {
        self.security.phase = JunkPhase::Scanning;
        self.security.items.clear();
        let (tx, rx) = channel();
        self.security.scan_rx = Some(rx);
        thread::spawn(move || {
            let ctx = reclaim_anywhere_ctx();
            let cancel = CancelToken::new();
            let collected: Mutex<Vec<CleanupItem>> = Mutex::new(Vec::new());
            let _ = tabibu_engine::smart_scan(&tabibu_malware::scanners(), &ctx, &cancel, &|it| {
                collected.lock().expect("security sink poisoned").push(it);
            });
            let mut items = collected.into_inner().expect("security sink poisoned");
            items.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
            let _ = tx.send(items);
        });
    }

    fn start_security_delete(&mut self) {
        let items: Vec<CleanupItem> = self
            .security
            .items
            .iter()
            .filter(|i| i.selected)
            .cloned()
            .collect();
        if items.is_empty() {
            return;
        }
        let n = items.len();
        self.security.phase = JunkPhase::Deleting;
        let (tx, rx) = channel();
        self.security.del_rx = Some(rx);
        thread::spawn(move || {
            let ctx = reclaim_anywhere_ctx();
            let freed = tabibu_engine::reclaim(&ctx, &items, &tabibu_sys::undo_dir())
                .map(|r| r.reclaimed_bytes)
                .unwrap_or(0);
            let _ = tx.send((freed, n));
        });
    }

    /// Start the live sampler thread once. It owns a long-lived `Sampler` (CPU
    /// deltas need a persistent `System`) and pushes a sample every ~1.2 s; it
    /// exits when the receiver is dropped (app close).
    fn ensure_monitor(&mut self) {
        if self.monitor.started {
            return;
        }
        self.monitor.started = true;
        let (tx, rx) = channel();
        self.monitor.rx = Some(rx);
        let by_cpu = self.monitor.by_cpu.clone();
        thread::spawn(move || {
            let mut sampler = Sampler::new();
            loop {
                let by = if by_cpu.load(Ordering::Relaxed) {
                    TopBy::Cpu
                } else {
                    TopBy::Memory
                };
                let sample = sampler.sample(40, by);
                if tx.send(sample).is_err() {
                    break; // UI gone
                }
                thread::sleep(Duration::from_millis(1200));
            }
        });
    }

    /// Find duplicate files across the home folder (reuses the Tauri default:
    /// whole-home walk, 4 KiB minimum). Runs the full dupe funnel on a worker.
    fn start_dupe_scan(&mut self) {
        self.dupes.phase = DupePhase::Scanning;
        self.dupes.groups.clear();
        self.dupes.selected.clear();
        let (tx, rx) = channel();
        self.dupes.scan_rx = Some(rx);
        thread::spawn(move || {
            // Whole home by default; `TABIBU_DUPE_ROOT` narrows it (fast testing).
            let root = std::env::var_os("TABIBU_DUPE_ROOT")
                .map(PathBuf::from)
                .unwrap_or_else(tabibu_sys::home_dir);
            let cancel = CancelToken::new();
            let cfg = DupeConfig::default();
            let res = tabibu_dupes::collect_candidates(&root, cfg.min_size, &cancel)
                .and_then(|files| tabibu_dupes::find_duplicates(&files, &cfg, &cancel, &|_g| {}))
                .map_err(|e| e.to_string());
            let _ = tx.send(res);
        });
    }

    /// Move the ticked duplicate copies to the Trash (reversible).
    fn start_dupe_delete(&mut self) {
        let items: Vec<CleanupItem> = self
            .dupes
            .groups
            .iter()
            .flat_map(|g| {
                g.paths
                    .iter()
                    .filter(|p| self.dupes.selected.contains(*p))
                    .map(move |p| {
                        let mut it = CleanupItem::new(
                            p.clone(),
                            Category::Duplicate,
                            g.size_bytes,
                            SafetyTier::Review,
                            "Duplicate copy",
                        );
                        it.selected = true;
                        it.action = ReclaimAction::Trash;
                        it
                    })
            })
            .collect();
        if items.is_empty() {
            return;
        }
        let n = items.len();
        self.dupes.phase = DupePhase::Deleting;
        let (tx, rx) = channel();
        self.dupes.del_rx = Some(rx);
        thread::spawn(move || {
            let ctx = reclaim_anywhere_ctx();
            let freed = tabibu_engine::reclaim(&ctx, &items, &tabibu_sys::undo_dir())
                .map(|r| r.reclaimed_bytes)
                .unwrap_or(0);
            let _ = tx.send((freed, n));
        });
    }

    /// Start the live throughput sampler (once) and kick a one-shot Wi-Fi probe.
    fn ensure_net(&mut self) {
        if self.net.started {
            return;
        }
        self.net.started = true;
        let (tx, rx) = channel();
        self.net.rx = Some(rx);
        thread::spawn(move || {
            let mut sampler = NetSampler::new();
            loop {
                let t = sampler.sample();
                if tx.send(t).is_err() {
                    break;
                }
                thread::sleep(Duration::from_millis(1000));
            }
        });
        let (wtx, wrx) = channel();
        self.net.wifi_rx = Some(wrx);
        thread::spawn(move || {
            let _ = wtx.send(tabibu_net::wifi_status());
        });
    }

    /// Run a connection test (ping + Wi-Fi) against Cloudflare on a worker.
    fn run_conn_test(&mut self) {
        self.net.conn_phase = ConnPhase::Testing;
        let (tx, rx) = channel();
        self.net.conn_rx = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(tabibu_net::connection_test("1.1.1.1"));
        });
    }
}

/// Drop nested descendants of a selected folder: trashing the folder removes the
/// child anyway, and the child's reclaim would fail once the parent is gone.
fn pruned_selection(sel: &HashMap<PathBuf, (u64, u64)>) -> Vec<PathBuf> {
    let mut paths: Vec<PathBuf> = sel.keys().cloned().collect();
    paths.sort(); // ancestors sort before descendants
    let mut kept: Vec<PathBuf> = Vec::new();
    for p in paths {
        if !kept.iter().any(|k| p.starts_with(k)) {
            kept.push(p);
        }
    }
    kept
}

/// Mark directories as expanded down to `max_depth` (test seam for screenshots).
fn expand_to_depth(node: &DirNode, max_depth: usize, out: &mut HashSet<PathBuf>) {
    if max_depth == 0 {
        return;
    }
    for c in &node.children {
        if c.is_dir && !c.children.is_empty() {
            out.insert(c.path.clone());
            expand_to_depth(c, max_depth - 1, out);
        }
    }
}

fn flatten(
    node: &DirNode,
    depth: usize,
    expanded: &HashSet<PathBuf>,
    guides: &mut Vec<bool>,
    out: &mut Vec<Row>,
) {
    let n = node.children.len();
    for (i, c) in node.children.iter().enumerate() {
        let is_last = i + 1 == n;
        let name = c
            .path
            .file_name()
            .map(|s| s.to_string_lossy().into_owned())
            .unwrap_or_else(|| c.path.to_string_lossy().into_owned());
        out.push(Row {
            depth,
            path: c.path.clone(),
            name,
            alloc: c.alloc_bytes,
            count: c.count,
            is_dir: c.is_dir,
            expandable: c.is_dir && !c.children.is_empty(),
            is_last,
            guides: guides.clone(),
        });
        if expanded.contains(&c.path) {
            guides.push(!is_last); // this level keeps a vertical line if more siblings follow
            flatten(c, depth + 1, expanded, guides, out);
            guides.pop();
        }
    }
}

impl eframe::App for TabibuApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Poll background work.
        if let Some(rx) = &self.scan_rx {
            if let Ok(res) = rx.try_recv() {
                self.scan_rx = None;
                match res {
                    Ok(root) => {
                        // Test seam: with TABIBU_SCAN_ROOT set, pre-expand the top
                        // levels so a headless screenshot shows the nested tree.
                        if std::env::var_os("TABIBU_SCAN_ROOT").is_some() {
                            expand_to_depth(&root, 2, &mut self.expanded);
                        }
                        self.root = Some(root);
                        self.phase = Phase::Browse;
                    }
                    Err(e) => self.phase = Phase::Error(e),
                }
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.del_rx {
            if let Ok((freed, items)) = rx.try_recv() {
                self.del_rx = None;
                self.phase = Phase::Done { freed, items };
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.junk.scan_rx {
            if let Ok(items) = rx.try_recv() {
                self.junk.scan_rx = None;
                self.junk.items = items;
                self.junk.phase = JunkPhase::Review;
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.junk.del_rx {
            if let Ok((freed, items)) = rx.try_recv() {
                self.junk.del_rx = None;
                self.junk.phase = JunkPhase::Done { freed, items };
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.large.scan_rx {
            if let Ok(items) = rx.try_recv() {
                self.large.scan_rx = None;
                self.large.items = items;
                self.large.phase = JunkPhase::Review;
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.large.del_rx {
            if let Ok((freed, items)) = rx.try_recv() {
                self.large.del_rx = None;
                self.large.phase = JunkPhase::Done { freed, items };
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.security.scan_rx {
            if let Ok(items) = rx.try_recv() {
                self.security.scan_rx = None;
                self.security.items = items;
                self.security.phase = JunkPhase::Review;
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.security.del_rx {
            if let Ok((freed, items)) = rx.try_recv() {
                self.security.del_rx = None;
                self.security.phase = JunkPhase::Done { freed, items };
            } else {
                ctx.request_repaint();
            }
        }
        // Drain live monitor samples (keep only the newest; track a short CPU
        // history for the sparkline).
        if let Some(rx) = &self.monitor.rx {
            while let Ok(s) = rx.try_recv() {
                self.monitor.cpu_hist.push_back(s.cpu_percent.min(100.0));
                while self.monitor.cpu_hist.len() > 60 {
                    self.monitor.cpu_hist.pop_front();
                }
                self.monitor.latest = Some(s);
            }
        }
        if self.view == View::Memory {
            self.ensure_monitor();
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if let Some(rx) = &self.dupes.scan_rx {
            if let Ok(res) = rx.try_recv() {
                self.dupes.scan_rx = None;
                match res {
                    Ok(groups) => {
                        // Default: keep the newest copy (paths[0]); pre-select the rest.
                        self.dupes.selected.clear();
                        for g in &groups {
                            for p in g.paths.iter().skip(1) {
                                self.dupes.selected.insert(p.clone());
                            }
                        }
                        self.dupes.groups = groups;
                        self.dupes.phase = DupePhase::Review;
                    }
                    Err(_) => self.dupes.phase = DupePhase::Idle,
                }
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.dupes.del_rx {
            if let Ok((freed, items)) = rx.try_recv() {
                self.dupes.del_rx = None;
                self.dupes.phase = DupePhase::Done { freed, items };
            } else {
                ctx.request_repaint();
            }
        }
        // Network: drain throughput, Wi-Fi probe, and connection-test results.
        if let Some(rx) = &self.net.rx {
            while let Ok(t) = rx.try_recv() {
                self.net.down_hist.push_back(t.down_bps);
                while self.net.down_hist.len() > 60 {
                    self.net.down_hist.pop_front();
                }
                self.net.latest = Some(t);
            }
        }
        if let Some(rx) = &self.net.wifi_rx {
            if let Ok(w) = rx.try_recv() {
                self.net.wifi_rx = None;
                self.net.wifi = Some(w);
            }
        }
        if let Some(rx) = &self.net.conn_rx {
            if let Ok(ct) = rx.try_recv() {
                self.net.conn_rx = None;
                self.net.wifi = Some(ct.wifi);
                self.net.ping = Some(ct.ping);
                self.net.conn_phase = ConnPhase::Done;
            } else {
                ctx.request_repaint();
            }
        }
        if self.view == View::Net {
            self.ensure_net();
            ctx.request_repaint_after(Duration::from_millis(700));
        }

        // Left navigation rail — more entries land here as the migration adds views.
        let p = palette(self.dark);
        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(180.0)
            .frame(egui::Frame {
                fill: p.bg_elev,
                inner_margin: egui::Margin::same(12),
                ..Default::default()
            })
            .show(ctx, |ui| {
                ui.add_space(6.0);
                ui.label(RichText::new("Tabibu").size(18.0).strong());
                ui.add_space(14.0);
                if nav_item(ui, "Space Analyzer", self.view == View::Space) {
                    self.view = View::Space;
                }
                if nav_item(ui, "Junk", self.view == View::Junk) {
                    self.view = View::Junk;
                }
                if nav_item(ui, "Large & Old", self.view == View::Large) {
                    self.view = View::Large;
                }
                if nav_item(ui, "Security", self.view == View::Security) {
                    self.view = View::Security;
                }
                if nav_item(ui, "Memory & CPU", self.view == View::Memory) {
                    self.view = View::Memory;
                }
                if nav_item(ui, "Duplicates", self.view == View::Dupes) {
                    self.view = View::Dupes;
                }
                if nav_item(ui, "Network", self.view == View::Net) {
                    self.view = View::Net;
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| match self.view {
            View::Space => match &self.phase {
                Phase::Idle => self.ui_landing(ui),
                Phase::Scanning => self.ui_center(ui, "Measuring your whole disk…", true),
                Phase::Deleting => self.ui_center(ui, "Moving selected items to the Trash…", true),
                Phase::Error(e) => {
                    let e = e.clone();
                    self.ui_center(ui, &format!("Couldn't read the disk: {e}"), false);
                }
                Phase::Done { freed, items } => {
                    let (freed, items) = (*freed, *items);
                    self.ui_done(ui, freed, items);
                }
                Phase::Browse => self.ui_tree(ui),
            },
            View::Junk => self.ui_junk(ui),
            View::Large => self.ui_large(ui),
            View::Security => self.ui_security(ui),
            View::Memory => self.ui_memory(ui),
            View::Dupes => self.ui_dupes(ui),
            View::Net => self.ui_net(ui),
        });
    }
}

/// One navigation-rail entry; returns true on click.
fn nav_item(ui: &mut egui::Ui, label: &str, active: bool) -> bool {
    ui.add_sized(
        [ui.available_width(), 32.0],
        egui::SelectableLabel::new(active, label),
    )
    .clicked()
}

impl TabibuApp {
    fn ui_landing(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            ui.label(RichText::new("Space Analyzer").size(26.0).strong());
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Scan the whole disk once; open any folder in place (no reload) and move \
                     whatever you don't need to the Trash — reversible.",
                )
                .color(p.text_dim),
            );
            ui.add_space(20.0);
            if ui
                .add(
                    egui::Button::new(RichText::new("Scan").size(15.0).color(Color32::WHITE))
                        .fill(p.accent),
                )
                .clicked()
            {
                self.start_scan();
            }
        });
    }

    fn ui_center(&self, ui: &mut egui::Ui, msg: &str, spinner: bool) {
        let p = palette(self.dark);
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.4);
            if spinner {
                ui.add(egui::Spinner::new().size(28.0).color(p.accent));
                ui.add_space(10.0);
            }
            ui.label(RichText::new(msg).color(p.text_dim));
        });
    }

    fn ui_done(&mut self, ui: &mut egui::Ui, freed: u64, items: usize) {
        let p = palette(self.dark);
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.35);
            ui.label(RichText::new("Removal complete").size(22.0).strong());
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!(
                    "{} moved to the Trash · {items} item(s)",
                    fmt_bytes(freed)
                ))
                .color(p.text_dim),
            );
            ui.add_space(16.0);
            if ui.button("Rescan").clicked() {
                self.start_scan();
            }
        });
    }

    fn ui_junk(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        match &self.junk.phase {
            JunkPhase::Idle => {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.label(RichText::new("Junk Cleanup").size(26.0).strong());
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Find caches, logs, temporary and leftover files across your Mac, \
                             then move what you don't need to the Trash — reversible.",
                        )
                        .color(p.text_dim),
                    );
                    ui.add_space(20.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Scan").size(15.0).color(Color32::WHITE),
                            )
                            .fill(p.accent),
                        )
                        .clicked()
                    {
                        self.start_junk_scan();
                    }
                });
            }
            JunkPhase::Scanning => self.ui_center(ui, "Scanning for junk…", true),
            JunkPhase::Deleting => self.ui_center(ui, "Moving selected items to the Trash…", true),
            JunkPhase::Done { freed, items } => {
                let (freed, items) = (*freed, *items);
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.label(RichText::new("Cleanup complete").size(22.0).strong());
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!(
                            "{} moved to the Trash · {items} item(s)",
                            fmt_bytes(freed)
                        ))
                        .color(p.text_dim),
                    );
                    ui.add_space(16.0);
                    if ui.button("Rescan").clicked() {
                        self.start_junk_scan();
                    }
                });
            }
            JunkPhase::Review => self.ui_junk_review(ui),
        }
    }

    fn ui_junk_review(&mut self, ui: &mut egui::Ui) {
        let act = cleanup_review_ui(ui, self.dark, "Junk", &mut self.junk.items);
        if act.rescan {
            self.start_junk_scan();
        }
        if act.trash {
            self.start_junk_delete();
        }
    }

    fn ui_large(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0x0e, 0xa5, 0xe9); // sky — webview "Large & Old" accent
        match &self.large.phase {
            JunkPhase::Idle => {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.label(RichText::new("Large & Old Files").size(26.0).strong());
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Find big, long-untouched files (mostly in Downloads) and move what \
                             you don't need to the Trash — reversible. Nothing is pre-selected.",
                        )
                        .color(p.text_dim),
                    );
                    ui.add_space(20.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Scan").size(15.0).color(Color32::WHITE),
                            )
                            .fill(accent),
                        )
                        .clicked()
                    {
                        self.start_large_scan();
                    }
                });
            }
            JunkPhase::Scanning => self.ui_center(ui, "Scanning for large, old files…", true),
            JunkPhase::Deleting => self.ui_center(ui, "Moving selected items to the Trash…", true),
            JunkPhase::Done { freed, items } => {
                let (freed, items) = (*freed, *items);
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.label(RichText::new("Done").size(22.0).strong());
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!(
                            "{} moved to the Trash · {items} item(s)",
                            fmt_bytes(freed)
                        ))
                        .color(p.text_dim),
                    );
                    ui.add_space(16.0);
                    if ui.button("Rescan").clicked() {
                        self.start_large_scan();
                    }
                });
            }
            JunkPhase::Review => {
                let act = cleanup_review_ui(ui, self.dark, "Large & Old", &mut self.large.items);
                if act.rescan {
                    self.start_large_scan();
                }
                if act.trash {
                    self.start_large_delete();
                }
            }
        }
    }

    fn ui_security(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0xec, 0x48, 0x99); // pink — webview security accent
        match &self.security.phase {
            JunkPhase::Idle => {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.label(RichText::new("Security Scan").size(26.0).strong());
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Scan for known adware, malware and rogue configuration profiles. \
                             Anything found is shown for review before it's quarantined to the \
                             Trash — reversible.",
                        )
                        .color(p.text_dim),
                    );
                    ui.add_space(20.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Scan").size(15.0).color(Color32::WHITE),
                            )
                            .fill(accent),
                        )
                        .clicked()
                    {
                        self.start_security_scan();
                    }
                });
            }
            JunkPhase::Scanning => self.ui_center(ui, "Scanning for threats…", true),
            JunkPhase::Deleting => self.ui_center(ui, "Quarantining to the Trash…", true),
            JunkPhase::Done { freed, items } => {
                let (freed, items) = (*freed, *items);
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.label(RichText::new("Done").size(22.0).strong());
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!(
                            "{} moved to the Trash · {items} item(s)",
                            fmt_bytes(freed)
                        ))
                        .color(p.text_dim),
                    );
                    ui.add_space(16.0);
                    if ui.button("Rescan").clicked() {
                        self.start_security_scan();
                    }
                });
            }
            JunkPhase::Review => {
                let act = cleanup_review_ui(ui, self.dark, "Security", &mut self.security.items);
                if act.rescan {
                    self.start_security_scan();
                }
                if act.trash {
                    self.start_security_delete();
                }
            }
        }
    }

    fn ui_memory(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        // Orange — matches the webview build's Memory & CPU accent.
        let accent = Color32::from_rgb(0xf9, 0x73, 0x16);

        let Some(s) = self.monitor.latest.clone() else {
            self.ui_center(ui, "Sampling system activity…", true);
            return;
        };

        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Memory & CPU").size(16.0).strong());
        });
        ui.add_space(8.0);

        // CPU dial + memory/swap stats.
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(130.0, 118.0), Sense::hover());
            let center = Pos2::new(rect.center().x, rect.min.y + 50.0);
            draw_ring(ui, center, 40.0, s.cpu_percent / 100.0, accent, p.track);
            ui.painter().text(
                center,
                egui::Align2::CENTER_CENTER,
                format!("{}%", s.cpu_percent.round() as i64),
                FontId::proportional(20.0),
                p.text,
            );
            ui.painter().text(
                Pos2::new(center.x, rect.max.y - 4.0),
                egui::Align2::CENTER_BOTTOM,
                "CPU",
                FontId::proportional(12.0),
                p.text_dim,
            );

            ui.add_space(16.0);
            let mem_frac = s.used_memory_bytes as f32 / s.total_memory_bytes.max(1) as f32;
            ui.vertical(|ui| {
                ui.add_space(20.0);
                ui.label(
                    RichText::new(format!(
                        "Memory  {} / {}",
                        fmt_ram(s.used_memory_bytes),
                        fmt_ram(s.total_memory_bytes)
                    ))
                    .strong(),
                );
                ui.add_space(6.0);
                let (bar, _) = ui.allocate_exact_size(Vec2::new(360.0, 8.0), Sense::hover());
                ui.painter()
                    .rect_filled(bar, CornerRadius::same(4), p.track);
                ui.painter().rect_filled(
                    Rect::from_min_size(
                        bar.min,
                        Vec2::new(bar.width() * mem_frac.clamp(0.0, 1.0), bar.height()),
                    ),
                    CornerRadius::same(4),
                    accent,
                );
                ui.add_space(10.0);
                ui.label(
                    RichText::new(format!("Swap used  {}", fmt_ram(s.used_swap_bytes)))
                        .color(p.text_dim),
                );
            });
        });

        // Live CPU history sparkline.
        let hist: Vec<f32> = self.monitor.cpu_hist.iter().copied().collect();
        if hist.len() >= 2 {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let w = (ui.available_width() - 8.0).max(60.0);
                let (strip, _) = ui.allocate_exact_size(Vec2::new(w, 40.0), Sense::hover());
                ui.painter()
                    .rect_filled(strip, CornerRadius::same(4), p.bg_elev);
                let dx = strip.width() / (hist.len() as f32 - 1.0);
                let pts: Vec<Pos2> = hist
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        Pos2::new(
                            strip.min.x + i as f32 * dx,
                            strip.max.y - (v / 100.0).clamp(0.0, 1.0) * strip.height(),
                        )
                    })
                    .collect();
                ui.painter()
                    .add(Shape::line(pts, Stroke::new(1.5_f32, accent)));
            });
        }

        ui.add_space(12.0);
        ui.separator();

        // Top processes + sort toggle.
        let by_cpu = self.monitor.by_cpu.load(Ordering::Relaxed);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Top processes").strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.selectable_label(!by_cpu, "By Memory").clicked() {
                    self.monitor.by_cpu.store(false, Ordering::Relaxed);
                }
                if ui.selectable_label(by_cpu, "By CPU").clicked() {
                    self.monitor.by_cpu.store(true, Ordering::Relaxed);
                }
            });
        });
        ui.add_space(4.0);

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for proc in &s.top_processes {
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(proc.name.as_str());
                        if proc.is_translated == Some(true) {
                            ui.label(RichText::new("Rosetta").size(10.0).color(accent));
                        }
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(8.0);
                            ui.label(
                                RichText::new(fmt_bytes(proc.memory_bytes))
                                    .monospace()
                                    .color(p.text_dim),
                            );
                            ui.add_space(16.0);
                            ui.label(
                                RichText::new(format!("{}%", proc.cpu_percent.round() as i64))
                                    .monospace(),
                            );
                        });
                    });
                }
            });
    }

    fn ui_dupes(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0x3b, 0x82, 0xf6); // blue — matches webview dupes accent
        match &self.dupes.phase {
            DupePhase::Idle => {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.label(RichText::new("Find Duplicate Files").size(26.0).strong());
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Scan your home folder for byte-identical copies, then move the extra \
                             copies to the Trash — reversible. The newest copy is kept by default.",
                        )
                        .color(p.text_dim),
                    );
                    ui.add_space(20.0);
                    if ui
                        .add(
                            egui::Button::new(
                                RichText::new("Scan").size(15.0).color(Color32::WHITE),
                            )
                            .fill(accent),
                        )
                        .clicked()
                    {
                        self.start_dupe_scan();
                    }
                });
            }
            DupePhase::Scanning => self.ui_center(ui, "Scanning for duplicate files…", true),
            DupePhase::Deleting => self.ui_center(ui, "Moving duplicates to the Trash…", true),
            DupePhase::Done { freed, items } => {
                let (freed, items) = (*freed, *items);
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.label(RichText::new("Done").size(22.0).strong());
                    ui.add_space(6.0);
                    ui.label(
                        RichText::new(format!(
                            "{} moved to the Trash · {items} copies",
                            fmt_bytes(freed)
                        ))
                        .color(p.text_dim),
                    );
                    ui.add_space(16.0);
                    if ui.button("Rescan").clicked() {
                        self.start_dupe_scan();
                    }
                });
            }
            DupePhase::Review => self.ui_dupes_review(ui, accent),
        }
    }

    fn ui_dupes_review(&mut self, ui: &mut egui::Ui, accent: Color32) {
        let p = palette(self.dark);
        let total_reclaim: u64 = self
            .dupes
            .groups
            .iter()
            .map(|g| {
                g.paths
                    .iter()
                    .filter(|pp| self.dupes.selected.contains(*pp))
                    .count() as u64
                    * g.size_bytes
            })
            .sum();
        let sel_n = self.dupes.selected.len();

        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new(format!("{} duplicate sets", self.dupes.groups.len()))
                    .size(16.0)
                    .strong(),
            );
            ui.label(
                RichText::new(format!("· up to {} reclaimable", fmt_bytes(total_reclaim)))
                    .color(p.text_dim),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.button("Rescan").clicked() {
                    self.start_dupe_scan();
                }
            });
        });
        ui.add_space(4.0);
        ui.separator();

        if self.dupes.groups.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No duplicate files found.").color(p.text_dim));
            });
            return;
        }

        let mut toggle: Option<PathBuf> = None;
        let avail_h = ui.available_height() - 52.0;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(avail_h.max(0.0))
            .show(ui, |ui| {
                for g in &self.dupes.groups {
                    ui.add_space(10.0);
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(
                            RichText::new(format!("{} each", fmt_bytes(g.size_bytes))).strong(),
                        );
                        ui.label(
                            RichText::new(format!("· {} copies", g.paths.len())).color(p.text_dim),
                        );
                    });
                    for (idx, path) in g.paths.iter().enumerate() {
                        let on = self.dupes.selected.contains(path);
                        ui.horizontal(|ui| {
                            ui.add_space(24.0);
                            let mut checked = on;
                            if ui.checkbox(&mut checked, "").changed() {
                                toggle = Some(path.clone());
                            }
                            if idx == 0 {
                                ui.label(RichText::new("keep").size(10.0).color(accent));
                            }
                            ui.add(
                                egui::Label::new(
                                    RichText::new(path.to_string_lossy()).color(if on {
                                        p.text
                                    } else {
                                        p.text_dim
                                    }),
                                )
                                .truncate(),
                            );
                        });
                    }
                }
            });

        if let Some(path) = toggle {
            if !self.dupes.selected.remove(&path) {
                self.dupes.selected.insert(path);
            }
        }

        ui.separator();
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            if sel_n == 0 {
                ui.label(
                    RichText::new("Tick the copies to remove (the newest is kept by default).")
                        .color(p.text_dim),
                );
            } else {
                ui.label(
                    RichText::new(format!("{sel_n} selected · {}", fmt_bytes(total_reclaim)))
                        .strong(),
                );
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                let btn = egui::Button::new(RichText::new("Move to Trash").color(Color32::WHITE))
                    .fill(if sel_n == 0 {
                        p.text_faint
                    } else {
                        Color32::from_rgb(0xdc, 0x26, 0x26)
                    });
                if ui.add_enabled(sel_n > 0, btn).clicked() {
                    self.start_dupe_delete();
                }
            });
        });
    }

    fn ui_net(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0x02, 0x84, 0xc7); // blue — webview network accent

        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Network").size(16.0).strong());
        });
        ui.add_space(10.0);

        // Live throughput.
        let (down, up, tdown, tup) = self
            .net
            .latest
            .map(|t| (t.down_bps, t.up_bps, t.total_down_bytes, t.total_up_bytes))
            .unwrap_or((0, 0, 0, 0));
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Download").color(p.text_dim));
                ui.label(
                    RichText::new(format!("{}/s", fmt_bytes(down)))
                        .size(22.0)
                        .strong()
                        .color(accent),
                );
                ui.label(
                    RichText::new(format!("{} total", fmt_bytes(tdown)))
                        .size(11.0)
                        .color(p.text_faint),
                );
            });
            ui.add_space(40.0);
            ui.vertical(|ui| {
                ui.label(RichText::new("Upload").color(p.text_dim));
                ui.label(
                    RichText::new(format!("{}/s", fmt_bytes(up)))
                        .size(22.0)
                        .strong(),
                );
                ui.label(
                    RichText::new(format!("{} total", fmt_bytes(tup)))
                        .size(11.0)
                        .color(p.text_faint),
                );
            });
        });

        // Download sparkline (auto-scaled to its own peak).
        let hist: Vec<u64> = self.net.down_hist.iter().copied().collect();
        if hist.len() >= 2 {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let w = (ui.available_width() - 8.0).max(60.0);
                let (strip, _) = ui.allocate_exact_size(Vec2::new(w, 40.0), Sense::hover());
                ui.painter()
                    .rect_filled(strip, CornerRadius::same(4), p.bg_elev);
                let maxv = hist.iter().copied().max().unwrap_or(1).max(1) as f32;
                let dx = strip.width() / (hist.len() as f32 - 1.0);
                let pts: Vec<Pos2> = hist
                    .iter()
                    .enumerate()
                    .map(|(i, v)| {
                        Pos2::new(
                            strip.min.x + i as f32 * dx,
                            strip.max.y - (*v as f32 / maxv) * strip.height(),
                        )
                    })
                    .collect();
                ui.painter()
                    .add(Shape::line(pts, Stroke::new(1.5_f32, accent)));
            });
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Wi-Fi").strong());
        });
        match &self.net.wifi {
            None => {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Checking…").color(p.text_dim));
                });
            }
            Some(w) if !w.connected => {
                ui.horizontal(|ui| {
                    ui.add_space(8.0);
                    ui.label(RichText::new("Not connected to Wi-Fi.").color(p.text_dim));
                });
            }
            Some(w) => {
                let em_dash = || "—".to_string();
                let rows = [
                    (
                        "Signal",
                        w.quality_pct.map_or_else(em_dash, |q| format!("{q}%")),
                    ),
                    (
                        "RSSI",
                        w.rssi_dbm.map_or_else(em_dash, |r| format!("{r} dBm")),
                    ),
                    (
                        "Link rate",
                        w.tx_rate_mbps.map_or_else(em_dash, |t| format!("{t} Mbps")),
                    ),
                    ("Mode", w.phy_mode.clone().unwrap_or_else(em_dash)),
                ];
                for (k, v) in rows {
                    ui.horizontal(|ui| {
                        ui.add_space(8.0);
                        ui.label(RichText::new(k).color(p.text_dim));
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(8.0);
                            ui.label(v);
                        });
                    });
                }
            }
        }

        ui.add_space(12.0);
        ui.separator();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Connection").strong());
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                let testing = matches!(self.net.conn_phase, ConnPhase::Testing);
                if ui
                    .add_enabled(
                        !testing,
                        egui::Button::new(if testing {
                            "Testing…"
                        } else {
                            "Test connection"
                        }),
                    )
                    .clicked()
                {
                    self.run_conn_test();
                }
            });
        });
        if let (ConnPhase::Done, Some(ping)) = (&self.net.conn_phase, &self.net.ping) {
            let desc = if ping.received == 0 {
                "No response — you appear to be offline.".to_string()
            } else {
                format!(
                    "{:.0}% loss · {} avg · {}/{} replies",
                    ping.loss_pct,
                    ping.avg_ms
                        .map_or_else(|| "—".to_string(), |m| format!("{m:.0} ms")),
                    ping.received,
                    ping.transmitted
                )
            };
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(desc).color(p.text_dim));
            });
        }
    }

    fn ui_tree(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let total = self.root.as_ref().map(|r| r.alloc_bytes).unwrap_or(0);
        let count = self.root.as_ref().map(|r| r.count).unwrap_or(0);

        // Header
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Macintosh HD").size(16.0).strong());
            ui.label(
                RichText::new(format!("· {} on disk · {} items", fmt_bytes(total), count))
                    .color(p.text_dim),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                if ui.button("Rescan").clicked() {
                    self.start_scan();
                }
            });
        });
        ui.add_space(4.0);
        ui.separator();

        // Flatten the expanded branches into owned rows (tree already fully in
        // memory — no re-walk on expand).
        let mut rows: Vec<Row> = Vec::new();
        if let Some(root) = &self.root {
            let mut guides: Vec<bool> = Vec::new();
            flatten(root, 0, &self.expanded, &mut guides, &mut rows);
        }

        let mut toggle_expand: Option<PathBuf> = None;
        let mut toggle_sel: Option<(PathBuf, u64, u64)> = None;

        let avail_h = ui.available_height() - 52.0; // leave room for the action bar
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height(avail_h.max(0.0))
            .show_rows(ui, ROW_H, rows.len(), |ui, range| {
                for i in range {
                    let r = &rows[i];
                    let (rect, resp) = ui.allocate_exact_size(
                        Vec2::new(ui.available_width(), ROW_H),
                        Sense::click(),
                    );
                    if resp.hovered() {
                        ui.painter()
                            .rect_filled(rect, CornerRadius::same(4), p.hover);
                    }
                    let sel_on = self.selected.contains_key(&r.path);
                    if sel_on {
                        ui.painter().rect_filled(rect, CornerRadius::same(4), p.sel);
                    }
                    let cy = rect.center().y;

                    // ---- tree guide lines (├ └ │) so nesting reads as branches ----
                    let indent = 16.0;
                    let left = rect.min.x + 14.0;
                    let col = |j: usize| left + (j as f32) * indent + 7.0;
                    let gst = Stroke::new(1.0_f32, p.border);
                    if r.depth >= 1 {
                        // vertical lines for ancestors (above the immediate parent)
                        // that still have following siblings
                        for j in 0..r.depth.saturating_sub(1) {
                            if r.guides.get(j).copied().unwrap_or(false) {
                                let gx = col(j);
                                ui.painter().line_segment(
                                    [Pos2::new(gx, rect.top()), Pos2::new(gx, rect.bottom())],
                                    gst,
                                );
                            }
                        }
                        // connector elbow to the parent, at the parent's column
                        let gx = col(r.depth - 1);
                        ui.painter()
                            .line_segment([Pos2::new(gx, rect.top()), Pos2::new(gx, cy)], gst);
                        if !r.is_last {
                            ui.painter().line_segment(
                                [Pos2::new(gx, cy), Pos2::new(gx, rect.bottom())],
                                gst,
                            );
                        }
                        ui.painter().line_segment(
                            [Pos2::new(gx, cy), Pos2::new(gx + indent * 0.7, cy)],
                            gst,
                        );
                    }

                    let mut x = left + (r.depth as f32) * indent;

                    // disclosure
                    let disc = Rect::from_center_size(Pos2::new(x + 7.0, cy), Vec2::splat(16.0));
                    if r.expandable {
                        draw_chevron(ui, disc, self.expanded.contains(&r.path), p.text_faint);
                        if ui
                            .interact(disc, resp.id.with("disc"), Sense::click())
                            .clicked()
                        {
                            toggle_expand = Some(r.path.clone());
                        }
                    }
                    x += 20.0;

                    // checkbox
                    let cb = Rect::from_center_size(Pos2::new(x + 8.0, cy), Vec2::splat(16.0));
                    let cb_resp = ui.interact(cb, resp.id.with("cb"), Sense::click());
                    ui.painter().rect_stroke(
                        cb,
                        CornerRadius::same(3),
                        Stroke::new(1.3_f32, if sel_on { p.accent } else { p.text_faint }),
                        StrokeKind::Middle,
                    );
                    if sel_on {
                        ui.painter()
                            .rect_filled(cb, CornerRadius::same(3), p.accent);
                        // check mark
                        let c = cb.center();
                        let st = Stroke::new(1.6_f32, Color32::WHITE);
                        ui.painter().line_segment(
                            [Pos2::new(c.x - 3.0, c.y), Pos2::new(c.x - 0.5, c.y + 2.5)],
                            st,
                        );
                        ui.painter().line_segment(
                            [
                                Pos2::new(c.x - 0.5, c.y + 2.5),
                                Pos2::new(c.x + 3.5, c.y - 2.5),
                            ],
                            st,
                        );
                    }
                    if cb_resp.clicked() {
                        toggle_sel = Some((r.path.clone(), r.alloc, r.count));
                    }
                    x += 20.0;

                    // icon
                    let icon_rect =
                        Rect::from_center_size(Pos2::new(x + 10.0, cy), Vec2::splat(18.0));
                    if r.is_dir {
                        draw_folder(ui, icon_rect, p.text_faint);
                    } else {
                        draw_file(ui, icon_rect, p.text_faint);
                    }
                    x += 24.0;

                    // name + subtext
                    let name_w = (rect.max.x - 230.0 - x).max(60.0);
                    let name_rect =
                        Rect::from_min_size(Pos2::new(x, rect.min.y), Vec2::new(name_w, ROW_H));
                    ui.painter().text(
                        Pos2::new(name_rect.min.x, cy - 7.0),
                        egui::Align2::LEFT_TOP,
                        elide(&r.name, name_w, 13.0),
                        FontId::proportional(13.0),
                        p.text,
                    );
                    let sub = if r.is_dir {
                        format!("{} items", r.count)
                    } else {
                        "File".to_string()
                    };
                    ui.painter().text(
                        Pos2::new(name_rect.min.x, cy + 6.0),
                        egui::Align2::LEFT_TOP,
                        sub,
                        FontId::proportional(10.0),
                        p.text_dim,
                    );

                    // size bar
                    let frac = if total > 0 {
                        r.alloc as f32 / total as f32
                    } else {
                        0.0
                    };
                    let bar = Rect::from_min_size(
                        Pos2::new(rect.max.x - 210.0, cy - 3.0),
                        Vec2::new(70.0, 6.0),
                    );
                    ui.painter()
                        .rect_filled(bar, CornerRadius::same(3), p.track);
                    let fillw = (bar.width() * frac).max(2.0).min(bar.width());
                    ui.painter().rect_filled(
                        Rect::from_min_size(bar.min, Vec2::new(fillw, bar.height())),
                        CornerRadius::same(3),
                        p.accent,
                    );

                    // size text (right aligned)
                    ui.painter().text(
                        Pos2::new(rect.max.x - 12.0, cy),
                        egui::Align2::RIGHT_CENTER,
                        fmt_bytes(r.alloc),
                        FontId::monospace(12.0),
                        p.text_dim,
                    );

                    // row click (not on controls) toggles expand for folders
                    if resp.clicked() && r.expandable {
                        toggle_expand = Some(r.path.clone());
                    }
                }
            });

        if let Some(p) = toggle_expand {
            if !self.expanded.remove(&p) {
                self.expanded.insert(p);
            }
        }
        if let Some((path, bytes, cnt)) = toggle_sel {
            if self.selected.remove(&path).is_none() {
                self.selected.insert(path, (bytes, cnt));
            }
        }

        // Action bar
        ui.separator();
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let pruned = pruned_selection(&self.selected);
            let bytes: u64 = pruned
                .iter()
                .filter_map(|p| self.selected.get(p))
                .map(|v| v.0)
                .sum();
            let n = pruned.len();
            if n == 0 {
                ui.label(
                    RichText::new(
                        "Open folders to explore; tick anything to remove it (reversible).",
                    )
                    .color(p.text_dim),
                );
            } else {
                ui.label(RichText::new(format!("{n} selected · {}", fmt_bytes(bytes))).strong());
            }
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(8.0);
                let btn = egui::Button::new(RichText::new("Move to Trash").color(Color32::WHITE))
                    .fill(if n == 0 {
                        p.text_faint
                    } else {
                        Color32::from_rgb(0xdc, 0x26, 0x26)
                    });
                if ui.add_enabled(n > 0, btn).clicked() {
                    self.start_delete();
                }
                if n > 0 && ui.button("Clear").clicked() {
                    self.selected.clear();
                }
            });
        });
    }
}

/// Rough text elision to a pixel width (avg glyph ~0.55*size).
fn elide(s: &str, max_w: f32, size: f32) -> String {
    let max_chars = (max_w / (size * 0.55)).floor() as usize;
    if s.chars().count() <= max_chars || max_chars < 2 {
        s.to_string()
    } else {
        let keep: String = s.chars().take(max_chars.saturating_sub(1)).collect();
        format!("{keep}…")
    }
}

fn main() -> eframe::Result<()> {
    let opts = eframe::NativeOptions {
        viewport: egui::ViewportBuilder::default()
            .with_inner_size([1120.0, 740.0])
            .with_min_inner_size([900.0, 600.0])
            .with_title("Tabibu"),
        ..Default::default()
    };
    eframe::run_native(
        "Tabibu",
        opts,
        Box::new(|cc| {
            let dark = cc.egui_ctx.style().visuals.dark_mode;
            apply_theme(&cc.egui_ctx, dark);
            let mut app = TabibuApp {
                dark,
                ..Default::default()
            };
            // Auto-start a scan when a root is pre-set (power users / testing);
            // otherwise show the landing with a Scan button.
            if std::env::var_os("TABIBU_SCAN_ROOT").is_some() {
                app.start_scan();
            }
            // Test seam: `TABIBU_VIEW=junk` opens the Junk view and kicks off a
            // scan (for headless screenshots of the cleanup UI).
            match std::env::var("TABIBU_VIEW").as_deref() {
                Ok("junk") => {
                    app.view = View::Junk;
                    app.start_junk_scan();
                }
                Ok("memory") => app.view = View::Memory,
                Ok("large") => {
                    app.view = View::Large;
                    app.start_large_scan();
                }
                Ok("security") => {
                    app.view = View::Security;
                    app.start_security_scan();
                }
                Ok("dupes") => {
                    app.view = View::Dupes;
                    app.start_dupe_scan();
                }
                Ok("net") => {
                    app.view = View::Net;
                    app.run_conn_test();
                }
                _ => {}
            }
            Ok(Box::new(app))
        }),
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn fmt_bytes_is_decimal() {
        assert_eq!(fmt_bytes(0), "0 B");
        assert_eq!(fmt_bytes(999), "999 B");
        assert_eq!(fmt_bytes(1_000), "1.0 KB");
        assert_eq!(fmt_bytes(1_500_000), "1.5 MB");
        assert_eq!(fmt_bytes(2_000_000_000), "2.0 GB");
    }

    #[test]
    fn fmt_ram_is_binary() {
        assert_eq!(fmt_ram(1_024), "1.0 KB");
        // 16 GiB of physical RAM must read "16 GB", not 17.2.
        assert_eq!(fmt_ram(16 * 1_024 * 1_024 * 1_024), "16 GB");
    }

    #[test]
    fn pruned_selection_drops_nested_descendants() {
        let mut sel: HashMap<PathBuf, (u64, u64)> = HashMap::new();
        sel.insert(PathBuf::from("/a"), (10, 1));
        sel.insert(PathBuf::from("/a/b"), (5, 1)); // inside /a — dropped
        sel.insert(PathBuf::from("/c"), (7, 1));
        let kept = pruned_selection(&sel);
        assert!(kept.contains(&PathBuf::from("/a")));
        assert!(kept.contains(&PathBuf::from("/c")));
        assert!(!kept.contains(&PathBuf::from("/a/b")));
        assert_eq!(kept.len(), 2);
    }
}

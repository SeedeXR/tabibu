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

use std::collections::{HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::mpsc::{channel, Receiver};
use std::thread;

use eframe::egui::{
    self, Align, Color32, CornerRadius, FontId, Layout, Pos2, Rect, RichText, Sense, Shape, Stroke,
    StrokeKind, Vec2,
};
use tabibu_engine::{CancelToken, Category, CleanupItem, ReclaimAction, SafetyTier, ScanCtx};
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

fn home_dir() -> PathBuf {
    std::env::var_os("HOME")
        .map(PathBuf::from)
        .unwrap_or_default()
}

fn undo_dir() -> PathBuf {
    home_dir().join("Library/Application Support/Tabibu/undo")
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------
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
    phase: Phase,
    root: Option<DirNode>,
    expanded: HashSet<PathBuf>,
    /// path -> (physical bytes, descendant count) for selected rows.
    selected: HashMap<PathBuf, (u64, u64)>,
    scan_rx: Option<Receiver<Result<DirNode, String>>>,
    del_rx: Option<Receiver<(u64, usize)>>,
}

impl Default for TabibuApp {
    fn default() -> Self {
        Self {
            dark: true,
            phase: Phase::Idle,
            root: None,
            expanded: HashSet::new(),
            selected: HashMap::new(),
            scan_rx: None,
            del_rx: None,
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
            let ctx = ScanCtx {
                home: home_dir(),
                allowed_roots: vec![PathBuf::from("/")],
                running_bundle_ids: HashSet::new(),
                full_disk_access: true,
            };
            let freed = match tabibu_engine::reclaim(&ctx, &items, &undo_dir()) {
                Ok(r) => r.reclaimed_bytes,
                Err(_) => 0,
            };
            let _ = tx.send((freed, n));
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

        egui::CentralPanel::default().show(ctx, |ui| match &self.phase {
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
        });
    }
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
                    let gst = Stroke::new(1.0, p.border);
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
            Ok(Box::new(app))
        }),
    )
}

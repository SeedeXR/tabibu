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

mod shell;
use shell::{Shell, ShellEvent};

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
    sidebar: Color32,
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
            sidebar: Color32::from_rgb(0x16, 0x17, 0x19),
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
            sidebar: Color32::from_rgb(0xec, 0xec, 0xef),
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

/// Fonts: 42dot Sans (bundled, subset to Latin + the symbols the UI uses) as the
/// proportional UI family — Medium body, SemiBold headings — Inter Bold for every
/// number metric, and the system SF Mono for file paths/code only (runtime-loaded,
/// not bundled, per Apple font licensing; falls back to egui's default mono).
fn install_fonts(ctx: &egui::Context) {
    use egui::FontFamily;
    let mut fonts = egui::FontDefinitions::default();
    // 42dot Sans (subset) — Medium (500) body, SemiBold (600) headings — plus
    // Inter Bold (700) for every number metric.
    for (name, bytes) in [
        (
            "Sans",
            &include_bytes!("../assets/42dotSans-Medium.ttf")[..],
        ),
        (
            "SansSemiBold",
            &include_bytes!("../assets/42dotSans-SemiBold.ttf")[..],
        ),
        ("InterBold", &include_bytes!("../assets/Inter-Bold.ttf")[..]),
        ("lucide", &include_bytes!("../assets/lucide.ttf")[..]),
    ] {
        fonts.font_data.insert(
            name.to_owned(),
            Arc::new(egui::FontData::from_static(bytes)),
        );
    }
    // Lucide icon font (subset) — the exact nav icons the webview uses.
    fonts
        .families
        .insert(FontFamily::Name("lucide".into()), vec!["lucide".to_owned()]);
    fonts
        .families
        .entry(FontFamily::Proportional)
        .or_default()
        .insert(0, "Sans".to_owned());
    fonts.families.insert(
        FontFamily::Name("semibold".into()),
        vec!["SansSemiBold".to_owned(), "Sans".to_owned()],
    );
    fonts.families.insert(
        FontFamily::Name("metric".into()),
        vec!["InterBold".to_owned()],
    );
    // System SF Mono — kept for file paths / code only (not number metrics).
    for path in [
        "/System/Library/Fonts/SFNSMono.ttf",
        "/System/Library/Fonts/Menlo.ttc",
    ] {
        if let Ok(bytes) = std::fs::read(path) {
            fonts.font_data.insert(
                "Mono".to_owned(),
                Arc::new(egui::FontData::from_owned(bytes)),
            );
            fonts
                .families
                .entry(FontFamily::Monospace)
                .or_default()
                .insert(0, "Mono".to_owned());
            break;
        }
    }
    ctx.set_fonts(fonts);
}

/// A number metric: Inter Bold (the chosen numeric face) — every value, size,
/// percentage and count.
fn metric(text: impl Into<String>) -> RichText {
    RichText::new(text).family(egui::FontFamily::Name("metric".into()))
}

/// Semibold `RichText` (Space Grotesk 600) for headings/emphasis — real weight
/// contrast over the Medium body (egui's `.strong()` only recolours).
fn semibold(text: impl Into<String>) -> RichText {
    RichText::new(text).family(egui::FontFamily::Name("semibold".into()))
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
    // Lean, Zed-like scrollbars: a thin floating handle that only thickens on
    // hover, instead of egui's chunky always-on bar.
    let mut scroll = egui::style::ScrollStyle::thin();
    scroll.floating = true;
    scroll.bar_width = 6.0;
    scroll.floating_width = 4.0;
    scroll.handle_min_length = 24.0;
    scroll.bar_inner_margin = 2.0;
    scroll.bar_outer_margin = 0.0;
    style.spacing.scroll = scroll;
    // Match the webview's type scale (13px base, SF Mono for values) instead of
    // egui's looser 14px default — the biggest single lever on "designed" text.
    use egui::{FontFamily, FontId, TextStyle};
    style.text_styles = [
        (
            TextStyle::Heading,
            FontId::new(17.0, FontFamily::Proportional),
        ),
        (TextStyle::Body, FontId::new(13.0, FontFamily::Proportional)),
        (
            TextStyle::Button,
            FontId::new(13.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Small,
            FontId::new(11.0, FontFamily::Proportional),
        ),
        (
            TextStyle::Monospace,
            FontId::new(12.5, FontFamily::Monospace),
        ),
    ]
    .into();
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

/// The Tabibu brand glyph: a "pulse / activity" line (the healer's vitals
/// trace), drawn inside `rect`. Round-joined so it reads crisp at logo size.
fn draw_pulse(ui: &egui::Ui, rect: Rect, color: Color32) {
    let r = rect.shrink2(Vec2::new(rect.width() * 0.18, rect.height() * 0.30));
    let y = r.center().y;
    let (x0, x1) = (r.left(), r.right());
    let w = x1 - x0;
    let pts = vec![
        Pos2::new(x0, y),
        Pos2::new(x0 + w * 0.28, y),
        Pos2::new(x0 + w * 0.42, r.top()),
        Pos2::new(x0 + w * 0.58, r.bottom()),
        Pos2::new(x0 + w * 0.72, y),
        Pos2::new(x1, y),
    ];
    let stroke = egui::epaint::PathStroke::new(1.8_f32, color);
    ui.painter()
        .add(egui::epaint::Shape::Path(egui::epaint::PathShape {
            points: pts,
            closed: false,
            fill: Color32::TRANSPARENT,
            stroke,
        }));
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

/// A circular progress ring (CPU/charge/disk dial): a faint full-circle track
/// with a foreground arc covering `frac`, starting at 12 o'clock and clockwise,
/// with ROUNDED end caps — matching the webview build's `stroke-linecap="round"`.
fn draw_ring(ui: &egui::Ui, center: Pos2, radius: f32, frac: f32, color: Color32, track: Color32) {
    let painter = ui.painter();
    let thickness = 8.0_f32;
    painter.circle_stroke(center, radius, Stroke::new(thickness, track));
    let frac = frac.clamp(0.0, 1.0);
    if frac <= 0.0 {
        return;
    }
    // One segment per ~3.75° keeps the arc visually smooth at any size.
    let steps = (frac * 96.0).ceil().max(2.0) as usize;
    let start = -std::f32::consts::FRAC_PI_2; // 12 o'clock
    let pts: Vec<Pos2> = (0..=steps)
        .map(|k| {
            let t = start + (k as f32 / steps as f32) * frac * std::f32::consts::TAU;
            Pos2::new(center.x + radius * t.cos(), center.y + radius * t.sin())
        })
        .collect();
    let cap = thickness / 2.0;
    // Round caps: a filled dot at each arc end (egui strokes are butt-capped).
    painter.circle_filled(pts[0], cap, color);
    painter.circle_filled(pts[pts.len() - 1], cap, color);
    painter.add(Shape::line(pts, Stroke::new(thickness, color)));
}

/// A smooth sparkline inside `rect`: a soft translucent area fill under a
/// round-capped line, with interior padding so peaks/troughs never clip the
/// edge. `max_hint` floors the vertical scale (e.g. 100.0 for a percentage).
fn draw_sparkline(ui: &egui::Ui, rect: Rect, values: &[f32], max_hint: f32, accent: Color32) {
    let p = ui.painter();
    if values.len() < 2 {
        return;
    }
    let pad = 6.0;
    let inner = Rect::from_min_max(
        Pos2::new(rect.min.x + pad, rect.min.y + pad),
        Pos2::new(rect.max.x - pad, rect.max.y - pad),
    );
    // Scale: honour the floor (`max_hint`, e.g. 100 for a %) but give the line
    // headroom above the observed peak so it never clips, and reserve a little
    // vertical room at the bottom so an idle trace still reads as a line, not a
    // smear glued to the floor.
    let observed = values.iter().copied().fold(0.0_f32, f32::max);
    let maxv = observed.max(max_hint).max(1.0) * 1.12;
    let n = values.len();
    let dx = inner.width() / (n as f32 - 1.0);
    let floor_gap = 3.0_f32; // keeps a near-zero line just off the baseline
    let plot_h = (inner.height() - floor_gap).max(1.0);
    let y_of = |v: f32| inner.max.y - floor_gap - (v / maxv).clamp(0.0, 1.0) * plot_h;
    let line: Vec<Pos2> = values
        .iter()
        .enumerate()
        .map(|(i, &v)| Pos2::new(inner.min.x + i as f32 * dx, y_of(v)))
        .collect();
    // Faint baseline so the plot reads as a chart even when flat.
    p.line_segment(
        [
            Pos2::new(inner.min.x, inner.max.y),
            Pos2::new(inner.max.x, inner.max.y),
        ],
        Stroke::new(1.0_f32, Color32::from_rgba_unmultiplied(128, 128, 128, 40)),
    );
    // Area fill: the line, closed down to the baseline.
    let mut area = line.clone();
    area.push(Pos2::new(inner.max.x, inner.max.y));
    area.push(Pos2::new(inner.min.x, inner.max.y));
    let fill = Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 55);
    p.add(Shape::Path(egui::epaint::PathShape {
        points: area,
        closed: true,
        fill,
        stroke: egui::epaint::PathStroke::NONE,
    }));
    // Round-capped line on top.
    let cap = 1.25_f32;
    p.circle_filled(line[0], cap, accent);
    p.circle_filled(line[line.len() - 1], cap, accent);
    p.add(Shape::line(line, Stroke::new(2.0_f32, accent)));
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

    // Header: title + Rescan.
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.label(semibold(title).size(19.0));
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(16.0);
            if ui.button("Rescan").clicked() {
                action.rescan = true;
            }
        });
    });
    ui.add_space(10.0);

    if n_found == 0 {
        ui.add_space(20.0);
        ui.vertical_centered(|ui| {
            ui.label(RichText::new("Nothing found — you're all clear.").color(p.text_dim));
        });
        return action;
    }

    // Category order (items already sorted biggest-first) + per-category totals.
    let mut order: Vec<Category> = Vec::new();
    for it in items.iter() {
        if !order.contains(&it.category) {
            order.push(it.category);
        }
    }
    let cat_total = |cat: Category| -> (u64, usize) {
        let mut b = 0u64;
        let mut n = 0usize;
        for it in items.iter().filter(|it| it.category == cat) {
            b += it.size_bytes;
            n += 1;
        }
        (b, n)
    };

    // Summary card: Found + Selected stats, then a proportion tally per category
    // (parity with the webview's `.summary` block).
    let inner_w = (ui.available_width() - 32.0).max(240.0);
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        card_frame(ui, &p, inner_w, 0.0, |ui| {
            ui.horizontal(|ui| {
                stat_pair(
                    ui,
                    &p,
                    "Found",
                    &fmt_bytes(total_found),
                    &format!("{n_found} items"),
                    false,
                );
                ui.add_space(32.0);
                stat_pair(
                    ui,
                    &p,
                    "Selected",
                    &fmt_bytes(sel_bytes),
                    &format!("{sel_n} items"),
                    sel_n > 0,
                );
            });
            ui.add_space(12.0);
            let total = total_found.max(1);
            for cat in &order {
                let (cb, cn) = cat_total(*cat);
                tally_row(ui, &p, cat_label(*cat), cb, cn, total);
            }
        });
    });
    ui.add_space(10.0);

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
                let (cat_bytes, cat_n) = cat_total(*cat);
                let all_sel = idxs.iter().all(|&i| items[i].selected);

                // Sticky-ish category head: select-all + name + "N · size".
                ui.add_space(10.0);
                ui.horizontal(|ui| {
                    ui.add_space(16.0);
                    let mut checked = all_sel;
                    if ui.checkbox(&mut checked, "").changed() {
                        bulk = Some((*cat, checked));
                    }
                    ui.label(semibold(cat_label(*cat)).size(13.0));
                    ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                        ui.add_space(16.0);
                        ui.label(
                            metric(format!("{cat_n} · {}", fmt_bytes(cat_bytes)))
                                .size(12.0)
                                .color(p.text_dim),
                        );
                    });
                });
                ui.separator();

                for &i in &idxs {
                    let it = &items[i];
                    let name = it
                        .path
                        .file_name()
                        .map(|s| s.to_string_lossy().into_owned())
                        .unwrap_or_else(|| it.path.to_string_lossy().into_owned());
                    let reason = it.reason.clone();
                    let path = it.path.clone();
                    let tier = it.tier;
                    let size = it.size_bytes;
                    let mut checked = it.selected;
                    ui.horizontal(|ui| {
                        ui.add_space(28.0);
                        if ui.checkbox(&mut checked, "").changed() {
                            toggle = Some(i);
                        }
                        let (dot, _) = ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                        ui.painter()
                            .circle_filled(dot.center(), 4.0, tier_color(tier));
                        // Right side: Reveal + size.
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(16.0);
                            ui.label(metric(fmt_bytes(size)).size(12.0).color(p.text_dim));
                            ui.add_space(10.0);
                            if ui
                                .add(egui::Button::new(
                                    RichText::new("Reveal").size(11.0).color(p.text_dim),
                                ))
                                .clicked()
                            {
                                let _ = std::process::Command::new("open")
                                    .arg("-R")
                                    .arg(&path)
                                    .spawn();
                            }
                            // Left-flow name + reason fills the remaining space.
                            ui.with_layout(Layout::left_to_right(Align::Center), |ui| {
                                ui.label(RichText::new(name).size(13.0));
                                if !reason.is_empty() {
                                    ui.label(
                                        RichText::new(truncate_mid(&reason, 48))
                                            .color(p.text_dim)
                                            .size(11.0),
                                    );
                                }
                            });
                        });
                    });
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
        ui.add_space(16.0);
        if sel_n == 0 {
            ui.label(RichText::new("Tick anything to remove it (reversible).").color(p.text_dim));
        } else {
            ui.label(semibold(format!(
                "{sel_n} selected · {}",
                fmt_bytes(sel_bytes)
            )));
        }
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.add_space(16.0);
            let danger = Color32::from_rgb(0xdc, 0x26, 0x26);
            let btn = egui::Button::new(RichText::new("Move to Trash").color(Color32::WHITE))
                .fill(if sel_n == 0 { p.text_faint } else { danger })
                .corner_radius(CornerRadius::same(7));
            if ui.add_enabled(sel_n > 0, btn).clicked() {
                action.trash = true;
            }
        });
    });
    action
}

/// A summary stat block: a dim key, a big Inter-bold value (accent when `sel`),
/// and a dim sub — the webview's `.summary .stat`.
fn stat_pair(ui: &mut egui::Ui, p: &Palette, k: &str, v: &str, sub: &str, sel: bool) {
    ui.vertical(|ui| {
        ui.label(RichText::new(k).size(11.0).color(p.text_dim));
        ui.label(
            metric(v)
                .size(24.0)
                .color(if sel { p.accent } else { p.text }),
        );
        ui.label(RichText::new(sub).size(11.0).color(p.text_dim));
    });
}

/// One category tally row: name, a proportional bar, count and size — the
/// webview's `.tally`.
fn tally_row(ui: &mut egui::Ui, p: &Palette, name: &str, bytes: u64, count: usize, total: u64) {
    let frac = (bytes as f32 / total as f32).clamp(0.02, 1.0);
    ui.horizontal(|ui| {
        ui.allocate_ui(Vec2::new(150.0, 16.0), |ui| {
            ui.label(RichText::new(name).size(12.0));
        });
        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
            ui.label(metric(fmt_bytes(bytes)).size(11.0));
            ui.add_space(10.0);
            ui.label(metric(count.to_string()).size(11.0).color(p.text_dim));
            ui.add_space(12.0);
            let w = (ui.available_width() - 6.0).max(20.0);
            let (bar, _) = ui.allocate_exact_size(Vec2::new(w, 8.0), Sense::hover());
            ui.painter()
                .rect_filled(bar, CornerRadius::same(4), p.track);
            let fill = Rect::from_min_size(bar.min, Vec2::new(bar.width() * frac, bar.height()));
            ui.painter()
                .rect_filled(fill, CornerRadius::same(4), p.accent);
        });
    });
    ui.add_space(4.0);
}

/// Middle-truncate a string to `max` chars (keeps the informative head + tail).
fn truncate_mid(s: &str, max: usize) -> String {
    if s.chars().count() <= max {
        return s.to_string();
    }
    let keep = max.saturating_sub(1) / 2;
    let head: String = s.chars().take(keep).collect();
    let tail: String = s
        .chars()
        .rev()
        .take(keep)
        .collect::<Vec<_>>()
        .into_iter()
        .rev()
        .collect();
    format!("{head}…{tail}")
}

/// Standard view header: a bold title + a dim "· subtitle", left-inset to match
/// the content margin. The consistent heading treatment across every view.
fn view_header(ui: &mut egui::Ui, p: &Palette, title: &str, subtitle: &str) {
    ui.add_space(10.0);
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.label(semibold(title).size(20.0));
        if !subtitle.is_empty() {
            ui.add_space(6.0);
            ui.label(
                RichText::new(format!("· {subtitle}"))
                    .color(p.text_dim)
                    .size(13.0),
            );
        }
    });
    ui.add_space(12.0);
}

/// A card surface (elevated fill + hairline border + 10px radius) sized `w` wide
/// and AT LEAST `min_h` tall, growing to fit its content (so a button can never
/// spill past the border). Uses egui's native `Frame` for correct bg-behind-
/// content + inner margin.
fn card_frame(ui: &mut egui::Ui, p: &Palette, w: f32, min_h: f32, add: impl FnOnce(&mut egui::Ui)) {
    egui::Frame::new()
        .fill(p.bg_elev)
        .stroke(Stroke::new(1.0_f32, p.border))
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::same(14))
        .show(ui, |ui| {
            ui.set_width(w - 28.0);
            ui.set_min_height((min_h - 28.0).max(0.0));
            // Force top-down: the card is often placed inside a horizontal row, and
            // Frame would otherwise inherit that layout and lay content sideways.
            ui.with_layout(Layout::top_down(Align::Min), add);
        });
}

/// A labelled dial (ring + centred % + caption) — the webview's `.dial`.
fn dial(ui: &mut egui::Ui, p: &Palette, label: &str, frac: f32, big: &str, color: Color32) {
    let (rect, _) = ui.allocate_exact_size(Vec2::new(100.0, 112.0), Sense::hover());
    let center = Pos2::new(rect.center().x, rect.min.y + 44.0);
    draw_ring(ui, center, 36.0, frac, color, p.track);
    ui.painter().text(
        center,
        egui::Align2::CENTER_CENTER,
        big,
        FontId::new(17.0, egui::FontFamily::Name("semibold".into())),
        p.text,
    );
    ui.painter().text(
        Pos2::new(center.x, rect.max.y - 2.0),
        egui::Align2::CENTER_BOTTOM,
        label,
        FontId::proportional(12.0),
        p.text_dim,
    );
}

/// A key/value row (dim key, Inter-bold value) — the webview's `.kv`.
fn kv_row(ui: &mut egui::Ui, p: &Palette, k: &str, v: &str) {
    ui.horizontal(|ui| {
        ui.allocate_ui(Vec2::new(130.0, 18.0), |ui| {
            ui.label(RichText::new(k).color(p.text_dim).size(12.5));
        });
        ui.label(metric(v).size(13.0));
    });
    ui.add_space(4.0);
}

/// Centered empty-state for the Docker view (icon + title + body + Re-check),
/// matching the webview's `centered(...)`. Sets `recheck` on the button click.
fn docker_empty_state(ui: &mut egui::Ui, p: &Palette, title: &str, body: &str, recheck: &mut bool) {
    ui.add_space(40.0);
    ui.horizontal(|ui| {
        ui.add_space(16.0);
        ui.allocate_ui_with_layout(
            Vec2::new(480.0, 200.0),
            Layout::top_down(Align::Min),
            |ui| {
                ui.set_width(480.0);
                ui.label(semibold(title).size(18.0));
                ui.add_space(8.0);
                ui.label(RichText::new(body).color(p.text_dim)); // wraps at 480
                ui.add_space(16.0);
                if ui.button("Re-check").clicked() {
                    *recheck = true;
                }
            },
        );
    });
}

/// A filled, accent-coloured primary button. Returns true on click.
fn accent_button(ui: &mut egui::Ui, label: &str, accent: Color32) -> bool {
    let btn = egui::Button::new(RichText::new(label).color(Color32::WHITE).strong())
        .fill(accent)
        .corner_radius(CornerRadius::same(7));
    ui.add(btn).clicked()
}

/// Draw a lucide glyph (by codepoint) centred in `rect`.
fn lucide_at(ui: &egui::Ui, rect: Rect, cp: u32, color: Color32, size: f32) {
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        char::from_u32(cp).unwrap_or('?'),
        FontId::new(size, egui::FontFamily::Name("lucide".into())),
        color,
    );
}

/// A dashboard metric card: a dim lucide-icon + title header, a big Inter value
/// (optionally coloured), and a dim sub — the webview's `.dcard`.
#[allow(clippy::too_many_arguments)]
fn dcard(
    ui: &mut egui::Ui,
    p: &Palette,
    w: f32,
    icon_cp: u32,
    title: &str,
    big: &str,
    big_color: Color32,
    small: &str,
) {
    card_frame(ui, p, w, 104.0, |ui| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
            lucide_at(ui, r, icon_cp, p.text_dim, 14.0);
            ui.add_space(5.0);
            ui.label(RichText::new(title).size(12.0).color(p.text_dim));
        });
        ui.add_space(8.0);
        ui.label(metric(big).size(24.0).color(big_color));
        ui.add_space(3.0);
        ui.label(RichText::new(small).size(11.5).color(p.text_dim));
    });
}

/// A small live graph card (header icon + title + big % + sparkline) — the
/// webview's `graphCard`.
fn graph_card(ui: &mut egui::Ui, p: &Palette, w: f32, title: &str, hist: &[f32], color: Color32) {
    card_frame(ui, p, w, 118.0, |ui| {
        ui.horizontal(|ui| {
            let (r, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
            lucide_at(ui, r, 57400, p.text_dim, 14.0); // activity
            ui.add_space(5.0);
            ui.label(RichText::new(title).size(12.0).color(p.text_dim));
        });
        ui.add_space(4.0);
        let last = hist.last().copied().unwrap_or(0.0);
        ui.label(
            metric(format!("{}%", last.round() as i64))
                .size(20.0)
                .color(color),
        );
        ui.add_space(6.0);
        let gw = ui.available_width().max(60.0);
        let (strip, _) = ui.allocate_exact_size(Vec2::new(gw, 42.0), Sense::hover());
        if hist.len() >= 2 {
            draw_sparkline(ui, strip, hist, 100.0, color);
        } else {
            ui.painter().text(
                strip.left_center() + Vec2::new(2.0, 0.0),
                egui::Align2::LEFT_CENTER,
                "collecting…",
                FontId::proportional(11.0),
                p.text_faint,
            );
        }
    });
}

/// The wide "Mac Health" hero card: a free-space ring + honest verdict, on a
/// dark-teal surface (the webview's `.health-hero`).
fn health_hero(
    ui: &mut egui::Ui,
    w: f32,
    free_frac: f32,
    verdict: &str,
    vcolor: Color32,
    sub: &str,
) {
    let fill = Color32::from_rgb(0x11, 0x2c, 0x33);
    let light = Color32::from_rgb(0xea, 0xf6, 0xf2);
    let light_dim = Color32::from_rgb(0x9f, 0xc7, 0xbd);
    let track = Color32::from_rgba_unmultiplied(255, 255, 255, 28);
    egui::Frame::new()
        .fill(fill)
        .corner_radius(CornerRadius::same(10))
        .inner_margin(egui::Margin::same(18))
        .show(ui, |ui| {
            ui.set_width(w - 36.0);
            ui.set_min_height(104.0);
            ui.horizontal(|ui| {
                let (rr, _) = ui.allocate_exact_size(Vec2::splat(104.0), Sense::hover());
                draw_ring(ui, rr.center(), 44.0, free_frac, vcolor, track);
                ui.painter().text(
                    rr.center(),
                    egui::Align2::CENTER_CENTER,
                    verdict.split(' ').next().unwrap_or(verdict),
                    FontId::new(15.0, egui::FontFamily::Name("semibold".into())),
                    light,
                );
                ui.add_space(22.0);
                ui.vertical(|ui| {
                    ui.add_space(14.0);
                    ui.label(RichText::new("Mac Health").color(light_dim).size(13.0));
                    ui.add_space(2.0);
                    ui.label(semibold(verdict).size(28.0).color(vcolor));
                    ui.add_space(6.0);
                    ui.label(RichText::new(sub).color(light_dim).size(12.0));
                });
            });
        });
}

/// An app bundle's display name: the file stem without the `.app` extension.
fn app_display_name(app: &Path) -> String {
    app.file_stem()
        .map(|s| s.to_string_lossy().into_owned())
        .unwrap_or_else(|| app.to_string_lossy().into_owned())
}

/// Human label for a `docker system df` artifact kind.
fn docker_kind_label(kind: &str) -> &str {
    match kind {
        "images" => "Images",
        "containers" => "Containers",
        "volumes" => "Volumes",
        "build_cache" => "Build cache",
        other => other,
    }
}

// ---------------------------------------------------------------------------
// App state
// ---------------------------------------------------------------------------

/// Which top-level feature is on screen. More land here as the migration
/// progresses (Dashboard, Duplicates, …).
#[derive(Clone, Copy, PartialEq, Eq)]
enum View {
    Dash,
    Smart,
    Space,
    Junk,
    Large,
    Security,
    Memory,
    Dupes,
    Net,
    Battery,
    Startup,
    Docker,
    Dev,
    Disk,
    Universal,
    Brew,
    Uninstall,
    Settings,
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

/// One-shot loaded Battery info (fetched off-thread so pmset/ioreg don't hang UI).
#[derive(Default)]
struct BatteryState {
    data: Option<tabibu_sys::BatteryInfo>,
    rx: Option<Receiver<tabibu_sys::BatteryInfo>>,
    started: bool,
}

/// One-shot loaded Startup (login) items.
#[derive(Default)]
struct StartupState {
    data: Option<tabibu_sys::StartupReport>,
    rx: Option<Receiver<tabibu_sys::StartupReport>>,
    started: bool,
}

/// Docker read-only analysis (`docker system df`). `None` report = not installed.
#[derive(Default)]
struct DockerState {
    report: Option<tabibu_docker::Report>,
    rx: Option<Receiver<Option<tabibu_docker::Report>>>,
    started: bool,
    loaded: bool,
}

/// One-shot disk-space reading for the Disk view.
#[derive(Default)]
struct DiskState {
    data: Option<tabibu_monitor::DiskSpace>,
    rx: Option<Receiver<tabibu_monitor::DiskSpace>>,
    started: bool,
}

/// Universal-binary scan (read-only). Always returns a report.
#[derive(Default)]
struct UniversalState {
    report: Option<tabibu_universal::UniversalReport>,
    rx: Option<Receiver<tabibu_universal::UniversalReport>>,
    started: bool,
    loaded: bool,
}

/// Homebrew analysis (read-only). `None` report = not installed.
#[derive(Default)]
struct BrewState {
    report: Option<tabibu_brew::Report>,
    rx: Option<Receiver<Option<tabibu_brew::Report>>>,
    started: bool,
    loaded: bool,
}

/// Installed-apps list + per-app uninstall (app bundle + leftovers → Trash).
#[derive(Default)]
struct UninstallState {
    apps: Vec<(PathBuf, String)>, // (app path, bundle id)
    rx: Option<Receiver<Vec<(PathBuf, String)>>>,
    started: bool,
    loaded: bool,
    busy: Option<PathBuf>,
    action_rx: Option<Receiver<(String, u64, usize)>>, // (name, freed, item count)
    last: Option<(String, u64, usize)>,
    query: String,
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
    mem_hist: VecDeque<f32>,
    started: bool,
}

impl Default for MonitorState {
    fn default() -> Self {
        Self {
            rx: None,
            latest: None,
            by_cpu: Arc::new(AtomicBool::new(true)),
            cpu_hist: VecDeque::new(),
            mem_hist: VecDeque::new(),
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
    dev: JunkState,
    smart: JunkState,
    monitor: MonitorState,
    dupes: DupesState,
    net: NetState,
    battery: BatteryState,
    startup: StartupState,
    docker: DockerState,
    disk: DiskState,
    universal: UniversalState,
    brew: BrewState,
    uninstall: UninstallState,
    /// Menu-bar shell (tray + menu + global shortcut). Created lazily on the
    /// first frame; `None` until then, or if the tray can't be built / was
    /// opted out via `TABIBU_NO_TRAY`.
    shell: Option<Shell>,
    shell_init: bool,
    /// Full Disk Access granted? Shown as a top banner when false. Defaults to
    /// `true` so no banner flashes before the real probe runs on frame 1.
    fda: bool,
    /// Floating health popover (a borderless always-on-top viewport under the
    /// tray) open? Toggled by tray left-click / Ctrl+Cmd+N.
    popover_open: bool,
    popover_pos: Pos2,
    /// The official app icon, loaded once as a texture for the brand logo.
    icon_tex: Option<egui::TextureHandle>,
}

/// Floating health popover size (logical points).
const POPOVER_W: f32 = 300.0;
const POPOVER_H: f32 = 196.0;

impl Default for TabibuApp {
    fn default() -> Self {
        Self {
            dark: true,
            view: View::Dash,
            phase: Phase::Idle,
            root: None,
            expanded: HashSet::new(),
            selected: HashMap::new(),
            scan_rx: None,
            del_rx: None,
            junk: JunkState::default(),
            large: JunkState::default(),
            security: JunkState::default(),
            dev: JunkState::default(),
            smart: JunkState::default(),
            monitor: MonitorState::default(),
            dupes: DupesState::default(),
            net: NetState::default(),
            battery: BatteryState::default(),
            startup: StartupState::default(),
            docker: DockerState::default(),
            disk: DiskState::default(),
            universal: UniversalState::default(),
            brew: BrewState::default(),
            uninstall: UninstallState::default(),
            shell: None,
            shell_init: false,
            fda: true,
            popover_open: false,
            popover_pos: Pos2::ZERO,
            icon_tex: None,
        }
    }
}

/// Decode the gourd SILHOUETTE (the tray template — a white gourd on alpha) into
/// an egui texture for the sidebar brand logo: drawn in white over a teal square,
/// exactly like the webview's `.brand .logo`. `None` on any decode surprise.
fn load_icon_texture(ctx: &egui::Context) -> Option<egui::TextureHandle> {
    const BYTES: &[u8] = include_bytes!("../assets/gourd-white.png");
    let decoder = png::Decoder::new(std::io::Cursor::new(BYTES));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    let image =
        egui::ColorImage::from_rgba_unmultiplied([info.width as usize, info.height as usize], &buf);
    Some(ctx.load_texture("tabibu_icon", image, egui::TextureOptions::LINEAR))
}

/// A compact ring for the menu-bar popover (smaller than the Dashboard's
/// `ring_card`): a labelled dial with the percentage in the middle.
fn mini_ring(ui: &mut egui::Ui, p: &Palette, title: &str, frac: f32, big: &str, color: Color32) {
    // Fixed-width centered column — a bare `vertical_centered` inside a
    // `horizontal` layout grabs the full remaining width, crowding out the
    // sibling rings.
    ui.allocate_ui_with_layout(
        Vec2::new(82.0, 102.0),
        Layout::top_down(Align::Center),
        |ui| {
            let (rect, _) = ui.allocate_exact_size(Vec2::new(82.0, 82.0), Sense::hover());
            let c = rect.center();
            draw_ring(ui, c, 30.0, frac, color, p.track);
            ui.painter().text(
                c,
                egui::Align2::CENTER_CENTER,
                big,
                FontId::proportional(15.0),
                p.text,
            );
            ui.label(RichText::new(title).color(p.text_dim).size(10.0));
        },
    );
}

/// Re-probe Full Disk Access (reads a TCC-gated path). Cheap but does IO, so
/// it's called only on the first frame and when the user taps "Re-check".
fn probe_fda() -> bool {
    tabibu_sys::has_full_disk_access(&tabibu_sys::home_dir())
}

/// Open the macOS Privacy → Full Disk Access settings pane.
fn open_fda_settings() {
    let _ = std::process::Command::new("open")
        .arg("x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles")
        .spawn();
}

/// LaunchAgent label / plist filename stem — the app's bundle id.
const AUTOSTART_LABEL: &str = "xr.seede.tabibu";

/// Path to the per-user LaunchAgent plist that enables launch-at-login.
fn autostart_plist_path() -> PathBuf {
    tabibu_sys::home_dir()
        .join("Library/LaunchAgents")
        .join(format!("{AUTOSTART_LABEL}.plist"))
}

/// Whether launch-at-login is currently enabled (the LaunchAgent plist exists).
fn autostart_enabled() -> bool {
    autostart_plist_path().exists()
}

/// Render a LaunchAgent plist that runs `exe` at login. The `--from-autostart`
/// marker matches the Tauri shell's convention (a login start can be told from a
/// manual open) and is harmless until the egui build acts on it.
fn autostart_plist(exe: &str) -> String {
    format!(
        "<?xml version=\"1.0\" encoding=\"UTF-8\"?>\n\
         <!DOCTYPE plist PUBLIC \"-//Apple//DTD PLIST 1.0//EN\" \"http://www.apple.com/DTDs/PropertyList-1.0.dtd\">\n\
         <plist version=\"1.0\">\n<dict>\n\
         \t<key>Label</key>\n\t<string>{AUTOSTART_LABEL}</string>\n\
         \t<key>ProgramArguments</key>\n\t<array>\n\t\t<string>{exe}</string>\n\t\t<string>--from-autostart</string>\n\t</array>\n\
         \t<key>RunAtLoad</key>\n\t<true/>\n\
         </dict>\n</plist>\n"
    )
}

/// Enable or disable launch-at-login by writing / removing the LaunchAgent plist
/// (pointed at the current executable). Best-effort; returns whether it changed.
fn set_autostart(enabled: bool) -> bool {
    let path = autostart_plist_path();
    if enabled {
        let Ok(exe) = std::env::current_exe() else {
            return false;
        };
        if let Some(dir) = path.parent() {
            let _ = std::fs::create_dir_all(dir);
        }
        std::fs::write(&path, autostart_plist(&exe.to_string_lossy())).is_ok()
    } else {
        std::fs::remove_file(&path).is_ok()
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

    /// Smart Scan: junk + malware scanners together (allowed-roots = `/`), one
    /// combined review. The deep "one scan across everything" entry point.
    fn start_smart_scan(&mut self) {
        self.smart.phase = JunkPhase::Scanning;
        self.smart.items.clear();
        let (tx, rx) = channel();
        self.smart.scan_rx = Some(rx);
        thread::spawn(move || {
            let ctx = reclaim_anywhere_ctx();
            let cancel = CancelToken::new();
            let scanners: Vec<Box<dyn Scanner>> = tabibu_junk::scanners()
                .into_iter()
                .chain(tabibu_malware::scanners())
                .collect();
            let collected: Mutex<Vec<CleanupItem>> = Mutex::new(Vec::new());
            let _ = tabibu_engine::smart_scan(&scanners, &ctx, &cancel, &|it| {
                collected.lock().expect("smart sink poisoned").push(it);
            });
            let mut items = collected.into_inner().expect("smart sink poisoned");
            items.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
            let _ = tx.send(items);
        });
    }

    fn start_smart_delete(&mut self) {
        let items: Vec<CleanupItem> = self
            .smart
            .items
            .iter()
            .filter(|i| i.selected)
            .cloned()
            .collect();
        if items.is_empty() {
            return;
        }
        let n = items.len();
        self.smart.phase = JunkPhase::Deleting;
        let (tx, rx) = channel();
        self.smart.del_rx = Some(rx);
        thread::spawn(move || {
            let ctx = reclaim_anywhere_ctx();
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

    /// Load battery info once (pmset/ioreg off the UI thread).
    fn ensure_battery(&mut self) {
        if self.battery.started {
            return;
        }
        self.battery.started = true;
        let (tx, rx) = channel();
        self.battery.rx = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(tabibu_sys::battery_info());
        });
    }

    /// Load login/startup items once (reads launchd dirs off the UI thread).
    fn ensure_startup(&mut self) {
        if self.startup.started {
            return;
        }
        self.startup.started = true;
        let (tx, rx) = channel();
        self.startup.rx = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(tabibu_sys::startup_items());
        });
    }

    /// Build-artifacts scan (node_modules, target, …) via tabibu_devscan, mapped
    /// to CleanupItems so the shared cleanup UI handles select + trash.
    fn start_dev_scan(&mut self) {
        self.dev.phase = JunkPhase::Scanning;
        self.dev.items.clear();
        let (tx, rx) = channel();
        self.dev.scan_rx = Some(rx);
        thread::spawn(move || {
            let home = tabibu_sys::home_dir();
            let cancel = CancelToken::new();
            let arts = tabibu_devscan::scan(std::slice::from_ref(&home), &home, &cancel);
            let mut items: Vec<CleanupItem> = arts
                .into_iter()
                .map(|a| {
                    let mut it = CleanupItem::new(
                        a.path,
                        Category::DevCache,
                        a.size_bytes,
                        SafetyTier::Review,
                        format!("{} · rebuild: {}", a.kind, a.rebuild),
                    );
                    it.action = ReclaimAction::Trash;
                    it
                })
                .collect();
            items.sort_by_key(|a| std::cmp::Reverse(a.size_bytes));
            let _ = tx.send(items);
        });
    }

    fn start_dev_delete(&mut self) {
        let items: Vec<CleanupItem> = self
            .dev
            .items
            .iter()
            .filter(|i| i.selected)
            .cloned()
            .collect();
        if items.is_empty() {
            return;
        }
        let n = items.len();
        self.dev.phase = JunkPhase::Deleting;
        let (tx, rx) = channel();
        self.dev.del_rx = Some(rx);
        thread::spawn(move || {
            let ctx = reclaim_anywhere_ctx();
            let freed = tabibu_engine::reclaim(&ctx, &items, &tabibu_sys::undo_dir())
                .map(|r| r.reclaimed_bytes)
                .unwrap_or(0);
            let _ = tx.send((freed, n));
        });
    }

    /// Load boot-volume disk space once (off-thread).
    fn ensure_disk(&mut self) {
        if self.disk.started {
            return;
        }
        self.disk.started = true;
        let (tx, rx) = channel();
        self.disk.rx = Some(rx);
        thread::spawn(move || {
            let _ = tx.send(tabibu_monitor::disk_space());
        });
    }

    /// Load the installed-apps list once (/Applications + ~/Applications).
    fn ensure_uninstall(&mut self) {
        if self.uninstall.started {
            return;
        }
        self.uninstall.started = true;
        let (tx, rx) = channel();
        self.uninstall.rx = Some(rx);
        thread::spawn(move || {
            let home = tabibu_sys::home_dir();
            let roots = vec![PathBuf::from("/Applications"), home.join("Applications")];
            let mut apps = tabibu_uninstall::installed_apps(&roots);
            apps.sort_by(|a, b| {
                app_display_name(&a.0)
                    .to_lowercase()
                    .cmp(&app_display_name(&b.0).to_lowercase())
            });
            let _ = tx.send(apps);
        });
    }

    /// Uninstall one app: move its bundle + leftover support files to the Trash
    /// (reversible). Finds leftovers via tabibu_uninstall::find_remnants.
    fn uninstall_app(&mut self, app: PathBuf, bundle_id: String) {
        if self.uninstall.busy.is_some() {
            return;
        }
        self.uninstall.busy = Some(app.clone());
        let (tx, rx) = channel();
        self.uninstall.action_rx = Some(rx);
        thread::spawn(move || {
            let name = app_display_name(&app);
            let ctx = reclaim_anywhere_ctx();
            let mut items: Vec<CleanupItem> = Vec::new();
            let mut app_item = CleanupItem::new(
                app.clone(),
                Category::UnusedApp,
                0,
                SafetyTier::Review,
                "Application bundle",
            );
            app_item.action = ReclaimAction::Trash;
            items.push(app_item);
            items.extend(tabibu_uninstall::find_remnants(&bundle_id, &name, &ctx));
            for it in &mut items {
                it.selected = true;
                it.action = ReclaimAction::Trash;
            }
            let count = items.len();
            let freed = tabibu_engine::reclaim(&ctx, &items, &tabibu_sys::undo_dir())
                .map(|r| r.reclaimed_bytes)
                .unwrap_or(0);
            let _ = tx.send((name, freed, count));
        });
    }

    /// Scan universal binaries once (/Applications + ~/Applications), off-thread.
    fn ensure_universal(&mut self) {
        if self.universal.started {
            return;
        }
        self.universal.started = true;
        let (tx, rx) = channel();
        self.universal.rx = Some(rx);
        thread::spawn(move || {
            let home = tabibu_sys::home_dir();
            let roots = vec![PathBuf::from("/Applications"), home.join("Applications")];
            let _ = tx.send(tabibu_universal::scan(&roots, &CancelToken::new()));
        });
    }

    /// Analyze Homebrew once, off-thread. `None` = not installed.
    fn ensure_brew(&mut self) {
        if self.brew.started {
            return;
        }
        self.brew.started = true;
        let (tx, rx) = channel();
        self.brew.rx = Some(rx);
        thread::spawn(move || {
            let report = tabibu_brew::Brew::detect().map(|b| b.analyze());
            let _ = tx.send(report);
        });
    }

    /// Analyze Docker once (`docker system df`, off-thread). `None` = not installed.
    fn ensure_docker(&mut self) {
        if self.docker.started {
            return;
        }
        self.docker.started = true;
        let (tx, rx) = channel();
        self.docker.rx = Some(rx);
        thread::spawn(move || {
            let report = tabibu_docker::Docker::detect().map(|d| d.analyze());
            let _ = tx.send(report);
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

impl TabibuApp {
    /// Reveal + focus the dashboard window (from a tray/menu/hotkey action),
    /// optionally navigating to a view. The window is only ever hidden (on
    /// close), never destroyed, so making it visible brings it right back.
    fn show_window(&mut self, ctx: &egui::Context, view: Option<View>) {
        // Target the ROOT viewport explicitly so this also works when called from
        // inside the popover viewport's closure (where the bare context refers to
        // the popover, not the main window).
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Visible(true));
        ctx.send_viewport_cmd_to(egui::ViewportId::ROOT, egui::ViewportCommand::Focus);
        if let Some(v) = view {
            self.view = v;
        }
    }

    /// Toggle the floating health popover. On open, start the samplers (so it has
    /// live data on any view) and anchor it top-right, just under the menu bar.
    fn toggle_popover(&mut self, ctx: &egui::Context) {
        self.popover_open = !self.popover_open;
        if self.popover_open {
            self.ensure_monitor();
            self.ensure_disk();
            let mw = ctx
                .input(|i| i.viewport().monitor_size)
                .map_or(1440.0, |s| s.x);
            self.popover_pos = Pos2::new((mw - POPOVER_W - 12.0).max(12.0), 36.0);
        }
    }

    /// Render the floating health popover as a borderless, always-on-top
    /// viewport. Uses `show_viewport_immediate` so the closure can read the live
    /// monitor/disk state directly. Auto-hides when it loses focus.
    fn popover_viewport(&mut self, ctx: &egui::Context) {
        let pos = self.popover_pos;
        ctx.show_viewport_immediate(
            egui::ViewportId::from_hash_of("tabibu_popover"),
            egui::ViewportBuilder::default()
                .with_title("Tabibu")
                .with_inner_size([POPOVER_W, POPOVER_H])
                .with_position(pos)
                .with_decorations(false)
                .with_resizable(false)
                .with_always_on_top(),
            |vp, _class| {
                let p = palette(self.dark);
                egui::CentralPanel::default()
                    .frame(egui::Frame {
                        fill: p.bg_elev,
                        inner_margin: egui::Margin::same(14),
                        stroke: Stroke::new(1.0_f32, p.border),
                        corner_radius: CornerRadius::same(10),
                        ..Default::default()
                    })
                    .show(vp, |ui| {
                        ui.horizontal(|ui| {
                            ui.label(RichText::new("Tabibu").strong());
                            ui.label(
                                RichText::new("· system health")
                                    .color(p.text_dim)
                                    .size(12.0),
                            );
                        });
                        ui.add_space(10.0);

                        let cpu = self.monitor.latest.as_ref().map_or(0.0, |s| s.cpu_percent);
                        let (mu, mt) = self
                            .monitor
                            .latest
                            .as_ref()
                            .map_or((0, 0), |s| (s.used_memory_bytes, s.total_memory_bytes));
                        let mem_frac = if mt > 0 { mu as f32 / mt as f32 } else { 0.0 };
                        let (du, dt, dfree) = self.disk.data.map_or((0, 0, 0), |d| {
                            (
                                d.total_bytes.saturating_sub(d.available_bytes),
                                d.total_bytes,
                                d.available_bytes,
                            )
                        });
                        let disk_frac = if dt > 0 { du as f32 / dt as f32 } else { 0.0 };

                        ui.horizontal(|ui| {
                            mini_ring(
                                ui,
                                &p,
                                "CPU",
                                cpu / 100.0,
                                &format!("{}%", cpu.round() as i64),
                                Color32::from_rgb(0xf9, 0x73, 0x16),
                            );
                            mini_ring(
                                ui,
                                &p,
                                "Memory",
                                mem_frac,
                                &format!("{}%", (mem_frac * 100.0).round() as i64),
                                Color32::from_rgb(0x10, 0xb9, 0x81),
                            );
                            mini_ring(
                                ui,
                                &p,
                                "Disk",
                                disk_frac,
                                &format!("{}%", (disk_frac * 100.0).round() as i64),
                                Color32::from_rgb(0xa8, 0x55, 0xf7),
                            );
                        });
                        ui.add_space(10.0);
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Open Dashboard").clicked() {
                                self.popover_open = false;
                                self.show_window(vp, Some(View::Dash));
                            }
                            ui.label(
                                RichText::new(format!("{} free", fmt_bytes(dfree)))
                                    .color(p.text_dim)
                                    .size(11.0),
                            );
                        });
                    });

                // Dismiss predictably: Escape, the window close button, or
                // toggling again from the tray / hotkey. (Blur-based auto-hide is
                // deferred — it needs the Tauri shell's click-debounce to stop a
                // tray-click dismiss from immediately reopening the popover.)
                if vp.input(|i| i.viewport().close_requested())
                    || vp.input(|i| i.key_pressed(egui::Key::Escape))
                {
                    self.popover_open = false;
                }
                vp.request_repaint_after(Duration::from_millis(500));
            },
        );
    }
}

impl eframe::App for TabibuApp {
    fn update(&mut self, ctx: &egui::Context, _frame: &mut eframe::Frame) {
        // Menu-bar shell: build once (after the event loop is up — an
        // NSStatusItem created earlier won't attach), then drain its tray / menu
        // / hotkey channels every frame. A short repaint heartbeat keeps update()
        // firing even while the window is hidden, so the tray stays responsive.
        if !self.shell_init {
            self.shell_init = true;
            self.fda = probe_fda();
            self.icon_tex = load_icon_texture(ctx);
            if std::env::var_os("TABIBU_NO_TRAY").is_none() {
                self.shell = Shell::new();
            }
            // Test seam: open the health popover on launch for a headless shot.
            if std::env::var_os("TABIBU_POPOVER").is_some() {
                self.toggle_popover(ctx);
            }
        }
        if self.shell.is_some() {
            let events = self.shell.as_ref().map(Shell::poll).unwrap_or_default();
            for ev in events {
                match ev {
                    ShellEvent::Open => self.show_window(ctx, None),
                    ShellEvent::TogglePopover => self.toggle_popover(ctx),
                    ShellEvent::Settings => self.show_window(ctx, Some(View::Settings)),
                    ShellEvent::Quit => std::process::exit(0),
                }
            }
            if self.popover_open {
                self.popover_viewport(ctx);
            }
            ctx.request_repaint_after(Duration::from_millis(150));
            // Closing the dashboard hides it and keeps the app alive in the menu
            // bar (parity with the Tauri shell). Only tray Quit / Cmd-Q exits.
            // ponytail: Dock icon lingers (no Regular↔Accessory toggle yet) —
            // that needs objc2-app-kit; deferred to the bundling phase.
            if ctx.input(|i| i.viewport().close_requested()) {
                ctx.send_viewport_cmd(egui::ViewportCommand::CancelClose);
                ctx.send_viewport_cmd(egui::ViewportCommand::Visible(false));
            }
        }

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
        if let Some(rx) = &self.smart.scan_rx {
            if let Ok(items) = rx.try_recv() {
                self.smart.scan_rx = None;
                self.smart.items = items;
                self.smart.phase = JunkPhase::Review;
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.smart.del_rx {
            if let Ok((freed, items)) = rx.try_recv() {
                self.smart.del_rx = None;
                self.smart.phase = JunkPhase::Done { freed, items };
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
        if let Some(rx) = &self.dev.scan_rx {
            if let Ok(items) = rx.try_recv() {
                self.dev.scan_rx = None;
                self.dev.items = items;
                self.dev.phase = JunkPhase::Review;
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.dev.del_rx {
            if let Ok((freed, items)) = rx.try_recv() {
                self.dev.del_rx = None;
                self.dev.phase = JunkPhase::Done { freed, items };
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.disk.rx {
            if let Ok(d) = rx.try_recv() {
                self.disk.rx = None;
                self.disk.data = Some(d);
            } else {
                ctx.request_repaint();
            }
        }
        // Drain live monitor samples (keep only the newest; track a short CPU
        // history for the sparkline).
        if let Some(rx) = &self.monitor.rx {
            while let Ok(s) = rx.try_recv() {
                self.monitor.cpu_hist.push_back(s.cpu_percent.min(100.0));
                let mem_pct = if s.total_memory_bytes > 0 {
                    (s.used_memory_bytes as f32 / s.total_memory_bytes as f32 * 100.0).min(100.0)
                } else {
                    0.0
                };
                self.monitor.mem_hist.push_back(mem_pct);
                while self.monitor.cpu_hist.len() > 60 {
                    self.monitor.cpu_hist.pop_front();
                }
                while self.monitor.mem_hist.len() > 60 {
                    self.monitor.mem_hist.pop_front();
                }
                self.monitor.latest = Some(s);
            }
        }
        if self.view == View::Memory {
            self.ensure_monitor();
            ctx.request_repaint_after(Duration::from_millis(500));
        }
        if self.view == View::Dash {
            self.ensure_monitor();
            self.ensure_disk();
            self.ensure_battery();
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
        if let Some(rx) = &self.battery.rx {
            if let Ok(b) = rx.try_recv() {
                self.battery.rx = None;
                self.battery.data = Some(b);
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.startup.rx {
            if let Ok(s) = rx.try_recv() {
                self.startup.rx = None;
                self.startup.data = Some(s);
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.docker.rx {
            if let Ok(r) = rx.try_recv() {
                self.docker.rx = None;
                self.docker.report = r;
                self.docker.loaded = true;
            } else {
                ctx.request_repaint();
            }
        }
        if self.view == View::Battery {
            self.ensure_battery();
        }
        if self.view == View::Startup {
            self.ensure_startup();
        }
        if self.view == View::Docker {
            self.ensure_docker();
        }
        if self.view == View::Disk {
            self.ensure_disk();
        }
        if let Some(rx) = &self.universal.rx {
            if let Ok(r) = rx.try_recv() {
                self.universal.rx = None;
                self.universal.report = Some(r);
                self.universal.loaded = true;
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.brew.rx {
            if let Ok(r) = rx.try_recv() {
                self.brew.rx = None;
                self.brew.report = r;
                self.brew.loaded = true;
            } else {
                ctx.request_repaint();
            }
        }
        if self.view == View::Universal {
            self.ensure_universal();
        }
        if self.view == View::Brew {
            self.ensure_brew();
        }
        if let Some(rx) = &self.uninstall.rx {
            if let Ok(apps) = rx.try_recv() {
                self.uninstall.rx = None;
                self.uninstall.apps = apps;
                self.uninstall.loaded = true;
            } else {
                ctx.request_repaint();
            }
        }
        if let Some(rx) = &self.uninstall.action_rx {
            if let Ok(res) = rx.try_recv() {
                self.uninstall.action_rx = None;
                self.uninstall.busy = None;
                self.uninstall.last = Some(res);
                // Refresh the app list now that one is gone.
                self.uninstall.started = false;
                self.uninstall.loaded = false;
            } else {
                ctx.request_repaint();
            }
        }
        if self.view == View::Uninstall {
            self.ensure_uninstall();
        }

        // Left navigation rail — more entries land here as the migration adds views.
        let p = palette(self.dark);

        // Full Disk Access banner: without FDA, scans miss system caches, other
        // apps' data and protected folders. Shown across every view until granted
        // (matches the Tauri shell's permission prompt). A dev binary run from the
        // terminal has no FDA, so this surfaces there too.
        if !self.fda {
            egui::TopBottomPanel::top("fda_banner")
                .frame(egui::Frame {
                    fill: Color32::from_rgb(0x7a, 0x5a, 0x12),
                    inner_margin: egui::Margin::symmetric(16, 10),
                    ..Default::default()
                })
                .show(ctx, |ui| {
                    ui.horizontal(|ui| {
                        ui.label(
                            RichText::new("⚠  Full Disk Access needed")
                                .color(Color32::WHITE)
                                .strong(),
                        );
                        ui.label(
                            RichText::new(
                                "— Tabibu can't see system caches, other apps' data, \
                                 or protected folders until you grant it.",
                            )
                            .color(Color32::from_rgb(0xf0, 0xe6, 0xcf)),
                        );
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            if ui.button("Re-check").clicked() {
                                self.fda = probe_fda();
                            }
                            if ui.button("Open System Settings").clicked() {
                                open_fda_settings();
                            }
                        });
                    });
                });
        }

        egui::SidePanel::left("nav")
            .resizable(false)
            .exact_width(214.0)
            .frame(egui::Frame {
                fill: p.sidebar,
                inner_margin: egui::Margin::same(10),
                stroke: Stroke::new(1.0_f32, p.border),
                ..Default::default()
            })
            .show(ctx, |ui| {
                // Brand: a teal square with the white gourd silhouette + wordmark
                // (the webview's `.brand .logo`). The Dock icon stays the full
                // gradient gourd; this in-app mark is white-on-teal.
                ui.add_space(6.0);
                ui.horizontal(|ui| {
                    ui.add_space(6.0);
                    let (lr, _) = ui.allocate_exact_size(Vec2::splat(26.0), Sense::hover());
                    ui.painter()
                        .rect_filled(lr, CornerRadius::same(7), view_accent(View::Dash));
                    if let Some(tex) = &self.icon_tex {
                        let g = Rect::from_center_size(lr.center(), Vec2::splat(17.0));
                        egui::Image::new(tex).tint(Color32::WHITE).paint_at(ui, g);
                    } else {
                        draw_pulse(ui, lr, Color32::WHITE);
                    }
                    ui.add_space(8.0);
                    ui.label(semibold("Tabibu").size(16.0));
                });
                ui.add_space(8.0);

                // Scrolls when the list outgrows a short window (like the webview).
                let mut next: Option<View> = None;
                egui::ScrollArea::vertical()
                    .auto_shrink([false, false])
                    .show(ui, |ui| {
                        let mut item = |ui: &mut egui::Ui, label: &str, v: View| {
                            if nav_item(ui, &p, label, self.view == v, v) {
                                next = Some(v);
                            }
                        };
                        nav_section(ui, &p, "Overview");
                        item(ui, "Dashboard", View::Dash);
                        item(ui, "Smart Scan", View::Smart);
                        nav_section(ui, &p, "Cleanup");
                        item(ui, "Junk", View::Junk);
                        item(ui, "Space Analyzer", View::Space);
                        item(ui, "Duplicates", View::Dupes);
                        item(ui, "Large & Old", View::Large);
                        nav_section(ui, &p, "Applications");
                        item(ui, "Uninstaller", View::Uninstall);
                        item(ui, "Developer / CLI", View::Brew);
                        item(ui, "Build Artifacts", View::Dev);
                        item(ui, "Docker", View::Docker);
                        item(ui, "Universal Binaries", View::Universal);
                        item(ui, "Startup", View::Startup);
                        nav_section(ui, &p, "Health");
                        item(ui, "Disk", View::Disk);
                        item(ui, "Memory & CPU", View::Memory);
                        item(ui, "Battery", View::Battery);
                        item(ui, "Network", View::Net);
                        nav_section(ui, &p, "Security");
                        item(ui, "Security", View::Security);
                        ui.add_space(10.0);
                        item(ui, "Settings", View::Settings);
                        ui.add_space(8.0);
                    });
                if let Some(v) = next {
                    self.view = v;
                }
            });

        egui::CentralPanel::default().show(ctx, |ui| match self.view {
            View::Dash => self.ui_dash(ui),
            View::Smart => self.ui_smart(ui),
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
            View::Battery => self.ui_battery(ui),
            View::Startup => self.ui_startup(ui),
            View::Docker => self.ui_docker(ui),
            View::Dev => self.ui_dev(ui),
            View::Disk => self.ui_disk(ui),
            View::Universal => self.ui_universal(ui),
            View::Brew => self.ui_brew(ui),
            View::Uninstall => self.ui_uninstall(ui),
            View::Settings => self.ui_settings(ui),
        });
    }
}

/// Per-view nav icon, rendered from the bundled lucide icon font — the exact
/// glyphs the webview uses. `rect` is ~16px.
fn draw_nav_icon(ui: &egui::Ui, rect: Rect, view: View, color: Color32) {
    ui.painter().text(
        rect.center(),
        egui::Align2::CENTER_CENTER,
        view_icon_char(view),
        FontId::new(16.0, egui::FontFamily::Name("lucide".into())),
        color,
    );
}

/// View -> lucide codepoint (matches the webview NAV icon map).
fn view_icon_char(view: View) -> char {
    let cp: u32 = match view {
        View::Dash => 57791,      // gauge
        View::Smart => 58386,     // sparkles
        View::Junk => 57742,      // trash-2
        View::Space => 58169,     // folder-search
        View::Dupes => 57502,     // copy
        View::Large => 57551,     // files
        View::Uninstall => 57990, // rocket
        View::Brew => 57729,      // terminal
        View::Dev => 57641,       // package
        View::Docker => 58581,    // container
        View::Universal => 58665, // layers
        View::Startup => 57400,   // activity
        View::Disk => 57581,      // hard-drive
        View::Memory => 57513,    // cpu
        View::Battery => 57427,   // battery
        View::Net => 57774,       // wifi
        View::Security => 57688,  // shield
        View::Settings => 57672,  // rotate-ccw
    };
    char::from_u32(cp).unwrap_or('?')
}

/// Per-view accent — ported from the webview's THEME map (app/src/main.js). Each
/// section carries its own colour (the premium "honest theming" signal); the nav
/// active state, view headers and rings all tint by it.
fn view_accent(v: View) -> Color32 {
    let hex = match v {
        View::Dash => 0x14b8a6,
        View::Smart => 0x8b5cf6,
        View::Junk => 0x16a34a,
        View::Dupes => 0x3b82f6,
        View::Large => 0x0ea5e9,
        View::Space => 0x8b5cf6,
        View::Uninstall => 0x6366f1,
        View::Brew => 0xd97706,
        View::Docker => 0x2496ed,
        View::Universal => 0x0891b2,
        View::Startup => 0xf59e0b,
        View::Dev => 0x10b981,
        View::Disk => 0xa855f7,
        View::Memory => 0xf97316,
        View::Battery => 0x22c55e,
        View::Net => 0x0284c7,
        View::Security => 0xec4899,
        View::Settings => 0x64748b,
    };
    Color32::from_rgb((hex >> 16) as u8, (hex >> 8) as u8, hex as u8)
}

/// An uppercase, faint section header in the nav rail (Overview / Cleanup / …).
fn nav_section(ui: &mut egui::Ui, p: &Palette, label: &str) {
    ui.add_space(12.0);
    ui.horizontal(|ui| {
        ui.add_space(8.0);
        ui.label(
            RichText::new(label.to_uppercase())
                .size(10.5)
                .color(p.text_faint)
                .strong(),
        );
    });
    ui.add_space(2.0);
}

/// One navigation-rail entry: a per-view icon + label, accent-tinted when active
/// (bg wash + accent icon/label), subtle hover otherwise. Returns true on click.
fn nav_item(ui: &mut egui::Ui, p: &Palette, label: &str, active: bool, view: View) -> bool {
    let accent = view_accent(view);
    let w = ui.available_width();
    let (rect, resp) = ui.allocate_exact_size(Vec2::new(w, 30.0), Sense::click());
    let r = CornerRadius::same(6);
    if active {
        ui.painter().rect_filled(
            rect,
            r,
            Color32::from_rgba_unmultiplied(accent.r(), accent.g(), accent.b(), 38),
        );
    } else if resp.hovered() {
        ui.painter().rect_filled(rect, r, p.hover);
    }
    let color = if active { accent } else { p.text };
    let icon = Rect::from_min_size(
        Pos2::new(rect.left() + 8.0, rect.center().y - 8.0),
        Vec2::splat(16.0),
    );
    draw_nav_icon(ui, icon, view, color);
    let font = if active {
        FontId::new(13.5, egui::FontFamily::Name("semibold".into()))
    } else {
        FontId::proportional(13.5)
    };
    ui.painter().text(
        Pos2::new(rect.left() + 34.0, rect.center().y),
        egui::Align2::LEFT_CENTER,
        label,
        font,
        color,
    );
    resp.clicked()
}

impl TabibuApp {
    fn ui_landing(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        ui.vertical_centered(|ui| {
            ui.add_space(ui.available_height() * 0.3);
            ui.label(semibold("Space Analyzer").size(26.0));
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
            ui.label(semibold("Removal complete").size(22.0));
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
                    ui.label(semibold("Junk Cleanup").size(26.0));
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
                    ui.label(semibold("Cleanup complete").size(22.0));
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
                    ui.label(semibold("Large & Old Files").size(26.0));
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
                    ui.label(semibold("Done").size(22.0));
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
                    ui.label(semibold("Security Scan").size(26.0));
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
                    ui.label(semibold("Done").size(22.0));
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

    fn ui_battery(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0x22, 0xc5, 0x5e); // green — webview battery accent
        view_header(ui, &p, "Battery", "power & health");

        let Some(b) = self.battery.data.clone() else {
            self.ui_center(ui, "Reading battery…", true);
            return;
        };
        if !b.has_battery {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("This Mac has no battery.").color(p.text_dim));
            });
            return;
        }

        let charge = b.charge_percent.unwrap_or(0);
        let color = if charge < 20 {
            Color32::from_rgb(0xdc, 0x26, 0x26)
        } else {
            accent
        };
        // Charge dial + state / time remaining.
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            dial(
                ui,
                &p,
                &b.state.clone().unwrap_or_default(),
                charge as f32 / 100.0,
                &format!("{charge}%"),
                color,
            );
            ui.add_space(28.0);
            ui.vertical(|ui| {
                ui.add_space(22.0);
                if let Some(st) = &b.state {
                    kv_row(ui, &p, "State", st);
                }
                if let Some(tr) = &b.time_remaining {
                    kv_row(ui, &p, "Time remaining", tr);
                }
            });
        });

        // Battery health card.
        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            card_frame(ui, &p, 420.0, 0.0, |ui| {
                ui.label(semibold("Battery health").size(14.0));
                ui.add_space(10.0);
                if let Some(h) = b.health_percent {
                    kv_row(ui, &p, "Capacity vs. design", &format!("{h}%"));
                }
                if let Some(c) = b.cycle_count {
                    kv_row(ui, &p, "Cycle count", &c.to_string());
                }
                if let Some(cond) = &b.condition {
                    kv_row(ui, &p, "Condition", cond);
                }
                if b.health_percent.is_none() && b.cycle_count.is_none() {
                    ui.label(
                        RichText::new(
                            "Detailed health metrics weren't available from this Mac's battery \
                             controller.",
                        )
                        .color(p.text_dim)
                        .size(12.0),
                    );
                }
            });
        });
    }

    fn ui_startup(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        view_header(ui, &p, "Startup Items", "what launches at login");

        let Some(rep) = self.startup.data.clone() else {
            self.ui_center(ui, "Reading login items…", true);
            return;
        };

        // Count + Open Login Items Settings (matches the webview header row).
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let n = rep.items.len();
            ui.label(
                semibold(format!("{n} startup item{}", if n == 1 { "" } else { "s" })).size(13.0),
            );
            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                ui.add_space(16.0);
                if ui.button("Open Login Items Settings").clicked() {
                    let _ = std::process::Command::new("open")
                        .arg("x-apple.systempreferences:com.apple.LoginItems-Settings.extension")
                        .spawn();
                }
            });
        });
        if rep.partial {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(
                    RichText::new("Some system items need Full Disk Access to read — this list may be incomplete.")
                        .color(tier_color(SafetyTier::Review))
                        .size(11.5),
                );
            });
        }
        ui.add_space(6.0);
        ui.separator();

        if rep.items.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("No startup items found.").color(p.text_dim));
            });
            return;
        }

        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for it in &rep.items {
                    ui.add_space(4.0);
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(it.label.as_str()).size(13.0));
                            ui.add(
                                egui::Label::new(
                                    RichText::new(it.program.as_str())
                                        .color(p.text_dim)
                                        .monospace()
                                        .size(11.0),
                                )
                                .truncate(),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(16.0);
                            if ui
                                .add(egui::Button::new(
                                    RichText::new("Reveal").size(11.0).color(p.text_dim),
                                ))
                                .clicked()
                            {
                                let _ = std::process::Command::new("open")
                                    .arg("-R")
                                    .arg(&it.path)
                                    .spawn();
                            }
                            ui.add_space(10.0);
                            ui.label(
                                RichText::new(it.scope.as_str())
                                    .color(p.text_faint)
                                    .size(11.0),
                            );
                        });
                    });
                    ui.add_space(4.0);
                    ui.separator();
                }
            });
    }

    fn ui_dev(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0x63, 0x66, 0xf1); // indigo
        match &self.dev.phase {
            JunkPhase::Idle => {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.label(semibold("Build Artifacts").size(26.0));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "Find regenerable developer output (node_modules, Rust/Maven target, \
                             build/dist dirs, …) and move what you don't need to the Trash — \
                             reversible. Each is safe to delete; your tooling rebuilds it.",
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
                        self.start_dev_scan();
                    }
                });
            }
            JunkPhase::Scanning => self.ui_center(ui, "Scanning for build artifacts…", true),
            JunkPhase::Deleting => self.ui_center(ui, "Moving selected items to the Trash…", true),
            JunkPhase::Done { freed, items } => {
                let (freed, items) = (*freed, *items);
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.label(semibold("Done").size(22.0));
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
                        self.start_dev_scan();
                    }
                });
            }
            JunkPhase::Review => {
                let act = cleanup_review_ui(ui, self.dark, "Build Artifacts", &mut self.dev.items);
                if act.rescan {
                    self.start_dev_scan();
                }
                if act.trash {
                    self.start_dev_delete();
                }
            }
        }
    }

    fn ui_disk(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0xa8, 0x55, 0xf7); // purple
        view_header(ui, &p, "Disk", "space on your startup volume");

        let Some(d) = self.disk.data else {
            self.ui_center(ui, "Reading disk…", true);
            return;
        };
        let used = d.total_bytes.saturating_sub(d.available_bytes);
        let frac = if d.total_bytes > 0 {
            used as f32 / d.total_bytes as f32
        } else {
            0.0
        };
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            let (rect, _) = ui.allocate_exact_size(Vec2::new(150.0, 150.0), Sense::hover());
            let center = rect.center();
            draw_ring(ui, center, 54.0, frac, accent, p.track);
            ui.painter().text(
                center,
                egui::Align2::CENTER_CENTER,
                format!("{:.0}%", frac * 100.0),
                FontId::proportional(24.0),
                p.text,
            );
            ui.add_space(20.0);
            ui.vertical(|ui| {
                ui.add_space(30.0);
                // Disk/storage uses DECIMAL units (matches Finder).
                ui.label(
                    RichText::new(format!("{} used", fmt_bytes(used)))
                        .size(18.0)
                        .strong(),
                );
                ui.add_space(4.0);
                ui.label(
                    RichText::new(format!("{} free", fmt_bytes(d.available_bytes))).color(accent),
                );
                ui.add_space(2.0);
                ui.label(
                    RichText::new(format!("{} total", fmt_bytes(d.total_bytes))).color(p.text_dim),
                );
            });
        });
    }

    fn ui_dash(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        view_header(ui, &p, "Dashboard", "system health at a glance");

        // Gather live facts (computed before the layout closures borrow `ui`).
        let cpu = self.monitor.latest.as_ref().map_or(0.0, |s| s.cpu_percent);
        let (mu, mt) = self
            .monitor
            .latest
            .as_ref()
            .map_or((0, 0), |s| (s.used_memory_bytes, s.total_memory_bytes));
        let mem_pct = if mt > 0 {
            (mu as f64 / mt as f64 * 100.0).round()
        } else {
            0.0
        };
        let (dt, dfree) = self
            .disk
            .data
            .map_or((0, 0), |d| (d.total_bytes, d.available_bytes));
        let dused_pct = if dt > 0 {
            ((dt - dfree) as f64 / dt as f64 * 100.0).round()
        } else {
            0.0
        };
        let battery = self.battery.data.clone();
        let cpu_hist: Vec<f32> = self.monitor.cpu_hist.iter().copied().collect();
        let mem_hist: Vec<f32> = self.monitor.mem_hist.iter().copied().collect();

        // Honest health verdict from free-space + memory headroom (matches the
        // webview's Mac Health hero).
        let free_frac = if dt > 0 {
            dfree as f32 / dt as f32
        } else {
            1.0
        };
        let (verdict, vcolor) = if free_frac < 0.05 {
            ("Low on space", Color32::from_rgb(0xef, 0x44, 0x44))
        } else if free_frac < 0.1 || mem_pct > 92.0 {
            ("Needs attention", Color32::from_rgb(0xf5, 0x9e, 0x0b))
        } else {
            ("Good", Color32::from_rgb(0x22, 0xc5, 0x5e))
        };

        // Icon'd stat cards (lucide codepoint, title, big value, color, sub).
        let text = p.text;
        let mut cards: Vec<(u32, &str, String, Color32, String)> = vec![
            (
                57581, // hard-drive
                "Storage",
                format!("{} free", fmt_bytes(dfree)),
                text,
                format!("of {} · {}% used", fmt_bytes(dt), dused_pct as i64),
            ),
            (
                57513, // cpu chip
                "Memory",
                format!("{}% used", mem_pct as i64),
                text,
                format!("{} of {}", fmt_ram(mu), fmt_ram(mt)),
            ),
            (
                57791, // gauge
                "CPU",
                format!("{}%", cpu.round() as i64),
                text,
                "across all cores".to_string(),
            ),
        ];
        if let Some(b) = battery.as_ref().filter(|b| b.has_battery) {
            let state = b.state.clone().unwrap_or_default();
            let sub = match b.cycle_count {
                Some(c) if !state.is_empty() => format!("{state} · {c} cycles"),
                Some(c) => format!("{c} cycles"),
                None => state,
            };
            cards.push((
                57427, // battery
                "Battery",
                format!("{}%", b.charge_percent.unwrap_or(0)),
                text,
                sub,
            ));
        }

        // Responsive grid: as many 240px-min columns as fit.
        let margin = 16.0;
        let gap = 14.0;
        let avail = (ui.available_width() - margin * 2.0).max(240.0);
        let cols = (((avail + gap) / (240.0 + gap)).floor() as usize).clamp(1, 4);
        let card_w = ((avail - gap * (cols as f32 - 1.0)) / cols as f32).floor();

        let mut run_smart = false;
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                ui.add_space(2.0);
                // Mac Health hero (wide).
                ui.horizontal(|ui| {
                    ui.add_space(margin);
                    let sub = if dt > 0 {
                        format!("{} free of {}", fmt_bytes(dfree), fmt_bytes(dt))
                    } else {
                        "Storage unavailable".to_string()
                    };
                    health_hero(ui, avail, free_frac, verdict, vcolor, &sub);
                });
                ui.add_space(gap);

                // Stat cards.
                for row in cards.chunks(cols) {
                    ui.horizontal(|ui| {
                        ui.add_space(margin);
                        for (i, (cp, title, big, bc, small)) in row.iter().enumerate() {
                            if i > 0 {
                                ui.add_space(gap);
                            }
                            dcard(ui, &p, card_w, *cp, title, big, *bc, small);
                        }
                    });
                    ui.add_space(gap);
                }

                // CPU + Memory live-graph cards (two across).
                ui.horizontal(|ui| {
                    ui.add_space(margin);
                    graph_card(
                        ui,
                        &p,
                        card_w,
                        "CPU",
                        &cpu_hist,
                        Color32::from_rgb(0xf9, 0x73, 0x16),
                    );
                    ui.add_space(gap);
                    graph_card(
                        ui,
                        &p,
                        card_w,
                        "Memory",
                        &mem_hist,
                        Color32::from_rgb(0x3b, 0x82, 0xf6),
                    );
                });
                ui.add_space(gap);

                // Smart Scan CTA (wide).
                ui.horizontal(|ui| {
                    ui.add_space(margin);
                    card_frame(ui, &p, avail, 92.0, |ui| {
                        ui.horizontal(|ui| {
                            let (r, _) = ui.allocate_exact_size(Vec2::splat(15.0), Sense::hover());
                            lucide_at(ui, r, 58386, p.text_dim, 14.0); // sparkles
                            ui.add_space(5.0);
                            ui.label(RichText::new("Smart Scan").size(12.0).color(p.text_dim));
                        });
                        ui.add_space(4.0);
                        ui.label(
                            RichText::new("No scan yet this session.")
                                .color(p.text_dim)
                                .size(13.0),
                        );
                        ui.add_space(8.0);
                        if accent_button(ui, "Run Smart Scan", view_accent(View::Smart)) {
                            run_smart = true;
                        }
                    });
                });
                ui.add_space(gap);
            });

        if run_smart {
            self.view = View::Smart;
            self.start_smart_scan();
        }
    }

    fn ui_smart(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0x8b, 0x5c, 0xf6); // violet — webview smart accent
        match &self.smart.phase {
            JunkPhase::Idle => {
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.3);
                    ui.label(semibold("Smart Scan").size(26.0));
                    ui.add_space(8.0);
                    ui.label(
                        RichText::new(
                            "One deep scan across caches, temporary files, logs, large & old \
                             files and threats. Nothing is removed until you review and choose — \
                             everything goes to the Trash, reversible.",
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
                        self.start_smart_scan();
                    }
                });
            }
            JunkPhase::Scanning => self.ui_center(ui, "Running a full smart scan…", true),
            JunkPhase::Deleting => self.ui_center(ui, "Moving selected items to the Trash…", true),
            JunkPhase::Done { freed, items } => {
                let (freed, items) = (*freed, *items);
                ui.vertical_centered(|ui| {
                    ui.add_space(ui.available_height() * 0.35);
                    ui.label(semibold("Done").size(22.0));
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
                        self.start_smart_scan();
                    }
                });
            }
            JunkPhase::Review => {
                let act = cleanup_review_ui(ui, self.dark, "Smart Scan", &mut self.smart.items);
                if act.rescan {
                    self.start_smart_scan();
                }
                if act.trash {
                    self.start_smart_delete();
                }
            }
        }
    }

    fn ui_uninstall(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0x63, 0x66, 0xf1); // indigo
        let n = self.uninstall.apps.len();
        let sub = if self.uninstall.loaded {
            format!("{n} apps")
        } else {
            "apps & their leftovers".to_string()
        };
        view_header(ui, &p, "Uninstaller", &sub);

        if let Some((name, freed, count)) = self.uninstall.last.clone() {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(
                    RichText::new(format!(
                        "Removed {name} — {} to the Trash · {count} item(s)",
                        fmt_bytes(freed)
                    ))
                    .color(accent),
                );
            });
            ui.add_space(4.0);
        }

        if !self.uninstall.loaded {
            self.ui_center(ui, "Finding installed apps…", true);
            return;
        }

        // Search box (filters the list, like the webview's app browser).
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.add(
                egui::TextEdit::singleline(&mut self.uninstall.query)
                    .hint_text("Search apps")
                    .desired_width(240.0),
            );
        });
        ui.add_space(6.0);
        ui.separator();

        let q = self.uninstall.query.to_lowercase();
        let mut to_remove: Option<(PathBuf, String)> = None;
        let busy = self.uninstall.busy.clone();
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .show(ui, |ui| {
                for (path, bundle) in &self.uninstall.apps {
                    let name = app_display_name(path);
                    if !q.is_empty() && !name.to_lowercase().contains(&q) {
                        continue;
                    }
                    ui.add_space(5.0);
                    ui.horizontal(|ui| {
                        ui.add_space(16.0);
                        ui.vertical(|ui| {
                            ui.label(RichText::new(&name).size(13.0));
                            ui.label(
                                RichText::new(bundle.as_str())
                                    .color(p.text_dim)
                                    .monospace()
                                    .size(11.0),
                            );
                        });
                        ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                            ui.add_space(16.0);
                            let this_busy = busy.as_deref() == Some(path.as_path());
                            let enabled = busy.is_none();
                            let label = if this_busy {
                                "Removing…"
                            } else {
                                "Uninstall"
                            };
                            let btn = egui::Button::new(RichText::new(label).color(Color32::WHITE))
                                .fill(if enabled {
                                    Color32::from_rgb(0xdc, 0x26, 0x26)
                                } else {
                                    p.text_faint
                                })
                                .corner_radius(CornerRadius::same(7));
                            if ui.add_enabled(enabled, btn).clicked() {
                                to_remove = Some((path.clone(), bundle.clone()));
                            }
                        });
                    });
                    ui.add_space(5.0);
                    ui.separator();
                }
            });

        if let Some((path, bundle)) = to_remove {
            self.uninstall_app(path, bundle);
        }
    }

    fn ui_settings(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        view_header(ui, &p, "Settings", "appearance, startup & about");
        ui.separator();
        ui.add_space(10.0);

        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Appearance").strong());
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let mut dark = self.dark;
            if ui.checkbox(&mut dark, "Dark mode").changed() {
                self.dark = dark;
                apply_theme(ui.ctx(), self.dark);
            }
        });

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Startup").strong());
        });
        ui.add_space(4.0);
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            let mut on = autostart_enabled();
            if ui.checkbox(&mut on, "Launch Tabibu at login").changed() {
                set_autostart(on);
            }
        });
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            ui.label(
                RichText::new("Starts quietly in the menu bar when you log in.")
                    .color(p.text_dim)
                    .size(11.0),
            );
        });

        ui.add_space(16.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("About").strong());
        });
        ui.add_space(4.0);
        for line in [
            format!("Tabibu {}", env!("CARGO_PKG_VERSION")),
            "Native egui build — the Rust core, called directly (no webview, no IPC).".to_string(),
        ] {
            ui.horizontal(|ui| {
                ui.add_space(16.0);
                ui.label(RichText::new(line).color(p.text_dim));
            });
            ui.add_space(2.0);
        }
    }

    fn ui_universal(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0x08, 0x91, 0xb2); // cyan
        view_header(ui, &p, "Universal Binaries", "reclaim unused CPU slices");

        if !self.universal.loaded {
            self.ui_center(ui, "Scanning apps for extra architecture slices…", true);
            return;
        }
        let Some(rep) = self.universal.report.as_ref() else {
            return;
        };
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                metric(format!(
                    "{} safely reclaimable",
                    fmt_bytes(rep.safe_reclaimable_bytes)
                ))
                .size(22.0)
                .color(accent),
            );
            ui.label(
                RichText::new(format!(
                    "· keeps {} · {} app(s)",
                    rep.native_arch, rep.safe_app_count
                ))
                .color(p.text_dim),
            );
        });
        ui.add_space(10.0);
        ui.separator();

        if rep.apps.is_empty() {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(
                    RichText::new("No universal apps with a reclaimable slice.").color(p.text_dim),
                );
            });
        } else {
            egui::ScrollArea::vertical()
                .auto_shrink([false, false])
                .show(ui, |ui| {
                    for a in &rep.apps {
                        let safe = a.category == "safe";
                        ui.add_space(6.0);
                        ui.horizontal(|ui| {
                            ui.add_space(8.0);
                            let (dot, _) =
                                ui.allocate_exact_size(Vec2::splat(12.0), Sense::hover());
                            ui.painter().circle_filled(
                                dot.center(),
                                4.0,
                                if safe {
                                    tier_color(SafetyTier::Safe)
                                } else {
                                    tier_color(SafetyTier::Risky)
                                },
                            );
                            ui.label(a.name.as_str());
                            ui.label(
                                RichText::new(format!("· {}", a.arches.join(" + ")))
                                    .size(11.0)
                                    .color(p.text_faint),
                            );
                            ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                                ui.add_space(8.0);
                                ui.label(
                                    metric(fmt_bytes(a.reclaimable_bytes))
                                        .size(12.0)
                                        .color(p.text_dim),
                                );
                            });
                        })
                        .response
                        .on_hover_text(format!(
                            "{} · signing: {}",
                            a.path.display(),
                            a.signing
                        ));
                    }
                });
        }
        ui.add_space(8.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Thinning (removing the non-native slice) is coming to this view; it rewrites \
                     the app bundle, so it stays opt-in per app.",
                )
                .size(11.0)
                .color(p.text_faint),
            );
        });
    }

    fn ui_brew(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0xd9, 0x77, 0x06); // amber
        view_header(ui, &p, "Developer / CLI", "Homebrew packages & cleanup");

        if !self.brew.loaded {
            self.ui_center(ui, "Checking Homebrew…", true);
            return;
        }
        let Some(rep) = self.brew.report.clone() else {
            ui.add_space(20.0);
            ui.vertical_centered(|ui| {
                ui.label(RichText::new("Homebrew isn't installed.").color(p.text_dim));
            });
            return;
        };
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                metric(format!(
                    "{} reclaimable",
                    fmt_bytes(rep.cleanup.freeable_bytes)
                ))
                .size(22.0)
                .color(accent),
            );
            // The version string already includes "Homebrew …"; don't double it.
            let ver = rep
                .status
                .version
                .clone()
                .unwrap_or_else(|| "Homebrew".to_owned());
            ui.label(
                RichText::new(format!("· {ver} · {} packages", rep.packages.len()))
                    .color(p.text_dim),
            );
        });
        ui.add_space(10.0);
        ui.separator();
        ui.add_space(6.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(RichText::new("Orphaned dependencies").strong());
            ui.label(
                RichText::new(format!("· {} removable", rep.autoremovable.len())).color(p.text_dim),
            );
        });
        egui::ScrollArea::vertical()
            .auto_shrink([false, false])
            .max_height((ui.available_height() - 40.0).max(60.0))
            .show(ui, |ui| {
                if rep.autoremovable.is_empty() {
                    ui.add_space(8.0);
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        ui.label(RichText::new("None — nothing to autoremove.").color(p.text_dim));
                    });
                }
                for name in &rep.autoremovable {
                    ui.horizontal(|ui| {
                        ui.add_space(24.0);
                        ui.label(name.as_str());
                    });
                }
            });
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "cleanup / autoremove / uninstall actions are coming to this view; for now \
                     use `brew cleanup` or the CLI companion.",
                )
                .size(11.0)
                .color(p.text_faint),
            );
        });
    }

    fn ui_docker(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        let accent = Color32::from_rgb(0x24, 0x96, 0xed); // docker blue
        view_header(ui, &p, "Docker", "images, containers & build cache");

        if !self.docker.loaded {
            self.ui_center(ui, "Checking Docker…", true);
            return;
        }
        let mut recheck = false;
        let Some(rep) = self.docker.report.clone() else {
            docker_empty_state(
                ui,
                &p,
                "Docker not found",
                "The docker CLI wasn't found. Install Docker Desktop to reclaim space from build \
                 cache, images, containers and volumes here.",
                &mut recheck,
            );
            if recheck {
                self.docker.started = false;
                self.docker.loaded = false;
            }
            return;
        };
        if !rep.status.running {
            docker_empty_state(
                ui,
                &p,
                "Docker isn't running",
                "The docker CLI is installed but its daemon isn't responding. Start Docker \
                 Desktop, then re-check.",
                &mut recheck,
            );
            if recheck {
                self.docker.started = false;
                self.docker.loaded = false;
            }
            return;
        }

        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                metric(format!(
                    "{} reclaimable",
                    fmt_bytes(rep.total_reclaimable_bytes)
                ))
                .size(22.0)
                .color(accent),
            );
            if let Some(v) = &rep.status.version {
                ui.label(RichText::new(format!("· Docker {v}")).color(p.text_faint));
            }
        });
        ui.add_space(10.0);
        ui.separator();

        for a in &rep.artifacts {
            ui.add_space(6.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                ui.label(RichText::new(docker_kind_label(&a.kind)).strong());
                ui.label(
                    RichText::new(format!("· {} of {} in use", a.active_count, a.total_count))
                        .color(p.text_dim),
                );
                ui.with_layout(Layout::right_to_left(Align::Center), |ui| {
                    ui.add_space(8.0);
                    ui.label(
                        metric(format!("{} reclaimable", fmt_bytes(a.reclaimable_bytes)))
                            .size(12.0)
                            .color(p.text_dim),
                    );
                });
            });
        }

        ui.add_space(14.0);
        ui.horizontal(|ui| {
            ui.add_space(8.0);
            ui.label(
                RichText::new(
                    "Pruning from this view is coming; for now reclaim with `docker system \
                     prune` or the CLI companion.",
                )
                .size(11.0)
                .color(p.text_faint),
            );
        });
    }

    fn ui_memory(&mut self, ui: &mut egui::Ui) {
        let p = palette(self.dark);
        // Orange — matches the webview build's Memory & CPU accent.
        let accent = Color32::from_rgb(0xf9, 0x73, 0x16);

        let Some(s) = self.monitor.latest.clone() else {
            self.ui_center(ui, "Sampling system activity…", true);
            return;
        };

        view_header(ui, &p, "Memory & CPU", "live system activity");

        let mem_frac = s.used_memory_bytes as f32 / s.total_memory_bytes.max(1) as f32;
        let mem_color = if mem_frac > 0.9 {
            Color32::from_rgb(0xdc, 0x26, 0x26)
        } else if mem_frac > 0.75 {
            Color32::from_rgb(0xf5, 0x9e, 0x0b)
        } else {
            Color32::from_rgb(0x16, 0xa3, 0x4a)
        };

        // Two dials (Memory + CPU) + a key/value block — matches the webview.
        ui.horizontal(|ui| {
            ui.add_space(16.0);
            dial(
                ui,
                &p,
                "Memory",
                mem_frac,
                &format!("{}%", (mem_frac * 100.0).round() as i64),
                mem_color,
            );
            ui.add_space(20.0);
            dial(
                ui,
                &p,
                "CPU",
                s.cpu_percent / 100.0,
                &format!("{}%", s.cpu_percent.round() as i64),
                accent,
            );
            ui.add_space(32.0);
            ui.vertical(|ui| {
                ui.add_space(16.0);
                kv_row(
                    ui,
                    &p,
                    "Memory used",
                    &format!(
                        "{} / {}",
                        fmt_ram(s.used_memory_bytes),
                        fmt_ram(s.total_memory_bytes)
                    ),
                );
                kv_row(ui, &p, "Swap used", &fmt_ram(s.used_swap_bytes));
                kv_row(
                    ui,
                    &p,
                    "Memory free",
                    &fmt_ram(s.total_memory_bytes.saturating_sub(s.used_memory_bytes)),
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
                let (strip, _) = ui.allocate_exact_size(Vec2::new(w, 48.0), Sense::hover());
                ui.painter()
                    .rect_filled(strip, CornerRadius::same(8), p.bg_elev);
                draw_sparkline(ui, strip, &hist, 100.0, accent);
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
                                metric(fmt_bytes(proc.memory_bytes))
                                    .size(12.0)
                                    .color(p.text_dim),
                            );
                            ui.add_space(16.0);
                            ui.label(
                                metric(format!("{}%", proc.cpu_percent.round() as i64)).size(12.0),
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
                    ui.label(semibold("Find Duplicate Files").size(26.0));
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
                    ui.label(semibold("Done").size(22.0));
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

        view_header(ui, &p, "Network", "connection & throughput");

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
                    metric(format!("{}/s", fmt_bytes(down)))
                        .size(22.0)
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
                ui.label(metric(format!("{}/s", fmt_bytes(up))).size(22.0));
                ui.label(
                    RichText::new(format!("{} total", fmt_bytes(tup)))
                        .size(11.0)
                        .color(p.text_faint),
                );
            });
        });

        // Download sparkline (auto-scaled to its own peak).
        let hist: Vec<f32> = self.net.down_hist.iter().map(|&v| v as f32).collect();
        if hist.len() >= 2 {
            ui.add_space(10.0);
            ui.horizontal(|ui| {
                ui.add_space(8.0);
                let w = (ui.available_width() - 8.0).max(60.0);
                let (strip, _) = ui.allocate_exact_size(Vec2::new(w, 48.0), Sense::hover());
                ui.painter()
                    .rect_filled(strip, CornerRadius::same(8), p.bg_elev);
                draw_sparkline(ui, strip, &hist, 1.0, accent);
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
            ui.label(semibold("Macintosh HD").size(19.0));
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

/// The official Tabibu app icon (gourd) for the window / Dock, decoded from the
/// bundled 256px PNG. Without this eframe shows a blank/white default icon.
fn window_icon() -> Option<egui::IconData> {
    const BYTES: &[u8] = include_bytes!("../assets/icon-256.png");
    let decoder = png::Decoder::new(std::io::Cursor::new(BYTES));
    let mut reader = decoder.read_info().ok()?;
    let mut buf = vec![0; reader.output_buffer_size()?];
    let info = reader.next_frame(&mut buf).ok()?;
    if info.color_type != png::ColorType::Rgba || info.bit_depth != png::BitDepth::Eight {
        return None;
    }
    buf.truncate(info.buffer_size());
    Some(egui::IconData {
        rgba: buf,
        width: info.width,
        height: info.height,
    })
}

fn main() -> eframe::Result<()> {
    let mut viewport = egui::ViewportBuilder::default()
        .with_inner_size([1120.0, 740.0])
        .with_min_inner_size([900.0, 600.0])
        .with_title("Tabibu");
    if let Some(icon) = window_icon() {
        viewport = viewport.with_icon(std::sync::Arc::new(icon));
    }
    let opts = eframe::NativeOptions {
        viewport,
        ..Default::default()
    };
    eframe::run_native(
        "Tabibu",
        opts,
        Box::new(|cc| {
            let dark = cc.egui_ctx.style().visuals.dark_mode;
            install_fonts(&cc.egui_ctx);
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
                Ok("dash") => app.view = View::Dash,
                Ok("smart") => {
                    app.view = View::Smart;
                    app.start_smart_scan();
                }
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
                Ok("battery") => app.view = View::Battery,
                Ok("startup") => app.view = View::Startup,
                Ok("docker") => app.view = View::Docker,
                Ok("dev") => {
                    app.view = View::Dev;
                    app.start_dev_scan();
                }
                Ok("disk") => app.view = View::Disk,
                Ok("universal") => app.view = View::Universal,
                Ok("brew") => app.view = View::Brew,
                Ok("uninstall") => app.view = View::Uninstall,
                Ok("settings") => app.view = View::Settings,
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
    fn autostart_plist_has_exe_label_and_runatload() {
        let xml = autostart_plist("/Applications/Tabibu.app/Contents/MacOS/Tabibu");
        assert!(xml.contains("<string>xr.seede.tabibu</string>"), "label");
        assert!(
            xml.contains("<string>/Applications/Tabibu.app/Contents/MacOS/Tabibu</string>"),
            "exe path in ProgramArguments"
        );
        assert!(xml.contains("--from-autostart"), "login-start marker");
        assert!(xml.contains("<key>RunAtLoad</key>"), "runs at load");
        // Well-formed enough to parse as a plist (lint via plutil in CI is overkill).
        assert!(xml.trim_start().starts_with("<?xml"));
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

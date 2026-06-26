// Tabibu — Tauri frontend. Talks to the Rust core via window.__TAURI__ (the
// commands in src-tauri/src/commands.rs). Data fields use snake_case to match
// serde; only top-level invoke arg keys are camelCase (Tauri maps them).

import { ICONS, BRAND_PATH } from "./icons.js";

const TAURI = window.__TAURI__;
const invoke = TAURI.core.invoke;
const Channel = TAURI.core.Channel;

// ---------- DOM helper ----------
function h(tag, props = {}, ...children) {
  const e = document.createElement(tag);
  for (const [k, v] of Object.entries(props || {})) {
    if (k === "class") e.className = v;
    else if (k === "html") e.innerHTML = v;
    else if (k.startsWith("on")) e.addEventListener(k.slice(2).toLowerCase(), v);
    else if (v !== null && v !== undefined) e.setAttribute(k, v);
  }
  for (const c of children.flat()) {
    if (c === null || c === undefined) continue;
    e.append(c.nodeType ? c : document.createTextNode(String(c)));
  }
  return e;
}
function icon(name, cls = "") {
  const key = name.replace(/-/g, "_");
  const svg = `<svg xmlns="http://www.w3.org/2000/svg" viewBox="0 0 24 24" fill="none" stroke="currentColor" stroke-width="2" stroke-linecap="round" stroke-linejoin="round">${ICONS[key] || ""}</svg>`;
  return h("span", { class: cls, html: svg });
}

// ---------- formatting ----------
function fmtBytes(n) {
  n = Number(n) || 0;
  if (n < 1024) return `${n} B`;
  const u = ["KB", "MB", "GB", "TB"];
  let i = -1;
  do { n /= 1024; i++; } while (n >= 1024 && i < u.length - 1);
  return `${n.toFixed(n < 10 ? 1 : 0)} ${u[i]}`;
}
function displayPath(p) {
  return state.home && p.startsWith(state.home) ? "~" + p.slice(state.home.length) : p;
}
const CAT_NAMES = {
  Trash: "Trash", UserCache: "User Caches", DevCache: "Developer Caches",
  Temp: "Temporary Files", Log: "Logs", Duplicate: "Duplicates",
  LargeOldFile: "Large & Old Files", AppRemnant: "App Remnants",
  OrphanedSupport: "Orphaned Support", UnusedApp: "Unused Apps",
  StaleBinary: "Stale Binaries", Malware: "Security",
};
const catName = (c) => CAT_NAMES[c] || c;

// ---------- app state ----------
const state = { home: "", fda: true, current: "smart" };

// ---------- nav ----------
const NAV = [
  { section: "Overview", items: [
    { id: "dashboard", title: "Dashboard", icon: "gauge" },
    { id: "smart", title: "Smart Scan", icon: "sparkles" },
  ]},
  { section: "Cleanup", items: [
    { id: "junk", title: "Junk", icon: "trash-2" },
    { id: "dupes", title: "Duplicates", icon: "copy" },
    { id: "large", title: "Large & Old", icon: "files" },
  ]},
  { section: "Applications", items: [
    { id: "uninstall", title: "Uninstaller", icon: "rocket" },
    { id: "brew", title: "Developer / CLI", icon: "terminal" },
    { id: "startup", title: "Startup Items", icon: "activity" },
  ]},
  { section: "Health", items: [
    { id: "disk", title: "Disk", icon: "hard-drive" },
    { id: "memory", title: "Memory & CPU", icon: "cpu" },
    { id: "battery", title: "Battery", icon: "battery" },
  ]},
  { section: "Security", items: [{ id: "security", title: "Security", icon: "shield" }] },
  { section: "", items: [{ id: "settings", title: "Settings", icon: "rotate_ccw" }] },
];

// Per-view accent (honest theming, inspired by CleanMyMac's section colors).
const THEME = {
  dashboard: "#14b8a6", smart: "#8b5cf6", junk: "#16a34a", dupes: "#3b82f6",
  large: "#0ea5e9", uninstall: "#6366f1", brew: "#d97706", startup: "#f59e0b", disk: "#a855f7",
  memory: "#f97316", battery: "#22c55e", security: "#ec4899", settings: "#64748b",
};
// Hero copy + sub-features per scan view.
const HERO = {
  smart: { feats: ["Caches, temp files & logs", "Large old downloads", "Reviewed before anything is removed"] },
  junk: { feats: ["User & developer caches", "Temporary files & logs", "Running-app caches are skipped"] },
  large: { feats: ["Big files in Downloads", "Suggestions only — nothing pre-selected", "You decide what goes"] },
};

function buildShell() {
  const nav = h("div", { class: "nav" });
  for (const grp of NAV) {
    const sec = h("div", { class: "nav-section" });
    if (grp.section) sec.append(h("div", { class: "label" }, grp.section));
    for (const it of grp.items) {
      const row = h("div", { class: "nav-item", "data-id": it.id, onClick: () => navigate(it.id) },
        icon(it.icon), h("span", {}, it.title));
      sec.append(row);
    }
    nav.append(sec);
  }
  const sidebar = h("div", { class: "sidebar" },
    h("div", { class: "brand" },
      h("span", { class: "logo", html: `<svg viewBox="0 0 551 888" fill="currentColor"><path d="${BRAND_PATH}" /></svg>` }),
      "Tabibu"),
    nav,
    h("div", { class: "footer", id: "footer" }));
  const content = h("div", { class: "content" },
    h("div", { class: "titlebar" }, h("h1", { id: "view-title" }, "Smart Scan"),
      h("div", { class: "actions", id: "view-actions" })),
    h("div", { class: "view", id: "view" }));
  document.getElementById("app").append(sidebar, content);
  updateFooter();
}

function updateFooter() {
  const f = document.getElementById("footer");
  f.innerHTML = "";
  const ok = state.fda;
  f.append(h("span", { class: "dot", style: `background:${ok ? "var(--tier-safe)" : "var(--tier-review)"}` }),
    h("span", {}, ok ? "Full Disk Access on" : "Limited access"));
}

function navigate(id) {
  state.current = id;
  document.querySelectorAll(".nav-item").forEach((n) =>
    n.classList.toggle("active", n.dataset.id === id));
  // Apply the section accent to both the view and the sidebar (active item).
  const accent = THEME[id] || "var(--accent)";
  document.getElementById("view").style.setProperty("--view-accent", accent);
  document.querySelector(".nav-item.active")?.style.setProperty("--view-accent", accent);
  cancelTimers();
  // Leaving the Developer/CLI view drops its transient action toast + stale flag
  // so neither can reappear without context on return.
  if (id !== "brew") { brewState.outcome = null; brewState.stale = false; }
  render();
}
function setTitle(t, actions = []) {
  document.getElementById("view-title").textContent = t;
  const a = document.getElementById("view-actions");
  a.innerHTML = "";
  actions.forEach((btn) => a.append(btn));
}
function setView(node) {
  const v = document.getElementById("view");
  v.innerHTML = "";
  v.append(node);
}

// timers (monitor polling) cleanup on navigation
let activeTimers = [];
function cancelTimers() { activeTimers.forEach(clearInterval); activeTimers = []; }

// =====================================================================
// Scan flow (Smart Scan / Junk / Large & Old)
// =====================================================================
const SCAN_DEFS = {
  smart: { title: "Smart Scan", icon: "sparkles", scanners: [],
    sub: "One scan across caches, temporary files, logs, and large old files. Nothing is removed until you review and choose." },
  junk: { title: "Junk", icon: "trash-2", scanners: ["trash", "user_cache", "dev_cache", "temp", "log"],
    sub: "Caches, temporary files, and logs that are safe to clear. Caches of running apps are skipped automatically." },
  large: { title: "Large & Old", icon: "files", scanners: ["large_old"],
    sub: "Suggestions only — big files in Downloads you may have forgotten. Nothing is selected for you." },
};

// Live-scan "stages": one chip per junk category, lit up as items arrive. Smart
// Scan (empty `scanners`) runs the whole junk set, so it shows them all.
const SCAN_STAGES = {
  trash: { cat: "Trash", label: "Trash", icon: "trash-2" },
  user_cache: { cat: "UserCache", label: "App Caches", icon: "package" },
  dev_cache: { cat: "DevCache", label: "Dev Caches", icon: "terminal" },
  temp: { cat: "Temp", label: "Temp Files", icon: "files" },
  log: { cat: "Log", label: "Logs", icon: "activity" },
  large_old: { cat: "LargeOldFile", label: "Large & Old", icon: "hard-drive" },
};
const ALL_JUNK_STAGES = ["trash", "user_cache", "dev_cache", "temp", "log", "large_old"];
function stagesFor(def) {
  return (def.scanners.length ? def.scanners : ALL_JUNK_STAGES).map((s) => SCAN_STAGES[s]).filter(Boolean);
}

const sessions = {}; // id -> session
function getSession(id) {
  if (!sessions[id]) sessions[id] = { phase: "idle", items: [], selection: new Set(), summary: null };
  return sessions[id];
}

function scanView(id) {
  const def = SCAN_DEFS[id];
  const s = getSession(id);
  const actions = [];
  if (s.phase === "scanning") actions.push(h("button", { onClick: () => stopScan(id) }, "Stop"));
  if (s.phase === "review") actions.push(h("button", { onClick: () => { resetSession(id); startScan(id); } }, "Rescan"));
  if (s.phase === "done" || s.phase === "error") actions.push(h("button", { onClick: () => { resetSession(id); startScan(id); } }, "Scan Again"));
  setTitle(def.title, actions);

  if (s.phase === "idle") return setView(heroLanding(def.icon, def.title, def.sub, (HERO[id] || {}).feats || [], () => startScan(id)));
  if (s.phase === "scanning") return setView(scanningPanel(id));
  if (s.phase === "review") return setView(reviewPanel(id));
  if (s.phase === "reclaiming") return setView(centeredSpinner("Moving items to the Trash…"));
  if (s.phase === "done") return setView(resultPanel(id));
  if (s.phase === "error") return setView(centered("circle-alert", "Something went wrong", s.error, "Try Again", () => { resetSession(id); startScan(id); }));
}

function startScan(id) {
  const def = SCAN_DEFS[id];
  const s = getSession(id);
  s.phase = "scanning"; s.items = []; s.selection = new Set(); s.summary = null;
  let dirty = false;
  const ch = new Channel();
  ch.onmessage = (msg) => {
    if (msg.kind === "item") {
      s.items.push(msg.item);
      if (msg.item.tier === "Safe") s.selection.add(msg.item.path);
      dirty = true;
    } else if (msg.kind === "done") {
      s.summary = { cancelled: msg.cancelled, scanners: msg.scanners };
      s.phase = s.items.length ? "review" : "idle";
      if (id === state.current) scanView(id);
    }
  };
  // batched re-render ~100ms so streaming never thrashes the UI
  const t = setInterval(() => {
    if (dirty && s.phase === "scanning" && id === state.current) { dirty = false; refreshScanning(id); }
  }, 100);
  activeTimers.push(t);
  invoke("scan", { scanners: def.scanners, onEvent: ch });
  scanView(id);
}
function stopScan(id) { invoke("cancel_scan"); const s = getSession(id); s.phase = s.items.length ? "review" : "idle"; scanView(id); }
function resetSession(id) { sessions[id] = { phase: "idle", items: [], selection: new Set(), summary: null }; }

function tallies(items) {
  const acc = {};
  for (const it of items) { const e = acc[it.category] || [0, 0]; acc[it.category] = [e[0] + 1, e[1] + Number(it.size_bytes)]; }
  return Object.entries(acc).map(([c, [n, b]]) => ({ category: catName(c), count: n, bytes: b }))
    .sort((a, b) => b.bytes - a.bytes);
}
const foundBytes = (s) => s.items.reduce((a, i) => a + Number(i.size_bytes), 0);
const selectedBytes = (s) => s.items.reduce((a, i) => s.selection.has(i.path) ? a + Number(i.size_bytes) : a, 0);

// Creative live-scan loader: an animated radar (built ONCE so its CSS animation
// isn't restarted by the ~100ms streaming re-renders), a ticking total-found
// counter, and a chip per category that lights up as items of that kind stream
// in. Only the numbers/chips update on each tick — the radar is left alone.
function scanningPanel(id) {
  const radar = h("div", {
    class: "scan-radar", "aria-hidden": "true", html:
      `<svg viewBox="0 0 120 120">
         <circle class="r-ring" cx="60" cy="60" r="14" />
         <circle class="r-ring" cx="60" cy="60" r="14" />
         <circle class="r-ring" cx="60" cy="60" r="14" />
         <line class="r-sweep" x1="60" y1="60" x2="60" y2="12" />
         <circle class="r-core" cx="60" cy="60" r="5" />
       </svg>` });
  const wrap = h("div", { class: "scan-stage" }, radar,
    h("div", { class: "scan-amt mono", id: "scan-amt" }, "0 B"),
    h("div", { class: "scan-sub dim", id: "scan-sub" }, "Starting scan…"),
    h("div", { class: "scan-chips", id: "scan-chips" }),
    h("div", { class: "row", style: "justify-content:center;margin-top:18px" },
      h("button", { onClick: () => stopScan(id) }, "Stop")),
    h("p", { class: "faint", style: "text-align:center;margin-top:10px;font-size:11px" },
      "Reviewing happens after the scan — nothing is removed yet."));
  scanLiveUpdate(id, wrap);
  return wrap;
}
function refreshScanning(id) { scanLiveUpdate(id); }
// Update only the dynamic parts (count, total, chips) — never the radar.
function scanLiveUpdate(id, root) {
  root = root || document.querySelector(".scan-stage");
  if (!root) return;
  const s = getSession(id), def = SCAN_DEFS[id];
  const amt = root.querySelector("#scan-amt");
  if (amt) amt.textContent = fmtBytes(foundBytes(s));
  const sub = root.querySelector("#scan-sub");
  if (sub) sub.textContent = `${s.items.length} item${s.items.length === 1 ? "" : "s"} found · scanning your Mac…`;
  const chipsEl = root.querySelector("#scan-chips");
  if (!chipsEl) return;
  const ts = {};
  for (const it of s.items) { const e = ts[it.category] || [0, 0]; ts[it.category] = [e[0] + 1, e[1] + Number(it.size_bytes)]; }
  chipsEl.innerHTML = "";
  for (const st of stagesFor(def)) {
    const t = ts[st.cat], active = !!t;
    chipsEl.append(h("div", { class: `scan-chip${active ? " on" : ""}` },
      h("span", { class: "ci" }, icon(st.icon)),
      h("span", { class: "cl" }, st.label),
      h("span", { class: "cc mono" }, active ? `${t[0]} · ${fmtBytes(t[1])}` : "…")));
  }
}

function reviewPanel(id) {
  const s = getSession(id);
  const wrap = h("div", { class: "review" });
  if (!state.fda) wrap.append(fdaNotice());
  // summary
  wrap.append(summaryHeader(s));
  wrap.append(scannerFooter(s));
  // grouped list
  const list = h("div", { class: "list", id: `list-${id}` });
  renderReviewList(list, id);
  wrap.append(list);
  // reclaim bar
  wrap.append(reclaimBar(id));
  return wrap;
}

function summaryHeader(s) {
  const sb = selectedBytes(s);
  const head = h("div", { class: "summary card" });
  head.append(h("div", { class: "stats" },
    statBlock("Found", fmtBytes(foundBytes(s)), `${s.items.length} items`, false),
    statBlock("Selected", fmtBytes(sb), `${s.selection.size} items`, sb > 0)));
  const total = foundBytes(s) || 1;
  for (const t of tallies(s.items)) {
    head.append(h("div", { class: "tally" },
      h("span", { class: "name" }, t.category),
      h("span", { class: "prop" }, h("div", { style: `width:${Math.max(t.bytes / total * 100, 2)}%` })),
      h("span", { class: "count mono" }, t.count),
      h("span", { class: "size" }, fmtBytes(t.bytes))));
  }
  return head;
}
function statBlock(k, v, sub, sel) {
  return h("div", { class: "stat" }, h("div", { class: "k" }, k),
    h("div", { class: "v" + (sel ? " sel" : "") }, v), h("div", { class: "sub" }, sub));
}
function scannerFooter(s) {
  if (!s.summary) return h("span");
  const blocked = s.summary.scanners.reduce((a, o) => a + Number(o.guard_rejected || 0), 0);
  const errored = s.summary.scanners.filter((o) => o.error);
  if (!blocked && !errored.length) return h("span");
  const row = h("div", { class: "row", style: "padding:4px 24px;font-size:11px;color:var(--text-dim)" });
  if (blocked) row.append(h("span", {}, `🛡 ${blocked} blocked by safety guard`));
  errored.forEach((o) => row.append(h("span", {}, `⚠ ${o.id}: ${o.error}`)));
  return row;
}

function renderReviewList(list, id) {
  const s = getSession(id);
  list.innerHTML = "";
  const groups = {};
  for (const it of s.items) (groups[it.category] ||= []).push(it);
  const ordered = Object.entries(groups).sort((a, b) =>
    b[1].reduce((x, i) => x + Number(i.size_bytes), 0) - a[1].reduce((x, i) => x + Number(i.size_bytes), 0));
  for (const [cat, items] of ordered) {
    const tot = items.reduce((x, i) => x + Number(i.size_bytes), 0);
    list.append(h("div", { class: "cat-head" }, h("span", {}, catName(cat)),
      h("span", { class: "dim mono", style: "font-size:12px" }, `${items.length} · ${fmtBytes(tot)}`)));
    for (const it of items) list.append(reviewRow(it, id, list));
  }
}
// A visible "Reveal in Finder" button for any result row. Clicking it must NOT
// toggle the row's selection, so it stops propagation.
// Build the button (with its parsed icon SVG) once, then cloneNode per row —
// avoids re-parsing the identical SVG for every result in a large list.
let _revealTemplate = null;
function revealBtn(path) {
  if (!_revealTemplate) {
    _revealTemplate = h("button", { class: "reveal-btn" }, icon("folder-search"), "Reveal");
  }
  const b = _revealTemplate.cloneNode(true);
  b.title = `Reveal in Finder:\n${path}`;
  b.addEventListener("click", (e) => { e.stopPropagation(); invoke("reveal_in_finder", { path }); });
  return b;
}
// Shared selectable result row: a checkbox + caller-supplied middle content + a
// Reveal button, where clicking anywhere on the row (except the buttons) toggles
// selection. Used by every review screen so the click/stop-propagation contract
// lives in one place. `onToggle(on)` mutates the owning selection set + bar; the
// factory owns the checkbox state. `path` also tags the checkbox so a bulk
// action can re-sync checkboxes in place without a full re-render.
function selectableRow({ rowClass = "item selectable", checked, path, content, onToggle }) {
  const cb = h("input", { type: "checkbox" });
  cb.checked = checked;
  if (path !== undefined) cb.dataset.path = path;
  const setSel = (on) => { cb.checked = on; onToggle(on); };
  cb.addEventListener("click", (e) => e.stopPropagation());
  cb.addEventListener("change", () => setSel(cb.checked));
  const row = h("div", { class: rowClass }, cb, ...content, revealBtn(path));
  row.addEventListener("click", () => setSel(!cb.checked));
  return row;
}
// Shared "items left untouched, with reason" box for any reclaim report. Returns
// null when there's nothing skipped, so callers can `if (box) c.append(box)`.
function skippedBox(report, heading) {
  const fails = (report.outcomes || []).filter((o) => o.error);
  if (!fails.length) return null;
  const box = h("div", { class: "toast-fail" }, h("div", { style: "font-weight:600;margin-bottom:6px" }, heading));
  fails.forEach((f) => box.append(h("div", { class: "ln" }, `• ${displayPath(f.path)} — ${f.error}`)));
  return box;
}
// Telemetry: unchecking a Safe suggestion is a false-positive signal. Backend
// no-ops unless the user opted in; only category/tier/size bucket — never path.
function maybeRecordDeselect(it) {
  if (it.tier !== "Safe") return;
  invoke("record_deselection", {
    category: it.category, tier: it.tier,
    sizeBytes: it.size_bytes, tsUnix: Math.floor(Date.now() / 1000),
  }).catch(() => {});
}
function reviewRow(it, id) {
  const s = getSession(id);
  return selectableRow({
    checked: s.selection.has(it.path), path: it.path,
    content: [
      h("div", { class: "path" }, h("div", { class: "p", title: it.path }, displayPath(it.path)),
        h("div", { class: "reason" }, it.reason)),
      h("span", { class: "isize" }, fmtBytes(it.size_bytes)),
      h("span", { class: `badge ${it.tier}` }, it.tier),
    ],
    onToggle: (on) => {
      if (on) s.selection.add(it.path);
      else { s.selection.delete(it.path); maybeRecordDeselect(it); }
      updateReclaimBar(id);
    },
  });
}

function reclaimBar(id) {
  const s = getSession(id);
  const bar = h("div", { class: "reclaim-bar", id: `bar-${id}` });
  renderReclaimBar(bar, id);
  return bar;
}
function updateReclaimBar(id) { const b = document.getElementById(`bar-${id}`); if (b) renderReclaimBar(b, id); }
function renderReclaimBar(bar, id) {
  const s = getSession(id);
  bar.innerHTML = "";
  bar.append(
    h("button", { onClick: () => { s.items.forEach((i) => { if (i.tier === "Safe") s.selection.add(i.path); }); rerenderReview(id); } }, "Select All Safe"),
    h("button", { onClick: () => { s.selection.clear(); rerenderReview(id); }, disabled: s.selection.size === 0 ? "" : null }, "Deselect All"),
    h("div", { class: "spacer" }),
    h("div", { class: "tot" }, h("div", { class: "v" }, fmtBytes(selectedBytes(s))), h("div", { class: "k" }, `${s.selection.size} selected`)),
    h("button", { class: "danger lg", disabled: s.selection.size === 0 ? "" : null, onClick: () => confirmReclaim(id) }, "Move to Trash"));
}
function rerenderReview(id) {
  const list = document.getElementById(`list-${id}`);
  if (list) renderReviewList(list, id);
  updateReclaimBar(id);
  // refresh summary selected figure
  if (id === state.current) scanView(id);
}

function confirmReclaim(id) {
  const s = getSession(id);
  const n = s.selection.size, b = fmtBytes(selectedBytes(s));
  if (!confirm(`Move ${n} item${n === 1 ? "" : "s"} (${b}) to the Trash?\n\nItems go to the Trash so you can restore them. Tabibu writes an undo record first.`)) return;
  const items = s.items.filter((i) => s.selection.has(i.path));
  s.phase = "reclaiming"; scanView(id);
  invoke("reclaim", { items, extraRoots: [] })
    .then((report) => { s.phase = "done"; s.report = report; if (id === state.current) scanView(id); })
    .catch((e) => { s.phase = "error"; s.error = String(e); if (id === state.current) scanView(id); });
}

function resultPanel(id) {
  const s = getSession(id), r = s.report;
  const c = h("div", { class: "center" },
    icon("sparkles", "glyph"),
    h("h2", {}, `${fmtBytes(r.reclaimed_bytes)} reclaimed`),
    h("p", {}, `${r.succeeded} item${r.succeeded === 1 ? "" : "s"} moved to the Trash` + (r.failed ? ` · ${r.failed} skipped` : "")));
  const sb = skippedBox(r, "Skipped (with reason):");
  if (sb) c.append(sb);
  const acts = h("div", { class: "row", style: "margin-top:8px" });
  if (r.manifest_path) acts.append(h("button", { onClick: () => invoke("reveal_in_finder", { path: r.manifest_path }) }, "Show Undo Record"));
  acts.append(h("button", { class: "primary", onClick: () => { resetSession(id); scanView(id); } }, "Done"));
  c.append(acts);
  return c;
}

// =====================================================================
// shared building blocks
// =====================================================================
function centered(ic, title, msg, btn, fn) {
  const c = h("div", { class: "center" }, icon(ic, "glyph"), h("h2", {}, title), h("p", {}, msg));
  if (btn) c.append(h("button", { class: "primary lg", onClick: fn }, btn));
  return c;
}
// Themed hero landing (CleanMyMac-inspired, honest copy): accent orb, title,
// description, feature sub-list, prominent Scan button.
function heroLanding(ic, title, sub, feats, onScan) {
  const featList = h("div", { class: "feats" });
  for (const f of feats) featList.append(h("div", { class: "feat" }, h("span", { class: "fi" }, icon("sparkles")), h("span", {}, f)));
  return h("div", { class: "hero" },
    h("div", { class: "orb" }, icon(ic)),
    h("h2", {}, title),
    h("div", { class: "sub" }, sub),
    feats.length ? featList : null,
    h("button", { class: "scan-btn", onClick: onScan }, "Scan"));
}
function centeredSpinner(msg) { return h("div", { class: "center" }, h("div", { class: "spinner" }), h("p", {}, msg)); }
// Spinner with a Stop button for the long synchronous scans (whole-home
// duplicates, leftovers, security) — fires cancel_sync in the backend.
function cancellableSpinner(msg) {
  return h("div", { class: "center" }, h("div", { class: "spinner" }), h("p", {}, msg),
    h("button", { style: "margin-top:8px", onClick: () => invoke("cancel_sync").catch(() => {}) }, "Stop"));
}
function fdaNotice() {
  const recheck = h("button", { onClick: recheckFDA }, "I've enabled it — re-check");
  return h("div", { class: "notice" }, icon("lock-keyhole"),
    h("div", { class: "body" },
      h("h3", {}, "Grant Full Disk Access once for complete results"),
      h("p", {}, "macOS gates other apps' containers, Safari/Mail data, and many caches behind Full Disk Access. This one toggle is the universal grant — Tabibu can't enable it for you (Apple security), and there's no per-folder shortcut. Turn Tabibu on in System Settings → Privacy & Security → Full Disk Access, then come back."),
      h("div", { class: "row", style: "gap:8px" },
        h("button", { class: "primary", onClick: () => invoke("open_url", { url: "x-apple.systempreferences:com.apple.preference.security?Privacy_AllFiles" }) }, "Open Privacy Settings"),
        recheck)));
}
async function recheckFDA() {
  try { const info = await invoke("system_info"); state.fda = info.full_disk_access; } catch {}
  updateFooter();
  render();
}

// =====================================================================
// Duplicates
// =====================================================================
const dupeState = { phase: "idle", groups: [], selection: new Set() };
function duplicatesView() {
  setTitle("Duplicates", []);
  const d = dupeState;
  if (d.phase === "idle") {
    return setView(heroLanding("copy", "Find Duplicate Files",
      "Tabibu scans your whole Mac and compares files by content (not just name). You choose which copies to remove — nothing is deleted without your say-so.",
      ["Scans everything in your home", "Content-compared, not name-matched", "You pick exactly what to delete"],
      scanDupes));
  }
  if (d.phase === "scanning") return setView(cancellableSpinner("Comparing files by content across your Mac… this can take a while."));
  if (d.phase === "reclaiming") return setView(centeredSpinner("Moving duplicates to the Trash…"));
  if (d.phase === "error") return setView(centered("circle-alert", "Something went wrong", d.error, "Try Again", () => { d.phase = "idle"; render(); }));
  if (d.phase === "done") return setView(dupeResult());
  return setView(dupeReview());
}
// Always scans the whole Mac (the home tree) — no folder choice.
async function scanDupes() {
  const d = dupeState;
  d.phase = "scanning"; d.groups = []; d.selection = new Set();
  render();
  try {
    d.groups = await invoke("find_duplicates", { root: null, minSize: 4096 });
    d.phase = d.groups.length ? "review" : "idle";
    if (!d.groups.length) setTimeout(() => alert("No duplicate files found."), 50);
  } catch (e) {
    if (/cancel/i.test(String(e))) { d.phase = "idle"; } else { d.phase = "error"; d.error = String(e); }
  }
  render();
}
function dupeReclaimBytes() {
  let t = 0;
  for (const g of dupeState.groups) for (const p of g.paths) if (dupeState.selection.has(p)) t += Number(g.size_bytes);
  return t;
}
function dupeReview() {
  const d = dupeState;
  const wrap = h("div", { class: "review" });
  const totDup = d.groups.reduce((a, g) => a + Number(g.size_bytes) * Math.max(g.paths.length - 1, 0), 0);
  wrap.append(h("div", { class: "row", style: "padding:16px 24px" },
    h("div", {}, h("div", { style: "font-weight:600" }, `${d.groups.length} duplicate sets · up to ${fmtBytes(totDup)} reclaimable`),
      h("div", { class: "dim", style: "font-size:11px" }, "Tick the copies to delete. The newest in each set is marked — but you can remove any copy you choose."))));
  const list = h("div", { class: "list" });
  for (const g of d.groups) {
    const box = h("div", { class: "dupe-group" }, h("div", { class: "gh" }, `${fmtBytes(g.size_bytes)} each · ${g.paths.length} copies`));
    g.paths.forEach((p, idx) => box.append(dupePathRow(g, p, idx)));
    list.append(box);
  }
  wrap.append(list);
  const bar = h("div", { class: "reclaim-bar" });
  // Hold live element refs so updates target THIS view's nodes (no global
  // getElementById by a literal id that could collide with another pane).
  dupeState.listEl = list;
  dupeState.barEl = bar;
  renderDupeBar(bar);
  wrap.append(bar);
  return wrap;
}
// Every copy is independently selectable (incl. the newest) + revealable.
function dupePathRow(g, p, idx) {
  const labelEls = [h("span", { class: "mono", style: "font-size:12px", title: p }, displayPath(p))];
  if (idx === 0) labelEls.push(h("span", { class: "keep" }, "newest"));
  return selectableRow({
    rowClass: "dupe-path selectable", checked: dupeState.selection.has(p), path: p,
    content: [h("div", { style: "flex:1;min-width:0;display:flex;align-items:center;gap:8px;overflow:hidden" }, ...labelEls)],
    onToggle: (on) => { if (on) dupeState.selection.add(p); else dupeState.selection.delete(p); updateDupeBar(); },
  });
}
// Reflect the selection set onto the existing checkboxes without rebuilding the
// (potentially huge) list — used by the bulk select/clear buttons.
function syncDupeRows() {
  if (!dupeState.listEl) return;
  dupeState.listEl.querySelectorAll("input[type=checkbox]").forEach((cb) => {
    if (cb.dataset.path !== undefined) cb.checked = dupeState.selection.has(cb.dataset.path);
  });
}
function updateDupeBar() { if (dupeState.barEl && dupeState.barEl.isConnected) renderDupeBar(dupeState.barEl); }
function renderDupeBar(bar) {
  bar.innerHTML = "";
  bar.append(
    h("button", { onClick: () => { dupeState.groups.forEach((g) => g.paths.slice(1).forEach((p) => dupeState.selection.add(p))); syncDupeRows(); updateDupeBar(); } }, "Select all but newest"),
    h("button", { onClick: () => { dupeState.selection.clear(); syncDupeRows(); updateDupeBar(); }, disabled: dupeState.selection.size === 0 ? "" : null }, "Clear"),
    h("div", { class: "spacer" }),
    h("div", { class: "tot" }, h("div", { class: "v" }, fmtBytes(dupeReclaimBytes())), h("div", { class: "k" }, `${dupeState.selection.size} selected`)),
    h("button", { class: "danger lg", disabled: dupeState.selection.size === 0 ? "" : null, onClick: reclaimDupes }, "Move to Trash"));
}
function reclaimDupes() {
  const d = dupeState;
  // Guard against deleting every copy of a file (data loss). Trash is
  // recoverable, but warn clearly.
  const wiped = d.groups.filter((g) => g.paths.every((p) => d.selection.has(p))).length;
  let msg = `Move ${d.selection.size} duplicate file(s) (${fmtBytes(dupeReclaimBytes())}) to the Trash?`;
  if (wiped > 0) msg += `\n\n⚠ ${wiped} set(s) have ALL copies selected — every copy will be trashed (recoverable from the Trash).`;
  if (!confirm(msg)) return;
  const items = [];
  for (const g of d.groups) {
    const keeper = g.paths[0];
    for (const p of g.paths) if (d.selection.has(p))
      items.push({ path: p, category: "Duplicate", size_bytes: g.size_bytes, tier: "Review",
        reason: `Duplicate of ${keeper.split("/").pop()}`, selected: true, action: "Trash" });
  }
  d.phase = "reclaiming"; render();
  // Whole-home scope; the engine still protects Documents/Desktop/Pictures/
  // Mail/iCloud via the denylist (those are reported skipped, never touched).
  invoke("reclaim", { items, extraRoots: [state.home] })
    .then((r) => { d.phase = "done"; d.report = r; render(); })
    .catch((e) => { d.phase = "error"; d.error = String(e); render(); });
}
function dupeResult() {
  const r = dupeState.report;
  const c = h("div", { class: "center" }, icon("sparkles", "glyph"),
    h("h2", {}, `${fmtBytes(r.reclaimed_bytes)} reclaimed`),
    h("p", {}, `${r.succeeded} duplicate${r.succeeded === 1 ? "" : "s"} moved to the Trash`
      + (r.failed ? ` · ${r.failed} skipped` : "")));
  // Show skipped items (e.g. copies in protected folders left untouched) so the
  // user understands why a selection wasn't fully removed.
  const sb = skippedBox(r, "Skipped (left untouched):");
  if (sb) c.append(sb);
  c.append(h("button", { class: "primary", onClick: () => { dupeState.phase = "idle"; dupeState.groups = []; dupeState.selection = new Set(); render(); } }, "Done"));
  return c;
}

// =====================================================================
// Disk (treemap)
// =====================================================================
const diskState = { loading: true, root: null, stack: [], error: null };
async function diskView() {
  setTitle("Disk", []);
  if (diskState.loading) {
    setView(centeredSpinner("Measuring your home folder…"));
    try {
      const tree = await invoke("size_tree", { root: state.home, maxDepth: 4 });
      diskState.root = tree; diskState.stack = [tree]; diskState.loading = false;
      if (state.current === "disk") diskView();
    } catch (e) { diskState.error = String(e); diskState.loading = false; if (state.current === "disk") diskView(); }
    return;
  }
  if (diskState.error) return setView(centered("circle-alert", "Couldn't read the disk", diskState.error, "Retry", () => { diskState.loading = true; diskState.error = null; diskView(); }));
  const node = diskState.stack[diskState.stack.length - 1];
  const wrap = h("div", {});
  // Free-space + SMART header (fetched once, cached on diskState).
  const hdr = h("div", { class: "row", style: "padding:14px 24px 4px", id: "disk-hdr" });
  wrap.append(hdr);
  if (!diskState.meta) {
    Promise.all([invoke("disk_space").catch(() => null), invoke("smart_status").catch(() => "Unknown")])
      .then(([disk, smart]) => { diskState.meta = { disk, smart }; const el = document.getElementById("disk-hdr"); if (el) fillDiskHeader(el); });
  } else { fillDiskHeader(hdr); }
  // breadcrumb
  const crumbs = h("div", { class: "crumbs" });
  diskState.stack.forEach((n, idx) => {
    if (idx) crumbs.append(icon("chevron-right"));
    const name = n.path.split("/").pop() || n.path;
    crumbs.append(h("span", { class: "c" + (idx === diskState.stack.length - 1 ? " last" : ""),
      onClick: () => { if (idx < diskState.stack.length - 1) { diskState.stack = diskState.stack.slice(0, idx + 1); diskView(); } } }, name));
  });
  wrap.append(crumbs);
  wrap.append(treemap(node));
  setView(wrap);
}
function fillDiskHeader(el) {
  const m = diskState.meta || {};
  el.innerHTML = "";
  if (m.disk) {
    const usedPct = Math.round((1 - m.disk.available_bytes / Math.max(m.disk.total_bytes, 1)) * 100);
    el.append(h("span", { style: "font-weight:600" }, `${fmtBytes(m.disk.available_bytes)} free`),
      h("span", { class: "dim" }, `of ${fmtBytes(m.disk.total_bytes)} · ${usedPct}% used`));
  }
  el.append(h("div", { class: "spacer" }));
  const ok = m.smart === "Verified";
  el.append(h("span", { class: "dim", style: "font-size:12px" }, "SMART:"),
    h("span", { style: `font-weight:600;color:${ok ? "var(--tier-safe)" : "var(--text-dim)"}` }, m.smart || "Unknown"));
}
function treemap(node) {
  const box = h("div", { class: "treemap", style: "height:520px" });
  const kids = (node.children || []).filter((c) => Number(c.size_bytes) > 0).sort((a, b) => Number(b.size_bytes) - Number(a.size_bytes));
  const total = kids.reduce((a, c) => a + Number(c.size_bytes), 0) || 1;
  // slice-and-dice
  const W = 100, H = 100; let offset = 0;
  const horizontal = true;
  kids.forEach((c, i) => {
    const frac = Number(c.size_bytes) / total;
    const hue = 165 + (c.path.length % 6) * 8;
    const tile = h("div", { class: "tile",
      style: `left:${offset}%;top:0;width:${frac * W}%;height:100%;background:hsl(${hue} 42% 46%)`,
      title: `${displayPath(c.path)} — ${fmtBytes(c.size_bytes)}`,
      onClick: () => { if (c.is_dir && (c.children || []).length) { diskState.stack.push(c); diskView(); } } });
    if (frac * W > 6) tile.append(h("div", { class: "tn" }, c.path.split("/").pop()), h("div", { class: "ts" }, fmtBytes(c.size_bytes)));
    box.append(tile);
    offset += frac * W;
  });
  if (!kids.length) box.append(h("div", { class: "center" }, h("p", { class: "dim" }, "This folder is empty or unreadable.")));
  return box;
}

// =====================================================================
// Memory & CPU
// =====================================================================
const DAEMON_NOTES = {
  kernel_task: "Not a bug to fix: macOS uses kernel_task to absorb CPU time and keep the Mac cool when it heats up. High usage usually means something else is generating heat.",
  mds_stores: "Spotlight building its search index. Spikes after updates or large file changes, then settles.",
  mdworker: "Spotlight indexing helper — temporary while indexing.",
  photoanalysisd: "Photos analyzing your library for faces and scenes. Finishes faster plugged in and idle.",
  WindowServer: "Draws everything on screen. High usage often comes from many windows or scaled/external displays.",
  bird: "iCloud Drive sync. Activity means files are uploading or downloading.",
  cloudd: "iCloud sync engine. Busy during large syncs.",
  trustd: "Verifies code signatures and certificates. Brief spikes when launching new apps.",
  backupd: "Time Machine backup in progress.",
};
const monState = { byCpu: true, sample: null, thermal: null };
function memoryView() {
  setTitle("Memory & CPU", []);
  invoke("thermal_info").then((t) => { monState.thermal = t; if (state.current === "memory") renderMemory(); }).catch(() => {});
  const tick = async () => {
    try { monState.sample = await invoke("monitor_sample", { topN: 12, byCpu: monState.byCpu }); }
    catch { return; }
    if (state.current === "memory") renderMemory();
  };
  tick();
  const t = setInterval(tick, 2000);
  activeTimers.push(t);
  renderMemory();
}
function ring(value, label, color) {
  const r = 38, c = 2 * Math.PI * r, off = c * (1 - Math.min(Math.max(value, 0), 1));
  return h("div", { class: "dial" },
    h("div", { html: `<svg viewBox="0 0 92 92"><circle cx="46" cy="46" r="${r}" fill="none" stroke="var(--track)" stroke-width="8"/><circle cx="46" cy="46" r="${r}" fill="none" stroke="${color}" stroke-width="8" stroke-linecap="round" stroke-dasharray="${c}" stroke-dashoffset="${off}" transform="rotate(-90 46 46)"/><text x="46" y="52" text-anchor="middle" font-size="18" font-weight="600" fill="var(--text)">${label}</text></svg>` }));
}
function renderMemory() {
  const s = monState.sample;
  if (!s) return setView(centeredSpinner("Sampling…"));
  const memFrac = Number(s.used_memory_bytes) / Math.max(Number(s.total_memory_bytes), 1);
  const wrap = h("div", {});
  const memColor = memFrac > 0.9 ? "var(--tier-risky)" : memFrac > 0.75 ? "var(--tier-review)" : "var(--tier-safe)";
  wrap.append(h("div", { class: "dials" },
    ring(memFrac, `${Math.round(memFrac * 100)}%`, memColor),
    h("div", { class: "dial" }, ring(Number(s.cpu_percent) / 100, `${Math.round(s.cpu_percent)}%`, "var(--accent)"), h("div", { class: "lbl" }, "CPU")),
    h("div", {},
      kv("Memory used", `${fmtBytes(s.used_memory_bytes)} / ${fmtBytes(s.total_memory_bytes)}`),
      kv("Swap used", fmtBytes(s.used_swap_bytes)),
      kv("Thermal", monState.thermal ? monState.thermal.pressure + (monState.thermal.speed_limit != null ? ` (CPU ${monState.thermal.speed_limit}%)` : "") : "—"))));
  if (memFrac > 0.9) {
    const heavy = [...s.top_processes].sort((a, b) => Number(b.memory_bytes) - Number(a.memory_bytes))[0];
    wrap.append(h("div", { class: "notice" }, icon("circle-alert"),
      h("div", { class: "body" }, h("p", {}, heavy ? `Memory is nearly full. The heaviest app right now is ${heavy.name} (${fmtBytes(heavy.memory_bytes)}). Quitting apps you're not using frees memory honestly — there's no magic button.` : "Memory is nearly full. Quitting unused apps is the real fix."))));
  }
  const head = h("div", { class: "row", style: "padding:8px 24px" }, h("h3", {}, "Top processes"), h("div", { class: "spacer" }),
    h("div", { class: "seg" },
      h("button", { class: monState.byCpu ? "on" : "", onClick: () => { monState.byCpu = true; renderMemory(); } }, "By CPU"),
      h("button", { class: !monState.byCpu ? "on" : "", onClick: () => { monState.byCpu = false; renderMemory(); } }, "By Memory")));
  wrap.append(head);
  for (const p of s.top_processes) {
    const nameEl = h("span", { class: "pname" }, p.name);
    if (p.is_translated === true) nameEl.append(h("span", { class: "rosetta", title: "Running under Rosetta (x86_64 translated)" }, "Rosetta"));
    const row = h("div", { class: "proc" }, nameEl);
    if (DAEMON_NOTES[p.name]) {
      const inf = icon("info"); inf.style.cursor = "pointer"; inf.style.color = "var(--text-dim)";
      inf.addEventListener("click", (e) => showPopover(e, DAEMON_NOTES[p.name]));
      row.append(inf);
    }
    const quit = h("button", { title: "Quit / Force Quit this process", style: "padding:2px 8px" }, "Quit");
    quit.addEventListener("click", () => quitProcess(p));
    row.append(h("div", { class: "spacer" }), h("span", { class: "pcpu" }, `${Math.round(p.cpu_percent)}%`),
      h("span", { class: "pmem" }, fmtBytes(p.memory_bytes)), quit);
    wrap.append(row);
  }
  setView(wrap);
}
function quitProcess(p) {
  const ok = confirm(`Quit "${p.name}" (pid ${p.pid})?\n\nThis asks the process to quit. Save your work first — Tabibu can't recover unsaved changes.\n\nPress OK to Quit, or Cancel.`);
  if (!ok) return;
  invoke("quit_process", { pid: p.pid, force: false })
    .then(() => setTimeout(() => renderMemory(), 600))
    .catch((e) => {
      if (confirm(`Couldn't quit gracefully (${e}).\n\nForce Quit "${p.name}"? This kills it immediately and may lose unsaved work.`)) {
        invoke("quit_process", { pid: p.pid, force: true }).then(() => setTimeout(() => renderMemory(), 600)).catch((e2) => alert(String(e2)));
      }
    });
}
function kv(k, v) { return h("div", { class: "kv" }, h("span", { class: "k" }, k), h("span", { class: "v" }, v)); }
let popoverEl = null;
function showPopover(e, text) {
  if (popoverEl) popoverEl.remove();
  popoverEl = h("div", { class: "popover-note" }, text);
  document.body.append(popoverEl);
  popoverEl.style.left = Math.min(e.clientX, window.innerWidth - 340) + "px";
  popoverEl.style.top = (e.clientY + 12) + "px";
  setTimeout(() => document.addEventListener("click", function close() { if (popoverEl) popoverEl.remove(); popoverEl = null; document.removeEventListener("click", close); }), 0);
}

// =====================================================================
// Battery
// =====================================================================
async function batteryView() {
  setTitle("Battery", []);
  const info = await invoke("battery_info");
  if (!info.has_battery) return setView(centered("battery", "No battery on this Mac", "This appears to be a desktop Mac or has no internal battery, so there's nothing to report here."));
  const wrap = h("div", { class: "pad" });
  if (info.charge_percent != null) {
    const color = info.charge_percent < 20 ? "var(--tier-risky)" : "var(--tier-safe)";
    wrap.append(h("div", { class: "dials" }, ring(info.charge_percent / 100, `${info.charge_percent}%`, color),
      h("div", {}, info.state ? kv("State", info.state) : null, info.time_remaining ? kv("Time remaining", info.time_remaining) : null)));
  }
  const health = h("div", { class: "card", style: "margin-top:16px;max-width:420px" }, h("h3", { style: "margin-bottom:8px" }, "Battery health"));
  if (info.health_percent != null) health.append(kv("Capacity vs. design", `${info.health_percent}%`));
  if (info.cycle_count != null) health.append(kv("Cycle count", String(info.cycle_count)));
  if (info.condition) health.append(kv("Condition", info.condition));
  if (info.health_percent == null && info.cycle_count == null) health.append(h("p", { class: "dim" }, "Detailed health metrics weren't available from this Mac's battery controller."));
  wrap.append(health);
  setView(wrap);
}

// =====================================================================
// Uninstaller
// =====================================================================
const uninstallState = { phase: "browsing", apps: [], query: "", selected: null, remnants: [], chosen: new Set(), trashApp: true };
async function uninstallerView() {
  setTitle("Uninstaller", []);
  const u = uninstallState;
  if (u.phase === "browsing") {
    if (!u.apps.length) { try { u.apps = await invoke("installed_apps"); } catch (e) { /* ignore */ } }
    return renderAppBrowser();
  }
  if (u.phase === "hunting") return setView(u.leftovers ? cancellableSpinner("Scanning for leftover files…") : centeredSpinner("Finding leftover files…"));
  if (u.phase === "reclaiming") return setView(centeredSpinner("Moving to the Trash…"));
  if (u.phase === "done") {
    const r = u.report;
    const c = h("div", { class: "center" }, icon("sparkles", "glyph"),
      h("h2", {}, `${fmtBytes(r.reclaimed_bytes)} reclaimed`),
      h("p", {}, `${r.succeeded} item(s) moved to the Trash` + (r.failed ? ` · ${r.failed} skipped` : "")));
    // Surface items reclaim left untouched (protected/failed) — this flow builds
    // items outside the scan-time guard, so it's the one place skips can occur.
    const sb = skippedBox(r, "Skipped (left untouched):");
    if (sb) c.append(sb);
    // And surface a failure to trash the app bundle itself (no longer swallowed).
    if (u.trashAppError) c.append(h("div", { class: "toast-fail" },
      h("div", { class: "ln" }, `• Could not move the app to the Trash — ${u.trashAppError}`)));
    c.append(h("button", { class: "primary", onClick: () => { u.phase = "browsing"; u.selected = null; u.trashAppError = null; render(); } }, "Done"));
    return setView(c);
  }
  if (u.phase === "error") return setView(centered("circle-alert", "Something went wrong", u.error, "Back", () => { u.phase = "browsing"; render(); }));
  return renderRemnants();
}
function renderAppBrowser() {
  const u = uninstallState;
  const wrap = h("div", {});
  if (!state.fda) wrap.append(fdaNotice());
  wrap.append(h("div", { class: "row", style: "padding:12px 24px" },
    (() => { const i = h("input", { class: "toolbar-input", placeholder: "Search apps" }); i.value = u.query; i.addEventListener("input", () => { u.query = i.value; renderAppList(listEl); }); return i; })(),
    h("div", { class: "spacer" }),
    h("button", { onClick: scanLeftovers, title: "Find support files left behind by apps you've already removed" }, "Find Leftovers…")));
  const listEl = h("div", {});
  wrap.append(listEl);
  renderAppList(listEl);
  setView(wrap);
}
function renderAppList(listEl) {
  const u = uninstallState;
  listEl.innerHTML = "";
  const apps = u.query ? u.apps.filter((a) => a.name.toLowerCase().includes(u.query.toLowerCase())) : u.apps;
  for (const a of apps) {
    listEl.append(h("div", { class: "list-row" },
      h("div", { class: "meta" }, h("div", { class: "t" }, a.name), h("div", { class: "s mono" }, a.bundle_id)),
      h("button", { onClick: () => huntRemnants(a) }, "Uninstall…")));
  }
}
async function huntRemnants(app) {
  const u = uninstallState;
  u.selected = app; u.leftovers = false; u.trashApp = true; u.phase = "hunting"; render();
  try {
    u.remnants = await invoke("find_remnants", { bundleId: app.bundle_id, appName: app.name });
    u.chosen = new Set(u.remnants.filter((r) => r.tier === "Review").map((r) => r.path));
    u.phase = "review";
  } catch (e) { u.phase = "error"; u.error = String(e); }
  render();
}
// Disk-wide leftover/orphan artifacts of apps already uninstalled.
async function scanLeftovers() {
  const u = uninstallState;
  u.selected = { name: "Leftovers", path: null }; u.leftovers = true; u.trashApp = false;
  u.phase = "hunting"; render();
  try {
    u.remnants = await invoke("scan_orphans");
    u.chosen = new Set(); // orphans are Risky — never pre-selected
    u.phase = "review";
  } catch (e) { u.phase = "error"; u.error = String(e); }
  render();
}
function renderRemnants() {
  const u = uninstallState;
  const wrap = h("div", { class: "review" });
  const title = u.leftovers ? "Leftovers from uninstalled apps" : `Uninstall ${u.selected.name}`;
  const sub = u.leftovers
    ? `${u.remnants.length} orphaned item(s) — owning app no longer installed. Review each: these are Risky, so nothing is pre-selected.`
    : `${u.remnants.length} related item(s) found`;
  wrap.append(h("div", { class: "row", style: "padding:16px 24px" },
    h("div", {}, h("div", { style: "font-weight:600" }, title),
      h("div", { class: "dim", style: "font-size:11px" }, sub)),
    h("div", { class: "spacer" }), h("button", { onClick: () => { u.phase = "browsing"; render(); } }, "Back")));
  if (!u.leftovers) {
    const tog = h("input", { type: "checkbox" }); tog.checked = u.trashApp;
    tog.addEventListener("change", () => { u.trashApp = tog.checked; updateRemnantBar(); });
    wrap.append(h("label", { class: "row", style: "padding:0 24px 8px" }, tog, h("span", {}, "Also move the app to the Trash")));
  }
  const list = h("div", { class: "list" });
  for (const it of u.remnants) {
    list.append(selectableRow({
      checked: u.chosen.has(it.path), path: it.path,
      content: [
        h("div", { class: "path" }, h("div", { class: "p", title: it.path }, displayPath(it.path)), h("div", { class: "reason" }, it.reason)),
        h("span", { class: "isize" }, fmtBytes(it.size_bytes)),
        h("span", { class: `badge ${it.tier}` }, it.tier),
      ],
      onToggle: (on) => { if (on) u.chosen.add(it.path); else u.chosen.delete(it.path); updateRemnantBar(); },
    }));
  }
  wrap.append(list);
  const bar = h("div", { class: "reclaim-bar" });
  u.barEl = bar; // live ref — avoids a global literal-id lookup
  wrap.append(bar);
  renderRemnantBar(bar);
  return setView(wrap);
}
function updateRemnantBar() { const u = uninstallState; if (u.barEl && u.barEl.isConnected) renderRemnantBar(u.barEl); }
function renderRemnantBar(bar) {
  const u = uninstallState;
  // Disable when there's nothing to do (no items chosen and not trashing the
  // app) — otherwise the button looks active but doUninstall silently no-ops.
  const nothingToDo = u.chosen.size === 0 && !u.trashApp;
  bar.innerHTML = "";
  bar.append(
    h("div", { class: "dim", style: "font-size:11px" }, `${u.chosen.size} selected`),
    h("div", { class: "spacer" }),
    h("button", { class: "danger lg", disabled: nothingToDo ? "" : null, onClick: doUninstall }, "Uninstall"));
}
async function doUninstall() {
  const u = uninstallState;
  const items = u.remnants.filter((r) => u.chosen.has(r.path));
  if (!items.length && !u.trashApp) return;
  u.phase = "reclaiming"; u.trashAppError = null; render();
  try {
    const home = state.home;
    const report = await invoke("reclaim", { items, extraRoots: [home + "/Library"] });
    // Trashing the .app can fail independently (perms, SIP, missing path);
    // capture it instead of swallowing so the done panel can report it.
    if (u.trashApp) {
      try { await invoke("trash_path", { path: u.selected.path }); }
      catch (e) { u.trashAppError = String(e); }
    }
    u.report = report; u.phase = "done";
  } catch (e) { u.phase = "error"; u.error = String(e); }
  render();
}

// =====================================================================
// Developer / CLI (Homebrew)
// =====================================================================
const brewState = { phase: "idle", report: null, error: null, working: null, outcome: null, stale: false, sort: "size", filter: "" };

// Relative install age. Homebrew records install time but NOT last-used time,
// so this is honestly labelled "installed N ago" — never "last used".
function agoText(unix) {
  if (!Number.isFinite(unix) || unix <= 0) return "date unknown";
  // Floor the difference of seconds (not two separately-floored day counts) so
  // there's no boundary off-by-one; future/skewed timestamps clamp to "today".
  const days = Math.floor((Date.now() / 1000 - unix) / 86400);
  if (days <= 0) return "today";
  if (days === 1) return "yesterday";
  if (days < 30) return `${days}d ago`;
  if (days < 365) return `${Math.round(days / 30)}mo ago`;
  return `${(days / 365).toFixed(1)}y ago`;
}

async function brewView() {
  setTitle("Developer / CLI", []);
  const b = brewState;
  if (b.phase === "idle") return setView(heroLanding("terminal", "Developer / CLI cleanup",
    "Review software installed from the terminal with Homebrew — reclaim old versions and caches, find orphaned dependencies, and spot tools you installed once and forgot.",
    ["Safe: every removal is delegated to brew itself", "Old versions & stale download cache", "Orphaned dependencies & rarely-used tools"],
    analyzeBrew));
  if (b.phase === "analyzing") return setView(centeredSpinner("Analyzing Homebrew packages… (this can take a few seconds)"));
  if (b.phase === "working") return setView(centeredSpinner(b.working || "Working…"));
  if (b.phase === "error") return setView(centered("circle-alert", "Homebrew error", b.error, "Back", () => { b.phase = "idle"; render(); }));
  return setView(renderBrewReport());
}

async function analyzeBrew() {
  const b = brewState;
  b.outcome = null; // a fresh analysis is not "after an action" — drop any toast
  b.phase = "analyzing"; render();
  try { b.report = await invoke("brew_analyze"); b.stale = false; b.phase = "ready"; }
  catch (e) { b.phase = "error"; b.error = String(e); }
  render();
}
// Refresh the report after a destructive action WITHOUT discarding the
// just-set outcome toast, and without bouncing to the error screen on a
// transient analyze failure — the action already happened, so keep showing its
// result over the last good report. If the refresh fails we flag the list as
// stale (so the user knows to re-analyze rather than acting on a removed row).
async function refreshBrewAfterAction() {
  const b = brewState;
  try { b.report = await invoke("brew_analyze"); b.stale = false; }
  catch (e) { b.stale = true; }
  b.phase = "ready"; render();
}

function renderBrewReport() {
  const b = brewState, r = b.report;
  if (!r.status.installed) {
    return centered("terminal", "Homebrew not detected",
      "Homebrew wasn't found at /opt/homebrew or /usr/local. This screen manages packages installed with Homebrew (brew); install it from brew.sh to use it here.",
      "Re-check", analyzeBrew);
  }
  const wrap = h("div", { class: "review" });
  wrap.append(h("div", { class: "row", style: "padding:16px 24px;align-items:center" },
    h("div", {}, h("div", { style: "font-weight:600" }, `${r.packages.length} package${r.packages.length === 1 ? "" : "s"} installed`),
      h("div", { class: "dim", style: "font-size:11px" }, `${r.status.version || "Homebrew"} · ${r.status.prefix || ""}`)),
    h("div", { class: "spacer" }),
    h("button", { onClick: analyzeBrew }, "Re-analyze")));
  if (b.outcome) {
    const o = b.outcome;
    wrap.append(h("div", { class: o.ok ? "toast" : "toast-fail", style: "margin:0 24px 10px" },
      h("div", { style: "font-weight:600;margin-bottom:4px" }, o.ok ? (o.freed_bytes ? `Done — about ${fmtBytes(o.freed_bytes)} freed` : "Done") : "Could not complete"),
      h("pre", { class: "brew-out" }, o.message)));
  }
  if (b.stale) {
    wrap.append(h("div", { class: "toast-fail", style: "margin:0 24px 10px" },
      h("div", { class: "ln" }, "The action completed, but the package list couldn't be refreshed — it may be out of date. Click “Re-analyze”.")));
  }
  wrap.append(brewCleanupCard(r));
  if (r.autoremovable.length) wrap.append(brewAutoremoveCard(r));
  wrap.append(brewPackages(r));
  return wrap;
}

function brewCleanupCard(r) {
  const free = Number(r.cleanup.freeable_bytes);
  return h("div", { class: "brew-card" },
    h("div", { class: "bc-icon" }, icon("trash-2")),
    h("div", { style: "flex:1;min-width:0" },
      h("div", { style: "font-weight:600" }, free > 0 ? `${fmtBytes(free)} reclaimable` : "Download cache is clean"),
      h("div", { class: "dim", style: "font-size:11px" }, "Old package versions and stale download cache. Your installed tools are untouched.")),
    free > 0 ? h("button", { class: "primary", onClick: () => runBrewAction("brew_cleanup", "brew cleanup",
      `Run “brew cleanup”?\n\nThis removes old versions and stale download cache (about ${fmtBytes(free)}). Your currently-installed packages are NOT removed.`) }, "Run cleanup") : null);
}

function brewAutoremoveCard(r) {
  const names = r.autoremovable;
  return h("div", { class: "brew-card" },
    h("div", { class: "bc-icon" }, icon("package")),
    h("div", { style: "flex:1;min-width:0" },
      h("div", { style: "font-weight:600" }, `${names.length} unused ${names.length === 1 ? "dependency" : "dependencies"}`),
      h("div", { class: "dim", style: "font-size:11px;word-break:break-word" }, names.join(", "))),
    h("button", { class: "primary", onClick: () => runBrewAction("brew_autoremove", "brew autoremove",
      `Remove ${names.length} orphaned dependenc${names.length === 1 ? "y" : "ies"} that nothing installed needs anymore?\n\n${names.join(", ")}`) }, "Remove unused"));
}

function brewPackages(r) {
  const b = brewState;
  const wrap = h("div", {});
  const search = h("input", { class: "toolbar-input", placeholder: "Filter packages" });
  search.value = b.filter;
  const listEl = h("div", { class: "list" });
  search.addEventListener("input", () => { b.filter = search.value; renderBrewList(listEl); });
  const sortSel = h("select", { class: "toolbar-input", style: "width:auto" },
    h("option", { value: "size" }, "Largest"),
    h("option", { value: "date" }, "Newest"),
    h("option", { value: "old" }, "Oldest"),
    h("option", { value: "name" }, "Name (A–Z)"));
  sortSel.value = b.sort;
  sortSel.addEventListener("change", () => { b.sort = sortSel.value; renderBrewList(listEl); });
  wrap.append(h("div", { class: "row", style: "padding:8px 24px;align-items:center;gap:8px" },
    h("div", { style: "font-weight:600" }, "Installed packages"), h("div", { class: "spacer" }), search, sortSel));
  wrap.append(h("div", { class: "dim", style: "padding:0 24px 8px;font-size:11px" },
    "Homebrew doesn't record last-used times — install date and dependency status are shown instead. “Uninstall” is refused by brew if another package still depends on it."));
  wrap.append(listEl);
  renderBrewList(listEl);
  return wrap;
}

function renderBrewList(listEl) {
  const b = brewState, r = b.report;
  const q = b.filter.trim().toLowerCase();
  const pkgs = r.packages
    .filter((p) => !q || p.name.toLowerCase().includes(q))
    .slice()
    .sort((a, c) => {
      if (b.sort === "name") return a.name.localeCompare(c.name);
      if (b.sort === "date") return (c.installed_unix || 0) - (a.installed_unix || 0);
      if (b.sort === "old") return (a.installed_unix || 0) - (c.installed_unix || 0);
      return (Number(c.size_bytes) || 0) - (Number(a.size_bytes) || 0);
    });
  listEl.innerHTML = "";
  if (!pkgs.length) { listEl.append(h("div", { class: "dim", style: "padding:16px 24px" }, "No matching packages.")); return; }
  for (const p of pkgs) listEl.append(brewPkgRow(p));
}

function brewPkgRow(p) {
  const tags = [];
  if (p.kind === "cask") tags.push(h("span", { class: "btag" }, "cask"));
  if (p.autoremovable) tags.push(h("span", { class: "btag review" }, "unused dep"));
  else if (p.as_dependency && !p.on_request) tags.push(h("span", { class: "btag" }, "dependency"));
  else if (p.on_request) tags.push(h("span", { class: "btag safe" }, "you installed"));
  // Honest "you installed this and it's been a while" hint — not a usage claim.
  const ageDays = Number.isFinite(p.installed_unix) && p.installed_unix > 0
    ? (Date.now() / 1000 - p.installed_unix) / 86400 : 0;
  if (p.on_request && !p.as_dependency && ageDays > 365) tags.push(h("span", { class: "btag review" }, "installed >1y ago"));
  return h("div", { class: "item" },
    h("div", { class: "path", style: "flex:1;min-width:0" },
      h("div", { class: "p", style: "direction:ltr;text-align:left" }, p.name,
        h("span", { class: "dim mono", style: "font-size:11px;margin-left:8px" }, p.version)),
      h("div", { class: "reason" }, `installed ${agoText(p.installed_unix)}`)),
    h("div", { class: "btags" }, ...tags),
    h("span", { class: "isize" }, fmtBytes(p.size_bytes)),
    h("button", { class: "danger", onClick: () => brewUninstall(p) }, "Uninstall"));
}

async function runBrewAction(cmd, label, confirmMsg) {
  if (!confirm(confirmMsg)) return;
  const b = brewState;
  b.phase = "working"; b.working = `Running ${label}…`; render();
  try { b.outcome = await invoke(cmd); }
  catch (e) { b.outcome = { ok: false, freed_bytes: 0, message: String(e) }; }
  await refreshBrewAfterAction();
}

async function brewUninstall(p) {
  const note = (p.as_dependency && !p.on_request)
    ? `\n\nNote: ${p.name} was installed as a dependency. If anything still needs it, brew will refuse (use “Remove unused” instead).`
    : "";
  if (!confirm(`Uninstall ${p.name} (${p.kind})?\n\nRuns “brew uninstall ${p.name}”. Homebrew refuses if another package depends on it — nothing is forced.${note}`)) return;
  const b = brewState;
  b.phase = "working"; b.working = `Uninstalling ${p.name}…`; render();
  try { b.outcome = await invoke("brew_uninstall", { name: p.name, cask: p.kind === "cask" }); }
  catch (e) { b.outcome = { ok: false, freed_bytes: 0, message: String(e) }; }
  await refreshBrewAfterAction();
}

// =====================================================================
// Startup
// =====================================================================
async function startupView() {
  setTitle("Startup Items", []);
  const rep = await invoke("startup_items");
  const wrap = h("div", {});
  wrap.append(h("div", { class: "row", style: "padding:16px 24px" },
    h("span", { style: "font-weight:600" }, `${rep.items.length} startup item${rep.items.length === 1 ? "" : "s"}`),
    h("div", { class: "spacer" }),
    h("button", { onClick: () => invoke("open_url", { url: "x-apple.systempreferences:com.apple.LoginItems-Settings.extension" }) }, "Open Login Items Settings")));
  if (rep.partial) wrap.append(h("p", { class: "dim", style: "padding:0 24px 8px" }, "Some system startup folders need Full Disk Access to read. The list below may be incomplete."));
  if (!rep.items.length) return setView(centered("activity", "No startup items found", "Nothing is configured to launch at login in the folders Tabibu can read."));
  for (const it of rep.items) {
    wrap.append(h("div", { class: "list-row" },
      h("div", { class: "meta" }, h("div", { class: "t" }, it.label), h("div", { class: "s mono" }, it.program)),
      h("span", { class: "dim", style: "font-size:11px" }, it.scope),
      h("button", { onClick: () => invoke("reveal_in_finder", { path: it.path }) }, "Reveal")));
  }
  setView(wrap);
}

// =====================================================================
// Security (placeholder — heuristics exist in core; scan UI is M7)
// =====================================================================
const secState = { phase: "idle", items: [] };
function securityView() {
  setTitle("Security", []);
  const s = secState;
  if (s.phase === "scanning") return setView(cancellableSpinner("Checking launch agents and configuration profiles…"));
  if (s.phase === "done") {
    if (!s.items.length) return setView(centered("shield", "No threats found",
      "Tabibu's heuristics found no adware launch agents or rogue browser-policy profiles. Tabibu never runs a resident background scanner — re-scan any time."));
    return setView(securityReview());
  }
  const hero = heroLanding("shield", "Security scan",
    "On-demand check for macOS adware launch agents and rogue managed-browser profiles. Detections go to a locked quarantine vault — never deleted. No resident background scanner.",
    ["Adware launch-agent heuristics", "Rogue browser-policy profiles", "Quarantine vault (move + lock, never delete)"],
    runSecurityScan);
  return setView(hero);
}
async function runSecurityScan() {
  secState.phase = "scanning"; render();
  try { secState.items = await invoke("scan_malware"); secState.phase = "done"; }
  catch (e) { secState.items = []; secState.phase = "done"; setTimeout(() => alert(String(e)), 50); }
  render();
}
function securityReview() {
  const wrap = h("div", { class: "review" });
  wrap.append(h("div", { class: "row", style: "padding:16px 24px" },
    h("span", { style: "font-weight:600" }, `${secState.items.length} item(s) to review`),
    h("div", { class: "spacer" }),
    h("button", { onClick: () => { secState.phase = "idle"; render(); } }, "Rescan")));
  wrap.append(h("p", { class: "dim", style: "padding:0 24px 8px" }, "These are heuristic matches — review each before quarantining. Quarantine moves the file to a locked vault; it is never deleted and can be restored."));
  const list = h("div", { class: "list" });
  for (const it of secState.items) {
    list.append(h("div", { class: "item" },
      h("div", { class: "path" }, h("div", { class: "p", title: it.path }, displayPath(it.path)), h("div", { class: "reason" }, it.reason)),
      h("span", { class: `badge ${it.tier}` }, it.tier),
      h("button", { onClick: () => invoke("reveal_in_finder", { path: it.path }) }, "Reveal"),
      h("button", { class: "danger", onClick: () => quarantineItem(it) }, "Quarantine")));
  }
  wrap.append(list);
  return wrap;
}
function quarantineItem(it) {
  if (!confirm(`Move this to the quarantine vault?\n\n${it.path}\n\nIt will be locked (not deleted) and can be restored.`)) return;
  invoke("quarantine", { path: it.path })
    .then(() => { secState.items = secState.items.filter((x) => x.path !== it.path); render(); })
    .catch((e) => alert(String(e)));
}

// =====================================================================
// Dashboard (Mac Health) — real, measured cards + live colorful line graphs.
// No invented metrics: every number comes straight from the OS.
// =====================================================================
const dashHist = { cpu: [], mem: [] }; // rolling live series (last ~40 samples)
function pushDashHist(sample) {
  dashHist.cpu.push(Math.min(sample.cpu_percent, 100));
  dashHist.mem.push(sample.used_memory_bytes / Math.max(sample.total_memory_bytes, 1) * 100);
  for (const k of ["cpu", "mem"]) if (dashHist[k].length > 40) dashHist[k].shift();
}
async function dashboardView() {
  setTitle("Dashboard", []);
  setView(centeredSpinner("Reading system health…"));
  // Slow-moving metrics fetched ONCE on entry — battery/thermal/disk each shell
  // out to subprocesses and change on minute scales, so they don't belong on
  // the 2s tick. record_free_space is called once here (backend throttles to
  // ≤1/hour regardless).
  const [disk, sample, battery, thermal] = await Promise.all([
    invoke("disk_space").catch(() => null),
    invoke("monitor_sample", { topN: 1, byCpu: true }).catch(() => null),
    invoke("battery_info").catch(() => null),
    invoke("thermal_info").catch(() => null),
  ]);
  if (state.current !== "dashboard") return;
  if (sample) pushDashHist(sample);
  let trend = [];
  if (disk) {
    trend = await invoke("record_free_space", {
      tsUnix: Math.floor(Date.now() / 1000), freeBytes: disk.available_bytes,
    }).catch(() => []);
  }
  if (state.current !== "dashboard") return;
  setView(buildDashboard({ disk, sample, battery, thermal, trend }));

  // Live updater: ONLY monitor_sample (cheap, in-process), updating the two
  // graphs and the CPU/Memory cards in place — no subprocess spawns, no full
  // grid rebuild.
  const t = setInterval(async () => {
    if (state.current !== "dashboard") return;
    const s = await invoke("monitor_sample", { topN: 1, byCpu: true }).catch(() => null);
    if (!s || state.current !== "dashboard") return;
    pushDashHist(s);
    replaceNode("dash-mem-card", memCard(s));
    replaceNode("dash-cpu-card", cpuCard(s));
    replaceNode("dash-cpu-graph", graphCard("CPU", "cpu", "#f97316", "%", "dash-cpu-graph"));
    replaceNode("dash-mem-graph", graphCard("Memory", "mem", "#3b82f6", "%", "dash-mem-graph"));
  }, 2000);
  activeTimers.push(t);
}
function replaceNode(id, node) {
  const el = document.getElementById(id);
  if (el) el.replaceWith(node);
}
function memCard(sample) {
  if (!sample) return h("div", { class: "dcard", id: "dash-mem-card" });
  const memFrac = sample.used_memory_bytes / Math.max(sample.total_memory_bytes, 1);
  return dcard("cpu", "Memory", `${Math.round(memFrac * 100)}% used`,
    `${fmtBytes(sample.used_memory_bytes)} of ${fmtBytes(sample.total_memory_bytes)}`
    + (sample.used_swap_bytes > 0 ? ` · swap ${fmtBytes(sample.used_swap_bytes)}` : ""),
    "dash-mem-card");
}
function cpuCard(sample) {
  if (!sample) return h("div", { class: "dcard", id: "dash-cpu-card" });
  return dcard("gauge", "CPU", `${Math.round(sample.cpu_percent)}%`, "across all cores", "dash-cpu-card");
}
function buildDashboard({ disk, sample, battery, thermal, trend }) {
  const grid = h("div", { class: "dash" });
  // Mac Health hero: an honest verdict from free-space + memory headroom.
  const freeFrac = disk && disk.total_bytes ? disk.available_bytes / disk.total_bytes : 1;
  const memFrac = sample && sample.total_memory_bytes ? sample.used_memory_bytes / sample.total_memory_bytes : 0;
  let verdict = "Good", vcolor = "#22c55e";
  if (freeFrac < 0.1 || memFrac > 0.92) { verdict = "Needs attention"; vcolor = "#f59e0b"; }
  if (freeFrac < 0.05) { verdict = "Low on space"; vcolor = "#ef4444"; }
  grid.append(h("div", { class: "dcard health-hero wide" },
    h("div", { class: "ring-wrap" }, ringSvg(freeFrac, verdict.split(" ")[0], vcolor, 120)),
    h("div", {}, h("h3", {}, "Mac Health"), h("div", { class: "verdict", style: `color:${vcolor}` }, verdict),
      h("div", { class: "small", style: "color:#9fc7bd;margin-top:6px" },
        disk ? `${fmtBytes(disk.available_bytes)} free of ${fmtBytes(disk.total_bytes)}` : "Storage unavailable"))));

  if (disk) {
    grid.append(dcard("hard-drive", "Storage", `${fmtBytes(disk.available_bytes)} free`,
      `of ${fmtBytes(disk.total_bytes)} · ${Math.round((1 - freeFrac) * 100)}% used`));
  }
  // Always render these (even if the first sample failed) so the live updater
  // can replace them by id; memCard/cpuCard return an id'd placeholder on null.
  grid.append(memCard(sample));
  grid.append(cpuCard(sample));
  if (battery && battery.has_battery) {
    grid.append(dcard("battery", "Battery", battery.charge_percent != null ? `${battery.charge_percent}%` : "—",
      [battery.state, battery.cycle_count != null ? `${battery.cycle_count} cycles` : null,
       battery.health_percent != null ? `${battery.health_percent}% health` : null].filter(Boolean).join(" · ")));
  }
  const sm = sessions.smart;
  grid.append(sm && sm.items && sm.items.length
    ? dcard("sparkles", "Last Smart Scan", fmtBytes(foundBytes(sm)) + " found", `${sm.items.length} items — open Smart Scan to review`)
    : h("div", { class: "dcard" }, h("div", { class: "dh" }, icon("sparkles"), "Smart Scan"),
        h("div", { class: "small", style: "margin-bottom:10px" }, "No scan yet this session."),
        h("button", { class: "primary", onClick: () => navigate("smart") }, "Run Smart Scan")));
  if (thermal) {
    const tcolor = thermal.pressure === "Nominal" ? "#22c55e" : thermal.pressure === "Fair" ? "#f59e0b" : "#ef4444";
    grid.append(h("div", { class: "dcard" }, h("div", { class: "dh" }, icon("gauge"), "Thermal"),
      h("div", { class: "big", style: `color:${tcolor}` }, thermal.pressure),
      h("div", { class: "small" }, thermal.speed_limit != null ? `CPU speed limit ${thermal.speed_limit}%` : "No throttling")));
  }
  grid.append(graphCard("CPU", "cpu", "#f97316", "%", "dash-cpu-graph"));
  grid.append(graphCard("Memory", "mem", "#3b82f6", "%", "dash-mem-graph"));
  if (trend && trend.length > 1) {
    const series = trend.map((p) => p[1] / 1e9); // GB
    grid.append(h("div", { class: "dcard wide" }, h("div", { class: "dh" }, icon("hard-drive"), "Free space trend"),
      lineChart(series, "#14b8a6", (v) => `${v.toFixed(0)} GB`, 720, 90)));
  }
  return grid;
}
function dcard(ic, title, big, small, id) {
  const props = id ? { class: "dcard", id } : { class: "dcard" };
  return h("div", props, h("div", { class: "dh" }, icon(ic), title),
    h("div", { class: "big" }, big), h("div", { class: "small" }, small || ""));
}
function graphCard(title, key, color, unit, id) {
  const series = dashHist[key];
  const last = series.length ? series[series.length - 1] : 0;
  return h("div", { class: "dcard", id }, h("div", { class: "dh" }, icon("activity"), title),
    h("div", { class: "big", style: `color:${color}` }, `${Math.round(last)}${unit}`),
    series.length > 1 ? lineChart(series, color, null, 280, 64) : h("div", { class: "small" }, "collecting…"));
}
// Monotonic id source so each chart's gradient <def> is unique — two charts
// on screen (CPU + Memory) must not collide, or both fills resolve to the
// first matching gradient and render the wrong color.
let gradSeq = 0;
// Colorful SVG line chart with a soft gradient fill. Pure DOM, no deps.
function lineChart(series, color, fmt, w, hgt) {
  const max = Math.max(...series, 1), min = Math.min(...series, 0);
  const range = max - min || 1;
  const n = series.length;
  const pts = series.map((v, i) => [i / (n - 1) * w, hgt - ((v - min) / range) * (hgt - 8) - 4]);
  const line = pts.map((p, i) => `${i ? "L" : "M"}${p[0].toFixed(1)} ${p[1].toFixed(1)}`).join(" ");
  const area = `${line} L${w} ${hgt} L0 ${hgt} Z`;
  const gid = "grad" + (gradSeq++);
  const label = fmt ? `<text x="2" y="12" font-size="10" fill="var(--text-dim)">${fmt(max)}</text>` : "";
  return h("div", { style: "margin-top:8px", html:
    `<svg width="100%" viewBox="0 0 ${w} ${hgt}" preserveAspectRatio="none" style="display:block">
       <defs><linearGradient id="${gid}" x1="0" y1="0" x2="0" y2="1">
         <stop offset="0" stop-color="${color}" stop-opacity="0.35"/>
         <stop offset="1" stop-color="${color}" stop-opacity="0"/>
       </linearGradient></defs>
       <path d="${area}" fill="url(#${gid})"/>
       <path d="${line}" fill="none" stroke="${color}" stroke-width="2" stroke-linejoin="round" stroke-linecap="round"/>
       ${label}
     </svg>` });
}
function ringSvg(value, label, color, size) {
  const r = size * 0.4, c = 2 * Math.PI * r, off = c * (1 - Math.min(Math.max(value, 0), 1)), cx = size / 2;
  return h("div", { html: `<svg width="${size}" height="${size}" viewBox="0 0 ${size} ${size}"><circle cx="${cx}" cy="${cx}" r="${r}" fill="none" stroke="rgba(255,255,255,0.15)" stroke-width="9"/><circle cx="${cx}" cy="${cx}" r="${r}" fill="none" stroke="${color}" stroke-width="9" stroke-linecap="round" stroke-dasharray="${c}" stroke-dashoffset="${off}" transform="rotate(-90 ${cx} ${cx})"/><text x="${cx}" y="${cx + 6}" text-anchor="middle" font-size="20" font-weight="700" fill="#fff">${label}</text></svg>` });
}

// =====================================================================
// Settings — telemetry opt-in (default OFF), with an honest explanation.
// =====================================================================
async function settingsView() {
  setTitle("Settings", []);
  const enabled = await invoke("telemetry_enabled").catch(() => false);
  const cb = h("input", { type: "checkbox" });
  cb.checked = enabled;
  cb.addEventListener("change", () => {
    invoke("set_telemetry_enabled", { on: cb.checked }).catch((e) => { alert(String(e)); cb.checked = !cb.checked; });
  });
  const sw = h("label", { class: "switch" }, cb, h("span", { class: "track" }));
  setView(h("div", { class: "pad" },
    h("div", { class: "setting" },
      h("div", { class: "body" },
        h("h3", {}, "Share deselection feedback"),
        h("p", {}, "Off by default. When on, Tabibu records only which CATEGORY of suggestion you unchecked (e.g. \"developer caches\"), the safety tier, and a rough size range — never file names, paths, or contents. It helps us learn which suggestions to trust less. Stored locally; turning this off deletes what was collected.")),
      sw)));
}

// =====================================================================
// router
// =====================================================================
function render() {
  const id = state.current;
  if (id === "dashboard") return dashboardView();
  if (["smart", "junk", "large"].includes(id)) return scanView(id);
  if (id === "dupes") return duplicatesView();
  if (id === "disk") return diskView();
  if (id === "memory") return memoryView();
  if (id === "battery") return batteryView();
  if (id === "uninstall") return uninstallerView();
  if (id === "brew") return brewView();
  if (id === "startup") return startupView();
  if (id === "security") return securityView();
  if (id === "settings") return settingsView();
}

// ---------- boot ----------
async function boot() {
  buildShell();
  try { const info = await invoke("system_info"); state.home = info.home; state.fda = info.full_disk_access; }
  catch (e) { console.error("system_info failed", e); }
  updateFooter();
  navigate("dashboard");
}
boot();

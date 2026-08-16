// Regression test for the menu-bar popover layout (menubar.html).
//
// The bug this guards: the popover window has a FIXED height (tauri.conf.json →
// windows[label=menubar].height) and its overview panel lives inside
// `.floating { overflow:hidden }`. When the panel's content grows taller than
// the window (e.g. a card was added), the bottom — the footer with Open/Quit —
// is silently CLIPPED. This test renders the real menubar.html in headless
// Chrome at the exact configured window size and fails if the content doesn't
// fit or the footer/Salama switch is clipped.
//
// Zero npm deps: drives Chrome directly over the DevTools Protocol (CDP) and
// serves app/src over a tiny static server (ES-module imports need http, not
// file://). Chrome path is the standard macOS location; skips clean if absent.
//
//   node app/tests/menubar-layout.mjs            # assert (CI)
//   node app/tests/menubar-layout.mjs --measure  # print the numbers
import { createServer } from "node:http";
import { readFile } from "node:fs/promises";
import { existsSync } from "node:fs";
import { spawn } from "node:child_process";
import { fileURLToPath } from "node:url";
import { dirname, join, extname, normalize } from "node:path";

const HERE = dirname(fileURLToPath(import.meta.url));
const SRC = normalize(join(HERE, "..", "src"));
const CONF = normalize(join(HERE, "..", "src-tauri", "tauri.conf.json"));
const CHROME =
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const MEASURE = process.argv.includes("--measure");

const MIME = {
  ".html": "text/html", ".js": "text/javascript", ".mjs": "text/javascript",
  ".css": "text/css", ".woff2": "font/woff2", ".svg": "image/svg+xml",
  ".png": "image/png", ".json": "application/json",
};

// Mock the Tauri bridge so menubar.html's invoke() calls resolve with
// plausibly-shaped data (values influence width, never the fixed vertical
// structure this test measures). Injected before any page script runs.
const TAURI_MOCK = `
window.__lastResize = null; // last popover_resize height the app requested
window.__TAURI__ = { core: { invoke: (cmd, args) => {
  if (cmd === "popover_resize") window.__lastResize = args && args.height;
  const M = {
    menubar_sample: { cpu_percent: 42, used_memory_bytes: 12e9, total_memory_bytes: 16e9, used_swap_bytes: 1e9, top_processes: [] },
    disk_space: { available_bytes: 120e9, total_bytes: 500e9 },
    battery_info: { has_battery: true, charge_percent: 80, state: "Charging", health_percent: 92, cycle_count: 120, condition: "Normal" },
    thermal_info: { pressure: "Nominal", speed_limit: null },
    network_sample: { down_bps: 2e6, up_bps: 5e5, total_down_bytes: 30e9, total_up_bytes: 5e9 },
    salama_engine_status: { installed: false },
    uptime_secs: 90000,
  };
  return Promise.resolve(cmd in M ? M[cmd] : null);
} } };
`;

async function readConfiguredHeight() {
  const conf = JSON.parse(await readFile(CONF, "utf8"));
  const mb = conf.app.windows.find((w) => w.label === "menubar");
  if (!mb) throw new Error("no menubar window in tauri.conf.json");
  return { width: mb.width, height: mb.height };
}

function startServer() {
  return new Promise((resolve) => {
    const srv = createServer(async (req, res) => {
      try {
        const p = normalize(join(SRC, decodeURIComponent(req.url.split("?")[0])));
        if (!p.startsWith(SRC)) { res.writeHead(403).end(); return; }
        const body = await readFile(p);
        res.writeHead(200, { "content-type": MIME[extname(p)] || "application/octet-stream" });
        res.end(body);
      } catch { res.writeHead(404).end("not found"); }
    });
    srv.listen(0, "127.0.0.1", () => resolve(srv));
  });
}

// Minimal CDP client over the browser-level WebSocket.
async function cdp(wsUrl) {
  const ws = new WebSocket(wsUrl);
  await new Promise((res, rej) => { ws.onopen = res; ws.onerror = rej; });
  let id = 0;
  const pending = new Map();
  const listeners = [];
  ws.onmessage = (m) => {
    const msg = JSON.parse(m.data);
    if (msg.id && pending.has(msg.id)) {
      const { resolve, reject } = pending.get(msg.id);
      pending.delete(msg.id);
      msg.error ? reject(new Error(msg.error.message)) : resolve(msg.result);
    } else if (msg.method) listeners.forEach((fn) => fn(msg));
  };
  const send = (method, params = {}, sessionId) =>
    new Promise((resolve, reject) => {
      const mid = ++id;
      pending.set(mid, { resolve, reject });
      ws.send(JSON.stringify({ id: mid, method, params, sessionId }));
    });
  const on = (fn) => listeners.push(fn);
  return { send, on, close: () => ws.close() };
}

async function main() {
  if (!existsSync(CHROME)) {
    console.log(`SKIP menubar-layout: Google Chrome not found at ${CHROME}`);
    return; // graceful skip on machines without Chrome (exit 0)
  }
  const { height: confH, width: confW } = await readConfiguredHeight();

  const chrome = spawn(CHROME, [
    "--headless=new", "--remote-debugging-port=9333",
    "--no-first-run", "--no-default-browser-check", "--disable-gpu",
    `--user-data-dir=${join(HERE, ".chrome-profile")}`,
  ], { stdio: "ignore" });

  const srv = await startServer();
  const port = srv.address().port;
  const url = `http://127.0.0.1:${port}/menubar.html`;

  const cleanup = () => { try { chrome.kill(); } catch {} try { srv.close(); } catch {} };

  try {
    // Wait for Chrome's debugging endpoint.
    let wsUrl;
    for (let i = 0; i < 50; i++) {
      try {
        const v = await fetch("http://127.0.0.1:9333/json/version").then((r) => r.json());
        wsUrl = v.webSocketDebuggerUrl; break;
      } catch { await new Promise((r) => setTimeout(r, 100)); }
    }
    if (!wsUrl) throw new Error("Chrome DevTools endpoint never came up");

    const client = await cdp(wsUrl);
    const { targetId } = await client.send("Target.createTarget", { url: "about:blank" });
    const { sessionId } = await client.send("Target.attachToTarget", { targetId, flatten: true });
    const S = (method, params) => client.send(method, params, sessionId);

    await S("Page.enable");
    await S("Runtime.enable");
    // Render at the EXACT configured popover size (2x, like a Retina Mac).
    // HEIGHT env forces a shorter window to exercise the overflow safety net.
    const renderH = process.env.HEIGHT ? Number(process.env.HEIGHT) : confH;
    await S("Emulation.setDeviceMetricsOverride", {
      width: confW, height: renderH, deviceScaleFactor: 2, mobile: false,
    });
    await S("Page.addScriptToEvaluateOnNewDocument", { source: TAURI_MOCK });

    const loaded = new Promise((res) => {
      client.on((m) => { if (m.method === "Page.loadEventFired") res(); });
    });
    await S("Page.navigate", { url });
    await loaded;

    const { result } = await S("Runtime.evaluate", {
      awaitPromise: true, returnByValue: true,
      expression: `(async () => {
        const raf = () => new Promise((r) => requestAnimationFrame(() => requestAnimationFrame(r)));
        await document.fonts.ready;
        await raf();
        const wrap = document.querySelector(".wrap");
        const panel = document.querySelector(".panel");
        const cards = document.querySelector(".cards");
        const vp = window.innerHeight;
        // Un-stretched natural content height of the overview (mirrors the app's
        // overviewContentHeight()); + 14 is the height fitHeight() would request.
        const measureNatural = () => {
          const pa = wrap.style.alignItems, ph = wrap.style.height;
          wrap.style.alignItems = "flex-start"; wrap.style.height = "auto";
          const h = Math.ceil(panel.getBoundingClientRect().height);
          wrap.style.alignItems = pa; wrap.style.height = ph;
          return h;
        };
        // Is a control clipped? A flex item can keep its bottom edge in-viewport
        // while its CONTENT is clipped (it shrank), so check each control's rect.
        const rectOf = (sel) => { const e = document.querySelector(sel); return e ? e.getBoundingClientRect() : null; };
        const clipped = (sel) => { const r = rectOf(sel); return !!r && (r.bottom > vp + 0.5 || r.top < -0.5); };
        const controls = ["#open", "#settings", "#quit", "#smart", "#sal-switch"];

        // Phase 1 — baseline.
        const natural = measureNatural();
        const requiredH = natural + 12;   // + wrap margins
        const fitRequest = natural + 14;  // what fitHeight() requests
        const clippedControls = controls.filter(clipped);

        // Phase 2 — "a component was added": append a card and let the app's
        // MutationObserver refit. Assert it requested a height covering the new
        // content (proves the popover ADJUSTS to future components).
        window.__lastResize = null;
        const c = document.createElement("div");
        c.className = "card sal";
        c.innerHTML = '<div class="salrow"><div style="min-width:0"><div class="t">Extra</div><div class="s">added component</div></div><button class="switch"><span class="knob"></span></button></div>';
        cards.appendChild(c);
        await raf();
        const naturalAfterAdd = measureNatural();
        const resizeAfterAdd = window.__lastResize; // set by the observer→fitHeight→invoke
        cards.removeChild(c);

        return { vp, natural, requiredH, fitRequest,
                 footerHeight: Math.round(rectOf("footer").height),
                 cardsScrolls: cards.scrollHeight > cards.clientHeight + 0.5,
                 clippedControls, anyClipped: clippedControls.length > 0,
                 naturalAfterAdd, requiredAfterAdd: naturalAfterAdd + 12, resizeAfterAdd };
      })()`,
    });
    const r = result.value;

    if (MEASURE) {
      console.log("configured window:", confW, "x", confH);
      console.log(JSON.stringify(r, null, 2));
      cleanup(); return;
    }

    const fails = [];
    // The real guarantee: the app's runtime fitHeight() request must cover the
    // actual content, so the self-sizing window never clips (in any webview).
    if (r.fitRequest < r.requiredH)
      fails.push(`runtime fit would request ${r.fitRequest}px but content needs ${r.requiredH}px — fitHeight() formula is short`);
    // Nothing clipped at the current window size (footer/controls fully
    // on-screen, not just their container's edge).
    if (r.anyClipped)
      fails.push(`controls clipped at ${r.vp}px window: ${r.clippedControls.join(", ")} (footer height ${r.footerHeight}px)`);
    // Responsiveness: adding a component must trigger an auto-refit (observer)
    // that covers the taller content — so the popover adjusts to new components.
    if (r.resizeAfterAdd == null)
      fails.push(`adding a component did NOT trigger an auto-refit (MutationObserver/fitHeight not wired)`);
    else if (r.resizeAfterAdd < r.requiredAfterAdd)
      fails.push(`after adding a component the app requested ${r.resizeAfterAdd}px but the new content needs ${r.requiredAfterAdd}px`);

    cleanup();
    if (fails.length) {
      console.error("FAIL menubar-layout:\n  - " + fails.join("\n  - "));
      process.exit(1);
    }
    console.log(
      `PASS menubar-layout: fit=${r.fitRequest}px covers ${r.requiredH}px, all controls visible at ${r.vp}px; ` +
      `adding a component auto-refit to ${r.resizeAfterAdd}px (covers ${r.requiredAfterAdd}px).`);
  } catch (e) {
    cleanup();
    console.error("menubar-layout test error:", e.message);
    process.exit(2);
  }
}

main();

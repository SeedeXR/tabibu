// Renders every docs/**/*.md into a styled HTML page that reuses the landing
// page's design (assets/docs.css + Space Grotesk + vendored mermaid). Markdown
// is the source of truth; this runs in CI on publish. Usage: `node build.mjs`.
import { readFileSync, writeFileSync, readdirSync } from "node:fs";
import { join, dirname, relative, sep } from "node:path";
import { fileURLToPath } from "node:url";
import { marked } from "marked";

const DOCS = dirname(fileURLToPath(import.meta.url));
const esc = (s) => s.replace(/&/g, "&amp;").replace(/</g, "&lt;").replace(/>/g, "&gt;");

// ```mermaid fences become <pre class="mermaid"> (raw, for client render);
// every other code block is escaped inside our themed code container.
marked.use({
  renderer: {
    code({ text, lang }) {
      return lang === "mermaid"
        ? `<pre class="mermaid">${text}</pre>`
        : `<div class="code"><pre><code>${esc(text)}</code></pre></div>`;
    },
  },
});

function mdFiles(dir) {
  return readdirSync(dir, { withFileTypes: true }).flatMap((e) => {
    if (e.name === "assets" || e.name === "node_modules") return [];
    const p = join(dir, e.name);
    if (e.isDirectory()) return mdFiles(p);
    return e.name.endsWith(".md") ? [p] : [];
  });
}

function shell(title, body, up, file) {
  return `<!doctype html>
<html lang="en"><head>
<meta charset="utf-8"><meta name="viewport" content="width=device-width, initial-scale=1">
<title>${esc(title)} · Tabibu docs</title>
<link rel="stylesheet" href="${up}assets/docs.css">
</head><body>
<nav>
  <a class="brand" href="${up}index.html"><svg viewBox="0 0 551 888" id="navmark" aria-hidden="true"></svg> Tabibu</a>
  <span class="links"><a href="${up}index.html">← Home</a></span>
</nav>
<main class="reveal"><div class="sheet"><div class="file">${esc(file)}</div><article class="doc">${body}</article></div></main>
<footer><div class="mono">Tabibu · docs</div></footer>
<script src="${up}assets/mermaid.min.js"></script>
<script src="${up}assets/mermaid-init.js"></script>
<script src="${up}assets/gsap.min.js"></script>
<script src="${up}assets/ScrollTrigger.min.js"></script>
<script src="${up}assets/navmark.js"></script>
<script>
if(window.gsap&&window.ScrollTrigger&&!matchMedia("(prefers-reduced-motion: reduce)").matches){gsap.registerPlugin(ScrollTrigger);gsap.from(".reveal",{opacity:0,y:32,duration:0.6,ease:"power3.out"});}</script>
</body></html>`;
}

let n = 0;
for (const file of mdFiles(DOCS)) {
  const md = readFileSync(file, "utf8");
  const title = (md.match(/^#\s+(.+)$/m)?.[1] ?? file.split(sep).pop().replace(/\.md$/, "")).trim();
  const depth = relative(DOCS, dirname(file)).split(sep).filter(Boolean).length;
  const up = "../".repeat(depth);
  const label = relative(DOCS, file); // e.g. "modules/engine.md"
  writeFileSync(file.replace(/\.md$/, ".html"), shell(title, marked.parse(md), up, label));
  n++;
}
console.log(`rendered ${n} doc page(s)`);

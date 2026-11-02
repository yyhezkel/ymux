// Dev Mode snapshot check — runs the REAL injected inspectScript() in
// headless Chrome against fixed fixtures and asserts on the capture.
// Usage: node app/scripts/devmode-snapshot-check.mjs [fixture...]
// Env: YMUX_CHROME overrides the Chrome binary. Exit: 0 pass, 1 fail, 2 no Chrome.
import fs from "node:fs";
import os from "node:os";
import path from "node:path";
import { fileURLToPath } from "node:url";
import { spawnSync } from "node:child_process";

const here = path.dirname(fileURLToPath(import.meta.url));
const src = fs.readFileSync(path.join(here, "../src/browserDevMode.ts"), "utf8");

// constants interpolated into the template
const names = [];
const values = [];
for (const m of src.matchAll(/^const (SHOT_\w+|HTML_MAX) = ([\d* ]+);/gm)) {
  names.push(m[1]);
  values.push(Function(`return ${m[2]};`)());
}
const start = src.indexOf("export function inspectScript");
const open = src.indexOf("return `", start) + "return `".length;
const close = src.indexOf("`;\n}", open);
if (start < 0 || open < start || close < 0) {
  console.error("FAIL  harness cannot locate inspectScript() template");
  process.exit(1);
}
const script = new Function(...names, "return `" + src.slice(open, close) + "`;")(...values);

const FIXTURES = {
  "bg-box": '<div id="t" style="background:#c33;border:3px solid blue;color:#fff;padding:10px;width:200px">box text</div>',
  "pre-transparent": '<pre id="t">transparent pre text</pre>',
  "blank-empty": '<div id="t" style="width:120px;height:40px"></div>',
  "zero-size": '<div id="t" style="width:0"></div>',
};
const want = process.argv.slice(2);
const unknown = want.filter((n) => !(n in FIXTURES));
if (unknown.length) {
  console.error(`FAIL  unknown fixture: ${unknown.join(", ")}`);
  process.exit(1);
}
const selected = want.length ? want : Object.keys(FIXTURES);

const driver = `
var FIX = ${JSON.stringify(selected)};
var out = {};
function stats(url, cb){
  var img = new Image();
  img.onload = function(){
    var c = document.createElement('canvas'); c.width = img.width; c.height = img.height;
    var x = c.getContext('2d'); x.drawImage(img, 0, 0);
    var d = x.getImageData(0, 0, c.width, c.height).data, opaque = 0;
    for (var i = 3; i < d.length; i += 4) if (d[i] !== 0) opaque++;
    function at(px, py){ var o = (py * c.width + px) * 4; return [d[o], d[o+1], d[o+2], d[o+3]]; }
    cb({ w: c.width, h: c.height, opaque: opaque, p00: at(0, 0), inner: at(8, 8) });
  };
  img.onerror = function(){ cb(null); };
  img.src = url;
}
function next(i){
  if (i >= FIX.length) { document.getElementById('res').textContent = JSON.stringify(out); return; }
  var el = document.getElementById('fx-' + FIX[i]);
  window.__ymuxTicket.capture(el, function(p){
    var rec = { shot: !!p.shot, shot_error: p.shot_error, bg: p.style.background, html: p.html.slice(0, 8), px: null };
    if (!p.shot) { out[FIX[i]] = rec; return next(i + 1); }
    stats(p.shot, function(s){ rec.px = s; out[FIX[i]] = rec; next(i + 1); });
  });
}
next(0);
`;
const body = selected
  .map((n) => FIXTURES[n].replace('id="t"', `id="fx-${n}"`))
  .join("\n");
const html = `<!doctype html><html><head><meta charset="utf-8"><style>body{background:#0d1117;color:#e6edf3}</style></head><body>
${body}
<pre id="res"></pre>
<script>${script}</script>
<script>${driver}</script>
</body></html>`;

const CHECKS = {
  "bg-box": (r) => {
    const q = r.px && r.px.inner;
    const ok = r.shot && r.shot_error === null && q && q[0] > 150 && q[1] < 100 && q[2] < 100 && q[3] === 255;
    return [ok, `shot=${r.shot} err=${r.shot_error} inner=${q}`];
  },
  "pre-transparent": (r) => {
    const ok = r.shot && r.px && r.px.p00[3] === 0 && r.px.opaque >= 1 &&
      r.bg === "rgba(0, 0, 0, 0)" && r.html.startsWith("<pre");
    return [ok, `shot=${r.shot} p00a=${r.px && r.px.p00[3]} opaque=${r.px && r.px.opaque} bg=${r.bg} html=${r.html}`];
  },
  "blank-empty": (r) => [!r.shot && r.shot_error === "blank", `shot=${r.shot} err=${r.shot_error}`],
  "zero-size": (r) => [!r.shot && r.shot_error === "zero-size", `shot=${r.shot} err=${r.shot_error}`],
};

const chrome = process.env.YMUX_CHROME || "google-chrome";
const dir = fs.mkdtempSync(path.join(os.tmpdir(), "ymux-devmode-"));
let code = 0;
try {
  const file = path.join(dir, "check.html");
  fs.writeFileSync(file, html);
  const run = spawnSync(
    chrome,
    ["--headless=new", "--no-sandbox", "--disable-gpu", "--virtual-time-budget=5000",
      `--user-data-dir=${path.join(dir, "profile")}`, "--dump-dom", `file://${file}`],
    { encoding: "utf8", timeout: 25000, maxBuffer: 64 * 1024 * 1024 },
  );
  if (run.error && run.error.code === "ENOENT") {
    console.error("chrome not found");
    code = 2;
  } else {
    const m = /<pre id="res">([\s\S]*?)<\/pre>/.exec(run.stdout || "");
    let res = null;
    try { res = m ? JSON.parse(m[1].replace(/&quot;/g, '"').replace(/&amp;/g, "&").replace(/&lt;/g, "<").replace(/&gt;/g, ">")) : null; } catch { /* reported below */ }
    for (const n of selected) {
      const r = res && res[n];
      const [ok, detail] = r ? CHECKS[n](r) : [false, "no result from page"];
      console.log(`${ok ? "PASS" : "FAIL"}  ${n} ${detail}`);
      if (!ok) code = 1;
    }
  }
} finally {
  fs.rmSync(dir, { recursive: true, force: true });
}
process.exit(code);

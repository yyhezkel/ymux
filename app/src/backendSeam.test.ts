// Guard for the Phase 106 backend seam. Run:
//   cd app && node --experimental-strip-types --test src/backendSeam.test.ts
// (Excluded from the app tsconfig -- node tests, not browser code.)
//
// Every host call goes through `src/backend/` so a browser build can answer
// it with the daemon instead of Tauri IPC (WEB-DESIGN §5). One direct
// `invoke` / `listen` import elsewhere is a call the WebBackend never sees.
import { test } from "node:test";
import assert from "node:assert/strict";
import { readdirSync, readFileSync, statSync } from "node:fs";
import { join, relative } from "node:path";
import { fileURLToPath } from "node:url";

const SRC = fileURLToPath(new URL(".", import.meta.url));
const FORBIDDEN = /from\s+["']@tauri-apps\/api\/(core|event)["']/;

function sources(dir: string, out: string[] = []): string[] {
  for (const name of readdirSync(dir)) {
    const p = join(dir, name);
    if (statSync(p).isDirectory()) {
      if (name !== "backend") sources(p, out);
    } else if (/\.tsx?$/.test(name) && !name.endsWith(".test.ts")) {
      out.push(p);
    }
  }
  return out;
}

test("only src/backend imports @tauri-apps/api/core or /event", () => {
  const offenders = sources(SRC)
    .filter((f) => FORBIDDEN.test(readFileSync(f, "utf8")))
    .map((f) => relative(SRC, f));
  assert.deepEqual(offenders, [], "call backend.call / backend.on instead");
});

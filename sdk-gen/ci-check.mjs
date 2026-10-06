// ci-check.mjs — the SDK drift-guard. Regenerates every spec + SDK from the
// current server source and fails if anything changed vs. what's committed. Run
// in CI (and locally before a PR): a red check here means someone changed a
// handler/frame without regenerating `npm run gen`.
import { execFileSync } from "node:child_process";
import { dirname, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..");

function run(cmd, args, opts = {}) {
  return execFileSync(cmd, args, { cwd: repo, encoding: "utf8", stdio: ["ignore", "pipe", "inherit"], ...opts });
}

console.log("↻ regenerating specs + SDKs …");
execFileSync(process.execPath, [resolve(here, "emit-specs.mjs")], { stdio: "inherit" });
execFileSync(process.execPath, [resolve(here, "gen-typescript.mjs")], { stdio: "inherit" });
execFileSync(process.execPath, [resolve(here, "gen-kotlin.mjs")], { stdio: "inherit" });

// Only guard the generated artifacts (specs + *.gen.ts + generated Kotlin).
const paths = [
  "sdk-gen/specs",
  "sdk/typescript/src/types.gen.ts",
  "sdk/typescript/src/frames.gen.ts",
  "sdk/kotlin/src/main/kotlin/dev/ymux/sdk/Frames.kt",
  "sdk/kotlin/src/main/kotlin/dev/ymux/sdk/Models.kt",
];
const diff = run("git", ["status", "--porcelain", "--", ...paths]).trim();
if (diff) {
  console.error("\n✗ SDK drift detected — generated output differs from committed. Run `npm run gen` and commit:\n");
  console.error(run("git", ["diff", "--stat", "--", ...paths]));
  // The diff itself, capped: without it the only way to see WHAT drifted is a
  // local Go toolchain, which Rule #17 keeps off dev boxes.
  console.error(run("git", ["diff", "--", ...paths]).split("\n").slice(0, 200).join("\n"));
  process.exit(1);
}
console.log("\n✓ SDKs are in sync with the server contract.");

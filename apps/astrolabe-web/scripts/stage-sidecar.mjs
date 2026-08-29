// Stage the `lait` sidecar so the pair ships — and runs — together.
//
// Astrolabe is not the daemon binary: it resolves a fixed `lait` beside its own
// executable, never from PATH, configuration, or the environment (see
// `tools/astrolabe/src/sidecar.rs` for why that is a rule rather than a
// convention). Two places need it, and they are different places:
//
//   dev     `src-tauri/binaries/lait-<triple>`, the same place bundling uses.
//           NOT `src-tauri/target/debug/` — that was the obvious answer and it
//           was wrong for a reason nothing said out loud: `externalBin` makes
//           Tauri copy `binaries/lait-<triple>` into the target directory
//           itself, mtime and all, *after* `beforeDevCommand` has run. So
//           anything staged there was overwritten on every launch, and the
//           binary the client actually spawned was whatever was last bundled.
//           It cost three launch cycles to see, because the copy is silent and
//           this script had already printed its success line.
//
//   bundle  `src-tauri/binaries/lait-<target-triple>`, which is what
//           `bundle.externalBin` expects; the bundler installs it beside the
//           application binary inside the app. That placement is the release
//           half of the pair rule (CLIENT-12) — one tree builds both, and the
//           installed client finds its sidecar exactly where it looks.
//
// Run automatically from `beforeDevCommand` (dev) and `beforeBuildCommand`
// (`--bundle`). Copies rather than symlinks, so the same script serves Windows
// without privileges.

import { execFileSync } from "node:child_process";
import { copyFileSync, cpSync, mkdirSync, renameSync, rmSync } from "node:fs";
import { dirname, join, resolve } from "node:path";
import { fileURLToPath } from "node:url";

const bundle = process.argv.includes("--bundle");
const here = dirname(fileURLToPath(import.meta.url));
const repo = resolve(here, "..", "..", "..");
const exe = process.platform === "win32" ? "lait.exe" : "lait";
const profile = bundle ? "release" : "debug";
const source = join(repo, "target", profile, exe);

// The bundler keys the sidecar by target triple, so ask the toolchain rather
// than mapping platform names here — a wrong guess produces a bundle that is
// missing its sidecar and says nothing until the client cannot start a daemon.
function hostTriple() {
  const out = execFileSync("rustc", ["-vV"], { encoding: "utf8" });
  const line = out.split("\n").find((l) => l.startsWith("host:"));
  if (line === undefined) throw new Error("rustc -vV did not report a host triple");
  return line.slice("host:".length).trim();
}

// One destination for both, because Tauri resolves the sidecar from exactly
// one place and staging anywhere else only looks like it worked.
const triple = hostTriple();
const targetDir = resolve(here, "..", "src-tauri", "binaries");
const targetName = process.platform === "win32" ? `lait-${triple}.exe` : `lait-${triple}`;
const target = join(targetDir, targetName);

const buildArgs = [
  "build",
  "-p", "lait",
  "--locked",
];
if (bundle) buildArgs.push("--release");
console.log(`staging the lait sidecar for ${bundle ? "bundling" : "development"} (cargo ${buildArgs.join(" ")})…`);
execFileSync("cargo", buildArgs, { cwd: repo, stdio: "inherit" });

mkdirSync(targetDir, { recursive: true });
try {
  // Remove first: copying over a symlink writes through it, and copying over
  // a running executable fails on Windows.
  rmSync(target, { force: true });
  copyFileSync(source, target);
} catch {
  // A running daemon holds the target open (Windows). Renaming a running
  // executable is allowed — the engine's own self-update relies on it — so
  // vacate the name and copy into it.
  const aside = `${target}.stale`;
  rmSync(aside, { force: true });
  renameSync(target, aside);
  copyFileSync(source, target);
  try {
    rmSync(aside, { force: true });
  } catch {
    // Still running; the next staging pass collects it.
  }
}
console.log(`staged ${targetName} at ${target}`);

// The native package carries the reviewed first-party catalog, not World code.
// It is a host release input in its own right: staging must never inspect a
// product manifest, artwork tree, runner, or build output. Catalog membership
// is why a row exists; choosing Install resolves the World's independently
// signed channel and downloads its native payload.
const catalogSource = resolve(here, "..", "catalog");
const worldCatalog = resolve(here, "..", "src-tauri", "world-catalog");
rmSync(worldCatalog, { recursive: true, force: true });
cpSync(catalogSource, worldCatalog, { recursive: true, errorOnExist: true });
console.log(`staged first-party World catalog at ${worldCatalog}`);

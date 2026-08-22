// Stage the `lait` sidecar so the pair ships — and runs — together.
//
// Astrolabe is not the daemon binary: it resolves a fixed `lait` beside its own
// executable, never from PATH, configuration, or the environment (see
// `tools/astrolabe/src/sidecar.rs` for why that is a rule rather than a
// convention). Two places need it, and they are different places:
//
//   dev     `src-tauri/target/debug/`, beside the host binary `tauri dev`
//           runs. Nothing else populates it, so a host launched without this
//           comes up with no daemon to spawn and waits on a supervisor that
//           cannot start.
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
import { copyFileSync, mkdirSync, renameSync, rmSync } from "node:fs";
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

const [targetDir, targetName] = bundle
  ? [resolve(here, "..", "src-tauri", "binaries"),
     process.platform === "win32" ? `lait-${hostTriple()}.exe` : `lait-${hostTriple()}`]
  : [resolve(here, "..", "src-tauri", "target", "debug"), exe];
const target = join(targetDir, targetName);

const buildArgs = ["build", "--bin", "lait", "--locked"];
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

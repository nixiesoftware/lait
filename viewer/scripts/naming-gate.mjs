// The TypeScript half of the semantic-naming gate (plan 13 §5.1).
//
// Project-owned identifiers carry no protocol- or storage-generation suffix.
// Encoded versions stay in *values* — a `formatVersion` field, a `"lait/1"`
// ALPN string — never in names. This walks a real TypeScript syntax tree for
// the same reason the Rust half uses `syn`: a regex over source lines cannot
// tell a declaration from a mention, and cannot tell `ipv4` from `frameV1`.
//
// Run: node scripts/naming-gate.mjs

import { readdirSync, readFileSync, statSync } from "node:fs";
import { dirname, join, relative, resolve } from "node:path";
import { fileURLToPath } from "node:url";
import ts from "typescript";

const viewerRoot = resolve(dirname(fileURLToPath(import.meta.url)), "..");
const repoRoot = resolve(viewerRoot, "..");

/** Directories whose contents are not ours to name. */
const SKIP = new Set(["node_modules", "dist", "coverage", ".vite"]);

/** Production TypeScript. Tests declare fixtures named after what they test. */
function sources(dir, out = []) {
  for (const entry of readdirSync(dir)) {
    if (SKIP.has(entry)) continue;
    const path = join(dir, entry);
    if (statSync(path).isDirectory()) {
      sources(path, out);
      continue;
    }
    if (!/\.(ts|tsx)$/.test(entry)) continue;
    if (/\.(test|spec)\.tsx?$/.test(entry) || /[\\/]__tests__[\\/]/.test(path)) continue;
    out.push(path);
  }
  return out;
}

/**
 * Whether an identifier carries a generation suffix. `Ipv4`/`ipv6Addr` are not
 * versions: the `V` must introduce a trailing number after a lowercase letter
 * or digit, or the name must end `_v<digits>`, or be a bare `v<digits>`.
 */
export function versionedIdent(name) {
  const lower = name.toLowerCase();
  const underscore = lower.lastIndexOf("_v");
  if (underscore >= 0 && /^\d+$/.test(lower.slice(underscore + 2))) return true;
  if (/^v\d+$/.test(lower)) return true;
  for (let i = 1; i < name.length; i += 1) {
    if (name[i] !== "V") continue;
    const tail = name.slice(i + 1);
    if (tail.length > 0 && /^\d+$/.test(tail) && /[a-z0-9]/.test(name[i - 1])) return true;
  }
  return false;
}

/** Every identifier a file declares — the violation unit. */
function declarations(text, fileName) {
  const file = ts.createSourceFile(fileName, text, ts.ScriptTarget.Latest, true);
  const found = new Set();
  const note = (node) => {
    if (node && ts.isIdentifier(node) && versionedIdent(node.text)) found.add(node.text);
  };
  const noteBinding = (name) => {
    if (!name) return;
    if (ts.isIdentifier(name)) return note(name);
    if (ts.isObjectBindingPattern(name) || ts.isArrayBindingPattern(name)) {
      for (const element of name.elements) {
        if (ts.isBindingElement(element)) noteBinding(element.name);
      }
    }
  };
  const walk = (node) => {
    if (
      ts.isVariableDeclaration(node) ||
      ts.isParameter(node) ||
      ts.isBindingElement(node)
    ) {
      noteBinding(node.name);
    } else if (
      ts.isFunctionDeclaration(node) ||
      ts.isClassDeclaration(node) ||
      ts.isInterfaceDeclaration(node) ||
      ts.isTypeAliasDeclaration(node) ||
      ts.isEnumDeclaration(node) ||
      ts.isModuleDeclaration(node) ||
      ts.isMethodDeclaration(node) ||
      ts.isMethodSignature(node) ||
      ts.isPropertyDeclaration(node) ||
      ts.isPropertySignature(node) ||
      ts.isEnumMember(node) ||
      ts.isTypeParameterDeclaration(node)
    ) {
      note(node.name);
    }
    ts.forEachChild(node, walk);
  };
  walk(file);
  return [...found].sort();
}

function main() {
  const violations = [];
  for (const path of sources(join(viewerRoot, "src"))) {
    const rel = relative(repoRoot, path).replaceAll("\\", "/");
    for (const name of declarations(readFileSync(path, "utf8"), path)) {
      violations.push(`${rel}: \`${name}\``);
    }
  }
  if (violations.length > 0) {
    console.error("version-suffixed identifier declarations in viewer sources:");
    for (const v of violations) console.error(`  ${v}`);
    process.exit(1);
  }

  // The detector must have teeth, or a green run means nothing.
  const positives = ["FrameV1", "decode_v2", "PAYLOAD_V3", "v1", "renderV10"];
  const negatives = ["Ipv4Header", "ipv6Addr", "formatVersion", "protocolVersion", "video"];
  for (const sample of positives) {
    if (!versionedIdent(sample)) {
      console.error(`detector missed: ${sample}`);
      process.exit(1);
    }
  }
  for (const sample of negatives) {
    if (versionedIdent(sample)) {
      console.error(`false positive: ${sample}`);
      process.exit(1);
    }
  }
  console.log(`naming gate: clean across ${sources(join(viewerRoot, "src")).length} viewer sources`);
}

main();

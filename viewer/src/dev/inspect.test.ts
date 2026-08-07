import { readdirSync, readFileSync, statSync } from "node:fs";
import { join } from "node:path";

import { describe, expect, it } from "vitest";

/**
 * The debug surface must stay unshippable.
 *
 * `dev/inspect.ts` puts measuring tools on `window.lait` — a read of any
 * element's geometry, a dump of the tree, a navigation dispatcher. None of that
 * belongs in a binary a user runs, and the thing keeping it out is one `if
 * (import.meta.env.DEV)` in `main.tsx`: Vite replaces that with a literal
 * `false`, the branch dies, and the dynamic `import()` inside it is never
 * emitted as a chunk.
 *
 * That guarantee is exactly one static `import` away from being false. A
 * component that imports `look` for a debug panel, or a refactor that hoists
 * the import to the top of `main.tsx` for tidiness, links the module into the
 * main graph and ships it — and nothing would look wrong, because in dev
 * everything still works. So the invariant is asserted on the source rather
 * than trusted to review.
 *
 * Asserted on source, not on the built bundle, deliberately: grepping
 * `src/serve/assets/app.js` would prove the stronger thing and would fail
 * whenever the assets are a rebuild behind the source, which is most of the
 * time while someone is working.
 */

const SRC = join(__dirname, "..");

function sourceFiles(dir: string): string[] {
  return readdirSync(dir).flatMap((entry) => {
    const full = join(dir, entry);
    if (statSync(full).isDirectory()) return sourceFiles(full);
    return /\.tsx?$/.test(entry) ? [full] : [];
  });
}

describe("the dev inspector never reaches a build", () => {
  it("nothing outside src/dev imports it except the guarded hook in main.tsx", () => {
    const offenders: string[] = [];

    for (const file of sourceFiles(SRC)) {
      const rel = file.slice(SRC.length + 1).replace(/\\/g, "/");
      if (rel.startsWith("dev/")) continue;

      readFileSync(file, "utf8")
        .split("\n")
        .forEach((line, i) => {
          if (!/["']\.{1,2}\/dev\/\w+["']/.test(line)) return;
          // `main.tsx` is the one door, and it may only use the two forms that
          // survive dead-code elimination: a dynamic `import()` (dropped with
          // its branch) or an `import type` (erased outright).
          const allowed =
            rel === "main.tsx" && (line.includes("import(") || line.trimStart().startsWith("import type"));
          if (!allowed) offenders.push(`${rel}:${i + 1}  ${line.trim().slice(0, 90)}`);
        });
    }

    expect(
      offenders,
      `src/dev/ is dev-only. A static import links it into the main graph and\n` +
        `ships the debug surface inside the binary. Reach it through the guarded\n` +
        `dynamic import in main.tsx, or not at all.\n\n` +
        offenders.join("\n"),
    ).toEqual([]);
  });

  it("main.tsx loads it behind import.meta.env.DEV", () => {
    const main = readFileSync(join(SRC, "main.tsx"), "utf8");
    const guard = main.indexOf("import.meta.env.DEV");
    const load = main.indexOf('import("./dev/inspect")');

    expect(guard, "main.tsx must gate the dev import on import.meta.env.DEV").toBeGreaterThan(-1);
    expect(load, "main.tsx must load ./dev/inspect dynamically").toBeGreaterThan(-1);
    // Order is the assertion: the literal `false` Vite substitutes only kills
    // the import if the import sits inside the branch it opens.
    expect(guard).toBeLessThan(load);
  });
});

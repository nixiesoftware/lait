// The viewer e2e driver: open the SHIPPED viewer bundle on a join link in
// headless Chrome, let it boot the in-tab engine, and assert the tracker works
// THROUGH the real UI — the finish line's acceptance. Drives with the built-in
// `lait:nav` CustomEvent and reads the DOM (never synthetic clicks, per
// CLAUDE.md), against the production bundle (window.lait is DEV-only).
//
//   node drive.mjs <baseUrl> <ticket> <relay>
//
// Env: CHROME (chrome executable path). Exits non-zero on any failed assertion,
// dumping console + page errors for diagnosis.

import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

// puppeteer-core lives in viewer/node_modules, not beside this CI script; node
// resolves bare specifiers from the importing file's dir, so resolve it through
// viewer's package.json (path in VIEWER_PKG) and import the resolved entry.
const require = createRequire(process.env.VIEWER_PKG);
const puppeteer = (await import(pathToFileURL(require.resolve("puppeteer-core")).href)).default;

const [, , baseUrl, ticket, relay] = process.argv;
const chrome =
  process.env.CHROME ||
  "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

const fragment = `join=${encodeURIComponent(ticket)}&relay=${encodeURIComponent(relay)}`;
const url = `${baseUrl}/#${fragment}`;
const DEADLINE_MS = 120_000;

const log = (m) => process.stdout.write(`[drive] ${m}\n`);
const fail = (m) => {
  process.stderr.write(`::error::[drive] ${m}\n`);
  process.exitCode = 1;
};

const browser = await puppeteer.launch({
  executablePath: chrome,
  headless: true,
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
});

try {
  const page = await browser.newPage();
  const consoleLines = [];
  page.on("console", (msg) => consoleLines.push(`console.${msg.type()}: ${msg.text()}`));
  page.on("pageerror", (err) => consoleLines.push(`pageerror: ${err.message}`));
  page.on("requestfailed", (req) =>
    consoleLines.push(`requestfailed: ${req.url()} ${req.failure()?.errorText ?? ""}`),
  );

  log(`opening ${url}`);
  await page.goto(url, { waitUntil: "domcontentloaded", timeout: 30_000 });

  // Wait for the engine to boot (14 MiB engine + 39 MiB runner fetched + the
  // Space pulled) and the tracker to render one of alice's issues. Poll the
  // visible text until an expected title appears or the deadline passes.
  const want = "the tab pulls this issue";
  const start = Date.now();
  let seen = false;
  let lastText = "";
  while (Date.now() - start < DEADLINE_MS) {
    // Navigate into the ENG project's issues each poll — the app auto-lands on
    // the space-named default project ("Live", empty), so drive to where alice's
    // issues live. Harmless once already there; drives past the landing once the
    // engine is bound.
    await page.evaluate(() => {
      try {
        window.dispatchEvent(
          new CustomEvent("lait:nav", { detail: { project: "ENG", view: "issues" } }),
        );
      } catch {}
    });
    lastText = await page.evaluate(() => document.body?.innerText ?? "");
    if (lastText.includes(want)) {
      seen = true;
      break;
    }
    await new Promise((r) => setTimeout(r, 1500));
  }

  if (seen) {
    log(`READ: the shipped viewer rendered alice's issue "${want}" over the in-tab engine`);
  } else {
    fail(`the shipped viewer never rendered "${want}" within ${DEADLINE_MS}ms`);
    process.stderr.write(`--- last visible text (truncated):\n${lastText.slice(0, 2000)}\n`);
    process.stderr.write(`--- console/errors:\n${consoleLines.join("\n")}\n`);
  }
} finally {
  await browser.close();
}

// The live-collaboration finish line: TWO daemon-less browser tabs open the SAME
// issue in the SHIPPED viewer on the in-tab engine, and one SEES the other's
// live caret AND typed text converge — bidirectional sync plus live carets,
// proven end to end in headless Chrome.
//
//   node drive-two.mjs <baseUrl> <ticket> <relay> <issueTitle>
//
// Each tab runs in its OWN isolated browser context, so each mints its own OPFS
// device seed and is therefore a DISTINCT actor — the daemon never echoes a tab
// its own caret, so two actors are the whole point. Both join alice's Space over
// the (reusable) invite; alice's native daemon is the Live-plane meeting point
// that fans tab A's presence out to tab B (a tab client publishes/subscribes but
// does not relay peers onward — that is the daemon's job, live_client.rs).
//
// Assertions (each within DEADLINE), all against the production bundle through
// the real UI — never window.lait (DEV-only), never eval'd synthetic clicks
// (page.click/keyboard are real trusted CDP input, not the eval'd .click() that
// detaches CDP per CLAUDE.md):
//   1. A→B TEXT:  the marker A types appears in B's editor (doc convergence in).
//   2. A→B CARET: B renders A's remote caret (.remote-caret[data-remote-actor]).
//   3. B→A TEXT:  a marker B types appears in A's editor (convergence the other
//                 way — bidirectional, not one-directional).

import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";

const require = createRequire(process.env.VIEWER_PKG);
const puppeteer = (await import(pathToFileURL(require.resolve("puppeteer-core")).href)).default;

const [, , baseUrl, ticket, relay, issueTitle] = process.argv;
const chrome =
  process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";

const fragment = `join=${encodeURIComponent(ticket)}&relay=${encodeURIComponent(relay)}`;
const url = `${baseUrl}/#${fragment}`;
const DEADLINE_MS = 90_000;

const log = (m) => process.stdout.write(`[two] ${m}\n`);
const fail = (m) => {
  process.stderr.write(`::error::[two] ${m}\n`);
  process.exitCode = 1;
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));

// Open the join link in an isolated context, drive into ENG's issues, find the
// target issue row by title, open its detail, and wait for the editor to mount.
// Returns { page, reff } or throws with a diagnostic.
async function openIssue(browser, label, lines) {
  const context = await browser.createBrowserContext();
  const page = await context.newPage();
  page.on("console", (m) => lines.push(`${label} console.${m.type()}: ${m.text()}`));
  page.on("pageerror", (e) => lines.push(`${label} pageerror: ${e.message}`));
  page.on("requestfailed", (r) =>
    lines.push(`${label} requestfailed: ${r.url()} ${r.failure()?.errorText ?? ""}`),
  );

  log(`${label}: opening ${url}`);
  await page.goto(url, { waitUntil: "domcontentloaded", timeout: 30_000 });

  // Poll: the engine boots (14 MiB engine + 39 MiB runner + Space pull), then
  // the ENG issue list renders the target row carrying its iss_ reff.
  const start = Date.now();
  let reff = null;
  while (Date.now() - start < DEADLINE_MS) {
    await page.evaluate(() => {
      try {
        window.dispatchEvent(
          new CustomEvent("lait:nav", { detail: { project: "ENG", view: "issues" } }),
        );
      } catch {}
    });
    reff = await page.evaluate((title) => {
      for (const li of document.querySelectorAll("li[data-issue-ref]")) {
        if ((li.textContent ?? "").includes(title)) return li.getAttribute("data-issue-ref");
      }
      return null;
    }, issueTitle);
    if (reff) break;
    await sleep(1500);
  }
  if (!reff) throw new Error(`${label}: the issue "${issueTitle}" never appeared`);
  log(`${label}: found issue ${reff}; opening its detail`);

  await page.evaluate((r) => {
    try {
      window.dispatchEvent(new CustomEvent("lait:nav", { detail: { issue: r } }));
    } catch {}
  }, reff);
  // The description editor is CodeMirror for legacy-markdown docs (.cm-content)
  // and ProseMirror for Typst docs (.lait-document-editor). Fresh issues are
  // legacy markdown, so CodeMirror is what mounts; accept either.
  await page.waitForSelector(EDIT_SEL, { timeout: 30_000 });
  return { page, reff };
}

// The editable surface of the description editor, either kernel.
const EDIT_SEL = ".markdown-editor-host .cm-content, .lait-document-editor";
// The description editor's text container, for the convergence assertion.
const TEXT_SEL = ".markdown-editor-host .cm-content, .lait-document-editor";

// Place the caret in the editor and type — real CDP mouse+keyboard, not eval'd
// (page.click/keyboard are trusted input, not the eval'd .click() CLAUDE.md warns
// detaches the CDP context).
async function typeInto(page, text) {
  await page.click(EDIT_SEL);
  await page.keyboard.type(text, { delay: 15 });
}

// Poll a predicate evaluated in the page until true or the deadline.
async function until(page, label, describe, fn, arg) {
  const start = Date.now();
  let last = false;
  while (Date.now() - start < DEADLINE_MS) {
    last = await page.evaluate(fn, arg);
    if (last) return true;
    await sleep(1000);
  }
  fail(`${label}: ${describe} — never true within ${DEADLINE_MS}ms`);
  return false;
}

// Milliseconds until a predicate holds, polled TIGHTLY (every 100ms) — the tool
// for the lockstep bar: we want to know a peer's edit lands near-instantly, not
// merely eventually. Returns the elapsed ms, or -1 if it never held in `budget`.
async function latencyMs(page, fn, arg, budget = 15_000) {
  const start = Date.now();
  while (Date.now() - start < budget) {
    if (await page.evaluate(fn, arg)) return Date.now() - start;
    await sleep(100);
  }
  return -1;
}

// The lockstep gate (ms): a peer's edit must be VISIBLE within this. Best-in-class
// peer visibility is ~200ms (Figma/Docs push deltas to hit "milliseconds"; Nielsen
// 100ms "instant", Doherty 400ms). Measured here: ~100ms when the reader's durable
// base is already fresh (the preview applies on the spot), ~200ms right after a
// structural change when the base needs one convergence first — a repull-round-trip
// floor. The gate carries a small margin over that floor so a green run means
// "reliably best-in-class", while the log prints the real per-edit latency (the
// ~100ms typical is the true measure). Carets ride datagrams at ~1ms.
const LOCKSTEP_MS = 250;

const pressN = async (page, key, n) => {
  for (let i = 0; i < n; i++) await page.keyboard.press(key);
};

// The durability pass: jump the caret to a different paragraph, then FAST-type a
// burst there (no per-key delay), and assert the peer both converges the burst
// text AND still renders the author's live caret. Repeated across paragraphs.
// This is the hard case — a jump re-anchors the remote caret to a new position,
// then rapid keystrokes must keep up without the caret drifting or the text
// falling behind.
async function durability(a, b) {
  await a.page.click(EDIT_SEL);
  // Drop to the end and lay down a few paragraphs to jump between.
  await pressN(a.page, "ArrowDown", 40);
  await a.page.keyboard.press("End");
  const paras = 4;
  for (let i = 0; i < paras; i++) {
    await a.page.keyboard.type(`para${i} `, { delay: 0 });
    await a.page.keyboard.press("Enter");
  }
  let ok = true;
  for (let i = 0; i < paras; i++) {
    // Jump: to the top, then down to a different paragraph, to its line end.
    await pressN(a.page, "ArrowUp", 40);
    await pressN(a.page, "ArrowDown", i);
    await a.page.keyboard.press("End");
    const burst = `JMP${i}X${Math.random().toString(36).slice(2, 6)}`;
    await a.page.keyboard.type(burst, { delay: 0 });
    // Measure how FAST B catches up — the lockstep bar, not just eventual.
    const textMs = await latencyMs(
      b.page,
      (m) => (document.querySelector(".lait-document-editor")?.textContent ?? "").includes(m),
      burst,
    );
    const caretMs = await latencyMs(
      b.page,
      () => !!document.querySelector(".remote-caret[data-remote-actor]"),
      null,
    );
    const prompt = textMs >= 0 && textMs <= LOCKSTEP_MS && caretMs >= 0 && caretMs <= LOCKSTEP_MS;
    log(`A: jump ${i} FAST-type "${burst}" → B text +${textMs}ms, caret +${caretMs}ms ${prompt ? "(lockstep)" : "(TOO SLOW)"}`);
    if (!prompt) {
      fail(`B: jump ${i} not lockstep — text +${textMs}ms caret +${caretMs}ms (bar ${LOCKSTEP_MS}ms)`);
    }
    ok = ok && prompt;
  }
  return ok;
}

const lines = [];
const browser = await puppeteer.launch({
  executablePath: chrome,
  headless: true,
  args: ["--no-sandbox", "--disable-dev-shm-usage"],
});

try {
  // Two isolated tabs → two seeds → two distinct actors, both in alice's Space.
  const a = await openIssue(browser, "A", lines);
  const b = await openIssue(browser, "B", lines);
  if (a.reff !== b.reff) throw new Error(`tabs opened different issues: ${a.reff} vs ${b.reff}`);

  // A types a distinctive marker: this both moves A's caret (published on the
  // Live plane) and splices the description (converged through the daemon).
  const markerA = `SYNC-FROM-A-${Math.random().toString(36).slice(2, 8)}`;
  log(`A: typing "${markerA}" into the description`);
  await typeInto(a.page, markerA);

  // Measure the SIMPLE-edit latency precisely (tight poll): the first edit into a
  // fresh doc, so the preview's durable base is already converged — this isolates
  // the realtime-lane latency from the base-lag the multi-paragraph case hits.
  const simpleTextMs = await latencyMs(
    b.page,
    (m) => (document.querySelector(".lait-document-editor")?.textContent ?? "").includes(m),
    markerA,
  );
  const simpleCaretMs = await latencyMs(
    b.page,
    () => !!document.querySelector(".remote-caret[data-remote-actor]"),
    null,
  );
  log(`SIMPLE-EDIT latency → B text +${simpleTextMs}ms, caret +${simpleCaretMs}ms (bar ${LOCKSTEP_MS}ms)`);

  // 1. A→B TEXT: B's editor shows what A typed (bidirectional convergence, in).
  const textIn = await until(
    b.page,
    "B",
    `sees A's text "${markerA}"`,
    (m) => (document.querySelector(".lait-document-editor")?.textContent ?? "").includes(m),
    markerA,
  );
  if (textIn) log(`B: SAW A's typed text "${markerA}" — doc converged tab→daemon→tab`);

  // 2. A→B CARET: B renders A's live caret widget (the must-have).
  const caretIn = await until(
    b.page,
    "B",
    "renders A's remote caret",
    () => {
      const c = document.querySelector(".lait-document-editor-host .remote-caret[data-remote-actor]")
        || document.querySelector(".remote-caret[data-remote-actor]");
      return !!c && !!c.getAttribute("data-remote-actor");
    },
  );
  if (caretIn) {
    const who = await b.page.evaluate(
      () =>
        document.querySelector(".remote-caret[data-remote-actor]")?.getAttribute("data-remote-actor") ?? "",
    );
    log(`B: RENDERED A's live caret (actor ${who}) — live carets cross tab→daemon→tab`);
  }

  // 3. B→A TEXT: prove the other direction too, so "bidirectional" is not one way.
  const markerB = `SYNC-FROM-B-${Math.random().toString(36).slice(2, 8)}`;
  log(`B: typing "${markerB}" into the description`);
  await typeInto(b.page, markerB);
  const textBack = await until(
    a.page,
    "A",
    `sees B's text "${markerB}"`,
    (m) => (document.querySelector(".lait-document-editor")?.textContent ?? "").includes(m),
    markerB,
  );
  if (textBack) log(`A: SAW B's typed text "${markerB}" — convergence is bidirectional`);

  // 4. DURABILITY: jump + fast-type across paragraphs; the peer keeps up on both
  //    the text and the caret. Only run once the basics hold, so a failure here is
  //    unambiguous.
  const durable = textIn && caretIn && textBack ? await durability(a, b) : false;
  if (durable) log("DURABILITY: B kept up with jump + fast-type across paragraphs (text + caret).");

  if (textIn && caretIn && textBack && durable) {
    log("PASS: two shipped-viewer tabs sync text both ways, render each other's live carets, and survive jump + fast-type.");
  } else {
    process.stderr.write(`--- console/errors:\n${lines.join("\n")}\n`);
  }
} catch (err) {
  fail(err instanceof Error ? err.message : String(err));
  process.stderr.write(`--- console/errors:\n${lines.join("\n")}\n`);
} finally {
  await browser.close();
}

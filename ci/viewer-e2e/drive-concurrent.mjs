// Characterize CONCURRENT simultaneous editing: A and B both type into the same
// description at the same time. Best-in-class: both peers' text survives on BOTH
// tabs (CRDT merge), and neither peer's own typing is lost when the other's edit
// converges in. Reports what actually happens.
//   node drive-concurrent.mjs <baseUrl> <ticket> <relay> <issueTitle>
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
const require = createRequire(process.env.VIEWER_PKG);
const puppeteer = (await import(pathToFileURL(require.resolve("puppeteer-core")).href)).default;
const [, , baseUrl, ticket, relay, issueTitle] = process.argv;
const chrome = process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const url = `${baseUrl}/#join=${encodeURIComponent(ticket)}&relay=${encodeURIComponent(relay)}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (m) => process.stdout.write(`[concurrent] ${m}\n`);
const EDIT = ".markdown-editor-host .cm-content, .lait-document-editor";

async function open(browser, label) {
  const ctx = await browser.createBrowserContext();
  const page = await ctx.newPage();
  await page.goto(url, { waitUntil: "domcontentloaded", timeout: 30_000 });
  let reff = null;
  for (let i = 0; i < 40 && !reff; i++) {
    await page.evaluate(() => window.dispatchEvent(new CustomEvent("lait:nav", { detail: { project: "ENG", view: "issues" } })));
    reff = await page.evaluate((t) => { for (const li of document.querySelectorAll("li[data-issue-ref]")) if ((li.textContent ?? "").includes(t)) return li.getAttribute("data-issue-ref"); return null; }, issueTitle);
    if (!reff) await sleep(1500);
  }
  await page.evaluate((r) => window.dispatchEvent(new CustomEvent("lait:nav", { detail: { issue: r } })), reff);
  await page.waitForSelector(EDIT, { timeout: 30_000 });
  log(`${label}: opened ${reff}`);
  return { page };
}
const text = (page) => page.evaluate((s) => document.querySelector(s)?.textContent ?? "", EDIT);

const browser = await puppeteer.launch({ executablePath: chrome, headless: true, args: ["--no-sandbox", "--disable-dev-shm-usage"] });
try {
  const a = await open(browser, "A");
  const b = await open(browser, "B");
  // Seed a little so both have a shared base, then let it settle.
  await a.page.click(EDIT);
  await a.page.keyboard.type("Shared start. ", { delay: 25 });
  await sleep(2500);

  // Both type their own distinctive burst AT THE SAME TIME.
  const mA = `AAA${Math.random().toString(36).slice(2, 6)}`;
  const mB = `BBB${Math.random().toString(36).slice(2, 6)}`;
  log(`A types "${mA}" and B types "${mB}" concurrently`);
  await Promise.all([
    (async () => { await a.page.click(EDIT); for (const c of mA) { await a.page.keyboard.type(c, { delay: 60 }); } })(),
    (async () => { await b.page.click(EDIT); for (const c of mB) { await b.page.keyboard.type(c, { delay: 60 }); } })(),
  ]);
  await sleep(4000); // settle

  const at = await text(a.page), bt = await text(b.page);
  log(`A editor: ${JSON.stringify(at.slice(0, 120))}`);
  log(`B editor: ${JSON.stringify(bt.slice(0, 120))}`);
  const aHasOwn = at.includes(mA), aHasPeer = at.includes(mB);
  const bHasOwn = bt.includes(mB), bHasPeer = bt.includes(mA);
  log(`A: own=${aHasOwn} peer=${bHasPeer ? "(n/a)" : ""}${aHasPeer}  |  B: own=${bHasOwn} peer=${bHasPeer}`);
  if (aHasOwn && aHasPeer && bHasOwn && bHasPeer) log("PASS: concurrent edits merged — both peers' text on both tabs.");
  else log(`FAIL: lost edits — A(own=${aHasOwn},peer=${aHasPeer}) B(own=${bHasOwn},peer=${bHasPeer})`);
} finally {
  await browser.close();
}

// Verify the read-only fix: after actor A has typed (sentences + Enter), a second
// tab B opens the same issue and must be EDITABLE — no "cannot edit safely" /
// Normalize lock — and able to type.
//   node drive-editcheck.mjs <baseUrl> <ticket> <relay> <issueTitle>
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
const require = createRequire(process.env.VIEWER_PKG);
const puppeteer = (await import(pathToFileURL(require.resolve("puppeteer-core")).href)).default;
const [, , baseUrl, ticket, relay, issueTitle] = process.argv;
const chrome = process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const url = `${baseUrl}/#join=${encodeURIComponent(ticket)}&relay=${encodeURIComponent(relay)}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (m) => process.stdout.write(`[editcheck] ${m}\n`);
const EDIT = ".markdown-editor-host .cm-content, .lait-document-editor";

const browser = await puppeteer.launch({ executablePath: chrome, headless: true, args: ["--no-sandbox", "--disable-dev-shm-usage"] });
try {
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
  await sleep(3000); // let A's converged text settle

  const state = await page.evaluate(() => {
    const body = document.body.innerText || "";
    const editor = document.querySelector(".lait-document-editor");
    return {
      readOnlyBanner: body.includes("cannot edit safely") || body.includes("Read-only") || body.includes("Normalize"),
      contentEditable: editor?.getAttribute("contenteditable"),
      text: (editor?.textContent ?? "").slice(0, 80),
    };
  });
  log(`readOnlyBanner=${state.readOnlyBanner} contentEditable=${state.contentEditable} text=${JSON.stringify(state.text)}`);

  // Try to actually edit as B: focus + type a marker, confirm it lands.
  const marker = `BEDIT${Math.random().toString(36).slice(2, 6)}`;
  await page.click(EDIT);
  await page.keyboard.type(marker, { delay: 20 });
  await sleep(500);
  const landed = await page.evaluate((m) => (document.querySelector(".lait-document-editor")?.textContent ?? "").includes(m), marker);
  log(`B could type "${marker}": ${landed}`);
  if (!state.readOnlyBanner && state.contentEditable === "true" && landed) log("PASS: B is editable after A's typing (no read-only lock).");
  else log("FAIL: B is not properly editable.");
} finally {
  await browser.close();
}

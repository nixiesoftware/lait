// Visual-pass helper: open ONE tab (actor A) in its own browser, open the issue,
// and edit continuously — move the caret around, type sentences, select — so a
// human (or Claude) watching the SAME issue in another browser sees the live
// caret, name label, selection and preview in motion. Holds open until killed.
//   node drive-typer.mjs <baseUrl> <ticket> <relay> <issueTitle>
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
const require = createRequire(process.env.VIEWER_PKG);
const puppeteer = (await import(pathToFileURL(require.resolve("puppeteer-core")).href)).default;
const [, , baseUrl, ticket, relay, issueTitle] = process.argv;
const chrome = process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const url = `${baseUrl}/#join=${encodeURIComponent(ticket)}&relay=${encodeURIComponent(relay)}`;
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const log = (m) => process.stdout.write(`[typer] ${m}\n`);
const EDIT = ".markdown-editor-host .cm-content, .lait-document-editor";

const browser = await puppeteer.launch({
  executablePath: chrome, headless: true, args: ["--no-sandbox", "--disable-dev-shm-usage"],
});
const ctx = await browser.createBrowserContext();
const page = await ctx.newPage();
await page.goto(url, { waitUntil: "domcontentloaded", timeout: 30_000 });
let reff = null;
for (let i = 0; i < 40 && !reff; i++) {
  await page.evaluate(() => window.dispatchEvent(new CustomEvent("lait:nav", { detail: { project: "ENG", view: "issues" } })));
  reff = await page.evaluate((t) => { for (const li of document.querySelectorAll("li[data-issue-ref]")) if ((li.textContent ?? "").includes(t)) return li.getAttribute("data-issue-ref"); return null; }, issueTitle);
  if (!reff) await sleep(1500);
}
if (!reff) { log("issue not found"); process.exit(1); }
await page.evaluate((r) => window.dispatchEvent(new CustomEvent("lait:nav", { detail: { issue: r } })), reff);
await page.waitForSelector(EDIT, { timeout: 30_000 });
await page.click(EDIT);
log(`editing ${reff} as actor A — watch the other browser`);

const lines = [
  "The quick brown fox jumps over the lazy dog. ",
  "Lockstep collaboration should feel instant. ",
  "A remote caret glides where I type. ",
];
for (let round = 0; ; round++) {
  const line = lines[round % lines.length];
  await page.keyboard.type(line, { delay: 45 }); // human-ish speed
  await page.keyboard.press("Enter");
  await sleep(700);
  // Occasionally jump the caret up and select, to show caret motion + selection.
  if (round % 3 === 2) {
    await page.keyboard.press("ArrowUp");
    await page.keyboard.down("Shift");
    await page.keyboard.press("End");
    await page.keyboard.up("Shift");
    await sleep(900);
    await page.keyboard.press("ArrowDown");
    await page.keyboard.press("End");
  }
  await sleep(500);
}

// Daemon-less hosting acceptance: parity, durability, multi-session — against
// the DEPLOYED foundation.pub/i and the real bucket, no daemon anywhere.
//
//   GATEWAY, BUCKET default to prod; pass a fresh work dir for the profiles.
//   VIEWER_PKG=<viewer/package.json> node ci/acceptance-daemonless.mjs
//
// PARITY       a bare visit founds a working Space and a founder write commits
//              (create a project) — the same app a daemon would host.
// DURABILITY   the founded Space's snapshot lands in the public bucket.
// MULTI-SESSION a second session of the same identity (relaunch, same profile)
//              recovers the Space and the write — state outlived the first tab.
import { createRequire } from "node:module";
import { pathToFileURL } from "node:url";
import { execFileSync } from "node:child_process";
import { mkdtempSync } from "node:fs";
import { tmpdir } from "node:os";
import { join } from "node:path";
const require = createRequire(process.env.VIEWER_PKG);
const puppeteer = (await import(pathToFileURL(require.resolve("puppeteer-core")).href)).default;
const CHROME = process.env.CHROME || "/Applications/Google Chrome.app/Contents/MacOS/Google Chrome";
const BUCKET = process.env.BUCKET || "gs://the-foundation-snapshots";
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const profile = mkdtempSync(join(tmpdir(), "lait-accept-"));
let failures = 0;
const check = (name, ok, detail = "") => { console.log(`${ok ? "PASS" : "FAIL"}  ${name}${detail ? " — " + detail : ""}`); if (!ok) failures++; };

async function session(label) {
  const browser = await puppeteer.launch({
    executablePath: CHROME, headless: true, userDataDir: profile,
    args: ["--no-sandbox", "--disable-dev-shm-usage"],
    defaultViewport: { width: 1456, height: 837 },
  });
  const page = await browser.newPage();
  let foundedSpace = null;
  const root = await page.target().createCDPSession();
  await root.send("Target.setAutoAttach", { autoAttach: true, waitForDebuggerOnStart: true, flatten: true });
  root.on("Target.attachedToTarget", async ({ sessionId, targetInfo }) => {
    const s = root.connection().session(sessionId); if (!s) return;
    try {
      await s.send("Runtime.enable");
      s.on("Runtime.consoleAPICalled", (e) => {
        const t = (e.args || []).map(a => a.value ?? a.description ?? "").join(" ");
        const m = t.match(/founded Space (ws_\w+)/); if (m) foundedSpace = m[1];
      });
      await s.send("Runtime.runIfWaitingForDebugger").catch(() => {});
    } catch {}
  });
  await page.goto("https://foundation.pub/i", { waitUntil: "domcontentloaded", timeout: 30000 });
  return { browser, page, space: () => foundedSpace };
}

const bucketCount = () => {
  try {
    const ls = execFileSync("gcloud", ["storage", "ls", `${BUCKET}/spaces/`], { encoding: "utf8" });
    return ls.trim().split("\n").filter(Boolean).length;
  } catch { return -1; }
};

const clickLast = (page, text) => page.evaluate((t) => {
  const els = [...document.querySelectorAll("button,[role=button]")].filter(e => e.textContent.trim() === t && e.offsetParent !== null);
  const el = els[els.length - 1]; if (!el) return false;
  const r = el.getBoundingClientRect(); el.dispatchEvent(new MouseEvent("mousedown", { bubbles: true })); return { x: r.x + r.width / 2, y: r.y + r.height / 2 };
}, text);
const bodyText = (page) => page.evaluate(() => (document.body?.innerText || "").replace(/\s+/g, " "));

// ---- Session 1: found + write (PARITY), then publish (DURABILITY) ----
const bucketBefore = bucketCount();
const s1 = await session("s1");
let space = null;
for (let i = 0; i < 80; i++) { space = s1.space(); if (space && /Create project/.test(await bodyText(s1.page))) break; await sleep(1000); }
check("PARITY: bare visit founds a Space", !!space, space || "no found");

// Create a project via real mouse events (React honors them).
const openRect = await clickLast(s1.page, "Create project");
if (openRect) await s1.page.mouse.click(openRect.x, openRect.y);
await sleep(1500);
const nameRect = await s1.page.evaluate(() => { const el = document.querySelector("input[type=text], input:not([type])"); if (!el) return null; const r = el.getBoundingClientRect(); return { x: r.x + r.width / 2, y: r.y + r.height / 2 }; });
if (nameRect) await s1.page.mouse.click(nameRect.x, nameRect.y);
await sleep(300);
await s1.page.keyboard.type("Parity Project");
await sleep(400);
const submit = await clickLast(s1.page, "Create project");
if (submit) await s1.page.mouse.click(submit.x, submit.y);
await sleep(3000);
const s1text = await bodyText(s1.page);
check("PARITY: a founder write commits (project created)", /Parity Project/.test(s1text) && /projects\//.test(s1.page.url()), s1.page.url());

// Give the boot publish + heartbeat a moment.
await sleep(8000);
await s1.browser.close();

// DURABILITY: this session added a snapshot to the public bucket. The object
// key is a one-way digest of the Space id (a read-privacy capability), so the
// harness cannot name it directly — but a fresh founded Space is a NEW object,
// so the count growing is this session's publish landing.
const bucketAfter = bucketCount();
check(
  "DURABILITY: this session published a snapshot to the public bucket",
  bucketBefore >= 0 && bucketAfter > bucketBefore,
  `bucket ${bucketBefore} -> ${bucketAfter}`,
);

// ---- Session 2: same profile, a new tab-session recovers state ----
const s2 = await session("s2");
let recovered = "";
for (let i = 0; i < 60; i++) { recovered = await bodyText(s2.page); if (/Parity Project/.test(recovered)) break; await sleep(1000); }
check("MULTI-SESSION: a new session recovers the Space and the write", /Parity Project/.test(recovered), s2.space() || "");
check("MULTI-SESSION: it is the SAME Space", s2.space() === space, `${space} vs ${s2.space()}`);
await s2.browser.close();

console.log(failures === 0 ? "\nACCEPTANCE: all green" : `\nACCEPTANCE: ${failures} failed`);
process.exit(failures === 0 ? 0 : 1);

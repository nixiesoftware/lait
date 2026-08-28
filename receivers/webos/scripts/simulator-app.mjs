// The store package, pointed at a local deployment, for the LG simulator.
//
// `package/` is the shipped launcher: an appinfo and a page that sends the
// television to the hosted receiver at the production root. The simulator
// wants the same shape pointed at the deployment `local-site.mjs` serves,
// so this writes that — a copy of the package with one URL changed — under
// `dist/simulator/`, which the simulator can launch as an app directory.
//
//   node scripts/simulator-app.mjs [--root localtest.me] [--port 443]

import { copyFile, mkdir, readFile, writeFile } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const here = path.dirname(fileURLToPath(import.meta.url));
const source = path.resolve(here, "..", "package");
const target = path.resolve(here, "..", "dist", "simulator");

const options = { root: "localtest.me", port: 443 };
const argv = process.argv.slice(2);
for (let index = 0; index < argv.length; index += 2) {
  const key = argv[index].replace(/^--/, "");
  if (!(key in options)) {
    console.error(`unknown option --${key}`);
    process.exit(2);
  }
  options[key] = key === "port" ? Number(argv[index + 1]) : argv[index + 1];
}
const suffix = options.port === 443 ? "" : `:${options.port}`;
const local = `https://astrolabe.${options.root}${suffix}/display/`;

await mkdir(target, { recursive: true });
for (const name of ["appinfo.json", "icon.png", "largeIcon.png", "splashBackground.png"]) {
  await copyFile(path.join(source, name), path.join(target, name));
}
const page = await readFile(path.join(source, "index.html"), "utf8");
await writeFile(
  path.join(target, "index.html"),
  page.replaceAll("https://astrolabe.foundation.pub/display/", local),
);
console.log(`${target}\n  -> ${local}`);

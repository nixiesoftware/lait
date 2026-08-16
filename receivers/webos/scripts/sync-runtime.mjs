import { copyFile, mkdir, readdir } from "node:fs/promises";
import path from "node:path";
import { fileURLToPath } from "node:url";

const scriptDirectory = path.dirname(fileURLToPath(import.meta.url));
const source = path.resolve(scriptDirectory, "..", "..", "shared", "web");
const destination = path.resolve(scriptDirectory, "..", "hosted", "runtime");

await mkdir(destination, { recursive: true });
for (const entry of await readdir(source, { withFileTypes: true })) {
  if (entry.isFile() && entry.name.endsWith(".mjs")) {
    await copyFile(path.join(source, entry.name), path.join(destination, entry.name));
  }
}

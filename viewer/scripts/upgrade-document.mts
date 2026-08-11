import { applySourceSplices, upgradeMarkdown } from "../src/core/document";

const encoded = process.env.LAIT_DOCUMENT_SOURCE_B64;
if (encoded === undefined) {
  throw new Error("LAIT_DOCUMENT_SOURCE_B64 is required");
}

const source = Buffer.from(encoded, "base64").toString("utf8");
const upgrade = upgradeMarkdown(source);
if (applySourceSplices(source, upgrade.splices) !== upgrade.source) {
  throw new Error("document upgrade splices do not reproduce the upgraded source");
}
process.stdout.write(JSON.stringify(upgrade));

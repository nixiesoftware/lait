/**
 * Which Astryx integrations this app loads.
 *
 * `@lait/ui` is ours — the components Astryx has no opinion about. Listing it
 * here is what makes them first-class: `astryx component --list` shows them
 * beside core's, and `astryx upgrade` runs their codemods alongside core's.
 */
import type { AstryxConfig } from "@astryxdesign/cli/authoring";

const config: AstryxConfig = {
  integrations: ["@lait/ui"],
};

export default config;

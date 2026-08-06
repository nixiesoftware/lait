/**
 * lait's contributions to Astryx.
 *
 * WHY A PACKAGE AND NOT A FOLDER. Everything in here is something Astryx has no
 * opinion about — a tracker's board, its timeline, the glyphs that encode
 * priority and status as shape. Left in `viewer/src/ui` those are components
 * that merely coexist with the design system. Declared as an integration they
 * become part of it: they show up in `astryx component --list`, they carry
 * their own docs for the same CLI (and therefore the same agents) that document
 * core's, and they ship their own codemods so our divergence gets the upgrade
 * machinery Astryx has for itself.
 *
 * That last point is the whole argument. The alternative to this file is
 * `astryx swizzle` — forking a component out of core, where no codemod can
 * reach it again. This is the door that stays open.
 *
 * Identity (name, version) comes from `package.json`, not from here.
 */
import type { AstryxIntegration } from "@astryxdesign/cli/authoring";

const integration: AstryxIntegration = {
  components: "./components",
  codemods: "./codemods",
};

export default integration;

import { StrictMode } from "react";
import { createRoot } from "react-dom/client";

import { registerIcons } from "@astryxdesign/core/Icon";

import { App } from "./App";
import { contribute, registry } from "./core/registry";
import { trackWindowControls } from "./core/windowControls";
import type { Inspect } from "./dev/inspect";
import { WorldViewStoreProvider } from "./core/worldViewReact";
import { ProjectViewerStore, ProjectViewerStoreProvider } from "./projectStore";
import { laitIcons } from "./theme/icons";
import "./styles.css";

/**
 * Astryx resolves `<Icon icon="funnel" />` through a registry, not an import,
 * so whoever fills it decides what every Astryx component's chrome looks like.
 * Fill it with lucide before the first render and the migrated surfaces draw
 * from the same set the rest of the app already uses.
 *
 * Global rather than `defineTheme({icons})` on purpose: `laitTheme.ts` is
 * generated and compiled by `astryx theme build`, and putting JSX in that path
 * means the icon set is only as portable as the CLI's loader. The theme's own
 * registry still wins where a theme declares one — ours does not, so this is
 * the only source.
 */
registerIcons(laitIcons);

/**
 * The seam, reachable from outside the bundle.
 *
 * Without this, "extensible" would mean "fork `viewer/` and rebuild", which is
 * not extensibility — it is a patch. The client ships as a compiled bundle inside
 * a Rust binary, so the only way a third party can add a command or rebind a key
 * *at runtime* is a handle on the registry. This is it.
 *
 * It is deliberately the same `contribute` the core uses for every one of its own
 * features. Nothing here is a special path for outsiders; there is one door and
 * everyone walks through it.
 *
 * Reaching it today means a userscript or the console — the page is same-origin
 * and served by the engine, so there is no third-party script tag to add and (by
 * design, given the `Origin` allowlist) no remote code loading. A first-class
 * extension host — user JS read from the config dir and served same-origin — is
 * the natural next step, and it would land on this same API.
 */
declare global {
  interface Window {
    lait: { contribute: typeof contribute; registry: typeof registry } & Partial<Inspect>;
  }
}

window.lait = { contribute, registry };

/**
 * The measuring tools, on the same handle and only in dev.
 *
 * `import.meta.env.DEV` is replaced with a literal `false` at build time, so
 * this branch — and the dynamic import inside it — are eliminated before the
 * bundle reaches `products/issues-app/assets/web/` and enters the immutable
 * Issues release. A shipped release has no debug surface; the type is `Partial`
 * because that absence is the truth about the shipped object, not a convenience.
 *
 * Why they live here at all: driving this app from outside means either taking
 * pictures of it or asking it questions, and pictures cost about four hundred
 * times what an answer does. See `dev/inspect.ts` for the four defect classes
 * that turned out to be numbers all along.
 */
if (import.meta.env.DEV) {
  void import("./dev/inspect").then(({ inspect }) => Object.assign(window.lait, inspect));
}

/**
 * Before the first paint, not during it.
 *
 * The shell spends `--window-controls-top` as padding on its two columns, so a
 * value that arrives a frame late is a frame of the app drawn underneath the
 * close button and then jumping out from under it. The host declares it before
 * this script runs, and restates it on the rare occasion it changes, so this
 * belongs on the element rather than in a hook that re-decides it on every
 * render — no surface should have to hold an opinion about the window frame to
 * lay itself out.
 */
trackWindowControls(document.documentElement);

const root = document.getElementById("root");
if (!root) throw new Error("#root missing from index.html");
const projectStore = new ProjectViewerStore();

createRoot(root).render(
  <StrictMode>
    <WorldViewStoreProvider store={projectStore.resources}>
      <ProjectViewerStoreProvider store={projectStore}>
        <App />
      </ProjectViewerStoreProvider>
    </WorldViewStoreProvider>
  </StrictMode>,
);

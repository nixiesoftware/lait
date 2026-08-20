/**
 * The browser-side spelling of the primary Rust/Flutter client boundary.
 *
 * `ClientView` is deliberately a whole snapshot. The core already publishes
 * snapshots rather than patches; retaining that rule keeps the web client
 * from growing a second, subtly divergent state model.
 */
export interface LibraryWorld {
  key: string;
  worldMount: string;
  displayName: string;
  opensAt: string | null;
  version: number | null;
  tagline: string | null;
  /** Packed 0xRRGGBB, as supplied by the bundled World. */
  accent: number | null;
  people: WorldPerson[] | null;
  update: WorldUpdate | null;
}

export interface WorldPerson {
  name: string;
  picture: string | null;
  presence: "online" | "away" | "offline" | null;
  agent: boolean;
  here: boolean;
}

export interface WorldUpdate {
  serving: string | null;
  available: string | null;
  behind: boolean;
  unmet: string[] | null;
}

export interface Head {
  id: string;
  kind: string;
  origin: string | null;
  owned: boolean;
  orbit: string | null;
}

export interface HostFacts {
  version: string;
  identityHome: string;
  spacesRoot: string;
  orbitCount: number;
}

export interface Notice {
  said: string;
  launched: string | null;
}

export interface Failure {
  what: string;
  error: string;
  retryable: boolean;
}

export type Staleness =
  | { kind: "neverLoaded" }
  | { kind: "signalled"; reason: string };

/** The portion of the core's ClientView consumed by the primary window. */
export interface ClientView {
  loading: boolean;
  stale: Staleness | null;
  /** null means unread; [] means authoritatively empty. */
  library: LibraryWorld[] | null;
  host: HostFacts | null;
  heads: Head[];
  notices: Notice[];
  failures: Failure[];
  /** Core action keys, not UI-local flags. */
  inFlight: string[];
}

export type ClientAction =
  | { type: "refresh" }
  | { type: "open"; entryPath: string }
  | { type: "updateWorld"; world: string }
  | { type: "stopHead"; id: string };

export const actionKey = {
  refresh: "refresh",
  open: (entryPath: string) => `open:${entryPath}`,
  updateWorld: (world: string) => `world.update:${world}`,
  stopHead: (id: string) => `head.stop:${id}`,
} as const;

export function keyFor(action: ClientAction): string {
  switch (action.type) {
    case "refresh": return actionKey.refresh;
    case "open": return actionKey.open(action.entryPath);
    case "updateWorld": return actionKey.updateWorld(action.world);
    case "stopHead": return actionKey.stopHead(action.id);
  }
}

/**
 * Implemented by the desktop host. `dispatch` returns the immediate snapshot
 * from the core, just as Flutter's generated bridge does; `watch` supplies
 * every later whole snapshot.
 */
export interface AstrolabeClientBridge {
  current(): Promise<ClientView>;
  watch(listener: (view: ClientView) => void): () => void;
  dispatch(action: ClientAction): Promise<ClientView>;
}

declare global {
  interface Window {
    __ASTROLABE_CLIENT__?: AstrolabeClientBridge;
  }
}

export interface ClientTransport extends AstrolabeClientBridge {
  readonly mode: "host" | "tauri" | "fixture" | "unavailable";
}

export const loadingClientView: ClientView = {
  loading: true,
  stale: { kind: "neverLoaded" },
  library: null,
  host: null,
  heads: [],
  notices: [],
  failures: [],
  inFlight: [],
};

/**
 * The standalone build never silently pretends that it has a local identity.
 * The fixture is a development transport only; packaged desktop builds must
 * inject `window.__ASTROLABE_CLIENT__` before this bundle runs.
 */
export function createClientTransport(): ClientTransport {
  const bridge = window.__ASTROLABE_CLIENT__;
  if (bridge !== undefined) return {
    mode: "host",
    current: () => bridge.current(),
    watch: (listener) => bridge.watch(listener),
    dispatch: (action) => bridge.dispatch(action),
  };
  if (isTauri()) return createTauriTransport();
  if (import.meta.env.DEV) return createFixtureTransport();
  return createUnavailableTransport();
}

/** The production desktop adapter. Tauri transports facts; Astrolabe owns them. */
function createTauriTransport(): ClientTransport {
  return {
    mode: "tauri",
    current: () => invoke<ClientView>("client_current"),
    dispatch: (action) => invoke<ClientView>("client_dispatch", { action }),
    watch(listener) {
      let active = true;
      let unlisten: (() => void) | undefined;
      void listen<ClientView>("astrolabe://client-view", (event) => {
        if (active) listener(event.payload);
      }).then((stop) => {
        if (active) unlisten = stop;
        else stop();
      });
      return () => { active = false; unlisten?.(); };
    },
  };
}

/** A small, stateful development implementation of the real client protocol. */
export function createFixtureTransport(initial = fixtureClientView): ClientTransport {
  let view = initial;
  const listeners = new Set<(next: ClientView) => void>();
  const publish = (next: ClientView) => {
    view = next;
    listeners.forEach((listener) => listener(view));
  };
  const complete = (key: string, apply: (current: ClientView) => ClientView) => {
    window.setTimeout(() => publish(apply({ ...view, inFlight: view.inFlight.filter((item) => item !== key) })), 500);
  };

  return {
    mode: "fixture",
    current: async () => view,
    watch(listener) {
      listeners.add(listener);
      return () => listeners.delete(listener);
    },
    async dispatch(action) {
      const key = keyFor(action);
      if (view.inFlight.includes(key)) return view;
      publish({ ...view, inFlight: [...view.inFlight, key] });

      switch (action.type) {
        case "refresh":
          complete(key, (current) => current);
          break;
        case "open":
          complete(key, (current) => ({
            ...current,
            heads: current.heads.some((head) => head.orbit === null)
              ? current.heads
              : [...current.heads, {
                  id: "identity:fixture",
                  kind: "browser",
                  origin: "http://127.0.0.1:52713/",
                  owned: true,
                  orbit: null,
                }],
            notices: [{ said: "World is ready in your browser.", launched: action.entryPath }, ...current.notices],
          }));
          break;
        case "stopHead":
          complete(key, (current) => ({
            ...current,
            heads: current.heads.filter((head) => head.id !== action.id),
            notices: [{ said: "Stopped the local browser head.", launched: null }, ...current.notices],
          }));
          break;
        case "updateWorld":
          complete(key, (current) => ({
            ...current,
            library: current.library?.map((world) => world.worldMount === action.world
              ? { ...world, update: world.update === null ? null : { ...world.update, behind: false, serving: world.update.available } }
              : world) ?? null,
            notices: [{ said: `Updated ${action.world}.`, launched: null }, ...current.notices],
          }));
          break;
      }
      return view;
    },
  };
}

function createUnavailableTransport(): ClientTransport {
  const unavailable: ClientView = {
    ...loadingClientView,
    loading: false,
    stale: { kind: "signalled", reason: "No desktop client bridge was supplied." },
    failures: [{
      what: "Connect to local identity",
      error: "The desktop host did not provide an Astrolabe client bridge.",
      retryable: false,
    }],
  };
  return {
    mode: "unavailable",
    current: async () => unavailable,
    watch: () => () => undefined,
    dispatch: async () => unavailable,
  };
}

/** The bundled Issues World is development data, not the application model. */
export const fixtureClientView: ClientView = {
  loading: false,
  stale: null,
  library: [{
    key: "issues",
    worldMount: "issues",
    displayName: "Issues",
    opensAt: "/",
    version: null,
    tagline: null,
    accent: 0x5b8def,
    people: [],
    update: null,
  }],
  heads: [],
  host: null,
  inFlight: [],
  failures: [],
  notices: [],
};
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

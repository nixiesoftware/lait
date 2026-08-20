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

export interface Device {
  id: string; label: string; state: string; owned: boolean; degraded: string | null;
  home: string; pid: number | null; canForceStop: boolean; lastError: string | null;
}

export interface Storage { orbit: string; name: string | null; bytesOnDisk: number | null; objectCount: number | null; lastVerifiedMs: number | null; missing: "notPlaced" | "unreachable" | null; }
export interface Orbit { space: string; name: string; path: string; lastOpened: number | null; }
export interface Member { id: string; nick: string | null; authoredName: string | null; admin: boolean; }
export interface Gate { id: string; label: string; state: "pass" | "wait" | "fail" | "warn" | "skip"; detail: string; }
export interface Space { space: string; whoami: string | null; admin: boolean; members: Member[]; devices: string[]; diagnosis: { gates: Gate[]; blockedOn: string | null; summary: string } | null; }

export interface Card {
  card: string; name: string; note: string; handles: string[]; addresses: string[];
  devices: string[]; agents: string[]; picture: string | null; groups: string[];
  selfClaim: boolean; presence: "online" | "away" | "offline" | null;
}
export interface Suggestion { suggestion: string; name: string; note: string; handles: string[]; }
export interface Book { cards: Card[]; migrationComplete: boolean; migrationPending: number; migrationImported: number; suggestions: Suggestion[]; }

export type DisplayTheme = "light" | "dark" | "highContrast";
export type DisplaySyncMode = "stayInSync" | "positional";
export type DisplayStaleAction = "keepWithNativeBanner" | "blank";
export interface DisplaySurface { world: string; surface: string; title: string; contractVersion: number; outputs: string[]; }
export interface DisplayHealth { revision: string; currentItem: string; elapsedMs: number; connection: string; playback: string; lastError: string; stagedItems: number; stagedBytes: number; driftResidualMs: number; correctionEvents: number; pipelineUnobservable: boolean; }
export interface DisplayReceiver { device: string; label: string; platform: string; build: string; issuedAtUnixMs: number; revokedAtUnixMs: number | null; health: DisplayHealth | null; }
export interface DisplayAssignment { assignment: string; device: string; orbit: string; space: string; program: string; world: string; surface: string; controller: string; theme: DisplayTheme; syncGroup: string | null; syncMode: DisplaySyncMode | null; staticDelayMs: number; expiresAtUnixMs: number | null; revokedAtUnixMs: number | null; }
export interface DisplayPairing { pairing: string; confirmationPhrase: string[]; certificateSha256: string; platform: string; build: string; createdAtUnixMs: number; expiresAtUnixMs: number; }
export interface Display { instance: string; label: string; origin: string; certificateSha256: string; certificatePem: string; surfaces: DisplaySurface[]; devices: DisplayReceiver[]; assignments: DisplayAssignment[]; pendingPairings: DisplayPairing[]; }
export interface McpBinding { path: string; detail: string; note: string | null; replaced: boolean; agent: string | null; written: boolean; world: string | null; }

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
  display: Display | null;
  heads: Head[];
  devices: Device[];
  storage: Storage[];
  orbits: Orbit[];
  space: Space | null;
  book: Book | null;
  mcp: McpBinding | null;
  notices: Notice[];
  failures: Failure[];
  /** Core action keys, not UI-local flags. */
  inFlight: string[];
}

export type ClientAction =
  | { type: "refresh" }
  | { type: "open"; entryPath: string }
  | { type: "updateWorld"; world: string }
  | { type: "startDevice"; id: string } | { type: "stopDevice"; id: string } | { type: "restartDevice"; id: string } | { type: "forceStopDevice"; id: string }
  | { type: "stopAllOwned" } | { type: "removeDevice"; id: string; deleteData: boolean } | { type: "readSpace"; orbit: string }
  | { type: "startHead" } | { type: "stopHead"; id: string } | { type: "forgetOrbit"; space: string }
  | { type: "bookPut"; card: string | null; name: string; note: string | null } | { type: "bookDelete"; card: string }
  | { type: "bookSetPicture"; card: string; path: string | null } | { type: "bookMerge"; from: string; into: string }
  | { type: "bookClaimSelf"; card: string } | { type: "bookLink"; card: string; handle: string } | { type: "bookUnlink"; card: string; handle: string }
  | { type: "bookExport"; path: string; cards: string[] | null } | { type: "bookImport"; path: string }
  | { type: "bookAccept"; suggestion: string } | { type: "bookDismiss"; suggestion: string }
  | { type: "installMcp"; client: string; scope: string | null; name: string; agent: string | null; noAgent: boolean; project: string; world: string | null; preview: boolean }
  | { type: "displayPairingApprove"; pairing: string; label: string } | { type: "displayPairingReject"; pairing: string }
  | { type: "displayAssignmentPut"; device: string; orbit: string; world: string; surface: string; inputJson: string; theme: DisplayTheme; staleAfterMs: number; onStale: DisplayStaleAction; syncGroup: string | null; syncMode: DisplaySyncMode; staticDelayMs: number; expiresAtUnixMs: number | null }
  | { type: "displayAssignmentRevoke"; assignment: string } | { type: "displayDeviceRevoke"; device: string };

export const actionKey = {
  refresh: "refresh",
  open: (entryPath: string) => `open:${entryPath}`,
  updateWorld: (world: string) => `world.update:${world}`,
  startDevice: (id: string) => `device.start:${id}`,
  stopDevice: (id: string) => `device.stop:${id}`,
  restartDevice: (id: string) => `device.restart:${id}`,
  forceStopDevice: (id: string) => `device.force-stop:${id}`,
  removeDevice: (id: string) => `device.remove:${id}`,
  readSpace: (orbit: string) => `space.read:${orbit}`,
  startHead: "head.start",
  stopHead: (id: string) => `head.stop:${id}`,
  forgetOrbit: (space: string) => `orbit.forget:${space}`,
  bookPut: (card: string | null) => card === null ? "book.put" : `book.put:${card}`,
  bookDelete: (card: string) => `book.delete:${card}`,
  bookSetPicture: (card: string) => `book.picture:${card}`,
  bookMerge: (from: string, into: string) => `book.merge:${from}:${into}`,
  bookClaimSelf: (card: string) => `book.claim:${card}`,
  bookLink: (card: string) => `book.link:${card}`,
  bookUnlink: (card: string) => `book.unlink:${card}`,
  bookExport: "book.export", bookImport: "book.import",
  bookAccept: (suggestion: string) => `book.accept:${suggestion}`,
  bookDismiss: (suggestion: string) => `book.dismiss:${suggestion}`,
  installMcp: (preview: boolean) => preview ? "mcp.preview" : "mcp.install",
  displayPairingApprove: (pairing: string) => `display.pairing.approve:${pairing}`,
  displayPairingReject: (pairing: string) => `display.pairing.reject:${pairing}`,
  displayAssignmentPut: (device: string) => `display.assignment.put:${device}`,
  displayAssignmentRevoke: (assignment: string) => `display.assignment.revoke:${assignment}`,
  displayDeviceRevoke: (device: string) => `display.device.revoke:${device}`,
} as const;

export function keyFor(action: ClientAction): string {
  switch (action.type) {
    case "refresh": return actionKey.refresh;
    case "open": return actionKey.open(action.entryPath);
    case "updateWorld": return actionKey.updateWorld(action.world);
    case "startDevice": return actionKey.startDevice(action.id);
    case "stopDevice": return actionKey.stopDevice(action.id);
    case "restartDevice": return actionKey.restartDevice(action.id);
    case "forceStopDevice": return actionKey.forceStopDevice(action.id);
    case "stopAllOwned": return "device.stop-all";
    case "removeDevice": return actionKey.removeDevice(action.id);
    case "readSpace": return actionKey.readSpace(action.orbit);
    case "startHead": return actionKey.startHead;
    case "stopHead": return actionKey.stopHead(action.id);
    case "forgetOrbit": return actionKey.forgetOrbit(action.space);
    case "bookPut": return actionKey.bookPut(action.card);
    case "bookDelete": return actionKey.bookDelete(action.card);
    case "bookSetPicture": return actionKey.bookSetPicture(action.card);
    case "bookMerge": return actionKey.bookMerge(action.from, action.into);
    case "bookClaimSelf": return actionKey.bookClaimSelf(action.card);
    case "bookLink": return actionKey.bookLink(action.card);
    case "bookUnlink": return actionKey.bookUnlink(action.card);
    case "bookExport": return actionKey.bookExport;
    case "bookImport": return actionKey.bookImport;
    case "bookAccept": return actionKey.bookAccept(action.suggestion);
    case "bookDismiss": return actionKey.bookDismiss(action.suggestion);
    case "installMcp": return actionKey.installMcp(action.preview);
    case "displayPairingApprove": return actionKey.displayPairingApprove(action.pairing);
    case "displayPairingReject": return actionKey.displayPairingReject(action.pairing);
    case "displayAssignmentPut": return actionKey.displayAssignmentPut(action.device);
    case "displayAssignmentRevoke": return actionKey.displayAssignmentRevoke(action.assignment);
    case "displayDeviceRevoke": return actionKey.displayDeviceRevoke(action.device);
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

/** The two Flutter-owned top-level surfaces. They are singleton OS windows. */
export type OwnedWindowSurface = "book" | "displays";

export function currentOwnedWindowSurface(location = window.location): OwnedWindowSurface | null {
  const surface = new URLSearchParams(location.search).get("surface");
  return surface === "book" || surface === "displays" ? surface : null;
}

/**
 * Ask the native host to create or restore the owned window. The browser
 * preview mirrors that shape with a named popup, never by replacing Library.
 */
export async function summonOwnedWindow(surface: OwnedWindowSurface): Promise<void> {
  if (isTauri()) {
    await invoke("summon_owned_window", { surface });
    return;
  }
  if (import.meta.env.DEV) {
    const url = new URL(window.location.href);
    url.searchParams.set("surface", surface);
    window.open(url, `astrolabe-${surface}`, surface === "book" ? "width=370,height=760" : "width=860,height=720");
  }
}

export async function closeOwnedWindow(): Promise<void> {
  if (isTauri()) {
    const { getCurrentWebviewWindow } = await import("@tauri-apps/api/webviewWindow");
    await getCurrentWebviewWindow().close();
    return;
  }
  window.close();
}

export const loadingClientView: ClientView = {
  loading: true,
  stale: { kind: "neverLoaded" },
  library: null,
  host: null,
  display: null,
  heads: [],
  devices: [],
  storage: [],
  orbits: [],
  space: null,
  book: null,
  mcp: null,
  notices: [],
  failures: [],
  inFlight: [],
};

/**
 * The standalone build never silently pretends that it has a local identity.
 * The fixture is a development transport only; packaged desktop builds use
 * the Tauri command/event bridge (or an embedding host bridge) before this
 * bundle considers the development fallback.
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
        // The fixture only models the library launch path.  The remaining
        // actions still complete so every desktop destination can be explored
        // without leaving its controls permanently pending.
        default:
          complete(key, (current) => current);
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
  display: null,
  devices: [],
  storage: [],
  orbits: [],
  space: null,
  book: { cards: [], migrationComplete: true, migrationPending: 0, migrationImported: 0, suggestions: [] },
  mcp: null,
  inFlight: [],
  failures: [],
  notices: [],
};
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";

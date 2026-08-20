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

/**
 * A person's correspondence, drawn as conversations rather than an inbox.
 * Which tabs are open, which is focused, and every message are shared state —
 * the same view the address book reads, so a click there opens the tab the
 * chat window draws.
 */
export interface CorrespondenceFacts {
  /** This identity's own device id on the plane, or null until it is known. */
  myDevice: string | null;
  contacts: Contact[];
  conversations: Conversation[];
  openTabs: string[];
  activeTab: string | null;
}
export interface Contact {
  id: string;
  name: string;
  devices: string[];
  /** In the book (a friend) vs an unadded stranger who wrote first. */
  added: boolean;
  isAgent: boolean;
  parentId: string | null;
  parentName: string | null;
  /** Unread received messages — the badge. Zero once opened. */
  unread: number;
}
export interface Conversation { peerId: string; peerName: string; messages: ChatMessage[]; }
export interface ChatMessage {
  mine: boolean;
  /** `message` (text) or `invitation` — each drawn with its own component. */
  kind: string;
  body: string | null;
  /** When it was written, unix seconds. */
  sentAt: number;
  fromDevice: string;
  /** Whether the carrier's word matched the proof — surfaced, never hidden. */
  provenanceAgrees: boolean;
}

/** Big Picture: this machine as a screen. Present while the mode is entered. */
export interface PresentationFacts {
  chosen: PresentationChoice | null;
  /** The last verified render, kept across a failed re-ask so a screen goes stale rather than dark. */
  program: PresentedProgram | null;
  /** Why the last attempt did not answer — beside `program`, never instead of it. */
  failure: string | null;
}
export interface PresentationChoice { orbit: string; world: string; surface: string; title: string; }
export interface PresentedProgram {
  /** `current`, `partial`, or `unavailable`. */
  assessment: string;
  partialReasons: string[];
  /** `hold_last`, `loop`, `poll_at_end`, or `blank_at_end`. */
  cycle: string;
  refreshAfterMs: number | null;
  items: PresentedItem[];
}
export interface PresentedItem {
  id: string;
  durationMs: number | null;
  assessment: string;
  spokenSummary: string | null;
  scene: PresentedScene;
}
export type PresentedScene =
  | { kind: "frame"; uri: string; width: number; height: number }
  | { kind: "blank"; reason: string }
  | { kind: "unsupported"; output: string };

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
  correspondence: CorrespondenceFacts | null;
  presentation: PresentationFacts | null;
  notices: Notice[];
  failures: Failure[];
  /** Core action keys, not UI-local flags. */
  inFlight: string[];
}

export type ClientAction =
  | { type: "refresh" }
  | { type: "open"; world: string; entryPath: string }
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
  | { type: "displayAssignmentRevoke"; assignment: string } | { type: "displayDeviceRevoke"; device: string }
  | { type: "sendMessage"; to: string; body: string } | { type: "collectMail" }
  | { type: "blockSender"; person: string } | { type: "acceptContact"; person: string }
  | { type: "openConversation"; person: string } | { type: "focusConversation"; person: string }
  | { type: "closeConversation"; person: string }
  | { type: "enterPresentation" }
  | { type: "presentHere"; orbit: string; world: string; surface: string; input: string; title: string }
  | { type: "presentRefresh" } | { type: "leavePresentation" };

export const actionKey = {
  refresh: "refresh",
  open: (world: string) => `open:${world}`,
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
  sendMessage: (to: string) => `correspondence.send:${to}`,
  collectMail: "correspondence.collect",
  blockSender: (person: string) => `correspondence.block:${person}`,
  acceptContact: (person: string) => `correspondence.accept:${person}`,
  openConversation: (person: string) => `correspondence.open:${person}`,
  focusConversation: (person: string) => `correspondence.focus:${person}`,
  closeConversation: (person: string) => `correspondence.close:${person}`,
  enterPresentation: "present.enter",
  presentHere: "present.choose",
  presentRefresh: "present.refresh",
  leavePresentation: "present.leave",
} as const;

export function keyFor(action: ClientAction): string {
  switch (action.type) {
    case "refresh": return actionKey.refresh;
    case "open": return actionKey.open(action.world);
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
    case "sendMessage": return actionKey.sendMessage(action.to);
    case "collectMail": return actionKey.collectMail;
    case "blockSender": return actionKey.blockSender(action.person);
    case "acceptContact": return actionKey.acceptContact(action.person);
    case "openConversation": return actionKey.openConversation(action.person);
    case "focusConversation": return actionKey.focusConversation(action.person);
    case "closeConversation": return actionKey.closeConversation(action.person);
    case "enterPresentation": return actionKey.enterPresentation;
    case "presentHere": return actionKey.presentHere;
    case "presentRefresh": return actionKey.presentRefresh;
    case "leavePresentation": return actionKey.leavePresentation;
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

/** The three Flutter-owned top-level surfaces. They are singleton OS windows. */
export type OwnedWindowSurface = "book" | "displays" | "chat";

export function currentOwnedWindowSurface(location = window.location): OwnedWindowSurface | null {
  const surface = new URLSearchParams(location.search).get("surface");
  return surface === "book" || surface === "displays" || surface === "chat" ? surface : null;
}

/**
 * A World-settings window is deliberately separate from the client: it
 * receives a read-only snapshot at open time and never watches the core.
 * The snapshot crosses in the window's own URL — the web spelling of the
 * Flutter client's `--world-settings=` argv payload.
 */
export interface WorldSettingsSnapshot {
  key: string;
  name: string;
  worldMount: string;
  entryPath: string | null;
  version: number | null;
  activeOrigin: string | null;
  dark: boolean;
}

export function encodeWorldSettingsSnapshot(snapshot: WorldSettingsSnapshot): string {
  const bytes = new TextEncoder().encode(JSON.stringify(snapshot));
  let binary = "";
  bytes.forEach((byte) => { binary += String.fromCharCode(byte); });
  return btoa(binary).replaceAll("+", "-").replaceAll("/", "_").replace(/=+$/, "");
}

export function decodeWorldSettingsSnapshot(encoded: string): WorldSettingsSnapshot | null {
  try {
    const binary = atob(encoded.replaceAll("-", "+").replaceAll("_", "/"));
    const bytes = Uint8Array.from(binary, (char) => char.charCodeAt(0));
    const value: unknown = JSON.parse(new TextDecoder().decode(bytes));
    if (typeof value !== "object" || value === null) return null;
    const record = value as Record<string, unknown>;
    if (typeof record.key !== "string" || typeof record.name !== "string"
      || typeof record.worldMount !== "string" || typeof record.dark !== "boolean") return null;
    return {
      key: record.key,
      name: record.name,
      worldMount: record.worldMount,
      entryPath: typeof record.entryPath === "string" ? record.entryPath : null,
      version: typeof record.version === "number" ? record.version : null,
      activeOrigin: typeof record.activeOrigin === "string" ? record.activeOrigin : null,
      dark: record.dark,
    };
  } catch {
    return null;
  }
}

export function currentWorldSettingsSnapshot(location = window.location): WorldSettingsSnapshot | null {
  const params = new URLSearchParams(location.search);
  if (params.get("surface") !== "world-settings") return null;
  const encoded = params.get("snapshot");
  return encoded === null ? null : decodeWorldSettingsSnapshot(encoded);
}

/** Summon (or refocus) the per-World settings window carrying this snapshot. */
export async function summonWorldSettings(snapshot: WorldSettingsSnapshot): Promise<void> {
  const encoded = encodeWorldSettingsSnapshot(snapshot);
  if (isTauri()) {
    await invoke("summon_world_settings", { key: snapshot.key, name: snapshot.name, snapshot: encoded });
    return;
  }
  if (import.meta.env.DEV) {
    const url = new URL(window.location.href);
    url.searchParams.set("surface", "world-settings");
    url.searchParams.set("snapshot", encoded);
    window.open(url, `astrolabe-world-settings-${snapshot.key}`, "width=560,height=680");
  }
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
    const shape = surface === "book" ? "width=370,height=760"
      : surface === "chat" ? "width=760,height=660"
      : "width=860,height=720";
    window.open(url, `astrolabe-${surface}`, shape);
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

/**
 * The artwork one World ships, as data URIs. Not part of the view, and the
 * omission is the core's design: artwork is a build constant that cannot
 * change while the process runs, so a surface asks once per mount and this
 * module caches the answer for the life of the window.
 */
export interface WorldArtwork {
  mark: string | null;
  hero: string | null;
}

const noArtwork: WorldArtwork = { mark: null, hero: null };
const artworkCache = new Map<string, Promise<WorldArtwork>>();

export function worldArtwork(mount: string): Promise<WorldArtwork> {
  const cached = artworkCache.get(mount);
  if (cached !== undefined) return cached;
  // An unknown mount — or a host with no artwork command — answers with no
  // artwork, not an error: the accent plate is a first-class face.
  const asked = isTauri()
    ? invoke<WorldArtwork>("world_artwork", { mount }).catch(() => noArtwork)
    : Promise.resolve(noArtwork);
  artworkCache.set(mount, asked);
  return asked;
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
  correspondence: null,
  presentation: null,
  notices: [],
  failures: [],
  inFlight: [],
};

/**
 * Native application-menu choices forwarded by the desktop host — the items
 * that act on the model (refresh, theme) rather than summoning a window.
 * Inert outside the desktop host, which is the only place a native menu is.
 */
export function watchMenu(listener: (id: string) => void): () => void {
  if (!isTauri()) return () => undefined;
  let active = true;
  let unlisten: (() => void) | undefined;
  void listen<string>("astrolabe://menu", (event) => {
    if (active) listener(event.payload);
  }).then((stop) => {
    if (active) unlisten = stop;
    else stop();
  });
  return () => { active = false; unlisten?.(); };
}

/**
 * Whether the desktop host owns the display. There, fullscreen is a window
 * fact nothing in the page can revoke; in a browser it is a grant that can be
 * refused or taken back, and the surface has to watch it.
 */
export function hostOwnsFullscreen(): boolean {
  return isTauri();
}

/**
 * Big Picture takes the display, not the work area. The desktop host does it
 * at the window; a browser grants it only inside a user gesture, so callers
 * invoke this in the press itself. A refusal leaves the surface windowed —
 * still drawn, and offering the retake control.
 */
export async function setFullscreen(fullscreen: boolean): Promise<void> {
  if (isTauri()) {
    await invoke("set_fullscreen", { fullscreen });
    return;
  }
  try {
    if (fullscreen && document.fullscreenElement === null) {
      await document.documentElement.requestFullscreen();
    } else if (!fullscreen && document.fullscreenElement !== null) {
      await document.exitFullscreen();
    }
  } catch {
    // Refused or unavailable: the page stays a window; the surface still
    // fills it and keeps offering fullscreen from its own control.
  }
}

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
        case "openConversation":
          complete(key, (current) => withCorrespondence(current, (facts) => ({
            ...facts,
            openTabs: facts.openTabs.includes(action.person) ? facts.openTabs : [...facts.openTabs, action.person],
            activeTab: action.person,
            contacts: facts.contacts.map((contact) => contact.id === action.person ? { ...contact, unread: 0 } : contact),
            conversations: facts.conversations.some((conversation) => conversation.peerId === action.person)
              ? facts.conversations
              : [...facts.conversations, {
                  peerId: action.person,
                  peerName: facts.contacts.find((contact) => contact.id === action.person)?.name ?? action.person,
                  messages: [],
                }],
          })));
          break;
        case "focusConversation":
          complete(key, (current) => withCorrespondence(current, (facts) => ({ ...facts, activeTab: action.person })));
          break;
        case "closeConversation":
          complete(key, (current) => withCorrespondence(current, (facts) => {
            const openTabs = facts.openTabs.filter((tab) => tab !== action.person);
            return { ...facts, openTabs, activeTab: facts.activeTab === action.person ? openTabs[0] ?? null : facts.activeTab };
          }));
          break;
        case "sendMessage":
          complete(key, (current) => withCorrespondence(current, (facts) => ({
            ...facts,
            conversations: facts.conversations.map((conversation) => conversation.peerId === action.to
              ? {
                  ...conversation,
                  messages: [...conversation.messages, {
                    mine: true, kind: "message", body: action.body,
                    sentAt: Math.floor(Date.now() / 1000), fromDevice: "dev_this", provenanceAgrees: true,
                  }],
                }
              : conversation),
          })));
          break;
        case "acceptContact":
          complete(key, (current) => withCorrespondence(current, (facts) => ({
            ...facts,
            contacts: facts.contacts.map((contact) => contact.id === action.person ? { ...contact, added: true } : contact),
          })));
          break;
        case "blockSender":
          complete(key, (current) => withCorrespondence(current, (facts) => ({
            ...facts,
            contacts: facts.contacts.filter((contact) => contact.id !== action.person),
            conversations: facts.conversations.filter((conversation) => conversation.peerId !== action.person),
            openTabs: facts.openTabs.filter((tab) => tab !== action.person),
            activeTab: facts.activeTab === action.person ? null : facts.activeTab,
          })));
          break;
        case "enterPresentation":
          complete(key, (current) => ({
            ...current,
            presentation: { chosen: null, program: null, failure: null },
          }));
          break;
        case "presentHere":
          complete(key, (current) => ({
            ...current,
            presentation: {
              chosen: { orbit: action.orbit, world: action.world, surface: action.surface, title: action.title },
              program: {
                assessment: "current",
                partialReasons: [],
                cycle: "hold_last",
                refreshAfterMs: null,
                items: [{
                  id: "itm_fixture",
                  durationMs: null,
                  assessment: "current",
                  spokenSummary: null,
                  scene: {
                    kind: "frame",
                    uri: "data:image/png;base64,iVBORw0KGgoAAAANSUhEUgAAAAEAAAABCAYAAAAfFcSJAAAADUlEQVR42mNkYPhfDwAChwGA60e6kgAAAABJRU5ErkJggg==",
                    width: 1,
                    height: 1,
                  },
                }],
              },
              failure: null,
            },
          }));
          break;
        case "leavePresentation":
          complete(key, (current) => ({ ...current, presentation: null }));
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

function withCorrespondence(view: ClientView, apply: (facts: CorrespondenceFacts) => CorrespondenceFacts): ClientView {
  return view.correspondence === null ? view : { ...view, correspondence: apply(view.correspondence) };
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
    people: [
      { name: "Ada", picture: null, presence: "online", agent: false, here: true },
      { name: "Scribe", picture: null, presence: "away", agent: true, here: false },
      { name: "Cole", picture: null, presence: null, agent: false, here: false },
      { name: "Brin", picture: null, presence: "offline", agent: false, here: false },
    ],
    update: null,
  }],
  heads: [],
  host: {
    version: "0.0.0-fixture",
    identityHome: "/home/fixture/.lait",
    spacesRoot: "/home/fixture/.lait/spaces",
    orbitCount: 1,
  },
  display: {
    instance: "dsp_fixture",
    label: "This device",
    origin: "https://192.168.1.20:7443",
    certificateSha256: "9f86d081884c7d659a2feaa0c55ad015a3bf4f1b2b0b822cd15d6c15b0f00a08",
    certificatePem: "-----BEGIN CERTIFICATE-----\nFIXTURE\n-----END CERTIFICATE-----",
    surfaces: [
      { world: "issues", surface: "board", title: "Issue board", contractVersion: 1, outputs: [] },
      { world: "com.lait.signage", surface: "signage.program", title: "Signage program", contractVersion: 1, outputs: [] },
    ],
    devices: [{
      device: "rcv_lounge",
      label: "Lounge TV",
      platform: "android_tv",
      build: "1.4.2",
      issuedAtUnixMs: 1_755_000_000_000,
      revokedAtUnixMs: null,
      health: {
        revision: "rev_9", currentItem: "itm_standup", elapsedMs: 5_400, connection: "connected",
        playback: "playing", lastError: "none", stagedItems: 4, stagedBytes: 8_192,
        driftResidualMs: 12, correctionEvents: 2, pipelineUnobservable: false,
      },
    }],
    assignments: [{
      assignment: "asg_lounge", device: "rcv_lounge", orbit: "orb_fixture", space: "orb_fixture",
      program: "prg_standup", world: "issues", surface: "board", controller: "ctl_fixture",
      theme: "dark", syncGroup: "lobby-wall", syncMode: "stayInSync", staticDelayMs: 0,
      expiresAtUnixMs: null, revokedAtUnixMs: null,
    }],
    pendingPairings: [{
      pairing: "pair_kitchen",
      confirmationPhrase: ["ember", "quartz", "linen", "harbor", "violet", "spruce"],
      certificateSha256: "2c26b46b68ffc68ff99b453c1d30413413422d706483bfa0f98a5e886266e7ae",
      platform: "fire_tv",
      build: "1.4.0",
      createdAtUnixMs: 1_755_000_000_000,
      expiresAtUnixMs: 1_755_000_600_000,
    }],
  },
  devices: [],
  storage: [],
  orbits: [{ space: "orb_fixture", name: "Fixture Space", path: "/home/fixture/.lait/spaces/orb_fixture", lastOpened: null }],
  space: null,
  book: {
    cards: [
      {
        card: "crd_me", name: "You", note: "", handles: ["actor:orb_fixture:you"], addresses: ["actor:orb_fixture:you"],
        devices: ["dev_this"], agents: [], picture: null, groups: [], selfClaim: true, presence: null,
      },
      {
        card: "crd_ada", name: "Ada", note: "Met at the workshop", handles: ["actor:orb_fixture:ada"],
        addresses: ["actor:orb_fixture:ada"], devices: [], agents: [], picture: null, groups: [],
        selfClaim: false, presence: "online",
      },
      {
        card: "crd_scribe", name: "Scribe", note: "", handles: ["agent:1f2e:scribe"], addresses: [],
        devices: [], agents: ["agent:1f2e:scribe"], picture: null, groups: ["Agents"], selfClaim: false, presence: "away",
      },
      {
        card: "crd_brin", name: "Brin", note: "", handles: ["actor:orb_fixture:brin"], addresses: ["actor:orb_fixture:brin"],
        devices: [], agents: [], picture: null, groups: [], selfClaim: false, presence: "offline",
      },
    ],
    migrationComplete: true,
    migrationPending: 0,
    migrationImported: 0,
    suggestions: [{
      suggestion: "sug_cole", name: "Cole", note: "From cards.json", handles: ["actor:orb_fixture:cole"],
    }],
  },
  mcp: null,
  correspondence: {
    myDevice: "dev_this",
    contacts: [
      { id: "peer_ada", name: "Ada", devices: ["dev_ada"], added: true, isAgent: false, parentId: null, parentName: null, unread: 1 },
      { id: "peer_scribe", name: "Scribe", devices: ["dev_scribe"], added: true, isAgent: true, parentId: "peer_ada", parentName: "Ada", unread: 0 },
      { id: "peer_nix", name: "Nix", devices: ["dev_nix"], added: false, isAgent: false, parentId: null, parentName: null, unread: 2 },
    ],
    conversations: [{
      peerId: "peer_ada",
      peerName: "Ada",
      messages: [
        { mine: false, kind: "message", body: "The workshop notes are in.", sentAt: 1_755_465_600, fromDevice: "dev_ada", provenanceAgrees: true },
        { mine: true, kind: "message", body: "Reading them now.", sentAt: 1_755_465_720, fromDevice: "dev_this", provenanceAgrees: true },
        { mine: false, kind: "invitation", body: null, sentAt: 1_755_552_000, fromDevice: "dev_ada", provenanceAgrees: true },
        { mine: false, kind: "message", body: "Sent you the Space invite.", sentAt: 1_755_552_060, fromDevice: "dev_ada_phone", provenanceAgrees: false },
      ],
    }],
    // Pre-opened in the fixture so the dev chat window has a conversation to
    // draw: each browser-preview window runs its own fixture, so a click in
    // the book popup cannot reach this one the way the shared core does.
    openTabs: ["peer_ada"],
    activeTab: "peer_ada",
  },
  presentation: null,
  inFlight: [],
  failures: [],
  notices: [],
};
import { invoke, isTauri } from "@tauri-apps/api/core";
import { listen } from "@tauri-apps/api/event";
